#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { Backend } from "./backend.mjs";
import { callTool, failed, textContent, tools } from "./tools.mjs";

const { name, version } = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const args = process.argv.slice(2);
if (args.length && (args.length !== 2 || args[0] !== "--project")) {
  console.error("Usage: pvisor-agent-mcp [--project DIR]");
  process.exit(2);
}
const project = args[1] || process.env.PVISOR_PLUGIN_PROJECT;
if (!project) {
  console.error("Set PVISOR_PLUGIN_PROJECT or pass --project DIR. Plugin hosts may start MCP in the installed plugin directory, not your project.");
  process.exit(2);
}
const backend = new Backend(project);
const active = new Map();
let initialized = false;
const send = (id, value, error = false) =>
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, [error ? "error" : "result"]: value })}\n`);

async function dispatch(message) {
  const { id, method, params = {} } = message;
  if (message.jsonrpc !== "2.0" || typeof method !== "string" ||
      (id !== undefined && typeof id !== "string" && typeof id !== "number") ||
      !params || typeof params !== "object" || Array.isArray(params)) {
    send(id ?? null, { code: -32600, message: "Invalid request" }, true);
    return;
  }
  if (id === undefined) {
    if (method === "notifications/cancelled") active.get(params.requestId)?.abort();
    return;
  }
  if (active.has(id)) {
    send(id, { code: -32600, message: "Duplicate active request ID" }, true);
    return;
  }
  if (active.size >= 32) {
    send(id, { code: -32600, message: "Too many active requests" }, true);
    return;
  }
  const controller = new AbortController();
  active.set(id, controller);
  try {
    if (method === "initialize") {
      const versions = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
      initialized = true;
      send(id, {
        protocolVersion: versions.includes(params.protocolVersion) ? params.protocolVersion : "2025-11-25",
        capabilities: { tools: {} },
        serverInfo: { name, version },
        instructions: "Task-only staging. Native Agent tools are not intercepted. No automatic retries or acceptance. Use the same exact stage selector as native pvisor status/review/apply/drop and let the user accept selected results.",
      });
    } else if (method === "ping") {
      send(id, {});
    } else if (!initialized) {
      send(id, { code: -32600, message: "Initialize the server first" }, true);
    } else if (method === "tools/list") {
      send(id, { tools: tools.map((tool) => ({
        ...tool,
        annotations: { readOnlyHint: tool.name !== "pvisor_spawn", destructiveHint: false, idempotentHint: tool.name !== "pvisor_spawn", openWorldHint: false },
      })) });
    } else if (method === "tools/call") {
      try {
        const value = await callTool(backend, params.name, params.arguments || {}, controller.signal);
        send(id, { content: textContent(value), isError: failed(value) });
      } catch (error) {
        send(id, { content: [{ type: "text", text: String(error) }], isError: true });
      }
    } else {
      send(id, { code: -32601, message: "Method not found" }, true);
    }
  } catch (error) {
    send(id, { code: -32602, message: String(error) }, true);
  } finally {
    active.delete(id);
  }
}

// JSON-RPC stdio is newline delimited. Bound each frame before parsing it.
let pending = Buffer.alloc(0);
process.stdin.on("data", (bytes) => {
  pending = Buffer.concat([pending, bytes]);
  let end;
  while ((end = pending.indexOf(10)) >= 0) {
    const line = pending.subarray(0, end);
    pending = pending.subarray(end + 1);
    if (line.length > 1024 * 1024) {
      send(null, { code: -32600, message: "Frame exceeds 1 MiB" }, true);
      continue;
    }
    try {
      const message = JSON.parse(line.toString("utf8"));
      if (!message || Array.isArray(message) || typeof message !== "object") throw new Error("Expected request object");
      void dispatch(message);
    } catch {
      send(null, { code: -32700, message: "Invalid JSON request" }, true);
    }
  }
  if (pending.length > 1024 * 1024) {
    console.error("Unterminated MCP frame exceeds 1 MiB");
    process.stdin.destroy();
    for (const controller of active.values()) controller.abort();
    process.exitCode = 1;
  }
});
process.stdin.on("end", () => {
  if (pending.length) send(null, { code: -32700, message: "Incomplete JSON request" }, true);
  for (const controller of active.values()) controller.abort();
});
