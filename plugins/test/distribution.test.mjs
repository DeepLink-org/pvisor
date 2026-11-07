import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { bundledRuntime, checkRuntimeContract, runtime } from "../runtime.mjs";
import { prepareRelease } from "../release.mjs";

const plugin = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
const names = ["LICENSE", "NOTICE", "bin/libkrunfw.SOURCE", "bin/pvisor", "bin/pvisor-daemon", "bin/pvisor-replay"];

async function fixture(t) {
  const directory = await mkdtemp(path.join(tmpdir(), "pvisor-distribution-test-"));
  t.after(() => rm(directory, { force: true, recursive: true }));
  const tree = path.join(directory, "tree");
  await mkdir(path.join(tree, "bin"), { recursive: true });
  for (const name of names) await writeFile(path.join(tree, name), name, { mode: 0o755 });
  const packageJson = path.join(tree, "package.json");
  await writeFile(packageJson, JSON.stringify({
    name: runtime.platforms["linux-x64"].package,
    version: plugin.version,
    pvisorRuntimeVersion: runtime.version,
    pvisorRuntimeDistribution: `${runtime.version}+g1.aaaaaaa`,
    pvisorRuntimeCommit: "a".repeat(40),
  }));
  const archive = path.join(directory, "runtime.tar.gz");
  execFileSync("tar", ["-czf", archive, "-C", tree, ...names]);
  return { directory, tree, packageJson, bytes: await readFile(archive) };
}

test("bundled runtime resolves exact platform package and complete native component set", async (t) => {
  const f = await fixture(t);
  const binary = await bundledRuntime({ platform: "linux", arch: "x64", resolve: () => f.packageJson });
  assert.equal(binary, path.join(f.tree, "bin/pvisor"));
  await rm(path.join(f.tree, "bin/pvisor-replay"));
  await assert.rejects(bundledRuntime({ platform: "linux", arch: "x64", resolve: () => f.packageJson }));
});

test("unsupported platforms and missing optional packages do not trigger execution or network fallback", async () => {
  await assert.rejects(bundledRuntime({ platform: "win32", arch: "x64" }), /do not support/);
  await assert.rejects(bundledRuntime({ platform: "linux", arch: "x64", resolve: () => { throw new Error("missing"); } }), /optional dependencies/);
});

test("plugin/runtime version mismatch is refused", async (t) => {
  const f = await fixture(t);
  await writeFile(f.packageJson, JSON.stringify({
    name: runtime.platforms["linux-x64"].package,
    version: plugin.version,
    pvisorRuntimeVersion: "0.0.0",
  }));
  await assert.rejects(bundledRuntime({ platform: "linux", arch: "x64", resolve: () => f.packageJson }), /mismatch/);
});

test("a runtime without selective native apply cannot start plugin work", async (t) => {
  const f = await fixture(t);
  const binary = path.join(f.tree, "bin/pvisor");
  await writeFile(binary, `#!${process.execPath}
console.log('--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all ');
`);
  await assert.rejects(checkRuntimeContract(binary), /apply lacks --path/);
  await writeFile(binary, `#!${process.execPath}
console.log('--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ');
`);
  assert.equal(await checkRuntimeContract(binary), binary);
});

test("release preparation verifies checksum before extracting exact native payload", async (t) => {
  const f = await fixture(t);
  const target = runtime.platforms["linux-x64"];
  const manifest = {
    ...runtime,
    platforms: { "linux-x64": target },
  };
  const version = `${runtime.version}+g1.abcdef0`;
  const commit = `abcdef0${"a".repeat(33)}`;
  const archive = `pvisor-${version}-${target.archivePlatform}.tar.gz`;
  await writeFile(path.join(f.directory, archive), f.bytes);
  await writeFile(path.join(f.directory, "SHA256SUMS"), `${createHash("sha256").update(f.bytes).digest("hex")}  ${archive}\n`);
  await writeFile(path.join(f.directory, "runtime-provenance.json"), JSON.stringify({
    version, commit, repository: runtime.repository,
  }));
  const output = path.join(f.directory, "release");
  const options = { manifest, artifactsDirectory: f.directory, expectedCommit: commit };
  const directories = await prepareRelease(output, options);
  assert.equal(directories.length, 1);
  const published = JSON.parse(await readFile(path.join(directories[0], "package.json"), "utf8"));
  assert.equal(published.name, target.package);
  assert.equal(published.version, plugin.version);
  assert.equal(published.pvisorRuntimeVersion, runtime.version);
  assert.deepEqual(published.os, ["linux"]);
  assert.deepEqual(published.cpu, ["x64"]);
  assert.equal(await readFile(path.join(directories[0], "bin/pvisor"), "utf8"), "bin/pvisor");
  assert.equal(published.pvisorRuntimeCommit, commit);
  await assert.rejects(prepareRelease(output, options), { code: "EEXIST" });
  await assert.rejects(prepareRelease(path.join(f.directory, "wrong-sha"), { ...options, expectedCommit: "b".repeat(40) }), /same-commit/);
  await writeFile(path.join(f.directory, archive), "corrupted");
  const broken = path.join(f.directory, "broken");
  await assert.rejects(prepareRelease(broken, options), /SHA-256 mismatch/);
  await assert.rejects(readFile(path.join(broken, archive)), { code: "ENOENT" });
});

test("npm pvisor launcher preserves the native argv and exit status", async (t) => {
  const directory = await mkdtemp(path.join(tmpdir(), "pvisor-launcher-test-"));
  t.after(() => rm(directory, { force: true, recursive: true }));
  const binary = path.join(directory, "native.mjs");
  await writeFile(binary, `#!${process.execPath}
console.log(JSON.stringify(process.argv.slice(2)));
process.exitCode = 7;
`, { mode: 0o700 });
  const child = spawn(process.execPath, [
    fileURLToPath(new URL("../pvisor.mjs", import.meta.url)), "apply", "/stage with spaces", "--path", "src",
  ], { env: { ...process.env, PVISOR_PLUGIN_BINARY: binary }, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  child.stdout.on("data", (bytes) => { stdout += bytes; });
  const [code] = await once(child, "close");
  assert.equal(code, 7);
  assert.deepEqual(JSON.parse(stdout), ["apply", "/stage with spaces", "--path", "src"]);
});
