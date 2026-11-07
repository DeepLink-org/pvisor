import assert from "node:assert/strict";
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

const examples = await documentationCases();

async function fixture(t) {
  const evidence = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR;
  if (evidence) await mkdir(evidence, { recursive: true });
  const root = await realpath(await mkdtemp(path.join(evidence || tmpdir(), "plugin-native-")));
  const project = path.join(root, "project with spaces");
  await mkdir(project);
  const binary = await resolveRuntime(process.env.PVISOR_PLUGIN_BINARY);
  const options = { binary, stateDir: path.join(root, "state") };
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
    PVISOR_PLUGIN_STATE_DIR: f.options.stateDir,
  });
  f.cleanups.push(() => client.close());
  const catalog = await client.request("tools/list");
  assert.ok(!catalog.tools.some((tool) => ["pvisor_apply", "pvisor_drop"].includes(tool.name)));
  return client;
}

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
    assert.match(review.stdout, /accepted\.txt/);
    assert.match(review.stdout, /discarded\.txt/);
    const selection = { selector: run.selector, paths: ["accepted.txt"] };
    ok(await host.call("pvisor_apply_preview", selection));
    await absent(f.project, "accepted.txt");

    if (kind === "pi") {
      for (const line of examples.pi) await host.command(line, run.selector);
      assert.equal(host.approvals.length, 2);
      assert.ok(host.approvals.every((request) => request.message.includes(run.selector)));
    } else if (kind === "dsh") {
      ok(await host.call("pvisor_apply", selection));
      assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
      ok(await host.call("pvisor_drop", { selector: run.selector }));
      assert.equal(host.approvals.length, 2);
      assert.ok(host.approvals.every((request) => request.reason.includes(run.selector)));
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
    const apply = () => kind === "pi" ? host.command(examples.pi[2], run.selector) : host.call("pvisor_apply", selection);
    const drop = () => kind === "pi" ? host.command(examples.pi[3], run.selector) : host.call("pvisor_drop", { selector: run.selector });
    host.allow(false);
    if (kind === "pi") await apply();
    else await assert.rejects(apply(), /not approved/);
    await absent(f.project, "accepted.txt");
    assert.match(ok(await host.call("pvisor_review", { selector: run.selector })).stdout, /accepted\.txt/);
    host.allow(true);
    await apply();
    host.allow(false);
    if (kind === "pi") await drop();
    else await assert.rejects(drop(), /not approved/);
    assert.match(ok(await host.call("pvisor_review", { selector: run.selector })).stdout, /discarded\.txt/);
    assert.equal(await readFile(path.join(f.project, "accepted.txt"), "utf8"), "candidate\n");
    host.allow(true);
    await drop();
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
    const b=new Backend(process.argv[2],{binary:process.argv[3]});
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
