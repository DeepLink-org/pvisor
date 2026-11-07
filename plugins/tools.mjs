import { Backend } from "./backend.mjs";

const selector = { type: "string", description: "Exact absolute stage selector returned by pvisor_spawn; also usable with native pvisor commands. Never 'last'." };
const paths = { type: "array", minItems: 1, maxItems: 200, items: { type: "string" }, description: "Explicit project-relative paths to preview." };
const object = (properties, required = []) => ({ type: "object", properties, required, additionalProperties: false });

/** Shared model-facing tools deliberately exclude publication and deletion. */
export const tools = [
  {
    name: "pvisor_doctor",
    description: "Check the installed pVisor CLI capabilities, binary path, version and bound project. Missing/incompatible runtime requires Homebrew or pip installation/upgrade. Reports task-only scope; does not certify mount availability or interception of native Agent tools.",
    inputSchema: object({}),
  },
  {
    name: "pvisor_spawn",
    description: "Run one NEW Host --safe staged task with no network. Use argv directly; shell syntax requires ['sh','-c','...']. Put dependent edits/tests in one invocation: tasks do not share a working view. Retains candidates even on failure. Never automatically retry uncertain side effects. Does not accept or discard results.",
    inputSchema: object({
      argv: { type: "array", minItems: 1, maxItems: 256, items: { type: "string" } },
      timeout_seconds: { type: "integer", minimum: 1, maximum: 600, default: 120, description: "Runtime deadline: 1–600 seconds; default 120." },
    }, ["argv"]),
  },
  {
    name: "pvisor_status",
    description: "Read exact task status, including unknown outcomes. Native completion evidence remains authoritative.",
    inputSchema: object({ selector }, ["selector"]),
  },
  {
    name: "pvisor_review",
    description: "Read a bounded diff of a stopped task. Review is not a historical snapshot or a successful apply conflict check.",
    inputSchema: object({ selector }, ["selector"]),
  },
  {
    name: "pvisor_apply_preview",
    description: "Preview selective apply with native conflict checks. Does not modify the project or accept candidates.",
    inputSchema: object({ selector, paths }, ["selector", "paths"]),
  },
];

/** Route only declared tools; tool arguments cannot choose binary or project. */
export async function callTool(backend, name, args, signal) {
  if (!tools.some((tool) => tool.name === name)) throw new Error(`Unknown tool: ${name}`);
  if (name === "pvisor_spawn") return backend.run(args, signal);
  if (name === "pvisor_doctor") return backend.doctor(args, signal);
  if (name === "pvisor_apply_preview") return backend.operate("preview", args, signal);
  return backend.operate(name.slice("pvisor_".length), args, signal);
}

export const createBackend = (project) => new Backend(project);
export const textContent = (value) => [{ type: "text", text: JSON.stringify(value, null, 2) }];
export const failed = (value) => value.exit_code !== 0 || value.interrupted;
