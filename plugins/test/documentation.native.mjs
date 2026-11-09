import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, realpath, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Backend, execute } from "../backend.mjs";
import registerPi from "../pi.mjs";
import { registerDsh } from "../dsh.mjs";
import { resolveRuntime } from "../runtime.mjs";
import { documentationCases } from "./documentation.mjs";
import { mcpClient } from "./mcp-client.mjs";
import { monitorHandler } from "../dsh-monitor.mjs";
import { executionPolicy, selectExecution } from "../execution.mjs";

const examples = await documentationCases();

async function fixture(t) {
  const evidence = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR;
  if (evidence) await mkdir(evidence, { recursive: true });
  const root = await realpath(await mkdtemp(path.join(evidence || tmpdir(), "plugin-native-")));
  const project = path.join(root, "project with spaces");
  await mkdir(project);
  const exportDir = path.join(root, "artifacts");
  await mkdir(exportDir, { mode: 0o700 });
  const binary = await resolveRuntime(process.env.PVISOR_PLUGIN_BINARY);
  const options = { binary, stateDir: path.join(root, "state"), exportDir, execution: "host" };
  const backend = new Backend(project, options);
  const cleanups = [];
  t.after(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup();
    if (!evidence) await rm(root, { recursive: true, force: true });
  });
  t.diagnostic(`Native case evidence: ${root}`);
  return { root, project, binary, options, backend, cleanups };
}

async function absent(project, name) {
  await assert.rejects(stat(path.join(project, name)), { code: "ENOENT" });
}

test("native VM preflight does not claim boot or choose Host without operator authorization", async (t) => {
  const f = await fixture(t);
  const probe = await execute(f.binary, ["vm-check", "--json"], { cwd: f.project });
  assert.equal(probe.exit_code, 0, JSON.stringify(probe));
  const capability = JSON.parse(probe.stdout);
  assert.equal(capability.operation, "vm-check");
  assert.equal(capability.boot_verified, false);
  assert.equal(typeof capability.can_create_vm, "boolean");
  const context = { binary: f.binary, project: f.project, state: f.options.stateDir, execute };
  const blocked = await selectExecution(executionPolicy(), context);
  assert.equal(blocked.selected, null, "No implicit host-root VM or unauthorized Host downgrade");
  assert.match(blocked.reason, /ROOTFS/);
  const fallback = await selectExecution(executionPolicy("auto", "true"), context);
  assert.equal(fallback.selected, "host");
  assert.equal(fallback.fallback, true);
  const vmOnly = await selectExecution(executionPolicy("vm", "true"), context);
  assert.equal(vmOnly.selected, null);
});

function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  return result;
}

async function route(kind, f) {
  const approvals = [];
  let allow = true;
  if (kind === "pi") {
    const definitions = new Map();
    const commands = new Map();
    const notices = [];
    const ctx = {
      cwd: f.project,
      hasUI: true,
      ui: {
        confirm: async (title, message) => { approvals.push({ title, message }); return allow; },
        notify: (message, level) => notices.push({ message, level }),
      },
    };
    registerPi({
      registerTool: (tool) => definitions.set(tool.name, tool),
      registerCommand: (name, command) => commands.set(name, command),
      on() {},
    }, { createBackend: (project) => new Backend(project, f.options) });
    return {
      approvals,
      allow: (value) => { allow = value; },
      call: async (name, args = {}) => (await definitions.get(name).execute(
        "doc-call", args, new AbortController().signal, undefined, ctx,
      )).details,
      async command(line, selector) {
        const previous = notices.length;
        await commands.get("pvisor").handler(line.replace("/pvisor ", "").replaceAll("STAGE_FROM_RESULT", selector), ctx);
        const notice = notices.slice(previous).at(-1);
        if (notice?.level === "error") throw new Error(notice.message);
      },
    };
  }
  if (kind === "dsh") {
    const definitions = new Map();
    registerDsh({
      tools: { register: (tool) => definitions.set(tool.name, tool) },
      approval: { request: async (request) => { approvals.push(request); return allow ? "allowed-once" : "rejected"; } },
    }, (definition) => definition, (project) => new Backend(project, f.options));
    return {
      approvals,
      allow: (value) => { allow = value; },
      call: (name, args = {}) => definitions.get(name).execute(args, {
        agent: { session: { header: { cwd: f.project } } },
        callId: "doc-call",
        signal: new AbortController().signal,
      }),
    };
  }
  const client = await mcpClient(f.project, {
    ...process.env,
    PVISOR_PLUGIN_BINARY: f.binary,
    PVISOR_PLUGIN_EXECUTION: "host",
    PVISOR_PLUGIN_STATE_DIR: f.options.stateDir,
    PVISOR_PLUGIN_EXPORT_DIR: f.options.exportDir,
  });
  f.cleanups.push(() => client.close());
  const catalog = await client.request("tools/list");
  assert.ok(!catalog.tools.some((tool) => ["pvisor_apply", "pvisor_drop"].includes(tool.name)));
  return client;
}

