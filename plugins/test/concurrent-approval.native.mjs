import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readdir, realpath, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { Backend, execute } from "../backend.mjs";
import { resolveRuntime } from "../runtime.mjs";
import { discoverTask, removeAfterReconcile } from "./disconnect-fixture.mjs";
import { approvalOperation, pendingApproval } from "./concurrent-approval-fixture.mjs";

function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  return result;
}

async function fixture(t, kind) {
  const controller = new AbortController();
  const operations = [];
  let root;
  let project;
  let options;
  let backend;
  let attemptedRun = false;
  let changedTmp = false;
  const previousTmp = process.env.TMPDIR;
  let host;
  // Register before any fallible setup. Every native operation is registered
  // before invocation, including capability probes and the second client.
  function track(callback, caller, milliseconds = 5000) {
    const signal = AbortSignal.any([
      controller.signal, t.signal, AbortSignal.timeout(milliseconds),
      ...(caller ? [caller] : []),
    ]);
    const promise = Promise.resolve().then(() => {
      signal.throwIfAborted();
      return callback(signal);
    });
    promise.catch(() => {});
    operations.push(promise);
    return promise;
  }
  t.after(async () => {
    controller.abort();
    host?.reject();
    // Includes spawn, pending approval, adapter preview/apply, direct CLI,
    // second-client apply and final review. No native work may race removal.
    await Promise.allSettled(operations);
    if (changedTmp) {
      if (previousTmp === undefined) delete process.env.TMPDIR;
      else process.env.TMPDIR = previousTmp;
    }
    if (!root) return;
    const cleanupDeadline = AbortSignal.timeout(10_000);
    const cleanupSignal = () => AbortSignal.any([cleanupDeadline, AbortSignal.timeout(2000)]);
    await removeAfterReconcile(root, async () => {
      if (!attemptedRun) return true;
      const selector = await discoverTask(options.stateDir, project);
      if (!selector) return false;
      const status = JSON.parse(ok(await backend.operate("status", { selector }, cleanupSignal())).stdout);
      assert.equal(status.live, false);
      assert.ok(["completed", "failed", "cancelled"].includes(status.state), JSON.stringify(status));
      const dropped = await backend.operate("drop", { selector }, cleanupSignal());
      if (dropped.exit_code !== 0) {
        assert.equal(dropped.interrupted, false);
        assert.match(dropped.stderr, /already applied; drop cannot undo changes/);
        assert.equal(status.run.overlay.state, "applied");
        assert.equal(status.filesystem.changed_files, 0);
      }
      // Refuse removal if any current-namespace mount still names this owned root.
      if (process.platform === "linux") {
        assert.ok(!(await readFile("/proc/self/mountinfo", "utf8")).includes(root));
        for (const pid of (await readdir("/proc")).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid)) {
          try {
            if ((await stat(`/proc/${pid}`)).uid !== process.getuid()) continue;
            assert.ok(!(await readFile(`/proc/${pid}/cmdline`, "utf8")).includes(root), `owned process ${pid} remains`);
          } catch (error) {
            if (!["ENOENT", "ESRCH", "EACCES"].includes(error.code)) throw error;
          }
        }
      }
      return true;
    });
  });
  // Track the entire setup too: a timeout during a filesystem await must not
  // allow setup to create a root after teardown has finished.
  return track(async (setupSignal) => {
    const binary = await track((signal) => resolveRuntime(process.env.PVISOR_PLUGIN_BINARY, { signal }), setupSignal, 2000);
    const base = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR || tmpdir();
    await mkdir(base, { recursive: true });
    // Retain the owned path even if the subsequent realpath fails.
    root = await mkdtemp(path.join(base, "plugin-concurrent-approval-"));
    root = await realpath(root);
    project = path.join(root, "project");
    const temporary = path.join(root, "tmp");
    await mkdir(project);
    await mkdir(temporary);
    setupSignal.throwIfAborted();
    process.env.TMPDIR = temporary;
    changedTmp = true;
    options = { binary, stateDir: path.join(root, "state") };
    backend = new Backend(project, options);
    t.diagnostic(`Concurrent approval evidence: ${root}`);
    attemptedRun = true;
    const run = ok(await track((signal) => backend.run({
      argv: ["sh", "-c", "printf 'candidate a\\n' > a.txt; printf 'candidate b\\n' > b.txt; printf 'candidate c\\n' > c.txt"],
      timeout_seconds: 5,
    }, signal), setupSignal, 15_000));
    const calls = [];
    host = pendingApproval(kind, project, () => ({
      project: () => backend.project(),
      operate: (action, input, signal) => {
        calls.push(action);
        return track((combined) => backend.operate(action, input, combined), signal);
      },
    }));
    return {
      root, project, host, calls, selector: run.selector, options,
      start: (input) => approvalOperation(host, input, (callback) => track(callback)),
      operate: (action, input) => track((signal) => backend.operate(action, input, signal)),
      otherApply: (input) => track((signal) => new Backend(project, options).operate("apply", input, signal)),
      native: (args) => track((signal) => execute(binary, args, { cwd: project, signal })),
    };
  }, undefined, 20_000);
}

