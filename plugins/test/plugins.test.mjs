import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { chmod, mkdtemp, mkdir, readFile, rename, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Backend, execute, selection } from "../backend.mjs";
import registerPi from "../pi.mjs";
import { registerDsh } from "../dsh.mjs";
import { callTool, tools } from "../tools.mjs";

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "pvisor-plugin-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const project = path.join(root, "project with spaces");
  await mkdir(project);
  const binary = path.join(root, "fake-pvisor.mjs");
  const log = path.join(root, "calls.jsonl");
  await writeFile(binary, `#!${process.execPath}
import { appendFileSync } from "node:fs";
const args = process.argv.slice(2);
if (args.includes("--help")) {
  console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ");
  process.exit(0);
}
appendFileSync(${JSON.stringify(log)}, JSON.stringify({ args, cwd: process.cwd(), home: process.env.HOME, cache: process.env.XDG_CACHE_HOME })+"\\n");
if (args.includes("wait")) { setTimeout(() => {}, 120000); }
else if (args[0] === "--version") console.log("pvisor fixture");
else if (args[0] === "review") console.log("--- accepted.txt\\n+++ accepted.txt\\n+candidate");
else {
  console.log(JSON.stringify({ args }));
  if (args.includes("fail")) process.exitCode = 7;
}
`, { mode: 0o700 });
  const options = { binary, stateDir: path.join(root, "private-state") };
  const backend = new Backend(project, options);
  const calls = async () => (await readFile(log, "utf8")).trim().split("\n").map(JSON.parse);
  return { root, project, binary, options, backend, calls };
}

test("an empty operator binary override fails instead of selecting a PATH runtime", async (t) => {
  const f = await fixture(t);
  const backend = new Backend(f.project, { ...f.options, binary: "" });
  await assert.rejects(backend.doctor(), /absolute executable path/);
  await assert.rejects(readFile(path.join(f.root, "calls.jsonl")), { code: "ENOENT" });
});

test("run always selects safe Host staging, exact argv, deny-all and deadline", async (t) => {
  const f = await fixture(t);
  const argv = ["sh", "-c", "printf '%s' 'a; $HOME' > 'a file'"];
  const result = await f.backend.run({ argv, timeout_seconds: 17 });
  assert.equal(result.exit_code, 0);
  const [call] = await f.calls();
  assert.equal(call.cwd, await f.backend.project());
  assert.notEqual(call.home, process.env.HOME);
  assert.equal(call.cache, path.join(call.home, ".cache"));
  await assert.rejects(readFile(call.home), { code: "ENOENT" });
  assert.deepEqual(call.args, [
    "spawn", "--no-agent-defaults", "--safe", "--executor", "host",
    "--overlaynet-deny-all", "--stdio", "capture", "--stage", result.stage,
    "--timeout", "17s", "--", ...argv,
  ]);
  const record = JSON.parse(await readFile(path.join(path.dirname(result.stage), "task.json"), "utf8"));
  assert.deepEqual(Object.keys(record).sort(), ["project", "schema_version", "task"]);
});

test("failure retains exact task identity and never retries or accepts", async (t) => {
  const f = await fixture(t);
  const result = await f.backend.run({ argv: ["fail"] });
  assert.equal(result.exit_code, 7);
  assert.equal((await f.calls()).length, 1);
  await f.backend.operate("status", { selector: result.selector });
  await f.backend.operate("review", { selector: result.selector });
  await f.backend.operate("preview", { selector: result.selector, paths: ["file with spaces", "src"] });
  const calls = await f.calls();
  assert.deepEqual(calls[1].args, ["status", result.stage, "--json"]);
  assert.deepEqual(calls[2].args, ["review", result.stage, "--diff"]);
  assert.deepEqual(calls[3].args, ["status", result.stage, "--json"]);
  assert.deepEqual(calls[4].args, ["apply", result.stage, "--json", "--dry-run", "--path", "file with spaces", "--path", "src"]);
});

test("reject config overrides, invalid argv, deadlines and selectors before execution", async (t) => {
  const f = await fixture(t);
  for (const input of [
    { argv: [] }, { argv: [""] }, { argv: ["a\0b"] }, { argv: ["true"], binary: "/bin/sh" },
    { argv: ["true"], timeout_seconds: 0 }, { argv: ["true"], timeout_seconds: 601 },
    { argv: ["true"], timeout_seconds: 1.5 }, { argv: ["true"], project: "/" },
  ]) await assert.rejects(f.backend.run(input));
  for (const selector of ["last", "../stage", "/tmp/stage", "--all"]) {
    await assert.rejects(f.backend.operate("drop", { selector }));
  }
  for (const paths of [[], ["/etc"], ["../x"], ["src/../x"], ["."], ["C:/x"], ["a\\b"], ["a//b"]]) {
    assert.throws(() => selection(paths));
  }
  await assert.rejects(readFile(path.join(f.root, "calls.jsonl")), { code: "ENOENT" });
});