test("monitor discovers a task while its real native execution is still running", { timeout: 30_000 }, async (t) => {
  const f = await fixture(t);
  const abort = new AbortController();
  const pending = f.backend.run({ argv: ["sh", "-c", "sleep 8; printf done"], timeout_seconds: 15 }, abort.signal);
  let running;
  let result;
  try {
    const deadline = Date.now() + 6000;
    while (Date.now() < deadline && !running) {
      const page = await f.backend.tasks({ order: "newest", limit: 10 });
      running = page.tasks.find((task) => task.state === "running" && task.live === true);
      if (!running) await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(running, "Task must be visible before completion, not only after spawn returns");
    result = ok(await pending);
    assert.equal(result.selector, running.selector);
    const completed = await f.backend.tasks({ order: "newest", limit: 10 });
    assert.equal(completed.tasks.find((task) => task.selector === running.selector).state, "completed");
  } finally {
    abort.abort();
    result ??= await pending.catch(() => null);
    if (result?.selector) await f.backend.operate("drop", { selector: result.selector });
  }
});

test("DSH monitor reads real task candidates/output without acceptance or cleanup", async (t) => {
  const f = await fixture(t);
  const run = ok(await f.backend.run({ argv: ["sh", "-c",
    "mkdir src; printf candidate > src/a; printf 'monitor output\\n'"] }));
  const handler = monitorHandler({
    sessions: { get: (id) => id === "case" ? { header: { cwd: f.project } } : undefined },
    get: () => undefined,
  }, (project) => new Backend(project, f.options));
  const read = async (params) => {
    const result = await handler(new Request(`http://localhost/api/pvisor/monitor?${new URLSearchParams({
      session: "case", ...params,
    })}`));
    assert.equal(result.status, 200, await result.clone().text());
    return result.json();
  };
  const page = await read({});
  assert.ok(page.tasks.some((task) => task.selector === run.selector));
  const detail = await read({ selector: run.selector });
  assert.equal(detail.detail_status, "complete", JSON.stringify(detail));
  assert.equal(detail.counts.file, 1);
  assert.equal(detail.counts.directory, 1);
  assert.ok(detail.changes.some((entry) => entry.path === "src/a"));
  const file = detail.files.find((entry) => entry.path === "src/a");
  assert.equal(file.diff_status, "text", JSON.stringify(detail.files));
  assert.equal(file.additions, 1);
  assert.equal(file.deletions, 0);
  assert.match(file.diff, /\+candidate/);
  const output = await read({ selector: run.selector, view: "output" });
  assert.equal(output.stdout, "monitor output\n");
  await absent(f.project, "src");
  const retained = JSON.parse(ok(await f.backend.operate("status", { selector: run.selector })).stdout);
  assert.equal(retained.run.overlay.state, "staged");
  const foreignProject = path.join(f.root, "foreign-project");
  await mkdir(foreignProject);
  const foreign = new Backend(foreignProject, f.options);
  const other = ok(await foreign.run({ argv: ["true"] }));
  assert.equal((await handler(new Request(`http://localhost/api/pvisor/monitor?${new URLSearchParams({
    session: "case", selector: other.selector,
  })}`))).status, 422);
  ok(await foreign.operate("drop", { selector: other.selector }));
  ok(await f.backend.operate("drop", { selector: run.selector }));
});

test("monitor file diffs use real native text, deletion, binary and directory evidence", async (t) => {
  const f = await fixture(t);
  await writeFile(path.join(f.project, "changed.txt"), "same\nold\n");
  await writeFile(path.join(f.project, "gone.txt"), "gone\n");
  const run = ok(await f.backend.run({ argv: ["sh", "-c",
    "printf 'same\\nnew\\nextra\\n' > changed.txt; rm gone.txt; printf '\\000' > binary.bin; mkdir dir; printf hello > dir/new.txt"] }));
  const handler = monitorHandler({
    sessions: { get: () => ({ header: { cwd: f.project } }) }, get: () => undefined,
  }, () => f.backend);
  const reply = await handler(new Request(`http://localhost/api/pvisor/monitor?${new URLSearchParams({
    session: "case", selector: run.selector,
  })}`));
  assert.equal(reply.status, 200, await reply.clone().text());
  const detail = await reply.json();
  const changed = detail.files.find((file) => file.path === "changed.txt");
  assert.equal(changed.diff_status, "text", JSON.stringify(detail.files));
  assert.equal(changed.additions, 2);
  assert.equal(changed.deletions, 1);
  const deleted = detail.files.find((file) => file.path === "gone.txt");
  assert.equal(deleted.additions, 0);
  assert.equal(deleted.deletions, 1);
  const binary = detail.files.find((file) => file.path === "binary.bin");
  assert.equal(binary.diff_status, "binary");
  assert.equal(binary.additions, null);
  const directory = detail.files.find((file) => file.path === "dir");
  assert.equal(directory.diff_status, "directory");
  assert.equal(directory.additions, null);
  assert.equal(await readFile(path.join(f.project, "changed.txt"), "utf8"), "same\nold\n");
  ok(await f.backend.operate("drop", { selector: run.selector }));
});

test("monitor file statistics stay unknown when the real native diff exceeds its budget", async (t) => {
  const f = await fixture(t);
  const run = ok(await f.backend.run({ argv: ["/usr/bin/python3", "-c",
    "from pathlib import Path; Path('huge.txt').write_text(('x'*32+'\\n')*16000)"] }));
  const handler = monitorHandler({
    sessions: { get: () => ({ header: { cwd: f.project } }) }, get: () => undefined,
  }, () => f.backend);
  const reply = await handler(new Request(`http://localhost/api/pvisor/monitor?${new URLSearchParams({
    session: "case", selector: run.selector,
  })}`));
  assert.equal(reply.status, 200, await reply.clone().text());
  const detail = await reply.json();
  const file = detail.files.find((entry) => entry.path === "huge.txt");
  assert.equal(file.diff_status, "partial", JSON.stringify(file));
  assert.equal(file.additions, null);
  assert.equal(file.deletions, null);
  ok(await f.backend.operate("drop", { selector: run.selector }));
});

test("structured review refreshes remaining candidates after partial apply and drop", async (t) => {
  const f = await fixture(t);
  const run = ok(await f.backend.run({ argv: ["sh", "-c",
    "mkdir src; printf a > src/a; printf b > b.txt"] }));
  const input = { selector: run.selector, include_diff: false };
  const initial = ok(await f.backend.operate("review", input));
  assert.equal(initial.review_status, "complete");
  assert.equal(initial.counts.directory, 1);
  assert.equal(initial.counts.file, 2);
  assert.equal(initial.stdout, "");
  assert.equal(initial.diff_status, "not_collected");
  ok(await f.backend.operate("apply", { selector: run.selector, paths: ["b.txt"] }));
  const remaining = ok(await f.backend.operate("review", input));
  assert.equal(remaining.review_status, "complete");
  assert.equal(remaining.counts.file, 1);
  assert.ok(!remaining.changes.some((entry) => entry.path === "b.txt"));
  assert.ok(remaining.changes.some((entry) => entry.path === "src/a"));
  ok(await f.backend.operate("drop", { selector: run.selector }));
  const dropped = ok(await f.backend.operate("review", input));
  assert.equal(dropped.review_status, "complete");
  assert.deepEqual(dropped.changes, []);
  assert.equal(dropped.in_scope, 0);
  assert.equal(await readFile(path.join(f.project, "b.txt"), "utf8"), "b");
  await absent(f.project, "src");
});

for (const kind of ["pi", "dsh", "codex"]) {
  test(`DOC-PLUGINS-01/02 ${kind}: documented two-file task, preview, selective apply and drop`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    const host = await route(kind, f);
    const doctor = ok(await host.call("pvisor_doctor"));
    assert.equal(doctor.project, f.project);
    assert.match(doctor.stdout, /pvisor/);
    assert.match(doctor.scope, /not intercepted/);
    const run = ok(await host.call("pvisor_spawn", examples.spawn));
    await writeFile(path.join(f.root, "spawn-result.json"), JSON.stringify(run, null, 2));
    await absent(f.project, "accepted.txt");
    await absent(f.project, "discarded.txt");
    ok(await host.call("pvisor_status", { selector: run.selector }));
    const review = ok(await host.call("pvisor_review", { selector: run.selector }));
    assert.equal(review.review_status, "complete", JSON.stringify(review));
    assert.equal(review.in_scope, 2);
    assert.equal(review.counts.file, 2);
    assert.ok(review.changes.some((entry) => entry.path === "accepted.txt"));
    assert.match(review.stdout, /accepted\.txt/);
    assert.match(review.stdout, /discarded\.txt/);
    const selection = { selector: run.selector, paths: ["accepted.txt"] };
    const preview = ok(await host.call("pvisor_apply_preview", selection));
    assert.equal(preview.approval_ready, true);
    assert.match(preview.summary, /ADD "accepted.txt"/);
    assert.match(preview.summary, /\+candidate/);
    assert.ok(!preview.summary.includes("discarded.txt"), "Summary must describe selected changes, not the whole stage");
    assert.ok(preview.diff.includes("candidate"));
    await absent(f.project, "accepted.txt");

    if (kind === "pi") {
      for (const line of examples.pi) await host.command(line, run.selector);
      assert.equal(host.approvals.length, 1);
      assert.ok(host.approvals.every((request) => request.message.includes(path.basename(path.dirname(run.selector)))));
      assert.ok(host.approvals.every((request) => !request.message.includes("+candidate")));
    } else if (kind === "dsh") {
      ok(await host.call("pvisor_apply", { ...selection, summary: "Accept the reviewed greeting file." }));
      assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
      ok(await host.call("pvisor_drop", { selector: run.selector }));
      assert.equal(host.approvals.length, 1);
      assert.ok(host.approvals.every((request) => request.reason.includes(path.basename(path.dirname(run.selector)))));
      assert.ok(host.approvals.every((request) => request.reason.includes("Accept the reviewed greeting file.")));
      assert.ok(host.approvals.every((request) => !request.reason.includes("+candidate")));
    } else {
      // Run the exact documented native commands with the documented shell
      // variable. The private PATH supplies the built CLI, without source edits.
      const bin = path.join(f.root, "cli-bin");
      await mkdir(bin);
      await symlink(f.binary, path.join(bin, "pvisor"));
      ok(await execute("sh", ["-eu", "-c", examples.handoff], {
        cwd: f.project,
        env: { ...process.env, stage: run.selector, PATH: `${bin}${path.delimiter}${process.env.PATH}` },
      }));
    }
    assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
    await absent(f.project, "discarded.txt");
    // Disposal is not an implicit second apply or a rollback of accepted data.
    const reopened = new Backend(f.project, f.options);
    assert.equal((await reopened.task(run.selector)).selector, run.selector);
  });
}

