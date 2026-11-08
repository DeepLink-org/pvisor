import assert from "node:assert/strict";
import { readdir, stat } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";
import { discoverTask } from "./disconnect-fixture.mjs";
import { completeObservation, ownedProcesses, requirePromptTeardown } from "./descendants-fixture.mjs";

const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";

for (const topology of ["background child", "setsid grandchild"]) {
  for (const outcome of ["nonzero", "cancel"]) {
    test(`descendant pipes: ${topology}, ${outcome}`, {
      skip: process.platform !== "linux", timeout: 90_000,
    }, async (t) => {
      let visibilityFailure;
      const f = await codingFixture(t, {
        runMilliseconds: 55_000,
        verifyOwnership: () => { if (visibilityFailure) throw visibilityFailure; },
      });
      const readinessBirths = new Map();
      async function observe() {
        const observed = await ownedProcesses(f.root, [...readinessBirths.values()]);
        console.log(`OWNERSHIP ${JSON.stringify(observed)}`);
        try { completeObservation(observed); }
        catch (error) { visibilityFailure = error; throw error; }
        return observed;
      }
      const cancel = new AbortController();
      const caller = AbortSignal.any([t.signal, cancel.signal]);
      const started = Date.now();
      const pending = f.run({
        argv: [python, "-u", "-c", `
import os, pathlib, signal, sys, time
# Alarms are not inherited across fork: arm each new process explicitly.
signal.alarm(45)
owner = sys.argv[1]
assert sys.stdin.read(1) == ""
r, w = os.pipe()
child = os.fork()
if child == 0:
    signal.alarm(45)
    os.close(r)
    if ${topology === "setsid grandchild" ? "True" : "False"}:
        grandchild = os.fork()
        if grandchild:
            os._exit(0)
        signal.alarm(45)
        os.setsid()
    # Keep stdin/stdout/stderr inherited, including EOF on stdin.
    assert sys.stdin.read(1) == ""
    print("DESCENDANT_STDOUT owner=" + owner, flush=True)
    print("DESCENDANT_STDERR owner=" + owner, file=sys.stderr, flush=True)
    os.write(w, b"R")
    os.close(w)
    time.sleep(40)
    os._exit(0)
os.close(w)
assert os.read(r, 1) == b"R"
os.close(r)
if ${topology === "setsid grandchild" ? "True" : "False"}:
    os.waitpid(child, 0)
pathlib.Path("candidate.txt").write_text("unfinished test: ${topology}, ${outcome}\\n")
pathlib.Path("descendant-ready.txt").write_text("descendant inherited all three descriptors\\n")
print("PARENT_READY owner=" + owner, flush=True)
print("PARENT_STDERR", file=sys.stderr, flush=True)
if ${outcome === "cancel" ? "True" : "False"}:
    time.sleep(40)
    print("UNEXPECTED_PARENT_FINISH", flush=True)
sys.exit(7)
`, f.root],
        timeout_seconds: 8,
      }, caller);
      // Observe early failure immediately; readiness must not hide settlement.
      const settled = pending.then(
        (result) => ({ result }),
        (error) => ({ error }),
      );
      let selector;
      if (outcome === "cancel") {
        const readiness = f.track(async (signal) => {
          for (;;) {
            signal.throwIfAborted();
            selector = await discoverTask(path.join(f.root, "state"), f.project);
            if (selector) {
              // Live review is not a staged-file visibility promise. Both
              // workload Python processes enter their sole timed sleep only
              // after the descriptor handshake and parent candidate writes.
              const observed = await observe();
              for (const entry of observed.processes) readinessBirths.set(entry.hostPid, entry);
              const sleepers = observed.processes.filter((entry) =>
                entry.command.split("\0")[0] === python &&
                entry.state === "S" && entry.waitChannel === "hrtimer_nanosleep");
              if (sleepers.length === 2) return;
            }
            await delay(50, undefined, { signal });
          }
        }, caller, 6000);
        await Promise.race([
          readiness,
          settled.then(({ result, error }) => { throw error || new Error(`Run settled before ready: ${JSON.stringify(result)}`); }),
        ]);
        const live = JSON.parse(ok(await f.operate("status", { selector }, caller)).stdout);
        console.log(`LIVE ${topology} ${JSON.stringify(live)}`);
        assert.equal(live.live, true);
        console.log(`OWNED_READY ${JSON.stringify(await observe())}`);
        for (const action of ["apply", "drop"]) {
          const refusal = await f.operate(action, {
            selector, ...(action === "apply" ? { paths: ["candidate.txt"] } : {}),
          }, caller);
          assert.notEqual(refusal.exit_code, 0, `${action} must refuse live work`);
          assert.equal(refusal.interrupted, false);
        }
        cancel.abort(new Error("User cancelled ready descendant fixture"));
      }
      const run = await pending;
      const elapsedMs = Date.now() - started;
      console.log(`DESCENDANT_RESULT ${JSON.stringify({ topology, outcome, elapsedMs, run })}`);
      // Observe immediately, before status/review operations can hide a late
      // self-exit. Any survivor fails even if safety cleanup later sees none.
      const terminalOwnership = await ownedProcesses(f.root, [...readinessBirths.values()]);
      console.log(`OWNED_TERMINAL ${JSON.stringify(terminalOwnership)}`);
      try { completeObservation(terminalOwnership); }
      catch (error) { visibilityFailure = error; }
      await requirePromptTeardown(terminalOwnership, Date.now() - started,
        (milliseconds) => delay(milliseconds, undefined, { signal: t.signal }));
      assert.ok(elapsedMs < 49_000, "runtime8s + adapter30s + escalation/drain10s");
      assert.equal(run.signal, null);
      assert.equal(run.interrupted, outcome === "cancel");
      assert.equal(run.exit_code, outcome === "cancel" ? 143 : 7);
      assert.equal(run.truncated, false);
      assert.match(run.stdout, /DESCENDANT_STDOUT/);
      assert.match(run.stdout, /PARENT_READY/);
      assert.match(run.stderr, /DESCENDANT_STDERR/);
      assert.match(run.stderr, /PARENT_STDERR/);
      assert.doesNotMatch(run.stdout, /UNEXPECTED_PARENT_FINISH/);
      selector ??= run.selector;
      assert.equal(run.selector, selector);
      assert.equal(run.stage, selector);
      assert.equal(await discoverTask(path.join(f.root, "state"), f.project), selector);
      const status = JSON.parse(ok(await f.operate("status", { selector })).stdout);
      console.log(`TERMINAL ${JSON.stringify(status)}`);
      assert.equal(status.live, false);
      assert.equal(status.state, outcome === "cancel" ? "cancelled" : "failed");
      assert.ok(status.run.finished_at_unix_ms > 0, "native finish time must be recorded");
      assert.equal(status.live_execution, null);
      await assert.rejects(stat(path.join(f.project, "candidate.txt")), { code: "ENOENT" });
      assert.match(ok(await f.operate("review", { selector })).stdout, /candidate\.txt/);
      const preview = ok(await f.operate("preview", { selector, paths: ["candidate.txt"] }));
      assert.equal(preview.summary.can_confirm, true);
      // Real adapter approval refusal, not a host UI claim. No acceptance here.
      const host = approvalHost("pi", f.project, () => f.client);
      const approval = approvalOperation(host, "apply", { selector, paths: ["candidate.txt"] }, t.signal);
      try {
        const request = await approval.entered();
        assert.ok(request.reason.includes(selector));
        host.reject();
        await approval.result;
        assert.equal(await approval.settled, null);
      } finally {
        await approval.close();
      }
      await assert.rejects(stat(path.join(f.project, "candidate.txt")), { code: "ENOENT" });
      assert.match(ok(await f.operate("review", { selector })).stdout, /candidate\.txt/);
      assert.equal((await readdir(path.join(f.root, "state"))).length, 1, "no automatic retry");
      console.log(`OPERATIONS ${JSON.stringify({ topology, outcome, actions: f.calls.map((call) => call.action) })}`);
      // codingFixture's joined hook rechecks terminal/nonlive, drops natively,
      // verifies no owned processes/mounts, then (and only then) removes root.
    });
  }
}
