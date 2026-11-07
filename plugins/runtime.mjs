import { access, lstat, readFile } from "node:fs/promises";
import { constants, readFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { promisify } from "node:util";

const require = createRequire(import.meta.url);
export const runtime = JSON.parse(readFileSync(new URL("./runtime.json", import.meta.url), "utf8"));
const plugin = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const components = ["pvisor", "pvisor-replay", "pvisor-daemon", "libkrunfw.SOURCE"];
let installed;

/** Select npm's matching, version-pinned native payload; never fetch at tool time. */
export async function bundledRuntime({
  platform = process.platform,
  arch = process.arch,
  resolve = (name) => require.resolve(`${name}/package.json`),
} = {}) {
  const target = runtime.platforms[`${platform}-${arch}`];
  if (!target) {
    throw new Error(`pVisor task plugins do not support ${platform}/${arch}; supported: Linux x64 and macOS arm64`);
  }
  let metadata;
  try {
    metadata = resolve(target.package);
  } catch {
    throw new Error(`Missing ${target.package}@${plugin.version}. Reinstall pvisor-agent-plugins with optional dependencies enabled; no host-execution fallback is allowed.`);
  }
  const manifest = JSON.parse(await readFile(metadata, "utf8"));
  if (manifest.name !== target.package || manifest.version !== plugin.version ||
      manifest.pvisorRuntimeVersion !== runtime.version ||
      !/^[0-9a-f]{40}$/.test(manifest.pvisorRuntimeCommit || "") ||
      !manifest.pvisorRuntimeDistribution?.startsWith(`${runtime.version}+g`)) {
    throw new Error("pVisor plugin/native package version mismatch; reinstall the matching plugin release");
  }
  const bin = path.join(path.dirname(metadata), "bin");
  for (const component of components) {
    const file = path.join(bin, component);
    const info = await lstat(file);
    if (!info.isFile() || info.isSymbolicLink() || (info.mode & 0o022) !== 0) {
      throw new Error(`Unsafe or incomplete native runtime component: ${component}`);
    }
    if (component !== "libkrunfw.SOURCE") await access(file, constants.X_OK);
  }
  return path.join(bin, "pvisor");
}

/** Probe the required inert CLI surface before a bundled runtime can start work. */
export async function checkRuntimeContract(binary) {
  const run = promisify(execFile);
  for (const [command, flags] of [
    ["spawn", ["--safe", "--no-agent-defaults", "--stage", "--timeout", "--overlaynet-deny-all"]],
    ["apply", ["--path", "--dry-run", "--json"]],
  ]) {
    const { stdout } = await run(binary, [command, "--help"], { timeout: 10_000, maxBuffer: 256 * 1024 });
    for (const flag of flags) {
      if (!stdout.includes(`${flag} `) && !stdout.includes(`${flag}\n`)) {
        throw new Error(`Incompatible bundled pVisor: ${command} lacks ${flag}; no downgraded execution is allowed`);
      }
    }
  }
  return binary;
}

/** An explicit operator override is never replaced silently by another runtime. */
export async function resolveRuntime(override) {
  if (override) return override;
  installed ??= bundledRuntime().then(checkRuntimeContract).catch((error) => { installed = undefined; throw error; });
  return installed;
}
