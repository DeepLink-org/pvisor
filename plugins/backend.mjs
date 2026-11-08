import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { resolveRuntime } from "./runtime.mjs";
import { execute } from "./process.mjs";
import { summarizePreview } from "./summary.mjs";

export { execute } from "./process.mjs";

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

/** One project-bound adapter; native pVisor remains the owner of Session state. */
export class Backend {
  constructor(project = process.env.PVISOR_PLUGIN_PROJECT || process.cwd(), options = {}) {
    this.projectInput = project;
    this.binary = options.binary ?? process.env.PVISOR_PLUGIN_BINARY;
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
    if (record.schema_version !== 1 || record.task !== id || record.project !== project) {
      throw new Error("Task does not belong to this project or has an unsupported schema");
    }
    return { selector, project, stage: selector };
  }

  async doctor(input = {}, signal) {
    fields(input, []);
    const project = await this.project();
    const binary = await resolveRuntime(this.binary, { signal });
    const result = await execute(binary, ["--version"], { cwd: project, signal });
    return {
      ...result,
      binary,
      project,
      scope: "Task tools only; native host tools are not intercepted. Host --safe, deny-all network. No mount attestation from doctor.",
    };
  }

  async run(input, signal) {
    fields(input, ["argv", "timeout_seconds"]);
    const { argv, timeout_seconds: seconds = 120 } = input;
    if (!Array.isArray(argv) || argv.length === 0 || argv.length > 256 ||
        argv.some((item) => typeof item !== "string" || item.includes("\0")) || !argv[0]) {
      throw new Error("argv must contain 1–256 strings and a nonempty executable");
    }
    if (!Number.isInteger(seconds) || seconds < 1 || seconds > 600) {
      throw new Error("timeout_seconds must be an integer from 1 to 600");
    }
    signal?.throwIfAborted();
    const project = await this.project();
    const root = await this.storage(project);
    const task = randomUUID();
    const directory = path.join(root, task);
    await mkdir(directory, { mode: 0o700 });
    const record = { schema_version: 1, task, project };
    await writeFile(path.join(directory, "task.json"), JSON.stringify(record), { flag: "wx", mode: 0o600 });
    const stage = path.join(directory, "stage");
    const args = [
      "spawn", "--no-agent-defaults", "--safe", "--executor", "host",
      // Native "capture" retains output only in the Session record. Inherit
      // our noninteractive pipes so the adapter's bounded supervisor can
      // return the actual workload output, not just CLI diagnostics.
      "--overlaynet-deny-all", "--stdio", "inherit", "--stage", stage,
      "--timeout", `${seconds}s`, "--", ...argv,
    ];
    try {
      const binary = await resolveRuntime(this.binary, { signal });
      const home = await privateHome(project);
      const result = {
        selector: stage, stage, project,
        ...await execute(binary, args, {
          cwd: project, env: taskEnvironment(home), signal, deadlineMs: seconds * 1000 + 30_000,
        }),
      };
      // Keep scratch after uncertain/failed execution. Workspace candidates
      // always remain in their persistent stage; never remove those here.
      if (result.exit_code === 0 && !result.interrupted && !result.signal) {
        try { await rm(home, { recursive: true }); }
        catch (error) { result.stderr += `\nRetained private task HOME ${home}: ${error.message}`; }
      }
      return result;
    } catch (error) {
      throw new Error(`Stage ${stage}; execution failed, do not retry automatically: ${error.message}`);
    }
  }

  async operate(action, input, signal) {
    const selecting = action === "apply" || action === "preview";
    if (!["status", "review", "apply", "preview", "drop"].includes(action)) {
      throw new Error(`Unsupported action: ${action}`);
    }
    fields(input, selecting ? ["selector", "paths"] : ["selector"]);
    const paths = selecting ? selection(input.paths) : [];
    const task = await this.task(input.selector);
    let args;
    if (selecting) {
      args = ["apply", task.stage, "--json"];
      if (action === "preview") args.push("--dry-run");
      for (const item of paths) args.push("--path", item);
    } else {
      args = [action, task.stage];
      if (action === "status") args.push("--json");
      if (action === "review") args.push("--diff");
    }
    const binary = await resolveRuntime(this.binary, { signal });
    if (action === "preview") {
      // Native preview only observes existing lock inodes. Ordinary Host
      // Sessions reconcile/initialize their control state through status first.
      // Do not create lock files ourselves or relax read-only native preview.
      const observed = await execute(binary, ["status", task.stage, "--json"], { cwd: task.project, signal });
      if (observed.exit_code !== 0 || observed.interrupted) return { ...task, ...observed };
    }
    const result = { ...task, ...await execute(binary, args, { cwd: task.project, signal }) };
    if (action === "preview") result.summary = summarizePreview(result, { ...input, paths }, task.project);
    return result;
  }
}
