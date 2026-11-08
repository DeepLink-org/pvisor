#!/usr/bin/env node
import { Backend } from "./backend.mjs";
import { callTool, failed, textContent, tools } from "./tools.mjs";

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
let disconnected = false;
let flushExpired = false;
function finishOverload() {
  // Node's process.stdout can retain pending native writes after destroy().
  // Exit explicitly only after all supervised operations have joined.
  if (flushExpired && active.size === 0) process.exit(1);
}
// Internal transport budget, measured after JSON escaping in UTF-8 bytes.
// Keep reading input while output is blocked so cancellation stays responsive.
// If completed replies accumulate, close rather than queue without a bound.
const OUTPUT_BACKLOG_LIMIT = 8 * 1024 * 1024;
function disconnect() {
  if (disconnected) return;
  disconnected = true;
  process.stdin.destroy();
  for (const controller of active.values()) controller.abort();
}
const send = (id, value, error = false) => {
  if (!disconnected) {
    const frame = Buffer.from(`${JSON.stringify({ jsonrpc: "2.0", id, [error ? "error" : "result"]: value })}\n`);
    if (process.stdout.writableLength + frame.length > OUTPUT_BACKLOG_LIMIT) {
      process.exitCode = 1;
      disconnect();
      console.error("MCP output backlog exceeds 8 MiB; closing transport. Undelivered results are uncertain; do not retry automatically.");
      // Flush already queued complete frames if the reader resumes promptly.
      // A stopped reader must not keep the server alive indefinitely. Forced
      // closure may truncate the last frame; it is not completion evidence.
      const deadline = setTimeout(() => {
        flushExpired = true;
        process.stdout.destroy();
        finishOverload();
      }, 1000);
      process.stdout.once("close", () => clearTimeout(deadline));
      process.stdout.end();
      return;
    }
    process.stdout.write(frame);
  }
};
// A client can lose its response pipe before closing its request pipe.
// Cancel supervised work instead of crashing on an unhandled EPIPE, and do
// not accept further requests or try to deliver results to the lost peer.
for (const [stream, name] of [[process.stdin, "input"], [process.stdout, "output"]]) {
  stream.on("error", (error) => {
    disconnect();
    if (error.code !== "EPIPE" && error.code !== "ERR_STREAM_DESTROYED") {
      process.exitCode = 1;
      console.error(`MCP ${name} failed: ${error.message}`);
    }
  });
}

async function dispatch(message) {
  if (disconnected) return;
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
        serverInfo: { name: "pvisor-agent-plugins", version: "0.1.0" },
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
    finishOverload();
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
    process.exitCode = 1;
    disconnect();
    console.error("Unterminated MCP frame exceeds 1 MiB");
  }
});
process.stdin.on("end", () => {
  if (pending.length) send(null, { code: -32700, message: "Incomplete JSON request" }, true);
  for (const controller of active.values()) controller.abort();
});
