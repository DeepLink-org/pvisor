import { spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

/** Real stdio transport; no fake tool implementation or host configuration. */
export async function mcpClient(project, env, entry = fileURLToPath(new URL("../mcp.mjs", import.meta.url))) {
  const child = spawn(process.execPath, [
    entry, "--project", project,
  ], { env, stdio: ["pipe", "pipe", "pipe"] });
  const pending = new Map();
  let sequence = 0;
  let stderr = "";
  let stopped;
  child.stderr.on("data", (bytes) => { stderr = (stderr + bytes).slice(-8192); });
  const closed = once(child, "close");
  const lines = createInterface({ input: child.stdout });
  lines.on("line", (line) => {
    const response = JSON.parse(line);
    const waiter = pending.get(response.id);
    if (waiter) {
      pending.delete(response.id);
      clearTimeout(waiter.timeout);
      if (response.error) waiter.reject(new Error(JSON.stringify(response.error)));
      else waiter.resolve(response.result);
    }
  });
  child.on("error", rejectPending);
  child.on("close", () => rejectPending(new Error(`MCP exited: ${stderr}`)));
  function rejectPending(error) {
    for (const waiter of pending.values()) {
      clearTimeout(waiter.timeout);
      waiter.reject(error);
    }
    pending.clear();
  }
  function request(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++sequence;
      const timeout = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`MCP request timed out: ${method}; ${stderr}`));
        child.kill("SIGTERM");
      }, 30_000);
      pending.set(id, { resolve, reject, timeout });
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }
  await request("initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "pvisor-documentation-tests", version: "1" } });
  child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
  return {
    request,
    async call(name, args = {}) {
      const result = await request("tools/call", { name, arguments: args });
      if (result.isError) throw new Error(result.content[0].text);
      return JSON.parse(result.content[0].text);
    },
    close() {
      stopped ??= (async () => {
        child.stdin.end();
        const deadline = setTimeout(() => child.kill("SIGKILL"), 5000);
        await closed;
        clearTimeout(deadline);
        lines.close();
      })();
      return stopped;
    },
  };
}
