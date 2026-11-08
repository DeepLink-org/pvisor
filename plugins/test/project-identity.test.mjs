import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";

test("project identity: validate private task records before any runtime lookup", { timeout: 15_000 }, async (t) => {
  let root;
  let setup;
  t.after(async () => {
    // Synthetic adapter records only: no stage/native work exists to reconcile.
    await setup?.catch(() => {});
    if (root) await rm(root, { recursive: true });
  });
  setup = (async () => {
    root = await mkdtemp(path.join(tmpdir(), "plugin-record-contract-"));
    root = await realpath(root);
    const project = path.join(root, "project");
    const other = path.join(root, "other");
    const stateDir = path.join(root, "state");
    await mkdir(project);
    await mkdir(other);
    await mkdir(stateDir, { mode: 0o700 });
    const task = randomUUID();
    const directory = path.join(stateDir, task);
    await mkdir(directory, { mode: 0o700 });
    return { project, other, stateDir, task, directory };
  })();
  setup.catch(() => {});
  const f = await setup;
  // This deliberately missing binary must never be consulted for bad identity.
  const backend = new Backend(f.project, { stateDir: f.stateDir, binary: path.join(root, "not-a-runtime") });
  const selector = path.join(f.directory, "stage");
  const recordPath = path.join(f.directory, "task.json");
  const valid = { schema_version: 1, task: f.task, project: f.project };
  await writeFile(recordPath, JSON.stringify(valid), { mode: 0o600 });
  assert.deepEqual(await backend.task(selector), { selector, project: f.project, stage: selector });
  for (const invalid of [
    { ...valid, schema_version: 2 },
    { ...valid, schema_version: "1" },
    { ...valid, task: randomUUID() },
    { ...valid, project: f.other },
    { schema_version: 1, project: f.project },
    { schema_version: 1, task: f.task },
  ]) {
    await writeFile(recordPath, JSON.stringify(invalid));
    for (const action of ["status", "review", "preview", "apply", "drop"]) {
      await assert.rejects(backend.operate(action, {
        selector, ...(["preview", "apply"].includes(action) ? { paths: ["shared.txt"] } : {}),
      }), /does not belong to this project or has an unsupported schema/);
    }
  }
  await writeFile(recordPath, "{");
  await assert.rejects(backend.task(selector), SyntaxError);
  await rm(recordPath);
  await assert.rejects(backend.task(selector), { code: "ENOENT" });
  await writeFile(recordPath, JSON.stringify(valid), { mode: 0o600 });
  assert.deepEqual(await new Backend(f.project, { stateDir: f.stateDir }).task(selector), {
    selector, project: f.project, stage: selector,
  });
});
