import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { executionPolicy, selectExecution } from "../execution.mjs";

async function fixture(t) {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "pvisor-execution-")));
  t.after(() => rm(root, { recursive: true, force: true }));
  const project = path.join(root, "project"), state = path.join(root, "state"), rootfs = path.join(root, "rootfs");
  for (const dir of [project, state, rootfs]) await mkdir(dir, { mode: 0o700 });
  return { root, project, state, rootfs };
}

const capability = (extra = {}) => ({ schema_version: 1, operation: "vm-check", backend: "kvm",
  can_create_vm: true, firmware: "embedded", boot_verified: false, reason: "created", ...extra });
const transport = (value) => async () => ({ exit_code: 0, interrupted: false, truncated: false, stdout: JSON.stringify(value) });

test("execution defaults to VM-first without Host fallback; invalid operator values never become defaults", () => {
  assert.deepEqual(executionPolicy(), { mode: "auto", allowHostFallback: false, rootfs: undefined });
  for (const mode of ["", "container", "AUTO", null]) assert.throws(() => executionPolicy(mode));
  for (const allow of ["", "yes", true, null]) assert.throws(() => executionPolicy("auto", allow));
  assert.throws(() => executionPolicy("auto", "false", "relative"));
});

test("VM selection requires both current-process virtualization and an explicit rootfs", async (t) => {
  const f = await fixture(t);
  const context = { ...f, binary: "/pvisor", execute: transport(capability()) };
  const blocked = await selectExecution(executionPolicy(), context);
  assert.equal(blocked.selected, null); assert.equal(blocked.ready, false);
  assert.match(blocked.reason, /ROOTFS/);
  const ready = await selectExecution(executionPolicy("auto", "false", f.rootfs), context);
  assert.equal(ready.selected, "vm"); assert.equal(ready.fallback, false);
  assert.equal(ready.virtualization.boot_verified, false);
  await assert.rejects(selectExecution(executionPolicy("auto", "true", f.rootfs),
    { ...context, hostToolchain: true }), /Host Rust toolchain/);
  const missingFirmware = await selectExecution(executionPolicy("auto", "false", f.rootfs),
    { ...context, execute: transport(capability({ firmware: "missing" })) });
  assert.equal(missingFirmware.selected, null);
});

test("only explicit auto fallback authorizes Host, never vm-only mode or a broken probe", async (t) => {
  const f = await fixture(t);
  const context = { ...f, binary: "/pvisor",
    execute: transport(capability({ can_create_vm: false, reason: "KVM permission denied" })) };
  const fallback = await selectExecution(executionPolicy("auto", "true"), context);
  assert.equal(fallback.selected, "host"); assert.equal(fallback.fallback, true);
  assert.match(fallback.reason, /permission denied/);
  const strict = await selectExecution(executionPolicy("vm", "true"), context);
  assert.equal(strict.selected, null);
  await assert.rejects(selectExecution(executionPolicy("auto", "true"),
    { ...context, execute: transport({}) }), /Invalid native/);
  await assert.rejects(selectExecution(executionPolicy("auto", "true"),
    { ...context, execute: async () => ({ exit_code: 1 }) }), /upgrade/);
  await assert.rejects(selectExecution(executionPolicy("auto", "true"),
    { ...context, execute: async () => ({ exit_code: 0, truncated: true }) }), /preflight unavailable/);
  assert.equal((await selectExecution(executionPolicy("host"),
    { ...context, execute: () => assert.fail("Explicit Host must not run VM probes") })).selected, "host");
});

test("invalid rootfs configuration cannot be excused by fallback permission or symlinks", async (t) => {
  const f = await fixture(t);
  const context = { ...f, binary: "/pvisor", execute: transport(capability()) };
  const alias = path.join(f.root, "project-link");
  await symlink(f.project, alias);
  for (const rootfs of ["/", f.root, f.project, f.state, alias]) {
    await assert.rejects(selectExecution(executionPolicy("auto", "true", rootfs), context), /rootfs/);
  }
});
