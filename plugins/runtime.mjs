import { access, stat } from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { execute } from "./process.mjs";

const installation = "Install or upgrade pVisor through Homebrew or pip (pvisor-sandbox): https://deeplink-org.github.io/pvisor/start/installation/. Restart the Agent host after changing PATH, or set PVISOR_PLUGIN_BINARY to an absolute executable path.";

async function executable(binary) {
  if (!(await stat(binary)).isFile()) {
    throw Object.assign(new Error("Not a regular executable"), { code: "EACCES" });
  }
  await access(binary, constants.X_OK);
  return binary;
}

/** Probe required CLI capabilities; package and runtime versions are independent. */
export async function checkRuntimeContract(binary, { signal } = {}) {
  signal?.throwIfAborted();
  try {
    for (const [command, flags] of [
      ["spawn", ["--safe", "--no-agent-defaults", "--stage", "--timeout", "--overlaynet-deny-all"]],
      ["apply", ["--path", "--dry-run", "--json"]],
    ]) {
      const result = await execute(binary, [command, "--help"], { signal, deadlineMs: 10_000 });
      signal?.throwIfAborted();
      if (result.exit_code !== 0 || result.interrupted || result.truncated) {
        throw new Error(`${command} probe failed (exit=${result.exit_code}, signal=${result.signal}, interrupted=${result.interrupted}, truncated=${result.truncated})`);
      }
      const { stdout } = result;
      for (const flag of flags) {
        if (!stdout.includes(`${flag} `) && !stdout.includes(`${flag}\n`)) {
          throw new Error(`${command} lacks ${flag}`);
        }
      }
    }
  } catch (error) {
    signal?.throwIfAborted();
    throw new Error(`Incompatible pVisor at ${binary}: ${error.message}. ${installation} No downgraded execution is allowed.`);
  }
  return binary;
}

/** Use an operator override or the host PATH; never install or download a runtime. */
export async function resolveRuntime(override, { searchPath = process.env.PATH || "", signal } = {}) {
  signal?.throwIfAborted();
  let binary;
  if (override !== undefined) {
    if (typeof override !== "string" || !path.isAbsolute(override) || override.includes("\0")) {
      throw new Error("PVISOR_PLUGIN_BINARY must be an absolute executable path");
    }
    try {
      binary = await executable(override);
    } catch (error) {
      throw new Error(`Cannot execute PVISOR_PLUGIN_BINARY ${override}: ${error.message}. ${installation}`);
    }
  } else {
    // Ignore cwd-dependent PATH entries. Allow Homebrew and venv symlinks.
    for (const directory of searchPath.split(path.delimiter)) {
      if (!path.isAbsolute(directory)) continue;
      try {
        binary = await executable(path.join(directory, "pvisor"));
        break;
      } catch (error) {
        if (!["ENOENT", "ENOTDIR", "EACCES"].includes(error.code)) throw error;
      }
    }
    if (!binary) throw new Error(`pVisor was not found on the Agent host PATH. ${installation}`);
  }
  // Revalidate separately upgraded runtimes; never fall back from incompatibility.
  return checkRuntimeContract(binary, { signal });
}
