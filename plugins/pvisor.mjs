#!/usr/bin/env node
import path from "node:path";
import { resolveRuntime } from "./runtime.mjs";

try {
  const binary = await resolveRuntime(process.env.PVISOR_PLUGIN_BINARY);
  if (!path.isAbsolute(binary)) throw new Error("PVISOR_PLUGIN_BINARY must be an absolute executable path");
  if (typeof process.execve !== "function") {
    throw new Error("The pVisor npm launcher requires Node.js 22.19+ on Linux or macOS");
  }
  // Replace this launcher rather than inserting a parent into native terminal,
  // foreground-group, signal and exit-status ownership.
  process.execve(binary, [binary, ...process.argv.slice(2)], process.env);
} catch (error) {
  console.error(String(error));
  process.exitCode = 1;
}
