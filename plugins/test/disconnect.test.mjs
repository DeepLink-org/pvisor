import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { discoverTask, disconnectFixture, removeAfterReconcile } from "./disconnect-fixture.mjs";

test("selector discovery waits for publication and rejects invalid completed records or extra tasks", async (t) => {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "plugin-record-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const id = "12345678-1234-4234-8234-123456789abc";
  const state = path.join(root, "state");
  const directory = path.join(state, id);
  await mkdir(directory, { recursive: true });
  const project = root;
  assert.equal(await discoverTask(state, project), false);
  const file = path.join(directory, "task.json");
  for (const partial of ["", "{", '{"schema_version":1,"task":']) {
    await writeFile(file, partial);
    assert.equal(await discoverTask(state, project), false);
  }
  const valid = { schema_version: 1, task: id, project };
  for (const invalid of [null, {}, { ...valid, task: "other" }, { ...valid, project: "/other" }, { ...valid, extra: true }]) {
    await writeFile(file, JSON.stringify(invalid));
    await assert.rejects(discoverTask(state, project));
  }
  await writeFile(file, JSON.stringify(valid));
  assert.equal(await discoverTask(state, project), path.join(directory, "stage"));
  await mkdir(path.join(state, "unexpected"));
  await assert.rejects(discoverTask(state, project), /extra task/);
});

test("cleanup preserves fixtures on uncertain ownership or reconciliation failure", async (t) => {
  const root = await mkdtemp(path.join(tmpdir(), "plugin-cleanup-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const file = path.join(root, "candidate");
  await writeFile(file, "retained");
  for (const reconcile of [async () => false, async () => { throw new Error("still live"); }]) {
    await assert.rejects(removeAfterReconcile(root, reconcile), (error) =>
      error.message.includes(`Preserved fixture ${root}`));
    assert.equal(await readFile(file, "utf8"), "retained");
  }
  await removeAfterReconcile(root, async () => true);
  await assert.rejects(stat(root), { code: "ENOENT" });
});

test("lost MCP output cancels active work even while the request pipe stays open", { timeout: 10_000 }, async (t) => {
  // Fake runtime proves transport supervision only, not native Session state.
  const f = await disconnectFixture(t, "/nonexistent");
  const binary = path.join(f.root, "runtime.mjs");
  const ready = path.join(f.root, "ready");
  const cancelled = path.join(f.root, "cancelled");
  await writeFile(binary, `#!${process.execPath}
import {writeFileSync} from "node:fs";
if (process.argv.includes("--help")) {
  console.log("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json");
} else {
  writeFileSync(${JSON.stringify(ready)}, String(process.pid));
  process.on("SIGTERM", () => {
    writeFileSync(${JSON.stringify(cancelled)}, "cancelled");
    process.exit(0);
  });
  setTimeout(() => process.exit(0), 3000);
}
`, { mode: 0o700 });
  // The executable lives in the first owned fixture. Start the actual fault
  // peer with that path fixed in its environment, as with an installed runtime.
  await f.child.stdin.end();
  await f.closed;
  const g = await disconnectFixture(t, binary);
  g.send(2, "tools/call", { name: "pvisor_spawn", arguments: { argv: ["wait"] } });
  await g.until(async () => {
    try { return await readFile(ready, "utf8"); }
    catch (error) { if (error.code === "ENOENT") return false; throw error; }
  }, "fake task readiness");
  g.child.stdout.destroy();
  g.send(3, "ping");
  const [code, signal] = await g.closed;
  assert.equal(signal, null);
  assert.equal(code, 0);
  assert.equal(await readFile(cancelled, "utf8"), "cancelled");
  assert.equal(g.child.stdin.destroyed, true);
});
