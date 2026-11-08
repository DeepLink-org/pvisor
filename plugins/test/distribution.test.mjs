import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
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

async function bounded(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("Probe did not settle within its bound")), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function stalledProbe(t, signal, { exitBeforeReady = false } = {}) {
  const f = await fixture(t);
  const ready = path.join(f.root, "ready");
  await writeFile(f.binary, exitBeforeReady ? `#!${process.execPath}
process.exit(2);
` : `#!${process.execPath}
import { writeFileSync } from "node:fs";
process.on("SIGTERM", () => {});
process.stdout.write("partial capability output\\n");
writeFileSync(${JSON.stringify(ready)}, String(process.pid));
setInterval(() => {}, 1000);
`);
  const controller = new AbortController();
  const probeSignal = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal;
  const running = resolveRuntime(f.binary, { signal: probeSignal });
  // Attach rejection handling immediately, including on fixture setup failure.
  running.catch(() => {});
  let settled = false;
  running.finally(() => { settled = true; }).catch(() => {});
  let pid;
  let stopping;
  const stop = () => stopping ??= (async () => {
    // Always cancel and await the owned probe, even if it never published a PID.
    controller.abort();
    await bounded(running.catch(() => {}), 12_000);
    if (pid) assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
  })();
  t.after(stop);
  try {
    const deadline = Date.now() + 2000;
    while (!pid) {
      probeSignal.throwIfAborted();
      if (settled) throw new Error("Capability probe exited before readiness");
      if (Date.now() >= deadline) throw new Error("Capability probe readiness timed out");
      try { pid = Number(await readFile(ready, "utf8")); }
      catch (error) { if (error.code !== "ENOENT") throw error; }
      if (!pid) await delay(10, undefined, { signal: probeSignal });
    }
  } catch (error) {
    await stop();
    throw error;
  }
  return { running };
}

test("a capability fixture exiting without readiness stops polling and awaits teardown", { timeout: 5000 }, async (t) => {
  await assert.rejects(stalledProbe(t, undefined, { exitBeforeReady: true }),
    /Capability probe exited before readiness/);
});

test("a pre-aborted runtime check never starts a capability probe", async (t) => {
  const f = await fixture(t);
  const invoked = path.join(f.root, "invoked");
  await writeFile(f.binary, `#!${process.execPath}
import { writeFileSync } from "node:fs";
writeFileSync(${JSON.stringify(invoked)}, "unexpected");
console.log(${JSON.stringify(flags)});
`);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(resolveRuntime(f.binary, { signal: controller.signal }), /abort/i);
  await assert.rejects(readFile(invoked), { code: "ENOENT" });
});

test("cancelling a capability probe settles even when the runtime ignores SIGTERM", { timeout: 12_000 }, async (t) => {
  const controller = new AbortController();
  const { running } = await stalledProbe(t, controller.signal);
  controller.abort();
  await bounded(assert.rejects(running, /abort|interrupted/i), 7000);
});

test("capability probe timeout escalates when the runtime ignores SIGTERM", { timeout: 22_000 }, async (t) => {
  const { running } = await stalledProbe(t);
  await bounded(assert.rejects(running, /Incompatible pVisor.*interrupted/s), 17_000);
});

test("capability probes get EOF on stdin rather than waiting for client input", async (t) => {
  const f = await fixture(t);
  await writeFile(f.binary, `#!${process.execPath}
process.stdin.resume();
process.stdin.on("end", () => console.log(${JSON.stringify(flags)}));
`);
  assert.equal(await bounded(resolveRuntime(f.binary), 2000), f.binary);
});
