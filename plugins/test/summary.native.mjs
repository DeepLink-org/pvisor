import assert from "node:assert/strict";
import { mkdtemp, mkdir, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";
import { formatPreview } from "../summary.mjs";

test("native summary: exact selected actions, binary/mode facts, excluded pending and host conflict", { timeout: 90_000 }, async (t) => {
  const controller = new AbortController();
  const signal = AbortSignal.any([controller.signal, t.signal]);
  const attempts = [];
  // Join all started work, including failed spawns. Retain roots and candidates:
  // this read-only preview test never infers authority to drop from a failure.
  t.after(async () => {
    controller.abort(new Error("Summary native fixture teardown"));
    await Promise.allSettled(attempts);
  });
  function track(work) {
    const pending = Promise.resolve().then(work);
    attempts.push(pending);
    pending.catch(() => {});
    return pending;
  }
  const evidence = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR || tmpdir();
  await mkdir(evidence, { recursive: true });
  const root = await realpath(await mkdtemp(path.join(evidence, "plugin-summary-")));
  t.diagnostic(`Retained summary native evidence (review explicitly before cleanup): ${root}`);
  const project = path.join(root, "project");
  await mkdir(project);
  await writeFile(path.join(project, "changed.txt"), "before\n");
  await writeFile(path.join(project, "deleted.txt"), "before\n");
  await writeFile(path.join(project, "mode-only"), "same\n", { mode: 0o644 });
  const backend = new Backend(project, { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(root, "state") });
  const run = await track(() => backend.run({
    argv: ["sh", "-c", "printf 'new\\n' > added.txt; printf 'after\\n' > changed.txt; rm deleted.txt; printf '\\000\\377' > binary.bin; chmod 755 mode-only; printf 'pending\\n' > excluded.txt"],
    timeout_seconds: 10,
  }, signal));
  assert.equal(run.exit_code, 0, JSON.stringify(run));
  assert.equal(run.interrupted, false, JSON.stringify(run));
  const input = { selector: run.selector, paths: ["added.txt", "changed.txt", "deleted.txt", "binary.bin", "mode-only"] };
  const preview = await track(() => backend.operate("preview", input, signal));
  assert.equal(preview.exit_code, 0, JSON.stringify(preview));
  assert.equal(preview.summary.can_confirm, true, JSON.stringify(preview));
  assert.deepEqual(preview.summary.selected.map(({ path, kind }) => [path, kind]).sort(), [
    ["added.txt", "added"], ["binary.bin", "added"], ["changed.txt", "modified"],
    ["deleted.txt", "deleted"], ["mode-only", "modified"],
  ]);
  assert.deepEqual(preview.summary.pending.map(({ path }) => path), ["excluded.txt"]);
  const text = formatPreview(preview.summary);
  assert.match(text, /Selected changes: 5/);
  assert.match(text, /Other staged candidates \(NOT applied by this selection\): 1/);
  assert.match(text, /mode 755 \(reported, not a mode delta\)/);
  assert.match(text, /Content\/binary classification, previous modes, semantic purpose and test results are not provided/);
  assert.equal(JSON.parse(preview.stdout).selected.length, 5, "native stdout remains unchanged JSON");
  await writeFile(path.join(project, "changed.txt"), "concurrent host edit\n");
  const blocked = await track(() => backend.operate("preview", input, signal));
  assert.equal(blocked.exit_code, 3, JSON.stringify(blocked));
  assert.equal(blocked.summary.can_confirm, false);
  assert.match(formatPreview(blocked.summary), /Blockers \/ conflicts: [1-9]/);
  const empty = await track(() => backend.operate("preview", { selector: run.selector, paths: ["missing.txt"] }, signal));
  assert.equal(empty.exit_code, 0, JSON.stringify(empty));
  assert.match(formatPreview(empty.summary), /no-op, not an all-pending apply/);
});
