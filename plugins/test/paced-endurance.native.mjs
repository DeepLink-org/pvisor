import assert from "node:assert/strict";
import { appendFile, lstat, readFile, readdir, realpath, stat, statfs } from "node:fs/promises";
import { loadavg } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Backend } from "../backend.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";
import { discoverTask } from "./disconnect-fixture.mjs";

const enabled = process.env.PVISOR_PLUGIN_PACED_ENDURANCE === "1";
const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";
const stories = ["accept", "exit7", "conflict", "cancel", "fresh", "noop"];
const gib = 1024 ** 3;

// Read-only observer. Host birth identities, never namespace-local printed PIDs.
// Visibility failures are evidence gaps, not proof of namespace containment.
async function observe(root) {
  const processes = [];
  const denied = [];
  let raced = 0;
  for (const pid of (await readdir("/proc")).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid)) {
    try {
      if ((await stat(`/proc/${pid}`)).uid !== process.getuid()) continue;
      const before = await readFile(`/proc/${pid}/stat`, "utf8");
      const command = await readFile(`/proc/${pid}/cmdline`, "utf8");
      if (!command.includes(path.join(root, "plugin-coding-task-"))) continue;
      const fields = before.slice(before.lastIndexOf(")") + 2).split(" ");
      const waitChannel = (await readFile(`/proc/${pid}/wchan`, "utf8")).trim();
      const mounts = (await readFile(`/proc/${pid}/mountinfo`, "utf8")).split("\n").filter((line) => line.includes(root));
      const after = await readFile(`/proc/${pid}/stat`, "utf8");
      if (after.slice(after.lastIndexOf(")") + 2).split(" ")[19] !== fields[19]) {
        raced++;
        continue;
      }
      processes.push({ pid: Number(pid), startTicks: fields[19], state: fields[0], waitChannel,
        ownedMounts: mounts.length, argv0: command.split("\0")[0] });
    } catch (error) {
      if (["EACCES", "EPERM"].includes(error.code)) denied.push(Number(pid));
      else if (["ENOENT", "ESRCH"].includes(error.code)) raced++;
      else throw error;
    }
  }
  let files = 0;
  let bytes = 0;
  let stages = 0;
  let stageBytes = 0;
  async function walk(directory, inStage = false) {
    let entries;
    try { entries = await readdir(directory, { withFileTypes: true }); }
    catch (error) { if (error.code === "ENOENT") return; throw error; }
    for (const entry of entries) {
      assert.ok(++files <= 8192, "owned retention entry limit exceeded");
      const name = path.join(directory, entry.name);
      const info = await lstat(name);
      const stage = inStage || entry.name === "stage";
      if (entry.isDirectory()) {
        if (entry.name === "stage") stages++;
        await walk(name, stage);
      } else {
        bytes += info.size;
        if (stage) stageBytes += info.size;
      }
    }
  }
  await walk(root);
  const selfMounts = (await readFile("/proc/self/mountinfo", "utf8")).split("\n").filter((line) => line.includes(root)).length;
  return { processes, processCount: processes.length, denied, raced, selfMounts,
    visibleOwnedMounts: selfMounts + processes.reduce((sum, p) => sum + p.ownedMounts, 0),
    stages, stageBytes, files, bytes, visibility: "self and observable same-UID owned processes only" };
}

