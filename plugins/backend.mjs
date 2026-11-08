import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { resolveRuntime, supportsLifecycleStatus } from "./runtime.mjs";
import { rustEnvironment, rustTaskEnvironment } from "./environment.mjs";
import { presentApplyPreview } from "./presentation.mjs";
import { structuredReview } from "./review.mjs";
import { executionPolicy, selectExecution } from "./execution.mjs";

const OUTPUT_LIMIT = 256 * 1024;
const REVIEW_LIMIT = 8 * 1024 * 1024;
const TASK_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

function within(parent, child) {
  const relative = path.relative(parent, child);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

async function canonicalDestination(destination) {
  try {
    return await realpath(destination);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    const parent = path.dirname(destination);
    if (parent === destination) throw error;
    return path.join(await canonicalDestination(parent), path.basename(destination));
  }
}

function fields(value, allowed) {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).some((key) => !allowed.includes(key))) {
    throw new Error(`Expected an object with only: ${allowed.join(", ")}`);
  }
}

async function privateHome(project) {
  // Staging the caller's HOME would inherit the workspace FUSE mount when
  // durable stages live there. Keep native nested-mount rejection intact.
  for (const candidate of [...new Set([tmpdir(), "/tmp", "/var/tmp"])]) {
    let base;
    try { base = await realpath(candidate); }
    catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }
    if (within(project, base)) continue;
    return realpath(await mkdtemp(path.join(base, "pvisor-agent-home-")));
  }
  throw new Error("No private temporary HOME is available outside the project");
}

function taskEnvironment(home) {
  return {
    ...process.env,
    HOME: home,
    CODEX_HOME: path.join(home, ".codex"),
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"),
    XDG_STATE_HOME: path.join(home, ".local/state"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
  };
}

/** Validate exact project-relative apply selections before spawning a process. */
export function selection(paths) {
  if (!Array.isArray(paths) || paths.length === 0 || paths.length > 200 ||
      paths.some((item) => typeof item !== "string" || !item || item.includes("\0") ||
        item.includes("\\") || path.posix.isAbsolute(item) || /^[A-Za-z]:/.test(item) ||
        item.split("/").some((part) => !part || part === "." || part === ".."))) {
    throw new Error("paths must contain 1–200 relative paths without empty, '.' or '..' components");
  }
  return [...new Set(paths)];
}

/** Shared validation also lets fanout reject every malformed task before starting any. */
export function taskInput(input) {
  fields(input, ["argv", "timeout_seconds"]);
  const { argv, timeout_seconds: seconds = 120 } = input;
  if (!Array.isArray(argv) || argv.length === 0 || argv.length > 256 ||
      argv.some((item) => typeof item !== "string" || item.includes("\0")) || !argv[0]) {
    throw new Error("argv must contain 1–256 strings and a nonempty executable");
  }
  if (!Number.isInteger(seconds) || seconds < 1 || seconds > 600) {
    throw new Error("timeout_seconds must be an integer from 1 to 600");
  }
  return { argv: [...argv], timeout_seconds: seconds };
}

/** A completed transport alone does not establish task success or known completion. */
export function failed(value) {
  return value.exit_code !== 0 || value.interrupted ||
    (value.task_exit_code !== undefined && value.task_exit_code !== null && value.task_exit_code !== 0) ||
    (value.task_state !== undefined && value.task_state !== "completed") ||
    value.output_status === "unavailable" || ["blocked", "unavailable"].includes(value.preview_state) ||
    value.export_status === "unconfirmed" || value.review_status === "unavailable";
}

/**
 * Drain both streams after reaching the display budget. Cancellation requests
 * native teardown; forced CLI termination remains an uncertain native outcome.
 */
export function execute(binary, args, { cwd, env, signal, deadlineMs = 30_000, outputLimit = OUTPUT_LIMIT } = {}) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], shell: false });
    const buffers = { stdout: [], stderr: [] };
    const sizes = { stdout: 0, stderr: 0 };
    let truncated = false;
    let interrupted = false;
    let escalation;
    let drainDeadline;
    const interrupt = () => {
      if (interrupted) return;
      interrupted = true;
      child.kill("SIGTERM");
      escalation = setTimeout(() => {
        child.kill("SIGKILL");
        // A lost supervisor's descendants may retain the output descriptors.
        // Stop waiting for those streams without claiming native teardown.
        drainDeadline = setTimeout(() => {
          child.stdout.destroy();
          child.stderr.destroy();
        }, 5_000);
        drainDeadline.unref();
      }, 5_000);
      escalation.unref();
    };
    const deadline = setTimeout(interrupt, deadlineMs);
    deadline.unref();
    const cleanup = () => {
      clearTimeout(deadline);
      clearTimeout(escalation);
      clearTimeout(drainDeadline);
      signal?.removeEventListener("abort", interrupt);
    };
    signal?.addEventListener("abort", interrupt, { once: true });
    if (signal?.aborted) interrupt();
    for (const stream of ["stdout", "stderr"]) {
      child[stream].on("data", (bytes) => {
        const remaining = outputLimit - sizes[stream];
        if (bytes.length > remaining) truncated = true;
        if (remaining > 0) {
          buffers[stream].push(bytes.subarray(0, remaining));
          sizes[stream] += Math.min(remaining, bytes.length);
        }
      });
    }
    child.on("error", (error) => { cleanup(); reject(error); });
    child.on("close", (code, terminationSignal) => {
      cleanup();
      resolve({
        exit_code: code,
        signal: terminationSignal,
        interrupted,
        stdout: Buffer.concat(buffers.stdout).toString("utf8"),
        stderr: Buffer.concat(buffers.stderr).toString("utf8"),
        truncated,
      });
    });
  });
}

