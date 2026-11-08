import assert from "node:assert/strict";
import { readFile, readdir, stat, statfs, writeFile } from "node:fs/promises";
import { loadavg } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Backend } from "../backend.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";
import { discoverTask } from "./disconnect-fixture.mjs";

// Deliberately real duration, never shortened by the ordinary native suite.
const enabled = process.env.PVISOR_PLUGIN_LONG_LIVED === "1";
const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";
const activeMs = 360_000;
const approvalMs = 180_000;

for (const mode of ["cancel", "deadline"]) {
  test(`long-lived ${mode}: retained output and paused selective approval`, {
    skip: !enabled, timeout: 700_000,
  }, async (t) => {
    const safety = new AbortController();
    let interval;
    let sampling = Promise.resolve();
    let guardError;
    const guards = [];
    // Register before setup; an asynchronous sampling failure cancels only our run.
    t.after(async () => {
      clearInterval(interval);
      await sampling;
      if (guardError) throw guardError;
    });
    async function guard(phase) {
      const memory = await readFile("/proc/meminfo", "utf8");
      const available = Number(memory.match(/^MemAvailable:\s+(\d+)/m)[1]) * 1024;
      const disk = await statfs(process.cwd());
      const sample = {
        utc: new Date().toISOString(), phase, available,
        disk: disk.bavail * disk.bsize, load1: loadavg()[0],
      };
      guards.push(sample);
      console.log(`RESOURCE ${JSON.stringify(sample)}`);
      assert.ok(available >= 6 * 1024 ** 3 && sample.disk >= 20 * 1024 ** 3 && sample.load1 <= 8,
        `resource guard crossed: ${JSON.stringify(sample)}`);
    }
    await guard("before setup");
    const f = await codingFixture(t, { runMilliseconds: 650_000 });
    interval = setInterval(() => {
      sampling = sampling.then(() => guard("minute sample")).catch((error) => {
        guardError = error;
        safety.abort(error);
      });
    }, 60_000);
    const cancel = new AbortController();
    const caller = AbortSignal.any([t.signal, safety.signal, cancel.signal]);
    const started = Date.now();
    const pending = f.run({
      argv: [python, "-u", "-c", `
import os, pathlib, subprocess, sys, time, unittest
# This owned token is visible in /proc cmdlines, including the stdout holder.
owner = ${JSON.stringify(f.root)}
assert sys.stdin.read(1) == ""
pathlib.Path("calc.py").write_text("def add(a, b):\\n    return a + b\\n")
suite = unittest.TestSuite([unittest.FunctionTestCase(lambda: None)])
result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
assert result.wasSuccessful()
pathlib.Path("test-result.txt").write_text("preflight: 1 test PASS; watch phase unfinished\\n")
pathlib.Path("scratch.txt").write_text("discard this watch scratch\\n")
child = subprocess.Popen([sys.executable, "-u", "-c",
    "import os,time; print('DESCENDANT_PID=' + str(os.getpid()), flush=True); time.sleep(500)", owner])
print("WATCH_STARTED owner=" + owner, flush=True)
for beat in range(1, 9):
    time.sleep(60)
    print("HEARTBEAT=" + str(beat), flush=True)
print("WATCH_FINISHED", flush=True)
child.wait(timeout=30)
`],
      timeout_seconds: mode === "cancel" ? 540 : 360,
    }, caller);
    // Readiness polling is bounded setup, not a substitute for the long wait.
    let selector;
    for (let i = 0; i < 100; i++) {
      selector = await discoverTask(path.join(f.root, "state"), f.project);
      if (selector) {
        try {
          const observed = await f.operate("status", { selector });
          if (observed.exit_code === 0 && JSON.parse(observed.stdout).live) break;
        } catch (error) {
          if (i === 99) throw error;
        }
      }
      await delay(100, undefined, { signal: caller });
    }
    assert.ok(selector);
    const live = JSON.parse(ok(await f.operate("status", { selector })).stdout);
    assert.equal(live.live, true);
    for (const action of ["apply", "drop"]) {
      const refused = await f.operate(action, {
        selector, ...(action === "apply" ? { paths: ["calc.py"] } : {}),
      });
      assert.notEqual(refused.exit_code, 0, `${action} must refuse live work`);
      assert.equal(refused.interrupted, false);
    }
    if (mode === "cancel") {
      await delay(Math.max(0, activeMs - (Date.now() - started)), undefined, { signal: caller });
      cancel.abort(new Error("User cancelled after six real minutes"));
    }
    const run = await pending;
    const activeElapsed = Date.now() - started;
    console.log(`PHASE ${mode} active_ms=${activeElapsed} result=${JSON.stringify(run)}`);
    assert.ok(activeElapsed >= 350_000 && activeElapsed < 630_000);
    assert.equal(run.selector, selector);
    assert.equal(run.truncated, false);
    assert.notEqual(run.exit_code, 0);
    assert.equal(run.interrupted, mode === "cancel");
    assert.equal(run.signal, null);
    if (mode === "deadline") {
      assert.equal(run.exit_code, 1);
      assert.match(run.stderr, /DeadlineExceeded: execution exceeded 360000 ms deadline/);
    } else {
      assert.equal(run.exit_code, 143);
    }
    assert.match(run.stdout, /WATCH_STARTED/);
    assert.match(run.stdout, /HEARTBEAT=5/);
    assert.doesNotMatch(run.stdout, /WATCH_FINISHED/);
    assert.match(run.stderr, /Ran 1 test[\s\S]*OK/);
    // The printed PID is namespace-local, not a host PID. Inspect host /proc
    // using the token inherited in both parent and descendant command lines.
    assert.match(run.stdout, /DESCENDANT_PID=\d+/);
    for (const pid of (await readdir("/proc")).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid)) {
      try {
        if ((await stat(`/proc/${pid}`)).uid !== process.getuid()) continue;
        const command = await readFile(`/proc/${pid}/cmdline`, "utf8");
        assert.ok(!command.includes(f.root), `owned process ${pid} still live; preserve ${f.root}`);
      } catch (error) {
        if (!["ENOENT", "ESRCH", "EACCES"].includes(error.code)) throw error;
      }
    }
    const observed = JSON.parse(ok(await f.operate("status", { selector })).stdout);
    console.log(`TERMINAL ${JSON.stringify(observed)}`);
    assert.equal(observed.live, false);
    assert.equal(observed.state, mode === "cancel" ? "cancelled" : "failed");
    await guard("terminal before approval");
    await assert.rejects(stat(path.join(f.project, "calc.py")), { code: "ENOENT" });
    // Reopen adapter over the existing exact task, never start another run.
    const reopened = new Backend(f.project, {
      binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(f.root, "state"),
    });
    const client = {
      project: () => reopened.project(),
      operate: (action, input, signal) => f.track((combined) => reopened.operate(action, input, combined),
        signal ? AbortSignal.any([signal, safety.signal]) : safety.signal),
    };
    assert.match(ok(await client.operate("review", { selector })).stdout, /test-result\.txt/);
    const input = { selector, paths: ["calc.py", "test-result.txt"] };
    const preview = ok(await client.operate("preview", input));
    assert.equal(preview.summary.can_confirm, true);
    assert.deepEqual(preview.summary.selected.map((item) => item.path).sort(), input.paths);
    assert.deepEqual(preview.summary.pending.map((item) => item.path), ["scratch.txt"]);
    const host = approvalHost("pi", f.project, () => client);
    const operation = approvalOperation(host, "apply", input,
      AbortSignal.any([t.signal, safety.signal]), 240_000);
    const idleStarted = Date.now();
    try {
      const request = await operation.entered();
      assert.ok(request.reason.includes(selector));
      assert.match(request.reason, /test-result\.txt/);
      await delay(approvalMs, undefined, { signal: AbortSignal.any([t.signal, safety.signal]) });
      await assert.rejects(stat(path.join(f.project, "calc.py")), { code: "ENOENT" });
      host.approve();
      await operation.result;
      assert.equal(await operation.settled, null);
    } finally {
      await operation.close();
    }
    console.log(`PHASE ${mode} approval_idle_ms=${Date.now() - idleStarted}`);
    assert.equal(await readFile(path.join(f.project, "test-result.txt"), "utf8"),
      "preflight: 1 test PASS; watch phase unfinished\n");
    assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"),
      "def add(a, b):\n    return a + b\n");
    assert.match(ok(await client.operate("review", { selector })).stdout, /scratch\.txt/);
    ok(await client.operate("drop", { selector }));
    await assert.rejects(stat(path.join(f.project, "scratch.txt")), { code: "ENOENT" });
    assert.equal((await readdir(path.join(f.root, "state"))).length, 1, "no automatic rerun");
    await writeFile(path.join(f.root, "long-lived-evidence.json"), JSON.stringify({ mode, activeElapsed, run, guards }));
    await guard("after drop");
  });
}
