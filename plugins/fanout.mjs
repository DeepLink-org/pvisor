import { failed, taskInput } from "./backend.mjs";

/** Bounded independent experiments, collected once with no retry, apply or drop. */
export async function fanout(backend, input, signal) {
  if (!input || typeof input !== "object" || Array.isArray(input) ||
      Object.keys(input).some((key) => !["tasks", "concurrency"].includes(key))) {
    throw new Error("Fanout expects only tasks and concurrency");
  }
  const { tasks, concurrency = 2 } = input;
  if (!Array.isArray(tasks) || tasks.length < 1 || tasks.length > 8 ||
      !Number.isInteger(concurrency) || concurrency < 1 || concurrency > 4) {
    throw new Error("Fanout requires 1–8 tasks and concurrency from 1 to 4");
  }
  const names = new Set();
  const prepared = tasks.map((task) => {
    const { name, ...command } = task || {};
    if (typeof name !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(name) || names.has(name)) {
      throw new Error("Fanout task names must be unique 1–64 character identifiers");
    }
    names.add(name);
    return { name, command: taskInput(command) };
  });
  signal?.throwIfAborted();
  // Reject missing runtime/profile before partially starting the experiment set.
  const doctor = await backend.doctor({}, signal);
  if (failed(doctor)) throw new Error(`Fanout runtime preflight failed: ${doctor.execution?.reason || "inspect pvisor_doctor"}; no tasks started`);
  const results = new Array(prepared.length);
  let next = 0;
  const worker = async () => {
    while (next < prepared.length) {
      const index = next++;
      const { name, command } = prepared[index];
      if (signal?.aborted) {
        results[index] = { name, outcome: "not_started" };
        continue;
      }
      const start = performance.now();
      try {
        const result = await backend.run(command, signal, { name });
        results[index] = { name, outcome: result.interrupted || result.output_status === "unavailable" ? "unknown" : failed(result) ? "needs_review" : "completed",
          elapsed_ms: Math.round(performance.now() - start), result };
      } catch (error) {
        results[index] = { name, outcome: "unknown", selector: error.selector ?? null,
          elapsed_ms: Math.round(performance.now() - start), error: error.message };
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, prepared.length) }, worker));
  return {
    exit_code: results.every((item) => item.outcome === "completed") ? 0 : 1,
    interrupted: !!signal?.aborted,
    results,
    elapsed_order: results.filter((item) => item.outcome === "completed")
      .sort((a, b) => a.elapsed_ms - b.elapsed_ms).map((item) => item.name),
    scope: "Elapsed order is informational, not correctness or benchmark evidence. Review outputs and exact selectors; no automatic acceptance or cleanup.",
  };
}