for (const kind of ["pi", "dsh"]) {
  test(`DOC-PLUGINS-03 ${kind}: rejection preserves candidates and later approval publishes only selected paths`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    const host = await route(kind, f);
    const run = ok(await host.call("pvisor_spawn", examples.spawn));
    const selection = { selector: run.selector, paths: ["accepted.txt"] };
    const apply = () => kind === "pi" ? host.command(examples.pi[2], run.selector) : host.call("pvisor_apply", { ...selection, summary: "Accept the reviewed greeting file." });
    const drop = () => kind === "pi" ? host.command(examples.pi[3], run.selector) : host.call("pvisor_drop", { selector: run.selector });
    host.allow(false);
    if (kind === "pi") await apply();
    else await assert.rejects(apply(), /not approved/);
    await absent(f.project, "accepted.txt");
    assert.match(ok(await host.call("pvisor_review", { selector: run.selector })).stdout, /accepted\.txt/);
    host.allow(true);
    await apply();
    host.allow(false);
    assert.match(ok(await host.call("pvisor_review", { selector: run.selector })).stdout, /discarded\.txt/);
    assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
    const approvals = host.approvals.length;
    await drop();
    assert.equal(host.approvals.length, approvals, "Drop must not ask for approval");
    assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
    await absent(f.project, "discarded.txt");
  });
}

