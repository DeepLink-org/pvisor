import { createBackend, callTool, failed, textContent, tools } from "./tools.mjs";
import { applyApprovalText, applyRequest } from "./presentation.mjs";

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
        if (failed(value) && tool.name !== "pvisor_fanout") throw new Error(value.summary || JSON.stringify(value));
        return { content: value.summary ? [{ type: "text", text: value.summary }] : textContent(value), details: value };
      },
    });
  }
  pi.registerCommand("pvisor", {
    description: 'tasks, status|review|output|drop STAGE, apply {"selector":"/absolute/stage","paths":["src"]}, drop-batch {"selectors":["/absolute/stage"]}',
    async handler(command, ctx) {
      try {
        const client = backend(ctx.cwd);
        if (command.trim() === "tasks") {
          const result = await client.tasks();
          ctx.ui.notify(JSON.stringify(result, null, 2), "info");
          return;
        }
        const match = /^(status|review|output|apply|drop|drop-batch)\s+(.+)$/s.exec(command.trim());
        if (!match) throw new Error('Use /pvisor tasks, status|review|output|drop STAGE, apply {"selector":"/absolute/stage","paths":["src"]}, or drop-batch {"selectors":["/absolute/stage"]}');
        const [, action, raw] = match;
        const { request: input, explanation } = action === "apply" ? applyRequest(JSON.parse(raw)) : { request: { selector: raw.trim() } };
        if (action === "drop-batch") {
          const result = await client.dropBatch(JSON.parse(raw));
          ctx.ui.notify(JSON.stringify(result, null, 2), failed(result) ? "error" : "info");
          return;
        }
        if (action === "apply") {
          if (!ctx.hasUI) throw new Error("An interactive user confirmation is required");
          const preview = await client.operate("preview", input);
          const description = applyApprovalText(preview, explanation);
          if (preview.preview_state === "no_changes" && !failed(preview)) {
            ctx.ui.notify(description, "info");
            return;
          }
          if (failed(preview) || !preview.approval_ready) throw new Error(description);
          const accepted = await ctx.ui.confirm(
            "pVisor apply",
            `${description}\n\nPublish these selected results?`,
          );
          if (!accepted) return;
        }
        const result = action === "output" ? await client.output(input) : await client.operate(action, input);
        ctx.ui.notify(JSON.stringify(result, null, 2), failed(result) ? "error" : "info");
      } catch (error) {
        ctx.ui.notify(String(error), "error");
      }
    },
  });
  pi.on("before_agent_start", async (event) => ({
    systemPrompt: `${event.systemPrompt}\nThe pVisor extension supplies task tools only. Native Pi tools are not intercepted. Fresh spawns are independent; use one invocation or explicitly fork a stopped retained candidate for dependent follow-ups. Keep exact selectors. Export extracts artifacts to an operator root without acceptance. Ask for user confirmation before apply. Unwanted candidates may be explicitly dropped without another confirmation; inspect unknown outcomes first. Never auto-drop on idle or bypass apply confirmation through native shell.`,
  }));
}
