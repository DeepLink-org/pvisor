import { createBackend, callTool, failed, textContent, tools } from "./tools.mjs";
import { applyApprovalText, applyRequest } from "./presentation.mjs";
import { registerMonitor } from "./dsh-monitor.mjs";

export const name = "pvisor-agent-tools";
export const inject = ["tools", "approval"];

function valueSchema(schema) {
  // Harness's parameter DSL accepts fewer keywords than JSON Schema.
  // Bounds remain authoritative in Backend, shared by every transport.
  const result = { type: schema.type };
  for (const key of ["description", "enum", "const"]) {
    if (schema[key] !== undefined) result[key] = schema[key];
  }
  if (schema.items) result.items = valueSchema(schema.items);
  if (schema.properties) {
    result.properties = Object.fromEntries(Object.entries(schema.properties).map(([name, value]) =>
      [name, { ...valueSchema(value), ...(schema.required?.includes(name) ? { required: true } : {}) }]));
    result.additionalProperties = schema.additionalProperties;
  }
  return result;
}

/** Register against injected host contracts, also used by dependency-free fixtures. */
export function registerDsh(ctx, defineTool, makeBackend = createBackend) {
  const definitions = [...tools,
    {
      name: "pvisor_apply",
      description: "Review the selected diff, then request approval. A concise summary of what changes and why is REQUIRED, preferably in the user's language. Do not paste diff/logs or claim tests passed without evidence. This explanation is unverified. Requires Harness Allow once.",
      inputSchema: {
        ...tools.find((tool) => tool.name === "pvisor_apply_preview").inputSchema,
        required: ["selector", "paths", "summary"],
        properties: {
          ...tools.find((tool) => tool.name === "pvisor_apply_preview").inputSchema.properties,
          summary: { type: "string", description: "Required brief explanation of what the selected changes do and why (1–512 bytes), based on review. No raw diff or logs; not an authorization or test result." },
        },
      },
    },
    {
      name: "pvisor_drop",
      description: "Discard remaining candidates of an explicitly selected task without an additional approval prompt. Does not revert applied files. Inspect unknown outcomes before deciding to discard.",
      inputSchema: tools.find((tool) => tool.name === "pvisor_status").inputSchema,
    },
    {
      name: "pvisor_drop_batch",
      description: "Discard candidates from 1–50 explicitly selected tasks without an additional approval prompt. No wildcard or automatic cleanup. Reports each native outcome; partial failure is not rolled back. Applied files remain.",
      inputSchema: {
        type: "object", additionalProperties: false, required: ["selectors"],
        properties: { selectors: { type: "array", minItems: 1, maxItems: 50,
          items: { type: "string" }, description: "Exact stage selectors to discard, typically selected from pvisor_tasks." } },
      },
    },
  ];
  for (const tool of definitions) {
    const parameters = Object.fromEntries(Object.entries(tool.inputSchema.properties).map(
      ([key, value]) => [key, {
        ...valueSchema(value),
        ...(tool.inputSchema.required.includes(key) ? { required: true } : {}),
      }],
    ));
    ctx.tools.register(defineTool({
      name: tool.name,
      description: tool.description,
      parameters,
      output: { schema: { type: "object", additionalProperties: true },
        render: (_args, value) => value.summary ? [{ type: "text", text: value.summary }] : textContent(value) },
      async execute(args, exec) {
        const project = exec.agent?.session?.header?.cwd;
        if (!project) throw new Error("pVisor requires an initiating Agent with a project cwd");
        exec.signal.throwIfAborted();
        const client = makeBackend(project);
        if (tool.name === "pvisor_drop_batch") {
          // Do not throw away the per-task report when one native drop fails.
          return client.dropBatch(args, exec.signal);
        }
        if (tool.name === "pvisor_drop") {
          const result = await client.operate("drop", args, exec.signal);
          if (failed(result)) throw new Error(JSON.stringify(result));
          return result;
        }
        if (tool.name === "pvisor_apply") {
          const { request, explanation } = applyRequest(args, { requireSummary: true });
          const preview = await client.operate("preview", request, exec.signal);
          const description = applyApprovalText(preview, explanation);
          if (preview.preview_state === "no_changes" && !failed(preview)) return preview;
          if (failed(preview) || !preview.approval_ready) throw new Error(description);
          if (typeof ctx.approval?.request !== "function") throw new Error("pVisor apply requires host approval");
          const answer = await ctx.approval.request({
            agent: exec.agent,
            toolName: tool.name,
            callId: exec.callId,
            reason: description,
            signal: exec.signal,
          });
          if (answer !== "allowed-once") throw new Error(`pVisor apply not approved: ${answer}`);
          exec.signal.throwIfAborted();
          const result = await client.operate("apply", request, exec.signal);
          if (failed(result)) throw new Error(JSON.stringify(result));
          return result;
        }
        const value = await callTool(client, tool.name, args, exec.signal);
        if (failed(value) && tool.name !== "pvisor_fanout") throw new Error(value.summary || JSON.stringify(value));
        return value;
      },
    }));
  }
}

/** DSH/Cordis functional plugin entrypoint; uses the host's dsh-tools peer. */
export async function apply(ctx) {
  const { defineTool } = await import("@deepseek-ai/dsh-tools");
  registerDsh(ctx, defineTool);
  registerMonitor(ctx);
}

export default { name, inject, apply };