test("tasks cannot cross projects, traverse symlinks or store state in project", async (t) => {
  const f = await fixture(t);
  const result = await f.backend.run({ argv: ["true"] });
  const other = path.join(f.root, "other");
  await mkdir(other);
  await assert.rejects(new Backend(other, f.options).operate("status", { selector: result.selector }), /belong/);
  const inside = new Backend(f.project, { ...f.options, stateDir: path.join(f.project, "state") });
  await assert.rejects(inside.run({ argv: ["true"] }), /outside/);
  const alias = path.join(f.root, "alias");
  await symlink(f.project, alias);
  const linked = new Backend(f.project, { ...f.options, stateDir: path.join(alias, "new-state") });
  await assert.rejects(linked.run({ argv: ["true"] }), /outside/);
  await assert.rejects(readFile(path.join(f.project, "new-state")), { code: "ENOENT" });
});

test("missing runtime preserves a task locator instead of running on host", async (t) => {
  const f = await fixture(t);
  const client = new Backend(f.project, { ...f.options, binary: path.join(f.root, "missing") });
  await assert.rejects(client.run({ argv: ["true"] }), /Stage .*do not retry/);
});

test("DOC-PLUGINS-08: unsafe state permissions and replaced task directories fail closed", async (t) => {
  const f = await fixture(t);
  await mkdir(f.options.stateDir, { mode: 0o700 });
  await chmod(f.options.stateDir, 0o755);
  await assert.rejects(f.backend.run({ argv: ["true"] }), /0700/);
  await assert.rejects(readFile(path.join(f.root, "calls.jsonl")), { code: "ENOENT" });
  await chmod(f.options.stateDir, 0o700);
  const result = await f.backend.run({ argv: ["true"] });
  const directory = path.dirname(result.selector);
  const replacement = path.join(f.root, "replacement");
  await rename(directory, replacement);
  await symlink(replacement, directory);
  await assert.rejects(f.backend.operate("status", { selector: result.selector }), /symlink/);
  assert.equal((await f.calls()).length, 1);
});

test("process output is bounded while both streams are drained", async () => {
  const result = await execute(process.execPath, [
    "-e", "process.stdout.write('a'.repeat(400000));process.stderr.write('b'.repeat(400000))",
  ]);
  assert.equal(result.exit_code, 0);
  assert.equal(result.stdout.length, 256 * 1024);
  assert.equal(result.stderr.length, 256 * 1024);
  assert.equal(result.truncated, true);
});

test("cancellation settles the child and marks the outcome interrupted", async () => {
  const controller = new AbortController();
  const running = execute(process.execPath, ["-e", "setInterval(()=>{},1000)"], { signal: controller.signal });
  controller.abort();
  const result = await running;
  assert.equal(result.interrupted, true);
  assert.equal(result.signal, "SIGTERM");
  assert.throws(() => execute(process.execPath, ["--version"], { signal: controller.signal }));
});

for (const kind of ["pi", "dsh"]) {
  test(`${kind} cancellation during capability probing preserves exact-selector recovery without a spawn`, { timeout: 5000 }, async (t) => {
    const f = await fixture(t);
    const original = await readFile(f.binary, "utf8");
    const ready = path.join(f.root, "probe-ready");
    await writeFile(f.binary, `#!${process.execPath}
import { writeFileSync } from "node:fs";
writeFileSync(${JSON.stringify(ready)}, String(process.pid));
setInterval(() => {}, 1000);
`);
    const controller = new AbortController();
    let running;
    if (kind === "pi") {
      const registered = [];
      registerPi({ registerTool: (tool) => registered.push(tool), registerCommand() {}, on() {} }, {
        createBackend: () => f.backend,
      });
      running = registered.find((tool) => tool.name === "pvisor_spawn")
        .execute("cancel-probe", { argv: ["never-run"] }, controller.signal, undefined, { cwd: f.project });
    } else {
      const definitions = new Map();
      registerDsh({ tools: { register: (tool) => definitions.set(tool.name, tool) } },
        (tool) => tool, () => f.backend);
      running = definitions.get("pvisor_spawn").execute({ argv: ["never-run"] }, {
        agent: { session: { header: { cwd: f.project } } }, signal: controller.signal,
      });
    }
    running.catch(() => {});
    t.after(async () => {
      controller.abort();
      await running.catch(() => {});
    });
    let pid;
    const deadline = Date.now() + 2000;
    while (!pid && Date.now() < deadline) {
      try { pid = Number(await readFile(ready, "utf8")); }
      catch (error) { if (error.code !== "ENOENT") throw error; }
      if (!pid) await delay(10);
    }
    assert.ok(pid, "capability probe must start before cancellation");
    controller.abort();
    let selector;
    await assert.rejects(running, (error) => {
      selector = /^Stage (.+); execution failed, do not retry automatically:/.exec(error.message)?.[1];
      assert.ok(selector, error.message);
      assert.match(error.message, /abort/i);
      return true;
    });
    assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
    await assert.rejects(readFile(path.join(f.root, "calls.jsonl")), { code: "ENOENT" });
    // Reopen after the operator fixes the runtime; never rerun the task.
    await writeFile(f.binary, original);
    const reopened = new Backend(f.project, f.options);
    assert.equal((await reopened.task(selector)).selector, selector);
    await reopened.operate("status", { selector });
    await reopened.operate("review", { selector });
    assert.deepEqual((await f.calls()).map((call) => call.args), [
      ["status", selector, "--json"], ["review", selector, "--diff"],
    ]);
  });
}

