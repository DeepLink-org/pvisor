import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { lstat, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { runtime } from "./runtime.mjs";

const root = path.dirname(fileURLToPath(import.meta.url));
const payload = ["LICENSE", "NOTICE", "bin/libkrunfw.SOURCE", "bin/pvisor", "bin/pvisor-daemon", "bin/pvisor-replay"];
const MAX_ARCHIVE_BYTES = 64 * 1024 * 1024;

/** Repackage the same-commit, accepted native distribution set, never 'latest'. */
export async function prepareRelease(destination, {
  artifactsDirectory,
  manifest = runtime,
  plugin,
  expectedCommit = process.env.GITHUB_SHA,
} = {}) {
  plugin ??= JSON.parse(await readFile(path.join(root, "package.json"), "utf8"));
  if (!artifactsDirectory) throw new Error("Provide the verified native artifact directory");
  const provenance = JSON.parse(await readFile(path.join(artifactsDirectory, "runtime-provenance.json"), "utf8"));
  if (provenance.repository !== manifest.repository || !/^[0-9a-f]{40}$/.test(provenance.commit) ||
      (expectedCommit && provenance.commit !== expectedCommit) ||
      typeof provenance.version !== "string" ||
      !provenance.version.startsWith(`${manifest.version}+g`) ||
      !/^[0-9]+\.[0-9]+\.[0-9]+\+g[0-9]+\.[0-9a-f]{7}$/.test(provenance.version) ||
      !provenance.version.endsWith(`.${provenance.commit.slice(0, 7)}`)) {
    throw new Error("Native artifacts must identify the accepted same-commit distribution");
  }
  const checksums = (await readFile(path.join(artifactsDirectory, "SHA256SUMS"), "utf8")).trim().split("\n");
  await mkdir(destination, { mode: 0o700 });
  const outputs = [];
  for (const [target, asset] of Object.entries(manifest.platforms)) {
    if (plugin.optionalDependencies[asset.package] !== plugin.version) {
      throw new Error(`Optional dependency must pin ${asset.package} to ${plugin.version}`);
    }
    const name = `pvisor-${provenance.version}-${asset.archivePlatform}.tar.gz`;
    const entriesForArchive = checksums.filter((line) => line.endsWith(`  ${name}`));
    if (entriesForArchive.length !== 1 || !/^[0-9a-f]{64}  /.test(entriesForArchive[0])) {
      throw new Error(`Expected one SHA-256 entry for ${name}`);
    }
    const sha256 = entriesForArchive[0].slice(0, 64);
    const input = path.join(artifactsDirectory, name);
    const info = await lstat(input);
    if (!info.isFile() || info.size > MAX_ARCHIVE_BYTES) throw new Error(`Invalid native archive: ${name}`);
    const bytes = await readFile(input);
    if (bytes.length > MAX_ARCHIVE_BYTES ||
        createHash("sha256").update(bytes).digest("hex") !== sha256) {
      throw new Error(`SHA-256 mismatch: ${name}; refusing to package`);
    }
    const archive = path.join(destination, name);
    await writeFile(archive, bytes, { flag: "wx", mode: 0o600 });
    // CI supplies an accepted native_archive output from this exact commit.
    const entries = execFileSync("tar", ["-tzf", archive], { encoding: "utf8", maxBuffer: 64 * 1024 }).trim().split("\n").sort();
    if (JSON.stringify(entries) !== JSON.stringify(payload)) throw new Error(`Unexpected native archive layout: ${name}`);
    const directory = path.join(destination, asset.package);
    await mkdir(directory);
    execFileSync("tar", ["-xzf", archive, "-C", directory, "--no-same-owner", "--no-same-permissions", "--keep-old-files"]);
    for (const entry of payload) {
      const info = await lstat(path.join(directory, entry));
      if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Non-regular runtime payload: ${entry}`);
    }
    const [os, cpu] = target.split("-");
    await writeFile(path.join(directory, "package.json"), JSON.stringify({
      name: asset.package,
      version: plugin.version,
      description: `pVisor ${manifest.version} native component set for ${target}`,
      license: "Apache-2.0",
      repository: plugin.repository,
      os: [os],
      cpu: [cpu],
      exports: { "./package.json": "./package.json" },
      files: ["bin", "LICENSE", "NOTICE"],
      publishConfig: plugin.publishConfig,
      pvisorRuntimeVersion: manifest.version,
      pvisorRuntimeDistribution: provenance.version,
      pvisorRuntimeCommit: provenance.commit,
      pvisorReleaseArchive: {
        url: `https://github.com/${manifest.repository}/releases/download/plugins-v${plugin.version}/${name}`,
        sha256,
      },
    }, null, 2) + "\n");
    outputs.push(directory);
  }
  return outputs;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length < 3 || process.argv.length > 4) {
      throw new Error("Usage: node release.mjs VERIFIED_NATIVE_DIRECTORY [OUTPUT_DIRECTORY]");
    }
    const destination = path.resolve(process.argv[3] || path.join(root, "dist"));
    const outputs = await prepareRelease(destination, { artifactsDirectory: path.resolve(process.argv[2]) });
    console.log(JSON.stringify({ plugin_version: JSON.parse(await readFile(path.join(root, "package.json"), "utf8")).version, runtime_version: runtime.version, packages: outputs }, null, 2));
  } catch (error) {
    console.error(String(error));
    process.exitCode = 1;
  }
}
