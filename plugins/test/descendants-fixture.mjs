import assert from "node:assert/strict";
import { readFile, readdir, stat } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";

function identity(text) {
  const fields = text.slice(text.lastIndexOf(")") + 2).split(" ");
  return { state: fields[0], startTicks: fields[19] };
}

/** Read-only host observer. Bracket metadata with birth/UID checks; never signal. */
export async function ownedProcesses(root, previous = [], io = { readFile, readdir, stat }) {
  const observation = { processes: [], inaccessible: [], unstable: [], reused: [] };
  for (const pid of (await io.readdir("/proc")).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid)) {
    try {
      const before = identity(await io.readFile(`/proc/${pid}/stat`, "utf8"));
      const uid = (await io.stat(`/proc/${pid}`)).uid;
      if (uid !== process.getuid()) continue;
      const command = await io.readFile(`/proc/${pid}/cmdline`, "utf8");
      const waitChannel = (await io.readFile(`/proc/${pid}/wchan`, "utf8")).trim();
      const uidAfter = (await io.stat(`/proc/${pid}`)).uid;
      const after = identity(await io.readFile(`/proc/${pid}/stat`, "utf8"));
      if (before.startTicks !== after.startTicks || uid !== uidAfter) {
        observation.unstable.push({ hostPid: Number(pid), before, after, uid, uidAfter });
        continue;
      }
      const prior = previous.find((entry) => entry.hostPid === Number(pid));
      const trackedBirth = prior?.startTicks === after.startTicks;
      if (prior && !trackedBirth) observation.reused.push({ hostPid: Number(pid), prior: prior.startTicks, current: after.startTicks });
      if (command.split("\0").includes(root) || trackedBirth) {
        observation.processes.push({ hostPid: Number(pid), ...after, waitChannel, command, trackedBirth });
      }
    } catch (error) {
      if (["EACCES", "EPERM"].includes(error.code)) {
        observation.inaccessible.push({ hostPid: Number(pid), code: error.code });
      } else if (!["ENOENT", "ESRCH"].includes(error.code)) throw error;
    }
  }
  return observation;
}

/** Fail closed on incomplete visibility; an empty visible set is not proof. */
export function completeObservation(observation) {
  assert.deepEqual(observation.inaccessible, [], "host /proc visibility incomplete");
  assert.deepEqual(observation.unstable, [], "host /proc identity changed during observation");
}

/**
 * The fixture requires prompt native teardown, not eventual self-exit.
 * Safety waiting is cleanup only: always rethrow the original failed assertion.
 */
export async function requirePromptTeardown(observation, elapsedMs, safetyWait = delay) {
  try {
    completeObservation(observation);
    assert.ok(elapsedMs < 15_000, "terminal observation must precede the fixture's 40s voluntary exit");
    assert.deepEqual(observation.processes, [], "owned process survived native terminal completion");
  } catch (failure) {
    try { await safetyWait(Math.max(0, 46_000 - elapsedMs)); }
    catch (cleanupError) { failure.cleanupError = cleanupError; }
    throw failure;
  }
}
