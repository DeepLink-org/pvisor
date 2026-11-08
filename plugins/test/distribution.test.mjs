import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { resolveRuntime } from "../runtime.mjs";

const flags = "--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all --path --dry-run --json ";

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "pvisor-runtime-test-"));
  t.after(() => rm(root, { force: true, recursive: true }));
  const bin = path.join(root, "bin with spaces");
  await mkdir(bin);
  const binary = path.join(bin, "pvisor");
  const write = (output = flags) => writeFile(binary,
    `#!${process.execPath}\nconsole.log(${JSON.stringify(output)});\n`, { mode: 0o755 });
  await write();
  return { root, bin, binary, write };
}

test("PATH resolves an independently installed runtime, including Homebrew/venv symlinks", async (t) => {
  const f = await fixture(t);
  const link = path.join(f.root, "links");
  await mkdir(link);
  await symlink(f.binary, path.join(link, "pvisor"));
  assert.equal(await resolveRuntime(undefined, { searchPath: link }), path.join(link, "pvisor"));
  assert.equal(await resolveRuntime(undefined, { searchPath: f.bin }), f.binary);
});

test("PATH skips non-executables and directories before an installed CLI", async (t) => {
  const f = await fixture(t);
  const bad = path.join(f.root, "bad");
  await mkdir(path.join(bad, "pvisor"), { recursive: true });
  const searchPath = `${bad}${path.delimiter}${f.bin}`;
  assert.equal(await resolveRuntime(undefined, { searchPath }), f.binary);
  await rm(path.join(bad, "pvisor"), { recursive: true });
  await writeFile(path.join(bad, "pvisor"), "not executable", { mode: 0o600 });
  assert.equal(await resolveRuntime(undefined, { searchPath }), f.binary);
});

test("missing runtime reports Homebrew/pip guidance and ignores cwd-dependent PATH entries", async () => {
  await assert.rejects(resolveRuntime(undefined, { searchPath: ":.:relative" }),
    /not found.*Homebrew or pip.*PVISOR_PLUGIN_BINARY/);
});

test("operator override wins and invalid overrides never fall back to PATH", async (t) => {
  const f = await fixture(t);
  const options = { searchPath: f.bin };
  assert.equal(await resolveRuntime(f.binary, { searchPath: "" }), f.binary);
  for (const override of ["", "pvisor", "relative/pvisor"]) {
    await assert.rejects(resolveRuntime(override, options), /absolute executable path/);
  }
  await assert.rejects(resolveRuntime(path.join(f.root, "missing"), options), /Cannot execute PVISOR_PLUGIN_BINARY/);
  await chmod(f.binary, 0o600);
  await assert.rejects(resolveRuntime(f.binary, options), /Cannot execute PVISOR_PLUGIN_BINARY/);
});

test("incompatible runtime is refused for PATH and overrides without fallback, including after upgrade", async (t) => {
  const f = await fixture(t);
  assert.equal(await resolveRuntime(f.binary), f.binary);
  await f.write("--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all ");
  await assert.rejects(resolveRuntime(f.binary), /apply lacks --path.*Homebrew or pip/);
  const good = path.join(f.root, "good");
  await mkdir(good);
  await writeFile(path.join(good, "pvisor"), `#!${process.execPath}\nconsole.log(${JSON.stringify(flags)});\n`, { mode: 0o755 });
  await assert.rejects(resolveRuntime(undefined, { searchPath: `${f.bin}${path.delimiter}${good}` }), /apply lacks --path/);
  await f.write("--path --dry-run --json ");
  await assert.rejects(resolveRuntime(f.binary), /spawn lacks --safe/);
  await f.write();
  assert.equal(await resolveRuntime(f.binary), f.binary);
});

test("failed CLI probes fail closed with upgrade guidance", async (t) => {
  const f = await fixture(t);
  await writeFile(f.binary, `#!${process.execPath}\nprocess.exit(2);\n`);
  await assert.rejects(resolveRuntime(f.binary), /Incompatible pVisor.*Homebrew or pip/s);
});