test("DOC-PLUGINS-04: failed tasks retain candidates without host writes, retry or implicit cleanup", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({ argv: ["sh", "-c", "printf 'partial\\n' > partial.txt; exit 7"] });
  assert.equal(run.exit_code, 7, JSON.stringify(run));
  await absent(f.project, "partial.txt");
  const reopened = new Backend(f.project, f.options);
  const status = ok(await reopened.operate("status", { selector: run.selector }));
  assert.equal(JSON.parse(status.stdout).state, "failed");
  assert.match(ok(await reopened.operate("review", { selector: run.selector })).stdout, /partial\.txt/);
  ok(await reopened.operate("drop", { selector: run.selector }));
});

test("DOC-PLUGINS-05: independent spawns cannot see another task's unaccepted files", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const first = ok(await f.backend.run(examples.spawn));
  const second = ok(await f.backend.run({ argv: ["sh", "-c", "test ! -e accepted.txt && test ! -e discarded.txt"] }));
  assert.notEqual(first.selector, second.selector);
  assert.match(ok(await f.backend.operate("review", { selector: first.selector })).stdout, /accepted\.txt/);
  ok(await f.backend.operate("drop", { selector: first.selector }));
});

test("DOC-PLUGINS-06: apply conflicts preserve external edits and remain reviewable", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  await writeFile(path.join(f.project, "existing.txt"), "baseline\n");
  const run = ok(await f.backend.run({ argv: ["sh", "-c", "cat existing.txt; printf 'proposal\\n' > existing.txt"] }));
  await writeFile(path.join(f.project, "existing.txt"), "external edit\n");
  const input = { selector: run.selector, paths: ["existing.txt"] };
  const preview = await f.backend.operate("preview", input);
  assert.notEqual(preview.exit_code, 0, JSON.stringify(preview));
  assert.match(preview.summary, /Blocked because:/);
  assert.match(preview.summary, /target changed after staging/);
  const apply = await f.backend.operate("apply", input);
  assert.notEqual(apply.exit_code, 0, JSON.stringify(apply));
  assert.equal(await readFile(path.join(f.project, "existing.txt"), "utf8"), "external edit\n");
  assert.match(ok(await f.backend.operate("review", { selector: run.selector })).stdout, /existing\.txt/);
  ok(await f.backend.operate("drop", { selector: run.selector }));
  assert.equal(await readFile(path.join(f.project, "existing.txt"), "utf8"), "external edit\n");
});