for (const kind of ["pi", "dsh"]) {
  for (const operation of ["partial apply", "partial apply with conflict", "all selected apply", "drop", "other plugin apply"]) {
    test(`concurrent approval ${kind}: terminal ${operation} during pending selective apply`, { timeout: 30_000 }, async (t) => {
      const f = await fixture(t, kind);
      const input = Object.freeze({ selector: f.selector, paths: Object.freeze(["a.txt", "b.txt"]) });
      const pending = f.start(input);
      const applying = pending.settled;
      const approval = await pending.ready;
      assert.match(approval.reason || approval.message, /a\.txt/);
      assert.match(approval.reason || approval.message, /b\.txt/);
      assert.ok((approval.reason || approval.message).includes(f.selector));
      assert.deepEqual(f.calls, ["preview"]);
      await assert.rejects(stat(path.join(f.project, "a.txt")), { code: "ENOENT" });
      if (operation === "other plugin apply") {
        ok(await f.otherApply({ selector: f.selector, paths: ["c.txt"] }));
      } else if (operation === "drop") {
        ok(await f.native(["drop", f.selector]));
      } else {
        const paths = operation.startsWith("partial apply") ? ["a.txt"] : ["a.txt", "b.txt"];
        ok(await f.native(["apply", f.selector, ...paths.flatMap((name) => ["--path", name]), "--json"]));
        assert.equal(await readFile(path.join(f.project, "a.txt"), "utf8"), "candidate a\n");
      }
      // A legitimate host edit after the concurrent operation must survive stale approval.
      if (operation !== "other plugin apply") await writeFile(path.join(f.project, "a.txt"), "user terminal edit\n");
      if (operation === "partial apply with conflict") await writeFile(path.join(f.project, "b.txt"), "user conflict edit\n");
      f.host.release();
      if (operation === "other plugin apply") {
        const result = JSON.parse(ok(await applying).stdout);
        assert.deepEqual(result.selected.map((item) => item.path), ["a.txt", "b.txt"]);
        assert.equal(await readFile(path.join(f.project, "a.txt"), "utf8"), "candidate a\n");
        assert.equal(await readFile(path.join(f.project, "b.txt"), "utf8"), "candidate b\n");
        assert.equal(await readFile(path.join(f.project, "c.txt"), "utf8"), "candidate c\n");
      } else {
        if (operation === "drop") await assert.rejects(applying, /discarded|dropped/i);
        else if (operation === "partial apply with conflict") await assert.rejects(applying, /target changed after staging.*b\.txt; refusing to overwrite concurrent changes/s);
        else {
          const result = JSON.parse(ok(await applying).stdout);
          assert.deepEqual(result.selected.map((item) => item.path), operation === "partial apply" ? ["b.txt"] : []);
          if (operation === "partial apply") assert.equal(result.execution.applied, 1);
          else assert.equal(result.execution, null, "empty selection is a native no-op");
        }
        assert.equal(await readFile(path.join(f.project, "a.txt"), "utf8"), "user terminal edit\n");
        if (operation === "partial apply with conflict") assert.equal(await readFile(path.join(f.project, "b.txt"), "utf8"), "user conflict edit\n");
        else if (operation.includes("apply")) assert.equal(await readFile(path.join(f.project, "b.txt"), "utf8"), "candidate b\n");
        else await assert.rejects(stat(path.join(f.project, "b.txt")), { code: "ENOENT" });
        await assert.rejects(stat(path.join(f.project, "c.txt")), { code: "ENOENT" });
      }
      const review = ok(await f.operate("review", { selector: f.selector }));
      if (operation === "partial apply with conflict") assert.match(review.stdout, /b\.txt/);
      if (operation.includes("apply") && operation !== "other plugin apply") assert.match(review.stdout, /c\.txt/);
      assert.deepEqual(f.calls, ["preview", "apply"], "no automatic retry or implicit drop");
      assert.equal(f.host.requests.length, 1);
      assert.equal((await readdir(f.options.stateDir)).length, 1, "no repeated spawn");
    });
  }
}
