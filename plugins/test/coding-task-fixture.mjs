import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readdir, realpath, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { Backend, execute } from "../backend.mjs";
import { discoverTask, removeAfterReconcile } from "./disconnect-fixture.mjs";

export function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.signal, null, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  assert.equal(result.truncated, false, JSON.stringify(result));
  return result;
}

/**
 * One fresh coding task, with durable ownership even when setup/spawn fails.
 * Optional verifyOwnership runs inside the removal guard after native disposal;
 * throwing preserves the root when an observer cannot certify ownership.
 */
export async function codingFixture(t, { runMilliseconds = 15_000, verifyOwnership = async () => {} } = {}) {
  const controller = new AbortController();
  const owned = [];
  let root;
  let project;
  let backend;
  let options;
  let attempted = false;
  let changedTmp = false;
  const previousTmp = process.env.TMPDIR;
  function track(callback, caller, milliseconds = 15_000) {
    const signal = AbortSignal.any([
      controller.signal, t.signal, AbortSignal.timeout(milliseconds),
      ...(caller ? [caller] : []),
    ]);
    const pending = Promise.resolve().then(() => {
      signal.throwIfAborted();
      return callback(signal);
    });
    owned.push(pending);
    pending.catch(() => {});
    return pending;
  }
  t.after(async () => {
    controller.abort(new Error("Coding fixture closing"));
    await Promise.allSettled(owned);
    if (changedTmp) {
      if (previousTmp === undefined) delete process.env.TMPDIR;
      else process.env.TMPDIR = previousTmp;
    }
    if (!root) return;
    await removeAfterReconcile(root, async () => {
      if (attempted) {
        const selector = await discoverTask(options.stateDir, project);
        assert.ok(selector, "attempted run has no discoverable authority");
        const deadline = AbortSignal.timeout(10_000);
        const signal = () => AbortSignal.any([deadline, AbortSignal.timeout(2000)]);
        const status = JSON.parse(ok(await backend.operate("status", { selector }, signal())).stdout);
        assert.equal(status.live, false);
        assert.ok(["completed", "failed", "cancelled"].includes(status.state), JSON.stringify(status));
        const dropped = await backend.operate("drop", { selector }, signal());
        if (dropped.exit_code !== 0) {
          assert.equal(dropped.interrupted, false);
          assert.equal(dropped.signal, null);
          assert.match(dropped.stderr, /already applied; drop cannot undo changes/);
          assert.equal(status.run.overlay.state, "applied");
          assert.equal(status.filesystem.changed_files, 0);
        } else ok(dropped);
      }
      if (process.platform === "linux") {
        await verifyOwnership();
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
  return track(async (signal) => {
    const base = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR || tmpdir();
    await mkdir(base, { recursive: true });
    root = await mkdtemp(path.join(base, "plugin-coding-task-"));
    root = await realpath(root);
    project = path.join(root, "project with spaces");
    await mkdir(project);
    const temporary = path.join(root, "tmp");
    await mkdir(temporary);
    signal.throwIfAborted();
    process.env.TMPDIR = temporary;
    changedTmp = true;
    options = { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(root, "state") };
    backend = new Backend(project, options);
    const calls = [];
    const client = {
      project: () => backend.project(),
      operate(action, input, caller) {
        calls.push({ action, input: structuredClone(input) });
        return track((combined) => backend.operate(action, input, combined), caller);
      },
    };
    t.diagnostic(`Coding task evidence: ${root}`);
    return {
      root, project, calls, client,
      run(input, caller) {
        assert.equal(attempted, false, "dependent edits/tests belong in one fresh task");
        attempted = true;
        return track((combined) => backend.run(input, combined), caller, runMilliseconds);
      },
      operate: client.operate,
      command: (binary, argv) => track((combined) => execute(binary, argv, { cwd: project, signal: combined })),
      track,
    };
  });
}