// Native drop deliberately refuses rollback of an applied Session. Only classify
// that as an adapter no-op after independent native reads confirm terminal,
// empty, already-applied state and a preview with no pending apply/recovery.
async function isAlreadyApplied(binary, task, signal) {
  const observed = await execute(binary, ["status", task.stage, "--json"], { cwd: task.project, signal });
  if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) return false;
  const status = JSON.parse(observed.stdout);
  if (status.schema_version !== 3 || status.live !== false ||
      !["completed", "failed", "cancelled", "terminated"].includes(status.state) ||
      status.run?.stage !== task.stage || status.run?.overlay?.target !== task.project ||
      status.run?.overlay?.state !== "applied" || status.filesystem?.changed_files !== 0) return false;
  const proof = await execute(binary, ["apply", task.stage, "--dry-run", "--json", "--all"], {
    cwd: task.project, signal, outputLimit: REVIEW_LIMIT,
  });
  if (proof.exit_code !== 0 || proof.interrupted || proof.truncated) return false;
  const plan = JSON.parse(proof.stdout);
  return plan.schema_version === 1 && plan.operation === "apply" && plan.mode === "preview" &&
    ["ready", "no_changes"].includes(plan.status) && plan.scope?.kind === "project" &&
    plan.scope?.target === task.project && plan.source?.stage === task.stage &&
    plan.source?.session_id === status.run.run_id &&
    Number.isSafeInteger(status.run.overlay.generation) && plan.source?.generation === status.run.overlay.generation &&
    ["selected", "required", "candidates", "blockers"].every((key) => Array.isArray(plan[key]) && plan[key].length === 0) &&
    plan.remaining?.in_scope === 0 && plan.remaining?.outside_scope === 0 && plan.execution === null;
}

/** One project-bound adapter; native pVisor remains the owner of Session state. */
export class Backend {
  constructor(project = process.env.PVISOR_PLUGIN_PROJECT || process.cwd(), options = {}) {
    this.projectInput = project;
    this.binary = options.binary ?? process.env.PVISOR_PLUGIN_BINARY;
    this.rustToolchain = options.rustToolchain ?? process.env.PVISOR_PLUGIN_RUST_TOOLCHAIN;
    this.exportDirectory = options.exportDir ?? process.env.PVISOR_PLUGIN_EXPORT_DIR;
    this.execution = options.execution ?? process.env.PVISOR_PLUGIN_EXECUTION;
    this.allowHostFallback = options.allowHostFallback ?? process.env.PVISOR_PLUGIN_ALLOW_HOST_FALLBACK;
    this.vmRootfs = options.vmRootfs ?? process.env.PVISOR_PLUGIN_VM_ROOTFS;
    this.stateInput = options.stateDir || process.env.PVISOR_PLUGIN_STATE_DIR ||
      path.join(homedir(), ".local", "state", "pvisor", "agent-plugins", "v1");
  }

  async project() {
    const project = await realpath(this.projectInput);
    if (!(await stat(project)).isDirectory()) throw new Error("Project must be a directory");
    return project;
  }

  async storage(project) {
    const requested = path.resolve(this.stateInput);
    // Check before mkdir too: a bad configuration must not create project files.
    if (within(project, await canonicalDestination(requested))) {
      throw new Error("Plugin state must be outside the project");
    }
    await mkdir(requested, { recursive: true, mode: 0o700 });
    const root = await realpath(requested);
    if (within(project, root)) throw new Error("Plugin state resolves inside the project");
    const metadata = await stat(root);
    if ((metadata.mode & 0o077) !== 0 ||
        (process.getuid && metadata.uid !== process.getuid())) {
      throw new Error("Plugin state must be owned by the current user with mode 0700");
    }
    return root;
  }