test("DOC-PLUGINS-09: runtime deadline reports failure and does not publish candidates", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const run = await f.backend.run({
    argv: ["sh", "-c", "printf 'pending\\n' > timeout.txt; sleep 30"], timeout_seconds: 2,
  });
  assert.notEqual(run.exit_code, 0, JSON.stringify(run));
  await absent(f.project, "timeout.txt");
  const status = ok(await f.backend.operate("status", { selector: run.selector }));
  assert.equal(JSON.parse(status.stdout).state, "failed");
  ok(await f.backend.operate("drop", { selector: run.selector }));
});

test("DOC-PLUGINS-10: MCP shutdown leaves pending candidates available for native review", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const client = await route("codex", f);
  const run = ok(await client.call("pvisor_spawn", examples.spawn));
  await client.close();
  await absent(f.project, "accepted.txt");
  const reopened = new Backend(f.project, f.options);
  assert.match(ok(await reopened.operate("review", { selector: run.selector })).stdout, /discarded\.txt/);
  ok(await reopened.operate("drop", { selector: run.selector }));
});

test("DOC-PLUGINS-11: a project and default persistent state under HOME support the complete task lifecycle", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const home = path.join(f.root, "home");
  const project = path.join(home, "project");
  await mkdir(project, { recursive: true });
  const env = { ...process.env, HOME: home };
  delete env.PVISOR_PLUGIN_STATE_DIR;
  const script = `
    import assert from "node:assert/strict";
    import {readFile,stat} from "node:fs/promises";
    import {pathToFileURL} from "node:url";
    const {Backend}=await import(pathToFileURL(process.argv[1]));
    const b=new Backend(process.argv[2],{binary:process.argv[3],execution:"host"});
    const r=await b.run(JSON.parse(process.argv[4]));
    assert.equal(r.exit_code,0,JSON.stringify(r));
    assert.ok(r.selector.startsWith(process.env.HOME+"/.local/state/pvisor/agent-plugins/v1/"));
    await assert.rejects(stat(process.argv[2]+"/accepted.txt"),{code:"ENOENT"});
    const selected={selector:r.selector,paths:["accepted.txt"]};
    for(const [action,input] of [["preview",selected],["apply",selected],["drop",{selector:r.selector}]]) {
      const result=await b.operate(action,input);
      assert.equal(result.exit_code,0,JSON.stringify(result));
    }
    assert.equal(await readFile(process.argv[2]+"/accepted.txt","utf8"),"candidate\\n");
    await assert.rejects(stat(process.argv[2]+"/discarded.txt"),{code:"ENOENT"});
  `;
  ok(await execute(process.execPath, [
    "--input-type=module", "-e", script, fileURLToPath(new URL("../backend.mjs", import.meta.url)),
    project, f.binary, JSON.stringify(examples.spawn),
  ], { cwd: project, env }));
});

test("captured output round-trips through native tasks, including nonzero exit and output truncation", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const probe = ok(await f.backend.doctor({ probe: true }));
  assert.equal(probe.probe.stdout, "pvisor staging probe\n");
  ok(await f.backend.dropBatch({ selectors: [probe.probe.selector] }));
  const run = await f.backend.run({ argv: ["sh", "-c", "printf 'stdout bytes\\n'; printf 'stderr bytes\\n' >&2; exit 7"] });
  assert.equal(run.exit_code, 7, JSON.stringify(run));
  assert.equal(run.task_exit_code, 7);
  assert.equal(run.stdout, "stdout bytes\n");
  assert.equal(run.stderr, "stderr bytes\n");
  assert.equal(run.output_status, "complete");
  assert.match(run.cli.stderr, /Session storage/);
  const read = await f.backend.output({ selector: run.selector });
  assert.equal(read.stdout, run.stdout);
  assert.equal(read.task_state, "failed");
  const long = ok(await f.backend.run({ argv: ["sh", "-c", "yes abcdefghijklmnopqrstuvwxyz | dd bs=1024 count=400 2>/dev/null"] }));
  assert.equal(Buffer.byteLength(long.stdout), 256 * 1024);
  assert.equal(long.truncated, true);
  ok(await f.backend.dropBatch({ selectors: [run.selector, long.selector] }));
});