const success = { exit_code: 0, interrupted: false, stdout: "preview", stderr: "", signal: null, truncated: false };

function hostBackend(log) {
  return {
    project: async () => "/project",
    operate: async (action, input) => { log.push({ action, input }); return success; },
    run: async (input) => { log.push({ action: "run", input }); return success; },
    doctor: async () => success,
  };
}

test("Pi registers additive tools and requires UI confirmation for each mutation", async () => {
  const commands = new Map();
  const registered = [];
  const log = [];
  registerPi({
    registerTool: (tool) => registered.push(tool),
    registerCommand: (name, command) => commands.set(name, command),
    on() {},
  }, { createBackend: () => hostBackend(log) });
  assert.deepEqual(registered.map((tool) => tool.name), tools.map((tool) => tool.name));
  assert.ok(!registered.some((tool) => ["bash", "write", "edit"].includes(tool.name)));
  const ctx = { cwd: "/project", hasUI: true, ui: { confirm: async () => false, notify() {} } };
  const apply = commands.get("pvisor").handler;
  await apply('apply {"selector":"/stage","paths":["src"]}', ctx);
  assert.deepEqual(log.map((call) => call.action), ["preview"]);
  ctx.ui.confirm = async () => true;
  await apply('apply {"selector":"/stage","paths":["src"]}', ctx);
  assert.deepEqual(log.map((call) => call.action), ["preview", "preview", "apply"]);
  ctx.hasUI = false;
  await commands.get("pvisor").handler("drop /stage", ctx);
  assert.ok(!log.some((call) => call.action === "drop"));
});

test("Pi uses call cwd and propagates tool cancellation and failures", async () => {
  const registered = [];
  let observed;
  registerPi({ registerTool: (tool) => registered.push(tool), registerCommand() {}, on() {} }, {
    createBackend: (cwd) => ({
      run: async (_args, signal) => {
        observed = { cwd, signal };
        return { ...success, exit_code: 7 };
      },
    }),
  });
  const signal = new AbortController().signal;
  await assert.rejects(registered.find((tool) => tool.name === "pvisor_spawn")
    .execute("call", { argv: ["false"] }, signal, undefined, { cwd: "/another" }), /"exit_code":7/);
  assert.deepEqual(observed, { cwd: "/another", signal });
});

test("DSH permits only allowed-once; cancellation or unavailable approval cannot mutate", async () => {
  const definitions = new Map();
  const log = [];
  let answer = "unavailable";
  let controller = new AbortController();
  const ctx = {
    tools: { register: (tool) => definitions.set(tool.name, tool) },
    approval: { request: async () => answer },
  };
  registerDsh(ctx, (tool) => tool, () => hostBackend(log));
  const exec = () => ({ agent: { session: { header: { cwd: "/project" } } }, signal: controller.signal, callId: "call" });
  const apply = definitions.get("pvisor_apply");
  assert.equal(apply.parameters.selector.required, true);
  assert.equal(definitions.get("pvisor_spawn").parameters.argv.minItems, undefined);
  assert.equal(definitions.get("pvisor_spawn").parameters.timeout_seconds.minimum, undefined);
  assert.equal(definitions.get("pvisor_spawn").parameters.timeout_seconds.required, undefined);
  for (answer of ["rejected", "cancelled", "unavailable", "allowed-always"]) {
    await assert.rejects(apply.execute({ selector: "/stage", paths: ["src"] }, exec()), /not approved/);
  }
  assert.ok(log.every((call) => call.action === "preview"));
  answer = "allowed-once";
  await apply.execute({ selector: "/stage", paths: ["src"] }, exec());
  assert.equal(log.at(-1).action, "apply");
  controller.abort();
  await assert.rejects(definitions.get("pvisor_drop").execute({ selector: "/stage" }, exec()));
  assert.ok(!log.some((call) => call.action === "drop"));
  await assert.rejects(apply.execute({}, { signal: new AbortController().signal }), /initiating/);
});

