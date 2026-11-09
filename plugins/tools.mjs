import { Backend } from "./backend.mjs";
import { fanout } from "./fanout.mjs";
export { failed } from "./backend.mjs";

const selector = { type: "string", description: "Exact absolute stage selector returned by pvisor_spawn; also usable with native pvisor commands. Never 'last'." };
const paths = { type: "array", minItems: 1, maxItems: 200, items: { type: "string" }, description: "Explicit project-relative paths to preview." };
const object = (properties, required = []) => ({ type: "object", properties, required, additionalProperties: false });

/** Shared model-facing tools deliberately exclude publication and deletion. */
export const tools = [
  {
    name: "pvisor_doctor",
    description: "Check installed pVisor CLI capabilities, environment, binary path, version and project. Optional probe starts a real safe task and retains its exact selector; it does not certify isolation or intercept native Agent tools. Missing/incompatible runtime requires operator installation/upgrade.",
    inputSchema: object({ probe: { type: "boolean", description: "Run a real safe startup probe, retaining an inspectable task. Default false; no automatic cleanup." } }),
  },
  {
    name: "pvisor_spawn",
    description: "Run one NEW --safe staged task with no network under operator VM-first policy. Host is used only when explicitly selected or authorized as preflight fallback; tools cannot change this policy. Use argv directly; shell syntax requires ['sh','-c','...']. Fresh spawns do not see unaccepted sibling edits. Retains candidates on failure; no automatic retry, acceptance or discard. Host-only fork requires Host authorization.",
    inputSchema: object({
      argv: { type: "array", minItems: 1, maxItems: 256, items: { type: "string" } },
      timeout_seconds: { type: "integer", minimum: 1, maximum: 600, default: 120, description: "Runtime deadline: 1–600 seconds; default 120." },
    }, ["argv"]),
  },
  {
    name: "pvisor_fork",
    description: "Start a NEW Host task from a stopped retained task's file candidate, with an explicit command. Inherits native safety and timeout; not memory/conversation continuation or a complete frozen host snapshot. Parent remains independent. No automatic retries.",
    inputSchema: object({
      selector,
      argv: { type: "array", minItems: 1, maxItems: 256, items: { type: "string" } },
    }, ["selector", "argv"]),
  },
  {
    name: "pvisor_export",
    description: "Export explicit retained task paths without acceptance or stage consumption. Linux only; operator must configure PVISOR_PLUGIN_EXPORT_DIR. output names one NEW child directory under that root. No overwrite, arbitrary destination, automatic drop or retry. A lost reply may leave artifacts: inspect the returned destination.",
    inputSchema: object({
      selector, paths,
      output: { type: "string", description: "New directory name relative to operator export root, 1–128 letters/digits/._-, starting with a letter or digit. No slashes." },
    }, ["selector", "paths", "output"]),
  },
  {
    name: "pvisor_fanout",
    description: "Run 1–8 independent staged experiments with bounded concurrency. Returns named results, exact selectors, captured output and informational elapsed ordering. No retries, acceptance, cleanup or correctness ranking.",
    inputSchema: object({
      tasks: { type: "array", minItems: 1, maxItems: 8, items: object({
        name: { type: "string", description: "Unique 1–64 character identifier using letters, digits, '_' or '-'." },
        argv: { type: "array", minItems: 1, maxItems: 256, items: { type: "string" } },
        timeout_seconds: { type: "integer", minimum: 1, maximum: 600, default: 120 },
      }, ["name", "argv"]) },
      concurrency: { type: "integer", minimum: 1, maximum: 4, default: 2 },
    }, ["tasks"]),
  },
  {
    name: "pvisor_tasks",
    description: "List this project's task selectors and native lifecycle status. Paginated; no deletion, retry, acceptance or automatic cleanup.",
    inputSchema: object({
      cursor: { type: "string", description: "next_cursor returned by the previous page; omit for the first page." },
      limit: { type: "integer", minimum: 1, maximum: 50, default: 20 },
      order: { type: "string", enum: ["id", "newest"], default: "id",
        description: "id preserves legacy UUID ordering; newest sorts creation time before pagination. Older records use labeled filesystem timestamp estimates." },
    }),
  },
  {
    name: "pvisor_output",
    description: "Read bounded captured stdout/stderr of the exact completed task through native validated review. Useful after a lost response; never reruns the task. Unfinished or invalid completion is refused.",
    inputSchema: object({ selector }, ["selector"]),
  },
  {
    name: "pvisor_status",
    description: "Read exact task status, including unknown outcomes. Native completion evidence remains authoritative.",
    inputSchema: object({ selector }, ["selector"]),
  },
  {
    name: "pvisor_review",
    description: "Read current structured changes, native project/environment scope counts and optional bounded text diff of a stopped task. Counts distinguish files from directories. Unknown review is unavailable, not empty. Diff is a separate observation, not a frozen snapshot or apply conflict check.",
    inputSchema: object({ selector, include_diff: { type: "boolean", default: true,
      description: "Include separately observed bounded text diff in stdout; false returns structured inventory only." } }, ["selector"]),
  },
  {
    name: "pvisor_apply_preview",
    description: "Preview selective apply with native conflict checks. Returns a readable summary, scoped text preview and blockers, with native JSON retained in stdout. Does not write project files; approval_ready=false cannot authorize apply.",
    inputSchema: object({ selector, paths }, ["selector", "paths"]),
  },
];

/** Route only declared tools; tool arguments cannot choose binary or project. */
export async function callTool(backend, name, args, signal) {
  if (!tools.some((tool) => tool.name === name)) throw new Error(`Unknown tool: ${name}`);
  if (name === "pvisor_spawn") return backend.run(args, signal);
  if (name === "pvisor_fork") return backend.fork(args, signal);
  if (name === "pvisor_export") return backend.export(args, signal);
  if (name === "pvisor_fanout") return fanout(backend, args, signal);
  if (name === "pvisor_doctor") return backend.doctor(args, signal);
  if (name === "pvisor_tasks") return backend.tasks(args, signal);
  if (name === "pvisor_output") return backend.output(args, signal);
  if (name === "pvisor_apply_preview") return backend.operate("preview", args, signal);
  return backend.operate(name.slice("pvisor_".length), args, signal);
}

export const createBackend = (project) => new Backend(project);
export const textContent = (value) => [{ type: "text", text: JSON.stringify(value, null, 2) }];