test("native Rust profile builds vendored dependencies offline with private caches and read-only toolchain", { timeout: 120_000 }, async (t) => {
  const f = await fixture(t);
  const toolchain = process.env.PVISOR_PLUGIN_TEST_RUST_TOOLCHAIN ||
    path.dirname(path.dirname(execFileSync("rustup", ["which", "rustc"], { encoding: "utf8" }).trim()));
  const backend = new Backend(f.project, { ...f.options, rustToolchain: toolchain });
  await mkdir(path.join(f.project, "src"));
  await mkdir(path.join(f.project, ".cargo"));
  await mkdir(path.join(f.project, "vendor/value/src"), { recursive: true });
  await writeFile(path.join(f.project, "Cargo.toml"), '[package]\nname="offline-profile"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nfixture-value="=0.1.0"\n');
  await writeFile(path.join(f.project, "src/lib.rs"), '#[test] fn vendored() { assert_eq!(fixture_value::value(), 42); }\n');
  await writeFile(path.join(f.project, ".cargo/config.toml"), '[source.crates-io]\nreplace-with="vendored"\n[source.vendored]\ndirectory="vendor"\n');
  const dependency = {
    "Cargo.toml": '[package]\nname="fixture-value"\nversion="0.1.0"\nedition="2021"\n',
    "src/lib.rs": "pub fn value() -> u32 { 42 }\n",
  };
  for (const [name, content] of Object.entries(dependency)) {
    await writeFile(path.join(f.project, "vendor/value", name), content);
  }
  await writeFile(path.join(f.project, "vendor/value/.cargo-checksum.json"), JSON.stringify({
    package: null,
    files: Object.fromEntries(Object.entries(dependency).map(([name, content]) => [name, createHash("sha256").update(content).digest("hex")])),
  }));
  const before = await readFile(path.join(toolchain, "bin/rustc"));
  const run = await backend.run({ argv: ["sh", "-eu", "-c", `
    test "$CARGO_NET_OFFLINE" = true
    test ! -e "$CARGO_HOME/credentials.toml"
    test "\${RUSTC_WRAPPER-unset}" = unset
    test "\${RUSTUP_HOME-unset}" = unset
    test "$CARGO_TARGET_DIR" != "$PWD/target"
    cargo generate-lockfile --offline
    cargo test --offline --locked
    # access(2)/test -w does not check Landlock. Request a write FD without
    # truncation or writes; even an enforcement regression cannot alter the file.
    if (exec 9<> "$RUSTC") 2>/dev/null; then exit 91; fi
    printf 'rust offline passed\\n'
    printf 'cargo-home=%s\\n' "$CARGO_HOME"
  `] });
  ok(run);
  assert.match(run.stdout, /rust offline passed/);
  assert.match(run.stdout, /1 passed/);
  assert.match(run.stderr, /Compiling fixture-value/);
  assert.equal(run.output_status, "complete");
  assert.deepEqual(await readFile(path.join(toolchain, "bin/rustc")), before);
  await absent(f.project, "Cargo.lock");
  await absent(f.project, "target");
  const doctor = ok(await backend.doctor());
  assert.equal(doctor.environment.kind, "rust-offline");
  const parentCargoHome = /^cargo-home=(.+)$/m.exec(run.stdout)[1];
  const forked = ok(await backend.fork({ selector: run.selector, argv: ["sh", "-eu", "-c", `
    test "$CARGO_NET_OFFLINE" = true
    test "$CARGO_HOME" != "$1"
    test ! -e "$CARGO_HOME/credentials.toml"
    cargo test --offline --locked
    printf 'rust fork passed\\n'
  `, "rust-fork", parentCargoHome] }));
  assert.match(forked.stdout, /rust fork passed/);
  ok(await backend.operate("drop", { selector: forked.selector }));
  ok(await backend.operate("preview", { selector: run.selector, paths: ["Cargo.lock"] }));
  ok(await backend.operate("apply", { selector: run.selector, paths: ["Cargo.lock"] }));
  assert.match(await readFile(path.join(f.project, "Cargo.lock"), "utf8"), /fixture-value/);
  // All project candidates were accepted; native drop must not be used as rollback.
});

test("native fanout returns exact selectors; task listing and batch drop leave host files alone", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const dsh = await route("dsh", f);
  const result = await dsh.call("pvisor_fanout", examples.fanout);
  ok(result);
  assert.deepEqual(result.results.map((item) => item.result.stdout), ["a\n", "b\n"]);
  const selectors = result.results.map((item) => item.result.selector);
  assert.equal(new Set(selectors).size, 2);
  await absent(f.project, "result.txt");
  const listed = await f.backend.tasks();
  assert.deepEqual(listed.tasks.map((item) => item.selector).sort(), selectors.sort());
  ok(await f.backend.operate("apply", { selector: selectors[0], paths: ["result.txt"] }));
  const accepted = await readFile(path.join(f.project, "result.txt"), "utf8");
  const dropped = await f.backend.dropBatch({ selectors });
  assert.equal(dropped.exit_code, 0, JSON.stringify(dropped));
  assert.deepEqual(dropped.results.map((item) => item.outcome), ["already_applied", "dropped"]);
  assert.equal(dropped.results[0].result.noop, true);
  const repeat = ok(await dsh.call("pvisor_drop", { selector: selectors[0] }));
  assert.equal(repeat.status, "already_applied");
  assert.equal(await readFile(path.join(f.project, "result.txt"), "utf8"), accepted);
});

