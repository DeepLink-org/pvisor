import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

// Fake CLI, real MCP subprocess and OS pipes. No native Sessions or network.
async function peer(t, { escaped = false, delayedCancel = false } = {}) {
  let root, child, closed;
  const errors = [];
  t.after(async () => {
    if (root) await writeFile(path.join(root, "release"), "cleanup");
    if (child) {
      child.stdout.resume();
      child.stdin.end();
      const timer = setTimeout(() => child.kill("SIGKILL"), 6000);
      try { await closed; } finally { clearTimeout(timer); }
    }
    if (root) {
      // Every fake process has a finite deadline; unknown ownership preserves root.
      const events = await readFile(path.join(root, "events"), "utf8").catch((error) => {
        if (error.code === "ENOENT") return "";
        throw error;
      });
      const live = new Set();
      for (const line of events.trim().split("\n").filter(Boolean)) {
        const [event, pid] = line.split(" ");
        if (event === "start") live.add(Number(pid));
        if (event === "exit") live.delete(Number(pid));
      }
      assert.deepEqual([...live], [], `Preserved fixture ${root}: fake processes not joined`);
      await rm(root, { recursive: true });
    }
  });
  root = await mkdtemp(path.join(tmpdir(), "plugin-transport-"));
  await mkdir(path.join(root, "project"));
  const events = path.join(root, "events");
  const binary = path.join(root, "runtime.mjs");
  await writeFile(binary, `#!${process.execPath}
import { appendFileSync, existsSync } from "node:fs";
const log = (event) => appendFileSync(${JSON.stringify(events)}, event + " " + process.pid + "\\n");
log("start");
process.on("exit", () => log("exit"));
if (process.argv.includes("--help")) {
  console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ");
} else if (process.argv.includes("--version")) {
  process.stdout.write(${JSON.stringify(escaped ? "\0" : "x")}.repeat(256 * 1024), () => log("version"));
} else {
  log("ready");
  process.on("SIGTERM", () => {
    log("cancel");
    if (${delayedCancel}) {
      const gate = setInterval(() => {
        if (existsSync(${JSON.stringify(path.join(root, "release"))})) {
          clearInterval(gate);
          log("released");
          process.exit(0);
        }
      }, 20);
    } else process.exit(0);
  });
  setTimeout(() => process.exit(2), ${delayedCancel ? 8000 : 15000});
}
`, { mode: 0o700 });
  child = spawn(process.execPath, [
    fileURLToPath(new URL("../mcp.mjs", import.meta.url)), "--project", path.join(root, "project"),
  ], {
    env: { ...process.env, TMPDIR: root, PVISOR_PLUGIN_BINARY: binary, PVISOR_PLUGIN_STATE_DIR: path.join(root, "state") },
    stdio: ["pipe", "pipe", "pipe"],
  });
  closed = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal }));
  });
  // Observe immediately, including setup failures.
  closed.catch((error) => errors.push(error));
  const exited = new Promise((resolve) => child.once("exit", (code, signal) => resolve({ code, signal })));
  child.stdin.on("error", (error) => {
    if (!["EPIPE", "ERR_STREAM_DESTROYED"].includes(error.code)) errors.push(error);
  });
  let stderr = "", output = "", total = 0;
  const responses = [];
  child.stderr.on("data", (bytes) => {
    stderr += bytes;
    assert.ok(stderr.length <= 8192, "bounded fixture diagnostics");
  });
  child.stdout.on("data", (bytes) => {
    total += bytes.length;
    assert.ok(total <= 18 * 1024 * 1024, "hard fixture response budget");
    output += bytes;
    let end;
    while ((end = output.indexOf("\n")) >= 0) {
      const line = output.slice(0, end);
      output = output.slice(end + 1);
      const value = JSON.parse(line);
      assert.equal(value.jsonrpc, "2.0");
      assert.equal(Object.hasOwn(value, "result") !== Object.hasOwn(value, "error"), true);
      assert.ok(!responses.some((item) => item.id === value.id), "one response per ID");
      responses.push(value);
    }
    assert.ok(output.length <= 2 * 1024 * 1024, "bounded pending frame");
  });
  async function until(check, message) {
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline) {
      const result = await check();
      if (result) return result;
      if (child.exitCode !== null || child.signalCode !== null) break;
      await delay(10);
    }
    assert.fail(`${message}; stderr=${stderr}; errors=${errors}`);
  }
  const send = (id, method, params = {}) => {
    if (!child.stdin.destroyed && !child.stdin.writableEnded) {
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    }
  };
  const log = async () => {
    try { return await readFile(events, "utf8"); }
    catch (error) { if (error.code === "ENOENT") return ""; throw error; }
  };
  send(1, "initialize");
  await until(() => responses.find((item) => item.id === 1), "initialize");
  return { child, closed, exited, send, until, responses, log, root,
    stderr: () => stderr, trailing: () => output, errors };
}

