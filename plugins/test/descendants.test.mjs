import assert from "node:assert/strict";
import test from "node:test";
import { completeObservation, ownedProcesses, requirePromptTeardown } from "./descendants-fixture.mjs";

const empty = () => ({ processes: [], inaccessible: [], unstable: [], reused: [] });
function procIo({ births = ["123", "123"], command = "python\0/owned\0", denied = false } = {}) {
  let reads = 0;
  return {
    readdir: async () => ["999999"],
    stat: async () => ({ uid: process.getuid() }),
    readFile: async (name) => {
      if (denied) throw Object.assign(new Error("not visible"), { code: "EACCES" });
      if (name.endsWith("/cmdline")) return command;
      if (name.endsWith("/wchan")) return "hrtimer_nanosleep";
      return `999999 (python) S ${Array(18).fill("0").join(" ")} ${births[Math.min(reads++, births.length - 1)]} 0`;
    },
  };
}

test("descendant observer: stable birth matches readiness even after argv changes", async () => {
  const ready = await ownedProcesses("/owned", [], procIo());
  completeObservation(ready);
  assert.equal(ready.processes[0].startTicks, "123");
  const terminal = await ownedProcesses("/owned", ready.processes, procIo({ command: "renamed\0" }));
  assert.equal(terminal.processes[0].trackedBirth, true);
  await assert.rejects(requirePromptTeardown(terminal, 100, async () => {}), /survived native/);
});

test("descendant observer: PID reuse is not the readiness birth", async () => {
  const prior = [{ hostPid: 999999, startTicks: "122" }];
  const observed = await ownedProcesses("/owned", prior, procIo({ command: "unrelated\0" }));
  completeObservation(observed);
  assert.deepEqual(observed.processes, []);
  assert.deepEqual(observed.reused, [{ hostPid: 999999, prior: "122", current: "123" }]);
});

test("descendant observer: birth race and denied visibility cannot certify zero", async () => {
  const raced = await ownedProcesses("/owned", [], procIo({ births: ["123", "124"] }));
  assert.throws(() => completeObservation(raced), /identity changed/);
  const denied = await ownedProcesses("/owned", [], procIo({ denied: true }));
  assert.deepEqual(denied.processes, []);
  assert.throws(() => completeObservation(denied), /visibility incomplete/);
});

test("descendant observer: safety self-exit never turns survivor failure into pass", async () => {
  const observed = empty();
  observed.processes.push({ hostPid: 999999, startTicks: "123" });
  let waited;
  await assert.rejects(requirePromptTeardown(observed, 200, async (ms) => {
    waited = ms;
    observed.processes.length = 0; // Simulate voluntary exit during cleanup.
  }), /survived native terminal completion/);
  assert.equal(waited, 45_800);
  await assert.rejects(requirePromptTeardown(empty(), 40_000, async () => {}), /must precede/);
});