test("operator fanout CLI executes the documented plan and returns parseable exact selectors", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const input = path.join(f.root, "experiments.json");
  await writeFile(input, JSON.stringify(examples.fanout));
  const result = ok(await execute(process.execPath, [
    fileURLToPath(new URL("../fanout-cli.mjs", import.meta.url)), "--project", f.project, "--input", input,
  ], { cwd: f.project, env: { ...process.env, PVISOR_PLUGIN_BINARY: f.binary, PVISOR_PLUGIN_STATE_DIR: f.options.stateDir, PVISOR_PLUGIN_EXECUTION: "host" } }));
  const report = JSON.parse(result.stdout);
  assert.deepEqual(report.results.map((item) => item.result.stdout), ["a\n", "b\n"]);
  await absent(f.project, "result.txt");
  ok(await f.backend.dropBatch({ selectors: report.results.map((item) => item.result.selector) }));
});

for (const kind of ["pi", "dsh", "codex"]) {
  test(`${kind}: native workspace fork branches retained files with independent outputs and lineage`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    const host = await route(kind, f);
    const parent = ok(await host.call("pvisor_spawn", {
      argv: ["sh", "-c", "printf 'parent\\n' > branch.txt; printf 'parent-home=%s\\n' \"$HOME\""],
    }));
    const child = await host.call("pvisor_fork", {
      selector: parent.selector,
      argv: ["sh", "-eu", "-c", "test \"$(cat branch.txt)\" = parent; printf 'child\\n' > branch.txt; printf 'child-output\\n'; printf 'child-home=%s\\n' \"$HOME\""],
    });
    ok(child);
    assert.notEqual(child.selector, parent.selector);
    assert.equal(child.parent_selector, parent.selector);
    assert.match(child.stdout, /^child-output\n/);
    assert.notEqual(/^parent-home=(.+)$/m.exec(parent.stdout)[1], /^child-home=(.+)$/m.exec(child.stdout)[1]);
    assert.ok(child.lineage?.parent_run_id);
    const readBundle = async (selector) => JSON.parse(ok(await execute(f.binary,
      ["status", selector, "--review", "--json"], { cwd: f.project })).stdout);
    const parentBundle = await readBundle(parent.selector);
    const childBundle = await readBundle(child.selector);
    assert.equal(childBundle.safety.network_non_bypassable, parentBundle.safety.network_non_bypassable);
    assert.equal(childBundle.safety.filesystem_non_bypassable, parentBundle.safety.filesystem_non_bypassable);
    assert.equal(childBundle.run.executor.isolation, parentBundle.run.executor.isolation);
    assert.match(ok(await f.backend.operate("review", { selector: parent.selector })).stdout, /\+parent/);
    assert.match(ok(await f.backend.operate("review", { selector: child.selector })).stdout, /\+child/);
    await absent(f.project, "branch.txt");
    ok(await f.backend.operate("apply", { selector: child.selector, paths: ["branch.txt"] }));
    assert.equal(await readFile(path.join(f.project, "branch.txt"), "utf8"), "child\n");
    // Parent still exists independently; do not apply it over the chosen child.
    ok(await f.backend.operate("drop", { selector: parent.selector }));
    await assert.rejects(f.backend.fork({ selector: parent.selector, argv: ["true"] }), /retained/);
    await assert.rejects(f.backend.fork({ selector: child.selector, argv: ["true"] }), /retained/);
    if (kind !== "codex") assert.equal(host.approvals.length, 0);
  });

  test(`${kind}: native export preserves failed-task evidence without acceptance, overwrite or stage consumption`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    await writeFile(path.join(f.project, "removed.txt"), "keep on host");
    const run = await f.backend.run({
      argv: ["sh", "-c", "printf 'evidence\\n' > report.txt; rm removed.txt; exit 7"],
    });
    assert.equal(run.task_exit_code, 7);
    const host = await route(kind, f);
    const input = { selector: run.selector, paths: ["report.txt", "removed.txt"], output: "case-a" };
    if (process.platform !== "linux") {
      await assert.rejects(host.call("pvisor_export", input), /Linux|linux|unsupported|Unsupported/);
      return;
    }
    const result = ok(await host.call("pvisor_export", input));
    assert.equal(result.export_status, "published");
    assert.equal(result.manifest.partial_results, true);
    assert.equal(result.manifest.exit_code, 7);
    assert.equal(result.destination, path.join(f.options.exportDir, "case-a"));
    assert.equal(await readFile(path.join(result.destination, "files/report.txt"), "utf8"), "evidence\n");
    const manifest = JSON.parse(await readFile(result.manifest_path, "utf8"));
    assert.deepEqual(manifest, result.manifest);
    assert.equal(manifest.deletions[0].path, "removed.txt");
    await absent(result.destination, "files/removed.txt");
    await absent(f.project, "report.txt");
    assert.equal(await readFile(path.join(f.project, "removed.txt"), "utf8"), "keep on host");
    await assert.rejects(host.call("pvisor_export", input), /exists|existing|exist|unconfirmed|File/);
    assert.equal(await readFile(path.join(result.destination, "files/report.txt"), "utf8"), "evidence\n");
    assert.match(ok(await f.backend.operate("review", { selector: run.selector })).stdout, /report\.txt/);
    ok(await f.backend.operate("drop", { selector: run.selector }));
    await assert.rejects(host.call("pvisor_export", { ...input, output: "after-drop" }));
    assert.equal(await readFile(path.join(result.destination, "files/report.txt"), "utf8"), "evidence\n");
    if (kind !== "codex") assert.equal(host.approvals.length, 0);
  });
}