test("transport: stopped reader has bounded reply backlog and cancels without retry or acceptance", { timeout: 30000 }, async (t) => {
  const f = await peer(t);
  f.send(2, "tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
  await f.until(async () => (await f.log()).includes("ready "), "active fake task");
  f.child.stdout.pause();
  // Exactly 36 finite diagnostics: 9 MiB raw plus about 9 MiB JSON replies.
  // Sequential small batches allow completed requests to release active slots.
  for (let batch = 0; batch < 9 && !f.stderr().includes("MCP output backlog"); batch++) {
    for (let n = 0; n < 4; n++) {
      f.send(10 + batch * 4 + n, "tools/call", { name: "pvisor_doctor" });
    }
    await f.until(async () => {
      const log = await f.log();
      return log.split("version ").length - 1 >= (batch + 1) * 4 ||
        f.stderr().includes("MCP output backlog");
    }, `bounded diagnostics batch ${batch}`);
    await delay(100);
  }
  t.diagnostic(`Completed bounded fake diagnostics: ${(await f.log()).split("version ").length - 1}; raw generation budget 9 MiB, serialized replies about 9 MiB maximum`);
  assert.match(f.stderr(), /MCP output backlog exceeds 8 MiB/, "completed requests must not queue unlimited replies");
  f.child.stdout.resume();
  const result = await f.closed;
  assert.deepEqual(result, { code: 1, signal: null });
  assert.equal(f.trailing(), "", "resumed reader receives only complete newline JSON-RPC frames");
  assert.ok((await f.log()).includes("cancel "), "overload cancels active task");
  assert.equal((await readdir(path.join(f.root, "state"))).length, 1, "no automatic rerun");
  assert.ok(f.responses.every((item) => item.id === 1 || (item.id >= 10 && item.id < 46)));
  assert.deepEqual(f.errors, []);
});

test("transport: escaped-byte backlog joins cancellation before forced output exit", { timeout: 15000 }, async (t) => {
  const f = await peer(t, { escaped: true, delayedCancel: true });
  f.send(2, "tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
  const ready = await f.until(async () => (await f.log()).match(/^ready (\d+)$/m), "active delayed-cancel task");
  const taskPid = ready[1];
  f.child.stdout.pause();
  // Six 256 KiB NUL diagnostics inflate through both JSON serialization layers.
  // Under 13 MiB combined raw/serialized generation, independent of RSS/timing.
  for (let n = 0; n < 6; n++) f.send(10 + n, "tools/call", { name: "pvisor_doctor" });
  await f.until(() => f.stderr().includes("MCP output backlog"), "escaped-byte overload");
  await f.until(async () => (await f.log()).includes(`cancel ${taskPid}\n`), "delayed cancellation acknowledged");
  // The fixture cannot complete on its own just after this observation:
  // release is controlled by this test, not a competing 1.8-second timer.
  await delay(1250);
  assert.equal(f.child.exitCode, null, "flush expiry must not abandon an active supervisor");
  assert.equal(f.child.signalCode, null);
  assert.ok(!(await f.log()).includes(`exit ${taskPid}\n`), "fake task still owns execution after flush deadline");
  await writeFile(path.join(f.root, "release"), "release after flush survival");
  assert.deepEqual(await f.exited, { code: 1, signal: null });
  assert.ok((await f.log()).includes(`released ${taskPid}\n`), "test released task before independent safety deadline");
  assert.ok((await f.log()).includes(`exit ${taskPid}\n`), "task exited before MCP exit");
  // Drain only after exit. A partial final frame is allowed on forced closure;
  // the shared parser still verifies every complete frame and exact unique ID.
  f.child.stdout.resume();
  await f.closed;
  assert.match(f.stderr(), /MCP output backlog exceeds 8 MiB/);
  assert.deepEqual(f.errors, []);
});

test("transport: paused reader resumes with exact IDs and explicit cancellation", { timeout: 15000 }, async (t) => {
  const f = await peer(t);
  f.send(2, "tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
  await f.until(async () => (await f.log()).includes("ready "), "active fake task");
  f.child.stdout.pause();
  f.send(3, "tools/call", { name: "pvisor_doctor" });
  await f.until(async () => (await f.log()).includes("version "), "one diagnostic");
  f.send(undefined, "notifications/cancelled", { requestId: 2 });
  await f.until(async () => (await f.log()).includes("cancel "), "cancellation while reader paused");
  f.send(4, "ping");
  f.child.stdout.resume();
  await f.until(() => [2, 3, 4].every((id) => f.responses.some((item) => item.id === id)), "resumed exact replies");
  assert.equal(f.responses.find((item) => item.id === 2).result.isError, true);
  f.child.stdin.end();
  assert.deepEqual(await f.closed, { code: 0, signal: null });
  assert.equal(f.trailing(), "");
  assert.equal(f.stderr(), "");
  assert.deepEqual(f.errors, []);
});

for (const fault of ["EOF", "EPIPE"]) {
  test(`transport: ${fault} cancels an active fake task with paused replies`, { timeout: 15000 }, async (t) => {
    const f = await peer(t);
    f.send(2, "tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
    await f.until(async () => (await f.log()).includes("ready "), "active fake task");
    f.child.stdout.pause();
    f.send(3, "tools/call", { name: "pvisor_doctor" });
    await f.until(async () => (await f.log()).includes("version "), "diagnostic before disconnect");
    if (fault === "EOF") {
      f.child.stdin.end();
      await f.until(async () => (await f.log()).includes("cancel "), "EOF cancels despite paused output");
      f.child.stdout.resume();
    }
    else {
      f.child.stdout.destroy();
      f.send(4, "ping");
    }
    assert.deepEqual(await f.closed, { code: 0, signal: null });
    assert.ok((await f.log()).includes("cancel "));
    assert.equal((await readdir(path.join(f.root, "state"))).length, 1);
    assert.equal(f.stderr(), "");
    assert.deepEqual(f.errors, []);
  });
}
