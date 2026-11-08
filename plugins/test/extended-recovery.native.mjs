import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { appendFile, mkdir, readFile, readdir, stat, statfs, writeFile } from "node:fs/promises";
import { loadavg } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Backend } from "../backend.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";
import { completeObservation, ownedProcesses } from "./descendants-fixture.mjs";
import { discoverTask } from "./disconnect-fixture.mjs";

// Real-duration opt-in, deliberately not configurable down to a short smoke test.
// Run ONE anchored name through the existing native task's serial Node PATH shim.
const optIn = process.env.PVISOR_PLUGIN_EXTENDED_RECOVERY;
const selectedCase = process.env.PVISOR_PLUGIN_EXTENDED_CASE;
const enabled = optIn === "1";
const python = "/usr/bin/python3";
const terminalStates = ["completed", "failed", "cancelled"];
const deadlineInput = process.env.PVISOR_PLUGIN_EXTENDED_DEADLINE;
const campaignEnd = Date.parse(deadlineInput || "");
const modes = ["cancel", "failure", "server-death", "stale-approval"];
if (optIn !== undefined || selectedCase !== undefined) {
  assert.equal(optIn, "1", "explicit extended recovery configuration requires exact opt-in 1");
  assert.ok(modes.includes(selectedCase), "select one explicit extended recovery case");
  assert.ok(Number.isFinite(campaignEnd) && /T.*(?:Z|\+00:00)$/.test(deadlineInput),
    "explicit timezone-aware ISO UTC campaign deadline required");
}

function birth(text) {
  return text.slice(text.lastIndexOf(")") + 2).split(" ")[19];
}

/** A scoped raw MCP peer. Native ownership belongs to the fixture, not this pipe. */
async function peer(t, f, signal) {
  const owner = new AbortController();
  signal = AbortSignal.any([signal, t.signal, owner.signal, AbortSignal.timeout(650_000)]);
  let child;
  let closed;
  let identity;
  let stopping;
  let bytes = 0;
  let output = "";
  let stderr = "";
  // Registered before spawn and before any fallible asynchronous setup.
  async function stop() {
    if (!child || child.exitCode !== null || child.signalCode !== null) {
      if (closed) await closed;
      return;
    }
    if (stopping) return stopping;
    stopping = (async () => {
      child.stdin.end();
      await Promise.race([closed, delay(5000)]);
      if (child.exitCode === null && child.signalCode === null) await kill();
      await closed;
    })();
    stopping.catch(() => {});
    return stopping;
  }
  t.after(async () => {
    owner.abort(new Error("Owned MCP peer closing"));
    await stop();
  });
  async function kill() {
    if (child.exitCode !== null || child.signalCode !== null) return;
    // Namespace PIDs printed by workloads are never used here.
    const before = birth(await readFile(`/proc/${child.pid}/stat`, "utf8"));
    const uid = (await stat(`/proc/${child.pid}`)).uid;
    const after = birth(await readFile(`/proc/${child.pid}/stat`, "utf8"));
    assert.equal(uid, process.getuid());
    assert.equal(before, identity);
    assert.equal(after, identity);
    assert.equal((await stat(`/proc/${child.pid}`)).uid, uid);
    child.kill("SIGKILL"); // Only this unreaped child, with verified host birth/UID.
  }
  return f.track(async (combined) => {
    combined.throwIfAborted();
    child = spawn(process.execPath, [
      fileURLToPath(new URL("../mcp.mjs", import.meta.url)), "--project", f.project,
    ], {
      env: { ...process.env, PVISOR_PLUGIN_STATE_DIR: path.join(f.root, "state") },
      stdio: ["pipe", "pipe", "pipe"],
    });
    closed = new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code, signal) => resolve({ code, signal }));
    });
    closed.catch(() => {});
    child.stdin.on("error", () => {});
    child.stdout.on("data", (chunk) => {
      bytes += chunk.length;
      if (bytes > 64 * 1024) {
        child.stdout.destroy();
        void stop().catch(() => {});
      } else output += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => { stderr = (stderr + chunk.toString("utf8")).slice(-8192); });
    const onAbort = () => { void stop().catch(() => {}); };
    signal.addEventListener("abort", onAbort, { once: true });
    closed.finally(() => signal.removeEventListener("abort", onAbort)).catch(() => {});
    if (signal.aborted) onAbort();
    identity = birth(await readFile(`/proc/${child.pid}/stat`, "utf8"));
    assert.equal((await stat(`/proc/${child.pid}`)).uid, process.getuid());
    return {
      child, closed, kill, stop,
      identity: { hostPid: child.pid, startTicks: identity, uid: process.getuid() },
      send(id, method, params) {
        signal.throwIfAborted();
        child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
      },
      replies() { return output.split("\n").slice(0, -1).filter(Boolean).map((line) => JSON.parse(line)); },
      diagnostic: () => ({ bytes, stderr }),
    };
  }, signal);
}