test("workspace fork inherits the native parent deadline instead of silently extending it", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const parent = ok(await f.backend.run({ argv: ["true"], timeout_seconds: 1 }));
  const child = await f.backend.fork({ selector: parent.selector, argv: ["sh", "-c", "sleep 3"] });
  assert.notEqual(child.exit_code, 0, JSON.stringify(child));
  assert.equal(child.interrupted, false, "The native deadline, not the adapter watchdog, must end the task");
  assert.equal(child.task_state, "failed", JSON.stringify(child));
  assert.match(child.cli.stderr, /[Dd]eadline/);
  ok(await f.backend.dropBatch({ selectors: [parent.selector, child.selector] }));
});

test("export preserves existing destination links and refuses unsupported symlinks or applied sources", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  if (process.platform !== "linux") {
    const run = ok(await f.backend.run({ argv: ["sh", "-c", "echo report > report.txt"] }));
    const result = await f.backend.export({ selector: run.selector, paths: ["report.txt"], output: "unsupported" });
    assert.notEqual(result.exit_code, 0);
    return;
  }
  const run = ok(await f.backend.run({ argv: ["sh", "-c", "echo report > report.txt; ln -s report.txt link.txt"] }));
  const victim = path.join(f.root, "victim");
  await mkdir(victim);
  await writeFile(path.join(victim, "keep"), "unchanged");
  await symlink(victim, path.join(f.options.exportDir, "existing-link"));
  const linked = await f.backend.export({ selector: run.selector, paths: ["report.txt"], output: "existing-link" });
  assert.notEqual(linked.exit_code, 0);
  assert.equal(await realpath(path.join(f.options.exportDir, "existing-link")), victim);
  assert.equal(await readFile(path.join(victim, "keep"), "utf8"), "unchanged");
  await absent(victim, "manifest.json");
  const unsupported = await f.backend.export({ selector: run.selector, paths: ["link.txt"], output: "symlink" });
  assert.notEqual(unsupported.exit_code, 0);
  assert.match(unsupported.stderr, /symlink/);
  await absent(f.options.exportDir, "symlink");
  const applied = ok(await f.backend.run({ argv: ["sh", "-c", "echo accepted > accepted.txt"] }));
  ok(await f.backend.operate("apply", { selector: applied.selector, paths: ["accepted.txt"] }));
  const refused = await f.backend.export({ selector: applied.selector, paths: ["accepted.txt"], output: "applied" });
  assert.notEqual(refused.exit_code, 0);
  await absent(f.options.exportDir, "applied");
  ok(await f.backend.operate("drop", { selector: run.selector }));
});

test("documented fork/export inputs branch and extract without changing the project", { timeout: 60_000 }, async (t) => {
  const f = await fixture(t);
  const parent = ok(await f.backend.run({ argv: ["sh", "-c", "printf 'parent\\n' > branch.txt"] }));
  const child = ok(await f.backend.fork({ ...examples.fork, selector: parent.selector }));
  assert.equal(child.stdout, "parent\n");
  await absent(f.project, "branch.txt");
  const artifact = await f.backend.export({ ...examples.export, selector: child.selector });
  if (process.platform === "linux") {
    ok(artifact);
    assert.equal(await readFile(path.join(artifact.destination, "files/branch.txt"), "utf8"), "child\n");
    await absent(f.project, "branch.txt");
  } else {
    assert.notEqual(artifact.exit_code, 0);
  }
  ok(await f.backend.dropBatch({ selectors: [parent.selector, child.selector] }));
});
