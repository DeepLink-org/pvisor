import assert from "node:assert/strict";
import { mkdir, readFile, readdir, rename, stat, symlink, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";

const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;

test("runtime upgrade: retained native task survives package symlink switches and truthful refusal", { timeout: 120_000 }, async (t) => {
  const f = await codingFixture(t);
  const native = process.env.PVISOR_PLUGIN_BINARY;
  assert.ok(path.isAbsolute(native), "select the read-only compatible native artifact explicitly");
  const paths = await f.track(async () => {
    const bin = path.join(f.root, "package manager bin");
    await mkdir(bin);
    const compatible = path.join(bin, "compatible");
    const failing = path.join(bin, "operation-fails");
    const incompatible = path.join(bin, "incompatible");
    const partial = path.join(bin, "partial-help");
    const log = path.join(bin, "invocations");
    const alias = path.join(bin, "pvisor");
    const record = `printf '%s\\n' "$*" >> ${quote(log)}\n`;
    await writeFile(compatible, `#!/bin/sh\n${record}exec ${quote(native)} "$@"\n`, { mode: 0o755 });
    await writeFile(failing, `#!/bin/sh\n${record}case "$*" in
  *--help|--version) exec ${quote(native)} "$@" ;;
  *) printf 'fixture runtime unavailable after successful probe\\n' >&2; exit 73 ;;
esac\n`, { mode: 0o755 });
    await writeFile(incompatible, `#!/bin/sh\n${record}printf 'old CLI without required capabilities\\n'\n`, { mode: 0o755 });
    await writeFile(partial, `#!/bin/sh\n${record}printf '%s\\n' '--safe --no-agent-defaults --stage --timeout --overlaynet-deny-all'
exit 2\n`, { mode: 0o755 });
    await symlink(compatible, alias);
    return { alias, compatible, failing, incompatible, partial, log, bin };
  });
  // A package-manager switch replaces only our symlink, never an executable.
  const switchTo = (target) => f.track(async () => {
    const next = `${paths.alias}.next`;
    await symlink(target, next);
    await rename(next, paths.alias);
  });
  const options = { binary: paths.alias, stateDir: path.join(f.root, "state") };
  const reopened = () => new Backend(f.project, options);
  const doctor = (backend = reopened()) => f.track((signal) => backend.doctor({}, signal));
  const operate = (action, input, backend = reopened(), caller) =>
    f.track((signal) => backend.operate(action, input, signal), caller);
  const calls = async () => (await readFile(paths.log, "utf8")).trim().split("\n");
  ok(await doctor());
  // The shared fixture owns spawn/reconciliation through the immutable artifact;
  // all reopen/approval operations below use the package-manager alias.
  const run = ok(await f.run({
    argv: ["/bin/sh", "-c", "printf 'retained upgrade work\\n' > candidate.txt"],
    timeout_seconds: 5,
  }));
  const input = { selector: run.selector };
  const recordPath = path.join(path.dirname(run.selector), "task.json");
  const originalRecord = await readFile(recordPath);
  const initialIds = await readdir(options.stateDir);
  const retained = reopened();
  assert.match(ok(await operate("review", input, retained)).stdout, /candidate\.txt/);
  for (const [target, message] of [
    [paths.incompatible, /Incompatible pVisor.*spawn lacks --safe/],
    [paths.partial, /Incompatible pVisor.*probe failed.*exit=2/],
    [path.join(paths.bin, "removed-by-upgrade"), /Cannot execute PVISOR_PLUGIN_BINARY/],
  ]) {
    await switchTo(target);
    await assert.rejects(doctor(), message);
    const before = (await calls()).length;
    for (const action of ["status", "review", "preview", "apply", "drop"]) {
      await assert.rejects(operate(action, {
        ...input, ...(["preview", "apply"].includes(action) ? { paths: ["candidate.txt"] } : {}),
      }, retained), message);
    }
    const invoked = (await calls()).slice(before);
    assert.ok(invoked.every((line) => line === "spawn --help"), JSON.stringify(invoked));
    assert.equal(invoked.length, target.endsWith("removed-by-upgrade") ? 0 : 5, "one probe per attempt, no retry/fallback");
    assert.deepEqual(await readFile(recordPath), originalRecord);
    assert.deepEqual(await readdir(options.stateDir), initialIds);
    await assert.rejects(stat(path.join(f.project, "candidate.txt")), { code: "ENOENT" });
    await switchTo(paths.compatible);
    assert.match(ok(await operate("review", input)).stdout, /candidate\.txt/);
  }
  // A successful Doctor is a capability observation, not a guarantee that a
  // later actual operation can run. Keep the real nonzero exit and diagnostic.
  await switchTo(paths.failing);
  ok(await doctor());
  const failed = await operate("review", input);
  assert.equal(failed.exit_code, 73);
  assert.equal(failed.interrupted, false);
  assert.match(failed.stderr, /fixture runtime unavailable after successful probe/);
  assert.equal(failed.selector, run.selector);
  await switchTo(paths.compatible);
  for (const kind of ["pi", "dsh"]) {
    const host = approvalHost(kind, f.project, () => ({
      project: () => f.track(() => reopened().project()),
      operate: (action, value, signal) => operate(action, value, reopened(), signal),
    }));
    const operation = approvalOperation({
      ...host,
      call: (action, value) => f.track(() => host.call(action, value)),
    }, "apply", { ...input, paths: ["candidate.txt"] }, t.signal);
    try {
      const request = await operation.entered();
      assert.ok(request.reason.includes(run.selector));
      await switchTo(paths.incompatible);
      const before = (await calls()).length;
      host.approve();
      assert.match((await operation.settled)?.message || "", /Incompatible pVisor/);
      assert.deepEqual((await calls()).slice(before), ["spawn --help"], "approval revalidates without actual apply or fallback");
      await assert.rejects(stat(path.join(f.project, "candidate.txt")), { code: "ENOENT" });
    } finally {
      await operation.close();
      await switchTo(paths.compatible);
    }
    assert.match(ok(await operate("review", input)).stdout, /candidate\.txt/);
  }
  // Invalid explicit overrides cannot borrow a good host PATH CLI.
  for (const binary of ["pvisor", "", path.join(paths.bin, "missing")]) {
    await assert.rejects(doctor(new Backend(f.project, { ...options, binary })),
      /absolute executable path|Cannot execute PVISOR_PLUGIN_BINARY/);
  }
  const finalStatus = JSON.parse(ok(await operate("status", input)).stdout);
  assert.equal(finalStatus.live, false);
  assert.equal(finalStatus.state, "completed");
  assert.match(ok(await operate("review", input)).stdout, /retained upgrade work/);
  ok(await operate("drop", input));
  assert.deepEqual(await readFile(recordPath), originalRecord);
  assert.deepEqual(await readdir(options.stateDir), initialIds, "reopen never creates another task");
});
