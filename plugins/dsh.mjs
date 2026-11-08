import { createBackend, callTool, failed, textContent, tools } from "./tools.mjs";

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
  return result;
}

/** Register against injected host contracts, also used by dependency-free fixtures. */
export function registerDsh(ctx, defineTool, makeBackend = createBackend) {
  const definitions = [...tools,
    {
      name: "pvisor_apply",
      description: "Preview and request user approval to publish selected task paths. Requires Allow once from the Harness approval service.",
      inputSchema: tools.find((tool) => tool.name === "pvisor_apply_preview").inputSchema,
    },
    {
      name: "pvisor_drop",
      description: "Request user approval to discard remaining task candidates. Does not revert already applied files.",
      inputSchema: tools.find((tool) => tool.name === "pvisor_status").inputSchema,
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
      output: { schema: { type: "object", additionalProperties: true }, render: (_args, value) => textContent(value) },
      async execute(args, exec) {
        const project = exec.agent?.session?.header?.cwd;
        if (!project) throw new Error("pVisor requires an initiating Agent with a project cwd");
        exec.signal.throwIfAborted();
        const client = makeBackend(project);
        if (tool.name === "pvisor_apply" || tool.name === "pvisor_drop") {
          const action = tool.name.slice("pvisor_".length);
          let preview = "";
          if (action === "apply") {
            const result = await client.operate("preview", args, exec.signal);
            if (failed(result)) throw new Error(JSON.stringify(result));
            preview = result.stdout;
          }
          const answer = await ctx.approval.request({
            agent: exec.agent,
            toolName: tool.name,
            callId: exec.callId,
            reason: `pVisor ${action}\nProject: ${await client.project()}\n${JSON.stringify(args)}\n${preview}\nDrop never reverts previously applied files.`,
            signal: exec.signal,
          });
          if (answer !== "allowed-once") throw new Error(`pVisor ${action} not approved: ${answer}`);
          exec.signal.throwIfAborted();
          const result = await client.operate(action, args, exec.signal);
          if (failed(result)) throw new Error(JSON.stringify(result));
          return result;
        }
        const value = await callTool(client, tool.name, args, exec.signal);
        if (failed(value)) throw new Error(JSON.stringify(value));
        return value;
      },
    }));
  }
}

/** DSH/Cordis functional plugin entrypoint; uses the host's dsh-tools peer. */
export async function apply(ctx) {
  const { defineTool } = await import("@deepseek-ai/dsh-tools");
  registerDsh(ctx, defineTool);
}

export default { name, inject, apply };