test("paced endurance: seeded coding and review cycles", {
  skip: !enabled, timeout: 820_000,
}, async (t) => {
  assert.equal(process.platform, "linux", "paced observer currently requires Linux");
  const segment = Number(process.env.PVISOR_PLUGIN_PACED_SEGMENT);
  assert.ok(Number.isInteger(segment) && segment >= 1 && segment <= 4, "select segment 1..4 explicitly");
  const seed = Number(process.env.PVISOR_PLUGIN_PACED_SEED || 1401);
  assert.ok(Number.isSafeInteger(seed) && seed >= 0 && seed <= 0xffffffff);
  assert.ok(process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR, "durable owned output directory required");
  const base = await realpath(process.env.PVISOR_PLUGIN_TEST_OUTPUT_DIR);
  const evidence = path.join(base, `paced-${seed}-segment-${segment}.jsonl`);
  // Never overwrite evidence or automatically rerun an uncertain segment.
  await appendFile(evidence, "", { flag: "wx", mode: 0o600 });
  let logBytes = 0;
  let attempts = 0;
  let operations = 0;
  let fixtureCommands = 0;
  let reconciliations = 0;
  let activeMs = 0;
  let idleMs = 0;
  let rng = (seed ^ segment) >>> 0;
  const random = () => { rng = (Math.imul(rng, 1664525) + 1013904223) >>> 0; return rng; };
  const safety = new AbortController();
  const caller = AbortSignal.any([t.signal, safety.signal]);
  const started = Date.now();
  let finished = false;
  async function log(event, detail = {}) {
    const line = JSON.stringify({ utc: new Date().toISOString(), event, segment, seed,
      attempts, operations, fixtureCommands, reconciliations, activeMs, idleMs, ...detail }) + "\n";
    logBytes += Buffer.byteLength(line);
    assert.ok(logBytes <= 4 * 1024 ** 2, "evidence retention limit exceeded");
    await appendFile(evidence, line);
    console.log(line.trimEnd());
  }
  async function sample(phase, quiescent = false) {
    const memory = await readFile("/proc/meminfo", "utf8");
    const available = Number(memory.match(/^MemAvailable:\s+(\d+)/m)[1]) * 1024;
    const disk = await statfs(base);
    const resources = { available, disk: disk.bavail * disk.bsize, load1: loadavg()[0] };
    const owned = await observe(base);
    await log("sample", { phase, resources, owned });
    if (available < 6 * gib || resources.disk < 20 * gib || resources.load1 > 8 || owned.bytes > 16 * 1024 ** 2) {
      const error = new Error(`Resource/retention guard crossed at ${phase}`);
      safety.abort(error);
      throw error;
    }
    if (quiescent) {
      assert.equal(owned.processCount, 0, "owned process remains; stop launching");
      assert.equal(owned.visibleOwnedMounts, 0, "visible owned mount remains; stop launching");
      assert.equal(owned.stages, 0, "retained stage remains; stop launching");
    }
    return owned;
  }
  // Install before setup. Fixtures register their own cancellation/reconciliation
  // hooks before creating roots; uncertain roots are never recursively removed here.
  t.after(async () => {
    safety.abort(new Error("Paced segment closing"));
    await sample("final quiescence", finished);
    await log("segment end", { finished, wallMs: Date.now() - started, logBytes });
  });
  await sample("segment start", true);
  let previous = "none";
  for (let cycle = 0; cycle < 8; cycle++) {
    caller.throwIfAborted();
    assert.ok(Date.now() < Date.parse("2026-11-08T09:04:00Z"),
      "campaign launch deadline reached; obtain a new campaign window before reuse");
    // Cover the six user stories first. Later choices follow the previous
    // observation: a discarded/no-op view is followed by a fresh coding view.
    const story = cycle < stories.length ? stories[(cycle + segment - 1) % stories.length]
      : ["noop", "exit7", "cancel"].includes(previous) ? "fresh" : stories[random() % 4];
    const token = `round14-${seed}-${segment}-${cycle}-${story}`;
    await log("cycle start", { cycle, story, previous, token });
    await sample(`cycle ${cycle} before`, true);
    const activeStarted = Date.now();
    let completed = false;
    let fixtureRoot;
    await t.test(`cycle ${cycle}: ${story}`, { timeout: 65_000 }, async (c) => {
      const f = await codingFixture(c, { runMilliseconds: 45_000 });
      fixtureRoot = f.root;
      const client = (cwd = f.project) => {
        const native = new Backend(cwd, { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(f.root, "state") });
        return {
          project: () => f.track(() => native.project(), caller),
          operate(action, input, signal) {
            operations++;
            return f.track((combined) => native.operate(action, input, combined),
              AbortSignal.any([caller, ...(signal ? [signal] : [])]));
          },
        };
      };
      const reopened = client();
      const command = (argv) => {
        fixtureCommands++;
        return f.command(python, argv);
      };
      async function decision(action, input, accept) {
        const host = approvalHost("pi", f.project, client);
        const operation = approvalOperation(host, action, input, caller);
        try {
          const request = await operation.entered();
          assert.ok(request.reason.includes(input.selector));
          if (accept) host.approve();
          else host.reject();
          await operation.result;
          assert.equal(await operation.settled, null);
        } finally { await operation.close(); }
      }
      // Baseline and proposal are tiny; Python performs a genuine offline test.
      await command(["-c", "from pathlib import Path; Path('src.txt').write_text('baseline\\n')"]);
      const cancel = new AbortController();
      attempts++;
      const pending = f.run({
        argv: [python, "-u", "-c", `
import pathlib, signal, sys, time, unittest
signal.alarm(35)
owner = sys.argv[1]
assert sys.stdin.read(1) == ""
if ${story === "noop" ? "False" : "True"}:
    pathlib.Path("src.txt").write_text(${JSON.stringify(`${token}\n`)})
    pathlib.Path("scratch.txt").write_text("discard me\\n")
expected = ${JSON.stringify(story === "noop" ? "baseline\n" : `${token}\n`)}
suite = unittest.TestSuite([unittest.FunctionTestCase(
    lambda: unittest.TestCase().assertEqual(pathlib.Path("src.txt").read_text(), expected))])
assert unittest.TextTestRunner(stream=sys.stderr).run(suite).wasSuccessful()
print(${JSON.stringify(token)}, flush=True)
if ${story === "cancel" ? "True" : "False"}:
    time.sleep(30)
    print("UNEXPECTED_FINISH", flush=True)
sys.exit(${story === "exit7" ? 7 : 0})
`, f.root],
        timeout_seconds: 10,
      }, AbortSignal.any([caller, cancel.signal]));
      const settled = pending.then((result) => ({ result }), (error) => ({ error }));
      let selector;
      if (story === "cancel") {
        const ready = f.track(async (signal) => {
          for (;;) {
            selector = await discoverTask(path.join(f.root, "state"), f.project);
            const owned = await observe(base);
            if (selector && owned.processes.some((p) => p.argv0 === python &&
              p.state === "S" && p.waitChannel === "hrtimer_nanosleep")) return;
            await delay(50, undefined, { signal });
          }
        }, caller, 6000);
        await Promise.race([ready, settled.then(({ result, error }) => {
          throw error || new Error(`Run settled before readiness: ${JSON.stringify(result)}`);
        })]);
        await sample(`cycle ${cycle} cancellation ready`);
        assert.equal(JSON.parse(ok(await reopened.operate("status", { selector })).stdout).live, true);
        for (const action of ["apply", "drop"]) {
          const refusal = await reopened.operate(action, { selector, ...(action === "apply" ? { paths: ["src.txt"] } : {}) });
          assert.notEqual(refusal.exit_code, 0);
          assert.equal(refusal.interrupted, false);
        }
        cancel.abort(new Error("User cancelled a ready test"));
      }
      const run = await pending;
      selector ??= run.selector;
      assert.equal(run.selector, selector);
      assert.equal(run.project, f.project);
      assert.equal(run.exit_code, story === "exit7" ? 7 : story === "cancel" ? 143 : 0);
      assert.equal(run.interrupted, story === "cancel");
      assert.equal(run.signal, null);
      assert.equal(run.truncated, false);
      assert.ok(Buffer.byteLength(run.stdout) + Buffer.byteLength(run.stderr) < 32 * 1024);
      assert.match(run.stderr, /Ran 1 test[\s\S]*OK/);
      assert.ok(run.stdout.includes(token));
      assert.doesNotMatch(run.stdout, /UNEXPECTED_FINISH/);
      await log("run", { cycle, story, run });
      const status = JSON.parse(ok(await reopened.operate("status", { selector })).stdout);
      assert.equal(status.live, false);
      assert.equal(status.state, story === "exit7" ? "failed" : story === "cancel" ? "cancelled" : "completed");
      assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"), "baseline\n");
      if (story !== "noop") {
        assert.match(ok(await reopened.operate("review", { selector })).stdout, /scratch\.txt/);
        const input = { selector, paths: ["src.txt"] };
        const preview = ok(await reopened.operate("preview", input));
        assert.equal(preview.summary.can_confirm, true);
        assert.deepEqual(preview.summary.selected.map((item) => item.path), ["src.txt"]);
        assert.deepEqual(preview.summary.pending.map((item) => item.path), ["scratch.txt"]);
        if (story === "conflict") {
          await command(["-c", "from pathlib import Path; Path('src.txt').write_text('editor change\\n')"]);
          const blocked = await reopened.operate("apply", input);
          assert.equal(blocked.exit_code, 3);
          assert.equal(blocked.interrupted, false);
          assert.equal(JSON.parse(blocked.stdout).status, "blocked");
          assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"), "editor change\n");
          await log("expected conflict", { cycle, result: blocked });
          // Only our disposable host fixture is restored, never native stage data.
          await command(["-c", "from pathlib import Path; Path('src.txt').write_text('baseline\\n')"]);
          ok(await reopened.operate("preview", input));
        }
        await decision("apply", input, !["exit7", "cancel", "fresh"].includes(story));
        assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"),
          ["exit7", "cancel", "fresh"].includes(story) ? "baseline\n" : `${token}\n`);
        if (["accept", "conflict"].includes(story)) {
          const remaining = ok(await reopened.operate("preview", input));
          assert.deepEqual(remaining.summary.selected, [], "accepted source cannot remain in the summary");
          assert.deepEqual(remaining.summary.pending.map((item) => item.path), ["scratch.txt"]);
          const repeated = JSON.parse(ok(await reopened.operate("apply", input)).stdout);
          assert.deepEqual(repeated.selected, []);
          assert.equal(repeated.execution, null, "already-applied explicit selection is a no-op, not all-pending apply");
          assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"), `${token}\n`);
          await log("already-applied no-op", { cycle, summary: remaining.summary, result: repeated });
        }
      } else {
        assert.equal(status.filesystem.changed_files, 0);
        const empty = ok(await reopened.operate("apply", { selector, paths: ["src.txt"] }));
        await log("no-op apply", { cycle, result: empty });
        const applied = JSON.parse(ok(await reopened.operate("status", { selector })).stdout);
        assert.equal(applied.filesystem.changed_files, 0);
      }
      if (story === "fresh") {
        const secondView = client();
        assert.equal(JSON.parse(ok(await secondView.operate("status", { selector })).stdout).state, "completed");
        assert.match(ok(await secondView.operate("review", { selector })).stdout, /src\.txt/);
        await assert.rejects(secondView.operate("status", { selector: "last" }), /exact absolute stage selector/);
      }
      await decision("drop", { selector }, true);
      assert.equal(JSON.parse(ok(await reopened.operate("status", { selector })).stdout).filesystem.changed_files, 0);
      await assert.rejects(stat(path.join(f.project, "scratch.txt")), { code: "ENOENT" });
      assert.equal((await readdir(path.join(f.root, "state"))).length, 1);
      // Fresh Backend objects must still address this exact retained task.
      // Two simultaneous same-project tasks remain covered by round6, not
      // claimed by this single-task endurance fixture.
      await sample(`cycle ${cycle} terminal before hook`);
      completed = true;
    });
    assert.equal(completed, true, `cycle ${cycle} failed; stop launching`);
    await assert.rejects(stat(fixtureRoot), { code: "ENOENT" });
    reconciliations++;
    activeMs += Date.now() - activeStarted;
    await sample(`cycle ${cycle} after hook`, true);
    previous = story;
    await log("cycle end", { cycle, story });
    // Real user think time between distinct cycles, never busy work or a
    // throughput benchmark. Segment boundaries retain the final gap except
    // after the campaign's final cycle.
    if (segment !== 4 || cycle !== 7) {
      const idleStarted = Date.now();
      await sample(`cycle ${cycle} idle before`, true);
      try {
        await delay(37_500, undefined, { signal: caller });
        await sample(`cycle ${cycle} idle midpoint`, true);
        await delay(37_500, undefined, { signal: caller });
        await sample(`cycle ${cycle} idle after`, true);
      } finally {
        idleMs += Date.now() - idleStarted;
      }
      await log("idle end", { cycle, intendedMs: 75_000 });
    }
  }
  finished = true;
});
