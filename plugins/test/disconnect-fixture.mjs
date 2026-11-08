import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, readdir, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

/** A task record is published after its directory; partial JSON is not ready. */
export async function discoverTask(stateDir, project) {
  let ids;
  try { ids = await readdir(stateDir); }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
  if (!ids.length) return false;
  assert.equal(ids.length, 1, "no automatic retry or extra task");
  const id = ids[0];
  assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  const directory = path.join(stateDir, id);
  assert.equal(await realpath(directory), directory, "task directory must be canonical");
  let record;
  try { record = JSON.parse(await readFile(path.join(directory, "task.json"), "utf8")); }
  catch (error) {
    if (error.code === "ENOENT" || error instanceof SyntaxError) return false;
    throw error;
  }
  assert.deepEqual(record, { schema_version: 1, task: id, project });
  assert.equal(await realpath(project), project, "project must be canonical");
  return path.join(directory, "stage");
}

/** Never remove a native fixture whose terminal ownership cannot be proven. */
export async function removeAfterReconcile(root, reconcile) {
  try {
    assert.equal(await reconcile(), true, "terminal ownership not established");
  } catch (error) {
    throw new Error(`Preserved fixture ${root}: ${error.message}`, { cause: error });
  }
  await rm(root, { recursive: true, force: true });
}

/** Owned raw stdio peer: faults affect only this server, never host clients. */
export async function disconnectFixture(t, binary, { reconcile = async () => true } = {}) {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "plugin-disconnect-")));
  const project = path.join(root, "project");
  const stateDir = path.join(root, "state");
  const temporary = path.join(root, "tmp");
  await mkdir(project);
  await mkdir(temporary);
  const child = spawn(process.execPath, [
    fileURLToPath(new URL("../mcp.mjs", import.meta.url)), "--project", project,
  ], {
    env: {
      ...process.env, TMPDIR: temporary,
      PVISOR_PLUGIN_BINARY: binary, PVISOR_PLUGIN_STATE_DIR: stateDir,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const closed = once(child, "close");
  const responses = [];
  let stderr = "";
  child.stderr.on("data", (bytes) => { stderr = (stderr + bytes).slice(-8192); });
  // Disconnect injection can race an in-flight transport write.
  child.stdin.on("error", () => {});
  const lines = createInterface({ input: child.stdout });
  lines.on("line", (line) => {
    assert.ok(line.length < 2 * 1024 * 1024);
    responses.push(JSON.parse(line));
  });
  async function until(check, message, milliseconds = 8000) {
    const deadline = Date.now() + milliseconds;
    while (Date.now() < deadline) {
      const result = await check();
      if (result) return result;
      await delay(20);
    }
    assert.fail(`${message}; server stderr: ${stderr}`);
  }
  function send(id, method, params = {}) {
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
  }
  async function response(id) {
    return until(() => responses.find((item) => item.id === id), `Missing response ${id}`);
  }
  t.after(async () => {
    child.stdin.end();
    const timer = setTimeout(() => child.kill("SIGKILL"), 6000);
    await closed;
    clearTimeout(timer);
    lines.close();
    await removeAfterReconcile(root, () => reconcile({ root, project, stateDir, until }));
  });
  send(1, "initialize", { protocolVersion: "2025-03-26" });
  assert.ok((await response(1)).result);
  return { root, project, stateDir, temporary, child, closed, responses, send, response, until };
}
