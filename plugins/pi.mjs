import { createBackend, callTool, failed, textContent, tools } from "./tools.mjs";
import { formatPreview, summarizePreview } from "./summary.mjs";

/** Pi official extension entrypoint; native Pi tools remain unchanged. */
export default function registerPi(pi, dependencies = {}) {
  const backend = dependencies.createBackend || createBackend;
  for (const tool of tools) {
    pi.registerTool({
      name: tool.name,
      label: tool.name,
      description: tool.description,
      parameters: tool.inputSchema,
      async execute(_id, args, signal, _update, ctx) {
        const value = await callTool(backend(ctx.cwd), tool.name, args, signal);
        if (failed(value)) throw new Error(JSON.stringify(value));
        return { content: textContent(value), details: value };
      },
    });
  }
  pi.registerCommand("pvisor", {
    description: 'status|review|drop STAGE, or apply {"selector":"/absolute/stage","paths":["src"]}; native pVisor workflow',
    async handler(command, ctx) {
      try {
        const match = /^(status|review|apply|drop)\s+(.+)$/s.exec(command.trim());
        if (!match) throw new Error('Use /pvisor status|review|drop STAGE, or /pvisor apply {"selector":"/absolute/stage","paths":["src"]}');
        const [, action, raw] = match;
        const input = action === "apply" ? JSON.parse(raw) : { selector: raw.trim() };
        const client = backend(ctx.cwd);
        if (action === "apply" || action === "drop") {
          if (!ctx.hasUI) throw new Error("An interactive user confirmation is required");
          let preview = "";
          if (action === "apply") {
            const result = await client.operate("preview", input);
            const summary = summarizePreview(result, input, await client.project());
            preview = formatPreview(summary);
            if (!summary.can_confirm) throw new Error(preview);
          }
          const project = await client.project();
          const accepted = await ctx.ui.confirm(
            `pVisor ${action}`,
            action === "apply" ? preview : `Project: ${JSON.stringify(project)}\nStage: ${JSON.stringify(input.selector)}\nDiscard remaining candidates? Previously applied files are not reverted.`,
          );
          if (!accepted) return;
        }
        const result = await client.operate(action, input);
        ctx.ui.notify(JSON.stringify(result, null, 2), failed(result) ? "error" : "info");
      } catch (error) {
        ctx.ui.notify(String(error), "error");
      }
    },
  });
  pi.on("before_agent_start", async (event) => ({
    systemPrompt: `${event.systemPrompt}\nThe pVisor extension supplies task tools only. Native Pi tools are not intercepted. Put dependent changes and tests in one pvisor_spawn, retain its exact stage selector, and ask the user to review and decide through pvisor review/apply/drop. Never use native shell tools to bypass confirmation.`,
  }));
}