for (const mode of modes) {
  test(`extended recovery ${mode}: exact-stage long-lived recovery`, {
    skip: !enabled || selectedCase !== mode, timeout: 800_000,
  }, async (t) => {
    const safety = new AbortController();
    const signal = AbortSignal.any([t.signal, safety.signal]);
    const cancel = new AbortController();
    let f;
    let timer;
    let sampling = Promise.resolve();
    let sampleError;
    let previous = [];
    let externalAttempt = false;
    let selector;
    let nativeStarted;
    let disposed = false;
    let transport;
    let eventBytes = 0;
    let events = Promise.resolve();
    let sequence = 0;
    const results = [];
    let phase = "setup";
    const base = process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR;
    assert.ok(base && path.isAbsolute(base), "durable absolute evidence directory required");
    const evidence = path.join(base, `round17-${mode}.jsonl`);
    // Tracking is synchronous; the write is serialized and rejection-observed.
    function log(event, detail = {}) {
      const line = JSON.stringify({ utc: new Date().toISOString(), mode, phase, event, ...detail }) + "\n";
      eventBytes += Buffer.byteLength(line);
      const pending = events.then(async () => {
        assert.ok(eventBytes < 2 * 1024 ** 2, "bounded durable evidence exceeded");
        await appendFile(evidence, line);
      });
      events = pending;
      pending.catch(() => {});
      return pending;
    }
    // Setup failure cannot strand an interval or unobserved write.
    t.after(async () => {
      clearInterval(timer);
      safety.abort(new Error("Round17 fixture closing"));
      await sampling;
      await events;
      if (sampleError) throw sampleError;
    });
    await mkdir(base, { recursive: true });
    await appendFile(evidence, "", { flag: "wx", mode: 0o600 });
    assert.ok(Date.now() < campaignEnd, "campaign deadline: no new workload");
    async function sample(label) {
      const available = Number((await readFile("/proc/meminfo", "utf8")).match(/^MemAvailable:\s+(\d+)/m)[1]) * 1024;
      const disk = await statfs(base);
      const resources = { available, disk: disk.bavail * disk.bsize, load1: loadavg()[0] };
      const owned = f ? await ownedProcesses(f.root, previous) : undefined;
      if (owned) previous = owned.processes;
      await log("sample", { label, resources, owned,
        selfOwnedMounts: f ? (await readFile("/proc/self/mountinfo", "utf8")).split("\n").filter((line) => line.includes(f.root)) : [] });
      assert.ok(available >= 6 * 1024 ** 3 && resources.disk >= 20 * 1024 ** 3 && resources.load1 <= 8,
        `resource gate crossed: ${JSON.stringify(resources)}`);
    }
    await sample("preflight");
    // Reconciliation here is deliberately independent of cancelled test signals.
    // It is also used by the removal guard if setup/body fails after MCP launch.
    async function reconcileExternal() {
      if (!externalAttempt || disposed) return;
      const deadline = nativeStarted + 625_000;
      let status;
      while (Date.now() < deadline) {
        selector ||= await discoverTask(path.join(f.root, "state"), f.project);
        if (selector) {
          const backend = new Backend(f.project, {
            binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(f.root, "state"),
          });
          await log("cleanup operation", { action: "status", selector });
          const result = await backend.operate("status", { selector }, AbortSignal.timeout(5000));
          if (result.exit_code === 0 && !result.interrupted) {
            status = JSON.parse(result.stdout);
            if (!status.live && terminalStates.includes(status.state)) {
              await log("cleanup terminal", { selector, status });
              const observed = await ownedProcesses(f.root, previous);
              completeObservation(observed);
              assert.deepEqual(observed.processes, [], "native task still owns a visible process");
              await log("cleanup operation", { action: "drop", selector });
              ok(await backend.operate("drop", { selector }, AbortSignal.timeout(5000)));
              disposed = true;
              return;
            }
          }
        }
        await delay(5000);
      }
      throw new Error(`Preserve unknown native owner at ${f.root}; selector=${selector}; status=${JSON.stringify(status)}`);
    }
    f = await codingFixture(t, {
      runMilliseconds: 650_000,
      async verifyOwnership() {
        await reconcileExternal();
        const observed = await ownedProcesses(f.root, previous);
        await log("removal observation", { root: f.root, selector, observed });
        completeObservation(observed);
        assert.deepEqual(observed.processes, [], "owned process remains; preserve root");
      },
    });
    t.after(async () => {
      await log("final resource check", { root: f.root, selector, disposed });
      await sample("post cleanup");
    });
    timer = setInterval(() => {
      sampling = sampling.then(() => sample("minute")).catch((error) => {
        sampleError ||= error;
        safety.abort(error);
      });
    }, 60_000);
    const options = { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(f.root, "state") };
    const reopened = new Backend(f.project, options);
    const client = {
      project: () => f.track(() => reopened.project(), signal),
      operate(action, input, caller) {
        const operation = ++sequence;
        return f.track(async (combined) => {
          await log("operation", { operation, action, input });
          const result = await reopened.operate(action, input, combined);
          results.push({ operation, action, input: structuredClone(input), result });
          await log("operation result", { operation, result });
          return result;
        }, caller ? AbortSignal.any([signal, caller]) : signal);
      },
    };
    async function until(check, milliseconds, label, caller = signal, pauseMs = 1000) {
      const end = Date.now() + milliseconds;
      while (Date.now() < end) {
        caller.throwIfAborted();
        const result = await check();
        if (result) return result;
        await delay(pauseMs, undefined, { signal: caller });
      }
      throw new Error(`Timed out: ${label}; preserve ${f.root}`);
    }
    async function status() {
      return JSON.parse(ok(await client.operate("status", { selector })).stdout);
    }
    async function review() {
      const result = ok(await client.operate("review", { selector }));
      assert.match(result.stdout, /calc\.py/);
      assert.match(result.stdout, /test-result\.txt/);
      return result;
    }
    async function approve(input, idleMs = 0, edit = false) {
      const host = approvalHost("pi", f.project, () => client);
      await log("approval attempt", { input, idleMs, edit });
      const operation = approvalOperation(host, "apply", input, signal, idleMs + 30_000);
      try {
        const request = await operation.entered();
        assert.ok(request.reason.includes(selector));
        assert.match(request.reason, /calc\.py/);
        const entered = Date.now();
        phase = edit ? "stale approval idle" : "approval idle";
        await log("approval entered", { entered, request });
        if (edit) {
          await delay(idleMs / 2, undefined, { signal });
          await writeFile(path.join(f.project, "calc.py"), "host editor change\n");
          await log("fixture host edit", { path: "calc.py" });
          await delay(idleMs / 2, undefined, { signal });
        } else await delay(idleMs, undefined, { signal });
        const elapsedMs = Date.now() - entered;
        await log("approval idle end", { elapsedMs });
        assert.ok(elapsedMs >= idleMs, "approval must remain genuinely paused");
        if (!edit) assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), "baseline\n");
        host.approve();
        const error = await operation.settled;
        if (edit) assert.ok(error, "old approval must reject the host conflict");
        else {
          await operation.result;
          assert.equal(error, null);
        }
        return error;
      } finally { await operation.close(); }
    }
    await writeFile(path.join(f.project, "calc.py"), "baseline\n");
    const runtime = mode === "cancel" ? 570 : mode === "failure" ? 570 : mode === "server-death" ? 570 : 30;
    const seconds = mode === "cancel" ? 550 : mode === "stale-approval" ? 0 : 510;
    const script = `
import pathlib, signal, sys, time, unittest
signal.alarm(590)
owner = sys.argv[1]
assert sys.stdin.read(1) == ""
pathlib.Path("calc.py").write_text("def add(a, b):\\n    return a + b\\n")
pathlib.Path("scratch.txt").write_text("discard watch scratch\\n")
pathlib.Path("test-result.txt").write_text("partial: source assertion passed; long phase unfinished\\n")
assert "a + b" in pathlib.Path("calc.py").read_text()
print("WATCH_STARTED owner=" + owner, flush=True)
end = time.monotonic() + ${seconds}
beat = 0
while time.monotonic() < end:
    time.sleep(min(60, max(0, end - time.monotonic())))
    beat += 1
    print("HEARTBEAT=" + str(beat), flush=True)
class SlowResult(unittest.TestCase):
    def test_source(self):
        scope = {}
        exec(pathlib.Path("calc.py").read_text(), scope)
        self.assertEqual(scope["add"](2, 3), ${mode === "failure" ? 6 : 5})
result = unittest.TextTestRunner(stream=sys.stderr).run(unittest.defaultTestLoader.loadTestsFromTestCase(SlowResult))
pathlib.Path("test-result.txt").write_text("final: unittest " + ("PASS" if result.wasSuccessful() else "FAIL") + "\\n")
print("WATCH_FINISHED", flush=True)
sys.exit(0 if result.wasSuccessful() else 1)
`;
    const input = { argv: [python, "-u", "-c", script, f.root], timeout_seconds: runtime };
    await log("run attempt", { input });
    assert.ok(Date.now() < campaignEnd, "campaign deadline: no new native task");
    nativeStarted = Date.now();
    phase = "native active";
    let pending;
    if (mode === "server-death") {
      transport = await peer(t, f, signal);
      previous.push(transport.identity);
      await log("owned MCP", { identity: transport.identity });
      transport.send(1, "initialize", { protocolVersion: "2025-03-26" });
      await until(() => transport.replies().find((reply) => reply.id === 1)?.result, 10_000, "MCP initialize");
      externalAttempt = true; // Record before dispatch; write failure is uncertain too.
      await log("MCP spawn attempt", { input });
      nativeStarted = Date.now();
      transport.send(2, "tools/call", { name: "pvisor_spawn", arguments: input });
    } else {
      pending = f.run(input, AbortSignal.any([signal, cancel.signal]));
      pending.catch(() => {});
    }
    let early;
    pending?.then((result) => { early = result; }, (error) => { early = error; });
    await until(async () => {
      if (early && mode !== "stale-approval") throw new Error(`Task settled before live observation: ${JSON.stringify(early)}`);
      selector = await discoverTask(options.stateDir, f.project);
      if (!selector) return false;
      const result = await client.operate("status", { selector });
      if (result.exit_code !== 0) return false;
      const observed = JSON.parse(result.stdout);
      if (mode === "stale-approval") return !observed.live && terminalStates.includes(observed.state);
      return observed.live && observed.filesystem?.sample_paths.includes("test-result.txt");
    }, 20_000, "exact staged readiness");
    if (mode !== "stale-approval") {
      for (const action of ["preview", "apply", "drop"]) {
        const refused = await client.operate(action, {
          selector, ...(action !== "drop" ? { paths: ["calc.py"] } : {}),
        });
        assert.notEqual(refused.exit_code, 0, `${action} must refuse live work`);
        assert.equal(refused.interrupted, false);
      }
    }
    if (mode === "cancel" || mode === "server-death") {
      await delay(Math.max(0, (mode === "cancel" ? 480_000 : 360_000) - (Date.now() - nativeStarted)), undefined, { signal });
      if (early) throw new Error(`Task ended before long-phase action: ${JSON.stringify(early)}`);
      assert.equal((await status()).live, true);
      if (mode === "cancel") cancel.abort(new Error("User cancelled after eight real minutes"));
      else {
        assert.ok(!transport.replies().some((reply) => reply.id === 2), "spawn response must still be outstanding");
        await log("MCP kill attempt", { identity: transport.identity });
        await transport.kill();
        assert.equal((await transport.closed).signal, "SIGKILL");
        phase = "lost transport reconciliation";
        await log("transport lost", { elapsedActiveMs: Date.now() - nativeStarted, diagnostic: transport.diagnostic(),
          outputRecovery: "in-memory adapter output lost; native inherit is not native capture" });
      }
    }
    let run;
    let terminal;
    if (pending) {
      run = await pending;
      const elapsedActiveMs = Date.now() - nativeStarted;
      await log("run result", { elapsedActiveMs, run });
      if (mode !== "stale-approval") assert.ok(elapsedActiveMs >= (mode === "cancel" ? 480_000 : 510_000));
      assert.equal(run.selector, selector);
      assert.equal(run.truncated, false);
      assert.equal(run.signal, null);
      assert.match(run.stdout, /WATCH_STARTED/);
      if (mode === "cancel") {
        assert.equal(run.interrupted, true);
        assert.notEqual(run.exit_code, 0);
        assert.match(run.stdout, /HEARTBEAT=7/);
        assert.doesNotMatch(run.stdout, /WATCH_FINISHED/);
      } else {
        assert.equal(run.interrupted, false);
        assert.equal(run.exit_code, mode === "failure" ? 1 : 0);
        assert.match(run.stderr, mode === "failure" ? /FAILED \(failures=1\)/ : /Ran 1 test[\s\S]*OK/);
        assert.match(run.stdout, /WATCH_FINISHED/);
      }
      terminal = await status();
      assert.equal(terminal.live, false);
      assert.equal(terminal.state, mode === "cancel" ? "cancelled" : mode === "failure" ? "failed" : "completed");
    } else {
      const lostAt = Date.now();
      terminal = await until(async () => {
        const observed = await status();
        return !observed.live && terminalStates.includes(observed.state) ? observed : false;
      }, Math.max(1, nativeStarted + 620_000 - Date.now()), "native terminal receipt after owned server death", signal, 5000);
      await log("loss reconciliation end", { elapsedLostMs: Date.now() - lostAt });
    }
    await log("terminal", { selector, terminal, elapsedToReceiptMs: Date.now() - nativeStarted });
    assert.ok(terminal.run.finished_at_unix_ms > 0, "native terminal receipt must record finish");
    assert.equal(terminal.live_execution, null);
    phase = "terminal review";
    const observed = await ownedProcesses(f.root, previous);
    await log("terminal owned observation", { observed });
    completeObservation(observed);
    assert.deepEqual(observed.processes, [], "owned task survived terminal receipt");
    const candidates = await review();
    if (mode === "server-death") {
      const elapsedToReceiptMs = Date.now() - nativeStarted;
      if (terminal.state === "completed") {
        assert.ok(elapsedToReceiptMs >= 510_000, "completed continuation must include the genuine long phase");
        assert.match(candidates.stdout, /^\+final: unittest PASS$/m,
          "native review must retain the final result candidate, not invented recovered stdout");
        await log("server-death classification", {
          branch: "completed long continuation",
          elapsedToReceiptMs, terminalProjection: terminal.run,
          resultCandidate: "native review contains final: unittest PASS",
          frontendExitCode: null,
          frontendExitCodeEvidence: "unknown: no spawn response received; public status does not expose numeric exit",
          recoveredStdout: false,
        });
      } else {
        assert.ok(["failed", "cancelled"].includes(terminal.state));
        const partial = /^\+partial: source assertion passed; long phase unfinished$/m.test(candidates.stdout);
        const final = /^\+final: unittest PASS$/m.test(candidates.stdout);
        assert.ok(partial || final, "native review must retain an identifiable partial or final result candidate");
        await log("server-death classification", {
          branch: "non-success terminal after transport loss; cause not established",
          elapsedToReceiptMs, terminalProjection: terminal.run,
          observedBeforeNaturalEnd: elapsedToReceiptMs < 510_000,
          resultCandidate: partial ? "partial assertion marker; unittest completion not established" : "final marker present; no stdout recovery",
          frontendExitCode: null,
          frontendExitCodeEvidence: "unknown: no spawn response received; public status does not expose numeric exit",
          recoveredStdout: false,
          claim: "not a 510-second continuation pass; no BrokenPipe cause inferred",
        });
      }
    }
    assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), "baseline\n");
    await assert.rejects(stat(path.join(f.project, "test-result.txt")), { code: "ENOENT" });
    await assert.rejects(stat(path.join(f.project, "scratch.txt")), { code: "ENOENT" });
    assert.equal((await readdir(options.stateDir)).length, 1, "exactly one native attempt, no rerun");
    if (mode === "cancel" || mode === "stale-approval") {
      const selection = { selector, paths: ["calc.py", "test-result.txt"] };
      const preview = ok(await client.operate("preview", selection));
      assert.equal(preview.summary.can_confirm, true);
      assert.deepEqual(preview.summary.selected.map((entry) => entry.path).sort(), selection.paths);
      assert.deepEqual(preview.summary.pending.map((entry) => entry.path), ["scratch.txt"]);
      const beforeConflict = sequence;
      await approve(selection, mode === "cancel" ? 120_000 : 540_000, mode === "stale-approval");
      if (mode === "stale-approval") {
        assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), "host editor change\n");
        await review();
        // Inspect the exact old approval's native apply, not just its UI notice
        // or a later preview that independently discovers the same conflict.
        const applies = results.filter((entry) => entry.operation > beforeConflict && entry.action === "apply");
        assert.equal(applies.length, 1, "old approval reaches native apply exactly once");
        assert.deepEqual(applies[0].input, selection);
        assert.equal(applies[0].result.selector, selector);
        assert.equal(applies[0].result.project, f.project);
        assert.equal(applies[0].result.exit_code, 3);
        assert.equal(applies[0].result.interrupted, false);
        const diagnostic = JSON.parse(applies[0].result.stdout);
        assert.equal(diagnostic.status, "blocked");
        assert.equal(diagnostic.source.stage, selector);
        assert.deepEqual(diagnostic.blockers, [
          `overlay apply failed: target changed after staging at ${path.join(f.project, "calc.py")}; refusing to overwrite concurrent changes`,
        ]);
        await log("explicit fixture-only restore", { path: "calc.py" });
        await writeFile(path.join(f.project, "calc.py"), "baseline\n");
        await review();
        ok(await client.operate("preview", selection));
        await approve(selection);
      }
      assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), "def add(a, b):\n    return a + b\n");
      assert.equal(await readFile(path.join(f.project, "test-result.txt"), "utf8"),
        mode === "cancel" ? "partial: source assertion passed; long phase unfinished\n" : "final: unittest PASS\n");
      assert.match(ok(await client.operate("review", { selector })).stdout, /scratch\.txt/);
    }
    await log("explicit drop attempt", { selector });
    ok(await client.operate("drop", { selector }));
    disposed = true;
    await assert.rejects(stat(path.join(f.project, "scratch.txt")), { code: "ENOENT" });
    if (mode === "failure" || mode === "server-death") {
      assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), "baseline\n");
      await assert.rejects(stat(path.join(f.project, "test-result.txt")), { code: "ENOENT" });
    }
    await sample("body complete");
    await log("case complete", { elapsedMs: Date.now() - nativeStarted, selector, disposed });
  });
}