test("Pi and DSH never ask for approval or publish after a blocked native preview", async () => {
  for (const kind of ["pi", "dsh"]) {
    const log = [];
    let prompts = 0;
    const client = hostBackend(log);
    client.operate = async (action) => {
      log.push({ action });
      return { ...success, exit_code: 3, stderr: "native conflict" };
    };
    if (kind === "pi") {
      const commands = new Map();
      const notices = [];
      registerPi({ registerTool() {}, registerCommand: (name, value) => commands.set(name, value), on() {} }, {
        createBackend: () => client,
      });
      await commands.get("pvisor").handler('apply {"selector":"/stage","paths":["src"]}', {
        cwd: "/project", hasUI: true,
        ui: {
          confirm: async () => { prompts++; return true; },
          notify: (_message, level) => notices.push(level),
        },
      });
      assert.deepEqual(notices, ["error"]);
    } else {
      const definitions = new Map();
      registerDsh({
        tools: { register: (tool) => definitions.set(tool.name, tool) },
        approval: { request: async () => { prompts++; return "allowed-once"; } },
      }, (value) => value, () => client);
      await assert.rejects(definitions.get("pvisor_apply").execute({ selector: "/stage", paths: ["src"] }, {
        agent: { session: { header: { cwd: "/project" } } }, signal: new AbortController().signal,
      }), /native conflict/);
    }
    assert.equal(prompts, 0);
    assert.deepEqual(log.map((call) => call.action), ["preview"]);
  }
});

test("DSH rechecks cancellation after an approval answer settles", async () => {
  const definitions = new Map();
  const log = [];
  const controller = new AbortController();
  registerDsh({
    tools: { register: (tool) => definitions.set(tool.name, tool) },
    approval: { request: async () => { controller.abort(); return "allowed-once"; } },
  }, (value) => value, () => hostBackend(log));
  await assert.rejects(definitions.get("pvisor_apply").execute({ selector: "/stage", paths: ["src"] }, {
    agent: { session: { header: { cwd: "/project" } } }, signal: controller.signal,
  }));
  assert.deepEqual(log.map((call) => call.action), ["preview"]);
});

test("shared model tool routing never exposes apply or drop", async () => {
  await assert.rejects(callTool({}, "pvisor_apply", {}), /Unknown tool/);
  await assert.rejects(callTool({}, "pvisor_drop", {}), /Unknown tool/);
});

test("MCP refuses an implicit plugin-cache project", async () => {
  const child = spawn(process.execPath, [fileURLToPath(new URL("../mcp.mjs", import.meta.url))], {
    env: { ...process.env, PVISOR_PLUGIN_PROJECT: "" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (bytes) => { stderr += bytes; });
  const [code] = await once(child, "close");
  assert.equal(code, 2);
  assert.match(stderr, /Set PVISOR_PLUGIN_PROJECT or pass --project/);
});

test("MCP initializes, reports tools, contains malformed input, and refuses mutation tools", { timeout: 5000 }, async (t) => {
  const f = await fixture(t);
  const child = spawn(process.execPath, [fileURLToPath(new URL("../mcp.mjs", import.meta.url)), "--project", f.project], {
    env: { ...process.env, PVISOR_PLUGIN_BINARY: f.binary, PVISOR_PLUGIN_STATE_DIR: f.options.stateDir },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const lines = createInterface({ input: child.stdout });
  const responses = new Map();
  let sequence = 0;
  lines.on("line", (line) => {
    const message = JSON.parse(line);
    responses.get(message.id)?.(message);
    responses.delete(message.id);
  });
  t.after(async () => {
    const closed = once(child, "close");
    child.stdin.end();
    await closed;
    lines.close();
  });
  const request = (method, params = {}) => new Promise((resolve) => {
    const id = ++sequence;
    responses.set(id, resolve);
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  });
  const initialized = await request("initialize", { protocolVersion: "2025-03-26" });
  assert.equal(initialized.result.protocolVersion, "2025-03-26");
  const listed = await request("tools/list");
  assert.deepEqual(listed.result.tools.map((tool) => tool.name), tools.map((tool) => tool.name));
  const invalid = await request("tools/list", null);
  assert.equal(invalid.error.code, -32600);
  assert.equal((await request("nonexistent")).error.code, -32601);
  const doctor = await request("tools/call", { name: "pvisor_doctor" });
  assert.equal(doctor.result.isError, false);
  assert.equal(JSON.parse(doctor.result.content[0].text).project, await f.backend.project());
  const mutation = await request("tools/call", { name: "pvisor_apply", arguments: {} });
  assert.equal(mutation.result.isError, true);
  assert.match(mutation.result.content[0].text, /Unknown tool/);
  const running = request("tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
  child.stdin.write(JSON.stringify({
    jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: sequence },
  }) + "\n");
  assert.equal((await running).result.isError, true);
});