  async task(selector) {
    if (typeof selector !== "string" || !path.isAbsolute(selector) ||
        selector !== path.normalize(selector) || path.basename(selector) !== "stage") {
      throw new Error("Expected the exact absolute stage selector returned by pvisor_spawn");
    }
    const project = await this.project();
    const root = await this.storage(project);
    const directory = path.dirname(selector);
    const id = path.basename(directory);
    if (!TASK_ID.test(id) || path.dirname(directory) !== root) {
      throw new Error("Selector is not a stage owned by this plugin");
    }
    if (await realpath(directory) !== directory) throw new Error("Task directory must not be a symlink");
    const record = JSON.parse(await readFile(path.join(directory, "task.json"), "utf8"));
    if (!record || record.schema_version !== 1 || record.task !== id || record.project !== project) {
      throw new Error("Task does not belong to this project or has an unsupported schema");
    }
    return { selector, project, stage: selector,
      ...(record.execution && ["host", "vm"].includes(record.execution.selected) ? { execution: record.execution } : {}),
      ...(typeof record.name === "string" ? { name: record.name.slice(0, 64) } : {}) };
  }

  async exportRoot(project) {
    if (typeof this.exportDirectory !== "string" || !path.isAbsolute(this.exportDirectory) ||
        this.exportDirectory.includes("\0")) {
      throw new Error("Set PVISOR_PLUGIN_EXPORT_DIR to an existing private artifact directory outside the project and plugin state");
    }
    const root = await realpath(this.exportDirectory);
    const state = await this.storage(project);
    const metadata = await stat(root);
    if (!metadata.isDirectory() || (metadata.mode & 0o777) !== 0o700 ||
        (process.getuid && metadata.uid !== process.getuid()) ||
        [project, state].some((protectedPath) => within(root, protectedPath) || within(protectedPath, root))) {
      throw new Error("Export root must be an owned 0700 directory disjoint from project and plugin state");
    }
    const profile = await rustEnvironment(this.rustToolchain, project, state);
    if (profile && (within(root, profile.root) || within(profile.root, root))) {
      throw new Error("Export root must not overlap the configured toolchain");
    }
    return root;
  }

  async doctor(input = {}, signal) {
    fields(input, ["probe"]);
    if (input.probe !== undefined && typeof input.probe !== "boolean") {
      throw new Error("doctor probe must be a boolean");
    }
    const project = await this.project();
    const binary = await resolveRuntime(this.binary);
    const result = await execute(binary, ["--version"], { cwd: project, signal });
    const profile = await rustEnvironment(this.rustToolchain, project, await this.storage(project));
    const execution = await selectExecution(executionPolicy(this.execution, this.allowHostFallback, this.vmRootfs), {
      binary, project, state: await this.storage(project), execute, signal, hostToolchain: !!profile,
    });
    const report = {
      ...result,
      binary,
      project,
      execution,
      environment: profile ? { kind: "rust-offline", toolchain: profile.root, dependencies: "project-vendored; no host Cargo HOME shared" } : { kind: "system" },
      capabilities: {
        fork: "Host workspace only; stopped retained source, native policy reconstruction; inherited timeout",
        export: { configured: this.exportDirectory !== undefined,
          root: this.exportDirectory === undefined ? null : await this.exportRoot(project),
          scope: "Linux directory uppers; explicit paths to a new artifact subdirectory; no acceptance" },
      },
      scope: "Task tools only; native host tools are not intercepted. VM-first operator policy, --safe, deny-all network. Preflight is not guest boot or workflow certification.",
    };
    if (!execution.ready) report.exit_code = 1;
    if (input.probe) {
      if (!execution.ready) return report;
      report.probe = await this.run({ argv: ["sh", "-c", "printf 'pvisor staging probe\\n'"] }, signal);
      if (failed(report.probe)) report.exit_code = 1;
    }
    return report;
  }

  async run(input, signal, metadata = {}) {
    fields(metadata, ["name"]);
    if (metadata.name !== undefined && (typeof metadata.name !== "string" ||
        !metadata.name.trim() || metadata.name.length > 64 || /[\x00-\x1f\x7f]/.test(metadata.name))) {
      throw new Error("Task display name must contain 1–64 characters without control characters");
    }
    const { argv, timeout_seconds: seconds } = taskInput(input);
    return this.startTask({ argv, seconds, name: metadata.name }, signal);
  }

