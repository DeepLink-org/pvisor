import { createBackend } from "./tools.mjs";
import { fileReview } from "./file-review.mjs";

/** Owner-only Fetch route below DSH's authenticated shared API channel. */
export const MONITOR_ROUTE = "/api/pvisor/monitor";

const response = (status, value) => Response.json(value, {
  status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
});

/**
 * Read-only monitor transport. The Session owns the project; the browser cannot
 * choose cwd, binary, state root, native flags or a mutating operation.
 * Connection's exact-route admission is responsible for owner authentication.
 */
export function monitorHandler(ctx, makeBackend = createBackend) {
  let active = 0;
  return async (request) => {
    if (request.method !== "GET") return response(405, { error: "Read-only monitor: GET required." });
    const params = new URL(request.url).searchParams;
    if ([...params.keys()].some((key) => !["session", "selector", "cursor", "view"].includes(key)) ||
        [...params.keys()].some((key) => params.getAll(key).length !== 1)) {
      return response(400, { error: "Invalid monitor parameters." });
    }
    const sessionId = params.get("session");
    const selector = params.get("selector");
    const cursor = params.get("cursor");
    const view = params.get("view") ?? "changes";
    if (!sessionId || sessionId.length > 256 || (selector !== null && (!selector || selector.length > 4096)) ||
        (cursor !== null && (!cursor || cursor.length > 64)) ||
        !["changes", "diff", "output", "terminal"].includes(view) || (selector !== null && cursor !== null)) {
      return response(400, { error: "A Session and valid monitor selection are required." });
    }
    if (active >= 4) return response(429, { error: "Monitor is busy; refresh later." });
    active++;
    const signal = AbortSignal.any([request.signal, AbortSignal.timeout(25_000)]);
    try {
      // Use only the Session header, never a request-supplied filesystem root.
      const header = ctx.sessions.get(sessionId)?.header ??
        (await ctx.get("sessionPersistence")?.stat(sessionId))?.header;
      if (!header?.cwd) return response(404, { error: "Session has no project working directory." });
      signal.throwIfAborted();
      const backend = makeBackend(header.cwd);
      const project = await backend.project();
      let result;
      if (selector === null) {
        result = await backend.tasks({ limit: 10, order: "newest", ...(cursor === null ? {} : { cursor }) }, signal);
        if (!Array.isArray(result.tasks) || result.tasks.some((task) =>
          typeof task.task !== "string" || typeof task.selector !== "string" ||
          typeof task.state !== "string" || (task.filesystem != null && typeof task.filesystem !== "string") ||
          (task.error != null && typeof task.error !== "string"))) {
          throw new Error("Native task inventory is invalid.");
        }
      } else if (view === "terminal") {
        result = await backend.terminal({ selector }, signal);
      } else {
        // Native status is an observation, not proof of reviewable completion.
        // Backend validates selector ownership before every operation.
        const observed = await backend.lifecycle({ selector }, signal);
        if (observed.exit_code !== 0 || observed.interrupted || observed.truncated) {
          throw new Error("Native task status is unavailable.");
        }
        const status = JSON.parse(observed.stdout);
        if (!((status.schema_version === 3) || (status.schema_version === 1 && status.detail === "lifecycle")) ||
            status.run?.stage !== selector ||
            status.run?.overlay?.target !== project || typeof status.live !== "boolean" ||
            typeof status.state !== "string" || typeof status.run.overlay.state !== "string") {
          throw new Error("Native task status identity or schema mismatch.");
        }
        result = { selector, name: observed.name, state: status.state, filesystem: status.run.overlay.state,
          execution: observed.execution ?? null,
          generation: status.run.overlay.generation ?? null, live: status.live,
          exec_supported: status.exec_supported === true,
          command: status.run.command ?? null, command_truncated: status.run.command_truncated ?? false,
          started_at_unix_ms: status.run.started_at_unix_ms ?? null,
          finished_at_unix_ms: status.run.finished_at_unix_ms ?? null,
          network: status.run.network ?? null, run_id: status.run.run_id,
          detail_status: "not_available", view };
        if (!status.live && ["completed", "failed", "cancelled"].includes(status.state)) {
          if (view === "changes" || view === "diff") {
            const review = await backend.operate("review", { selector, include_diff: true }, signal);
            result = { ...result, detail_status: review.review_status,
              ...(view === "diff" ? { diff: review.stdout, diff_status: review.diff_status } : {}),
              files: fileReview(review),
              changes: review.changes, counts: review.counts,
              in_scope: review.in_scope, outside_scope: review.outside_scope,
              unclassified: review.unclassified, handoff: review.handoff,
              review_context: review.review_context, error: review.review_error };
          } else {
            const output = await backend.output({ selector }, signal);
            result = { ...result, detail_status: output.output_status,
              stdout: output.stdout, stderr: output.stderr,
              output_truncated: output.truncated,
              task_state: output.task_state, task_exit_code: output.task_exit_code,
              lineage: output.lineage, error: output.output_error };
          }
        }
      }
      signal.throwIfAborted();
      return response(200, { schema_version: 1, project, observed_at: new Date().toISOString(), ...result });
    } catch (error) {
      return response(signal.aborted ? 408 : 422, {
        error: signal.aborted ? "Monitor read interrupted or timed out; task state is not established." : error.message,
      });
    } finally {
      active--;
    }
  };
}

/** Optional web capability: CLI-only DSH compositions keep their existing tools. */
export function registerMonitor(ctx, makeBackend = createBackend) {
  return ctx.inject(["connection", "sessions"], (web) => {
    if (typeof web.connection?.fetch?.register !== "function") {
      web.logger.warn("pVisor monitor requires DSH's owner-authenticated Connection Fetch routes (tested with 0.2.2-rc.1).");
      return;
    }
    web.effect(() => web.connection.fetch.register({
      path: MONITOR_ROUTE, methods: ["GET"], requestBody: "buffered",
      fetch: monitorHandler(web, makeBackend),
    }), "pvisor: read-only monitor route");
  });
}