  /** Branch retained Host file state, never a process or conversational continuation. */
  async fork(input, signal) {
    fields(input, ["selector", "argv"]);
    const { argv } = taskInput({ argv: input.argv });
    const parent = await this.task(input.selector);
    const observed = await this.operate("status", { selector: parent.selector }, signal);
    if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) {
      throw new Error("Fork requires a confirmed stopped retained source; inspect native status");
    }
    const status = JSON.parse(observed.stdout);
    if (status.schema_version !== 3 || status.live !== false ||
        !["completed", "failed", "cancelled", "terminated"].includes(status.state) ||
        status.run?.stage !== parent.stage || status.run?.overlay?.target !== parent.project ||
        status.run?.overlay?.state !== "staged") {
      throw new Error("Fork requires a stopped retained workspace; live, unknown, suspended, applied and dropped sources are refused");
    }
    // Public review validates source completion. A failed task can still provide
    // a useful file candidate; do not conflate that with a failed read operation.
    const { bundle } = await this.#completed({ selector: parent.selector }, signal);
    if (bundle.run.run_id !== status.run.run_id || bundle.run.execution_id !== status.run.execution_id) {
      throw new Error("Fork source completion changed; inspect the exact source again");
    }
    if (bundle.run.executor?.kind !== "process" || bundle.safety?.filesystem_non_bypassable !== true ||
        bundle.safety?.network_non_bypassable !== true || bundle.network?.policy?.capability?.mode !== "deny") {
      throw new Error("Fork requires an enforced Host filesystem boundary and deny-all networking; no policy downgrade is allowed");
    }
    const policy = executionPolicy(this.execution, this.allowHostFallback, this.vmRootfs);
    if (policy.mode === "vm" || (policy.mode === "auto" && !policy.allowHostFallback)) {
      throw new Error("Host workspace fork is disallowed by the current VM-first policy; explicitly authorize Host execution or start a new VM task");
    }
    // fork has no timeout override: the native launcher inherits the bounded
    // source deadline. Only the transport watchdog uses the plugin maximum.
    return this.startTask({ argv, seconds: 600, parent }, signal);
  }

  async startTask({ argv, seconds, parent, name }, signal) {
    signal?.throwIfAborted();
    const project = await this.project();
    if (parent && parent.project !== project) throw new Error("Fork source project changed");
    const root = await this.storage(project);
    const profile = await rustEnvironment(this.rustToolchain, project, root);
    const task = randomUUID();
    const directory = path.join(root, task);
    await mkdir(directory, { mode: 0o700 });
    const record = { schema_version: 1, task, project, created_at_unix_ms: Date.now(),
      ...(name ? { name } : {}) };
    await writeFile(path.join(directory, "task.json"), JSON.stringify(record), { flag: "wx", mode: 0o600 });
    const stage = path.join(directory, "stage");
    const args = parent ? ["fork", parent.stage, "--state", "workspace", "--stage", stage] : [
      "spawn", "--no-agent-defaults", "--safe", "--executor", "host",
      "--overlaynet-deny-all", "--stdio", "capture", "--stage", stage,
      "--timeout", `${seconds}s`,
    ];
    if (profile && !parent) {
      args.push("--mount", `${profile.root}:read`);
      for (const key of Object.keys(rustTaskEnvironment(profile, "/private-home"))) {
        if (key !== "PATH") args.push("--pass-env", key);
      }
    }
    args.push("--", ...argv);
    try {
      const binary = await resolveRuntime(this.binary);
      const execution = parent ? { requested: "inherited", selected: "host", ready: true, fallback: false } :
        await selectExecution(executionPolicy(this.execution, this.allowHostFallback, this.vmRootfs), {
          binary, project, state: root, execute, signal, hostToolchain: !!profile,
        });
      if (!execution.ready) throw new Error(`VM execution not ready: ${execution.reason}. Host fallback is disabled by this policy; explicitly select PVISOR_PLUGIN_EXECUTION=host, or auto with PVISOR_PLUGIN_ALLOW_HOST_FALLBACK=true`);
      if (!parent && execution.selected === "vm") {
        args[args.indexOf("--executor") + 1] = "vm";
        args.splice(args.indexOf("--"), 0, "--rootfs", execution.rootfs);
      }
      record.execution = { requested: execution.requested, selected: execution.selected,
        fallback: execution.fallback, reason: execution.reason ?? null };
      await writeFile(path.join(directory, "task.json"), JSON.stringify(record), { mode: 0o600 });
      const home = await privateHome(project);
      const env = taskEnvironment(home);
      // Avoid inherited rustup overrides/wrappers and Cargo credential variables.
      for (const key of Object.keys(env)) {
        if (/^(CARGO_|RUST|SCCACHE_)/.test(key)) delete env[key];
      }
      Object.assign(env, rustTaskEnvironment(profile, home));
      const cli = await execute(binary, args, {
        cwd: project, env, signal, deadlineMs: seconds * 1000 + 30_000,
      });
      const result = {
        selector: stage, stage, project,
        execution,
        ...cli,
        cli,
        ...(parent ? { parent_selector: parent.selector, fork_kind: "workspace", timeout_policy: "inherited; transport watchdog 630s" } : {}),
        stdout: "",
        stderr: "",
        output_status: "unavailable",
        output_error: "Task completion not queried after CLI interruption",
      };
      // Query the runtime's validated public read model, never completion.json or
      // model-selected paths. Interrupted owners may still be running.
      if (!cli.interrupted && !cli.signal) {
        try {
          Object.assign(result, await this.output({ selector: stage }, signal));
          result.exit_code = cli.exit_code;
          delete result.output_error;
        } catch (error) {
          result.output_error = error.message;
        }
      }
      // Keep scratch after uncertain/failed execution. Workspace candidates
      // always remain in their persistent stage; never remove those here.
      if (result.exit_code === 0 && !result.interrupted && !result.signal &&
          result.output_status === "complete" && result.task_state === "completed") {
        try { await rm(home, { recursive: true }); }
        catch (error) { result.scratch_home = home; result.cleanup_error = error.message; }
      } else {
        result.scratch_home = home;
      }
      return result;
    } catch (error) {
      throw Object.assign(new Error(`Stage ${stage}; execution failed, do not retry automatically: ${error.message}`), { selector: stage });
    }
  }

  /** Read only the owning runtime's completion-validated public Bundle. */
  async #completed(input, signal) {
    fields(input, ["selector"]);
    const task = await this.task(input.selector);
    const binary = await resolveRuntime(this.binary);
    const observed = await execute(binary, ["status", task.stage, "--review", "--json"], {
      cwd: task.project, signal, outputLimit: REVIEW_LIMIT,
    });
    if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) {
      throw new Error(`Task output unavailable; native review failed or exceeded its 8 MiB budget: ${observed.stderr}`);
    }
    const bundle = JSON.parse(observed.stdout);
    const run = bundle.run;
    if (bundle.schema_version !== 6 || bundle.filesystem?.target !== task.project ||
        !["completed", "failed", "cancelled"].includes(run?.state) ||
        typeof run.run_id !== "string" || !run.run_id || typeof run.execution_id !== "string" || !run.execution_id ||
        !Number.isSafeInteger(run.finished_at_unix_ms) || !run.output ||
        typeof run.output !== "object" || Array.isArray(run.output) ||
        (run.exit_code != null && !Number.isSafeInteger(run.exit_code)) ||
        [run.output.stdout_truncated, run.output.stderr_truncated].some((value) => value !== undefined && typeof value !== "boolean") ||
        [run.output.stdout, run.output.stderr].some((value) => value !== undefined && value !== null && typeof value !== "string")) {
      throw new Error("Task output unavailable; review has no matching terminal task output");
    }
    return { task, bundle };
  }

  /** Captured task output is distinct from CLI diagnostics and is always bounded. */
  async output(input, signal) {
    const { task, bundle } = await this.#completed(input, signal);
    const run = bundle.run;
    const bounded = (value) => new TextDecoder().decode(Buffer.from(value || "").subarray(0, OUTPUT_LIMIT), { stream: true });
    return {
      ...task,
      exit_code: 0, interrupted: false, signal: null,
      stdout: bounded(run.output.stdout), stderr: bounded(run.output.stderr),
      truncated: !!(run.output.stdout_truncated || run.output.stderr_truncated ||
        Buffer.byteLength(run.output.stdout || "") > OUTPUT_LIMIT || Buffer.byteLength(run.output.stderr || "") > OUTPUT_LIMIT),
      task_exit_code: run.exit_code ?? null,
      task_state: run.state,
      output_status: "complete",
      lineage: bundle.lineage ?? null,
    };
  }

  /** Native non-consuming artifact export to an operator-scoped new directory. */
  async export(input, signal) {
    fields(input, ["selector", "paths", "output"]);
    const paths = selection(input.paths);
    if (typeof input.output !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(input.output)) {
      throw new Error("export output must be one new artifact-directory name (1–128 letters/digits/._-), not an absolute or nested path");
    }
    signal?.throwIfAborted();
    const task = await this.task(input.selector);
    const root = await this.exportRoot(task.project);
    const destination = path.join(root, input.output);
    const binary = await resolveRuntime(this.binary);
    const args = ["export", task.stage, "--output", destination, "--json"];
    for (const item of paths) args.push(`--path=${item}`);
    const result = { ...task, destination,
      ...await execute(binary, args, { cwd: task.project, signal, deadlineMs: 120_000, outputLimit: REVIEW_LIMIT }),
      export_status: "unconfirmed",
    };
    if (result.exit_code === 0 && !result.interrupted && !result.signal && !result.truncated) {
      try {
        const manifest = JSON.parse(result.stdout);
        if (manifest.schema_version !== 1 || manifest.destination !== destination ||
            manifest.project_root !== task.project || !Array.isArray(manifest.entries) ||
            !Array.isArray(manifest.deletions) || typeof manifest.session_id !== "string" || !manifest.session_id ||
            !["completed", "failed", "cancelled"].includes(manifest.state) ||
            (manifest.exit_code != null && !Number.isSafeInteger(manifest.exit_code)) ||
            (manifest.execution_id !== null && (typeof manifest.execution_id !== "string" || !manifest.execution_id)) ||
            typeof manifest.partial_results !== "boolean") throw new Error("Export response scope or schema mismatch");
        result.export_status = "published";
        result.manifest = manifest;
        result.manifest_path = path.join(destination, "manifest.json");
      } catch (error) {
        result.export_error = error.message;
      }
    }
    // Cancellation or a lost reply can race publication. Preserve the exact
    // destination; never rerun, replace it or delete uncertain artifacts here.
    return result;
  }

  /** Prepare literal argv for an operator-owned DSH terminal; never launch here. */
  async terminal(input, signal) {
    fields(input, ["selector"]);
    const observed = await this.lifecycle(input, signal);
    if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) {
      throw new Error("Session exec availability is unknown; no terminal was opened");
    }
    const status = JSON.parse(observed.stdout);
    if (status.schema_version !== 1 || status.detail !== "lifecycle" ||
        status.run?.stage !== observed.stage || status.run?.overlay?.target !== observed.project ||
        status.state !== "running" || status.live !== true || status.exec_supported !== true) {
      throw new Error("CAPABILITY_UNSUPPORTED: terminal requires a running Linux Host namespace Session and a native CLI build with Session exec; no implicit resume");
    }
    const binary = await resolveRuntime(this.binary);
    const help = await execute(binary, ["exec", "--help"], { cwd: observed.project, signal });
    if (help.exit_code !== 0 || help.interrupted || help.truncated || !/--session(?:\s|$)/.test(help.stdout)) {
      throw new Error("Native Session exec capability is unavailable; no terminal was opened");
    }
    return { selector: observed.selector, project: observed.project, terminal_status: "ready",
      execution_id: status.run.execution_id, session_id: status.run.run_id,
      argv: [binary, "exec", "--session", "-it", observed.stage, "/bin/sh"] };
  }

  /** Lightweight status when supported; older runtimes retain their existing contract. */
  async lifecycle(input, signal) {
    fields(input, ["selector"]);
    const task = await this.task(input.selector);
    const binary = await resolveRuntime(this.binary);
    const lifecycle = await supportsLifecycleStatus(binary);
    return { ...task, ...await execute(binary,
      ["status", task.stage, "--json", ...(lifecycle ? ["--lifecycle"] : [])],
      { cwd: task.project, signal }) };
  }

  /** Paginate only this project's adapter tasks; native status owns lifecycle truth. */
  async tasks(input = {}, signal) {
    fields(input, ["cursor", "limit", "order"]);
    const { cursor, limit = 20, order = "id" } = input;
    if ((cursor !== undefined && !TASK_ID.test(cursor)) || !Number.isInteger(limit) || limit < 1 || limit > 50 ||
        !["id", "newest"].includes(order)) {
      throw new Error("tasks requires an optional task-ID cursor, limit from 1 to 50 and order id/newest");
    }
    const project = await this.project();
    const root = await this.storage(project);
    const names = (await readdir(root)).filter((name) => TASK_ID.test(name)).sort();
    const records = [];
    for (const name of names) {
      signal?.throwIfAborted();
      const selector = path.join(root, name, "stage");
      // Do not list another project's task, or follow replaced adapter dirs.
      let record;
      try {
        if (await realpath(path.dirname(selector)) !== path.dirname(selector)) continue;
        record = JSON.parse(await readFile(path.join(root, name, "task.json"), "utf8"));
      }
      catch { continue; }
      if (!record || record.schema_version !== 1 || record.task !== name || record.project !== project) continue;
      let created = record.created_at_unix_ms;
      let source = "adapter";
      if (!Number.isSafeInteger(created) || created < 0) {
        try {
          const metadata = await stat(path.join(root, name, "task.json"));
          created = Math.trunc(metadata.birthtimeMs > 0 ? metadata.birthtimeMs : metadata.mtimeMs);
          source = metadata.birthtimeMs > 0 ? "record_birthtime" : "record_mtime";
        } catch { continue; }
      }
      records.push({ task: name, selector, created_at_unix_ms: created, created_at_source: source,
        ...(record.execution && ["host", "vm"].includes(record.execution.selected) ? { execution: record.execution } : {}),
        ...(typeof record.name === "string" ? { name: record.name.slice(0, 64) } : {}) });
    }
    if (order === "newest") {
      records.sort((a, b) => b.created_at_unix_ms - a.created_at_unix_ms || b.task.localeCompare(a.task));
    }
    let offset = 0;
    if (cursor) {
      if (order === "newest") {
        const index = records.findIndex((record) => record.task === cursor);
        if (index < 0) throw new Error("Task cursor is no longer available; refresh the first page");
        offset = index + 1;
      } else {
        offset = records.findIndex((record) => record.task > cursor);
        if (offset < 0) offset = records.length;
      }
    }
    const page = records.slice(offset, offset + limit);
    const items = new Array(page.length);
    // Validate the separately installed runtime once per page, not per row.
    // Bound each read independently: one stale owner must not stall discovery
    // of every other task or consume the monitor's whole request budget.
    const runtime = page.length ? resolveRuntime(this.binary).then(async (binary) => ({
      binary, lifecycle: await supportsLifecycleStatus(binary),
    })) : null;
    let nextIndex = 0;
    const readNext = async () => {
      while (nextIndex < page.length) {
        signal?.throwIfAborted();
        const index = nextIndex++;
        const record = page[index];
        try {
          const { binary, lifecycle } = await runtime;
          const task = await this.task(record.selector);
          const result = await execute(binary, ["status", task.stage, "--json", ...(lifecycle ? ["--lifecycle"] : [])], {
            cwd: task.project, signal, deadlineMs: 2000,
          });
          if (result.exit_code !== 0 || result.interrupted || result.truncated) throw new Error("Native status unavailable");
          const status = JSON.parse(result.stdout);
          if (!((status.schema_version === 3) || (status.schema_version === 1 && status.detail === "lifecycle")) ||
              typeof status.state !== "string" ||
              typeof status.live !== "boolean" || status.run?.stage !== task.stage ||
              status.run?.overlay?.target !== task.project) {
            throw new Error("Unsupported or foreign native status response");
          }
          const command = status.run?.command;
          items[index] = { ...record, state: status.state, live: status.live ?? null,
            filesystem: status.run?.overlay?.state ?? null,
            command: Array.isArray(command) && command.every((arg) => typeof arg === "string") ? command : null,
            command_truncated: status.run?.command_truncated ?? false,
            started_at_unix_ms: status.run?.started_at_unix_ms ?? null,
            finished_at_unix_ms: status.run?.finished_at_unix_ms ?? null,
            changed_files: status.filesystem?.changed_files ?? null };
        } catch (error) {
          signal?.throwIfAborted();
          items[index] = { ...record, state: "unknown", error: error.message };
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(4, page.length) }, readNext));
    return { exit_code: 0, interrupted: false, project, tasks: items, order,
      next_cursor: offset + limit < records.length ? page.at(-1).task : null };
  }

  /** Validate the complete explicit selection before any native drop is attempted. */
  async prepareDrop(input) {
    fields(input, ["selectors"]);
    if (!Array.isArray(input.selectors) || input.selectors.length < 1 || input.selectors.length > 50) {
      throw new Error("Batch drop requires 1–50 explicit selectors; no wildcard or implicit all");
    }
    const selectors = [...new Set(input.selectors)];
    for (const selector of selectors) await this.task(selector);
    return { selectors };
  }

  /** Best-effort native drops, with partial/cancelled outcomes; never rm adapter or runtime trees. */
  async dropBatch(input, signal) {
    const { selectors } = await this.prepareDrop(input);
    const results = [];
    for (const selector of selectors) {
      if (signal?.aborted) {
        results.push({ selector, outcome: "not_attempted" });
        continue;
      }
      try {
        const result = await this.operate("drop", { selector }, signal);
        results.push({ selector, outcome: result.interrupted ? "unknown" : result.status === "already_applied" ? "already_applied" : result.exit_code === 0 ? "dropped" : "refused", result });
      } catch (error) {
        results.push({ selector, outcome: "unknown", error: error.message });
      }
    }
    return { exit_code: results.every((item) => ["dropped", "already_applied"].includes(item.outcome)) ? 0 : 1,
      interrupted: !!signal?.aborted, results };
  }

  async operate(action, input, signal) {
    const selecting = action === "apply" || action === "preview";
    if (!["status", "review", "apply", "preview", "drop"].includes(action)) {
      throw new Error(`Unsupported action: ${action}`);
    }
    fields(input, selecting ? ["selector", "paths"] : action === "review" ? ["selector", "include_diff"] : ["selector"]);
    if (action === "review" && input.include_diff !== undefined && typeof input.include_diff !== "boolean") {
      throw new Error("include_diff must be a boolean");
    }
    const paths = selecting ? selection(input.paths) : [];
    const task = await this.task(input.selector);
    if (action === "review") {
      const binary = await resolveRuntime(this.binary);
      const json = await execute(binary, ["review", task.stage, "--json"], {
        cwd: task.project, signal, outputLimit: REVIEW_LIMIT,
      });
      const inventory = structuredReview(json, task.project);
      if (inventory.review_status !== "complete" || input.include_diff === false) {
        return { ...task, ...json, stdout: "", ...inventory, diff_status: "not_collected" };
      }
      // Native JSON and diff cannot be requested together. Keep this separate
      // observation explicit: neither response is a frozen historical snapshot.
      const detail = await execute(binary, ["review", task.stage, "--diff"], {
        cwd: task.project, signal,
      });
      return { ...task, ...detail, ...inventory,
        diff_status: detail.exit_code !== 0 || detail.interrupted ? "unavailable" :
          detail.truncated ? "truncated" : "separate_observation" };
    }
    let args;
    if (selecting) {
      args = ["apply", task.stage, "--json"];
      if (action === "preview") args.push("--dry-run");
      for (const item of paths) args.push("--path", item);
    } else {
      args = [action, task.stage];
      if (action === "status") args.push("--json");
    }
    const binary = await resolveRuntime(this.binary);
    if (action === "preview") {
      // Native preview only observes existing lock inodes. Ordinary Host
      // Sessions reconcile/initialize their control state through status first.
      // Do not create lock files ourselves or relax read-only native preview.
      const observed = await execute(binary, ["status", task.stage, "--json"], { cwd: task.project, signal });
      if (observed.exit_code !== 0 || observed.interrupted) {
        return presentApplyPreview({ ...task, ...observed }, input);
      }
    }
    const result = { ...task, ...await execute(binary, args, {
      cwd: task.project, signal, outputLimit: action === "preview" ? REVIEW_LIMIT : OUTPUT_LIMIT,
    }) };
    if (action === "drop" && result.exit_code !== 0 && !result.interrupted && !result.signal) {
      try {
        if (await isAlreadyApplied(binary, task, signal)) {
          return { ...task, exit_code: 0, native_exit_code: result.exit_code, signal: null, interrupted: false,
            stdout: "Already applied; no staged candidates to discard. Applied files were not rolled back.",
            stderr: "", truncated: false, status: "already_applied", noop: true };
        }
      } catch {
        // Unavailable or malformed evidence must not turn a real refusal into success.
      }
      signal?.throwIfAborted();
    }
    if (action !== "preview") return result;
    const initial = presentApplyPreview(result, input);
    if (!initial.approval_ready) return initial;
    // 0.3.2 rejects --json together with --diff. Use two read-only interfaces,
    // then bind the display to a stable native plan rather than mixing streams.
    const detailArgs = ["apply", task.stage, "--dry-run", "--diff"];
    for (const item of paths) detailArgs.push("--path", item);
    const detail = await execute(binary, detailArgs, { cwd: task.project, signal, outputLimit: REVIEW_LIMIT });
    if (detail.interrupted || detail.signal || signal?.aborted) {
      return { ...initial, approval_ready: false, preview_state: "unavailable", interrupted: true,
        summary: `${initial.summary}\nPreview collection was interrupted. No approval requested; obtain a fresh preview.` };
    }
    const fresh = { ...task, ...await execute(binary, args, { cwd: task.project, signal, outputLimit: REVIEW_LIMIT }) };
    const shown = presentApplyPreview(fresh, input, detail.stdout);
    if (!shown.approval_ready) return shown;
    if (detail.exit_code !== 0 || detail.truncated ||
        JSON.parse(initial.stdout).plan !== JSON.parse(shown.stdout).plan) {
      return { ...shown, approval_ready: false, preview_state: "unavailable", diff: "",
        summary: `${presentApplyPreview(fresh, input).summary}\nThe native plan changed or its text preview failed. No approval requested; obtain a fresh preview.` };
    }
    return shown;
  }
}
