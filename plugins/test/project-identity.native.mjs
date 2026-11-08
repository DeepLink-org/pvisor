import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, stat, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { removeAfterReconcile } from "./disconnect-fixture.mjs";

function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  return result;
}

test("project identity: reopen, switch windows and lose cwd with two retained native tasks", { timeout: 120_000 }, async (t) => {
  const controller = new AbortController();
  const owned = [];
  const attempts = [];
  const approvals = [];
  const calls = [];
  let root;
  let options;
  let temporaryChanged = false;
  const previousTmp = process.env.TMPDIR;
  // Register before invoking setup or native work, and observe rejections now.
  function track(work, caller, milliseconds = 5000) {
    const signal = AbortSignal.any([
      controller.signal, t.signal, AbortSignal.timeout(milliseconds),
      ...(caller ? [caller] : []),
    ]);
    const pending = Promise.resolve().then(() => {
      signal.throwIfAborted();
      return work(signal);
    });
    owned.push(pending);
    pending.catch(() => {});
    return pending;
  }
  const client = (cwd) => {
    const backend = new Backend(cwd, options);
    return {
      project: () => track(() => backend.project()),
      operate: (action, input, caller) => track(async (signal) => {
        const result = await backend.operate(action, input, signal);
        calls.push({ action, input: structuredClone(input), result });
        return result;
      }, caller),
    };
  };
  const startApproval = (host, input) => {
    const call = host.call.bind(host);
    const operation = approvalOperation({
      ...host,
      call: (action, raw) => track(() => call(action, raw)),
    }, "apply", input, t.signal);
    approvals.push(operation);
    return operation;
  };
  t.after(async () => {
    controller.abort();
    for (const operation of approvals) await operation.close();
    await Promise.allSettled(owned);
    if (temporaryChanged) {
      if (previousTmp === undefined) delete process.env.TMPDIR;
      else process.env.TMPDIR = previousTmp;
    }
    if (!root) return;
    await removeAfterReconcile(root, async () => {
      const deadline = AbortSignal.timeout(15_000);
      const cleanupSignal = () => AbortSignal.any([deadline, AbortSignal.timeout(3000)]);
      let ids = [];
      try { ids = await readdir(options.stateDir); }
      catch (error) {
        if (error.code !== "ENOENT" || attempts.length) throw error;
      }
      assert.equal(ids.length, attempts.length, "every attempted task must be recoverable");
      for (const id of ids) {
        assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
        const directory = path.join(options.stateDir, id);
        assert.equal(await realpath(directory), directory);
        assert.equal((await stat(directory)).mode & 0o777, 0o700);
        assert.equal((await stat(path.join(directory, "task.json"))).mode & 0o777, 0o600);
        const record = JSON.parse(await readFile(path.join(directory, "task.json"), "utf8"));
        assert.deepEqual(record, { schema_version: 1, task: id, project: record.project });
        assert.ok(attempts.includes(record.project));
        const backend = new Backend(record.project, options);
        const selector = path.join(directory, "stage");
        const status = JSON.parse(ok(await backend.operate("status", { selector }, cleanupSignal())).stdout);
        assert.equal(status.live, false);
        assert.ok(["completed", "failed", "cancelled"].includes(status.state));
        const dropped = await backend.operate("drop", { selector }, cleanupSignal());
        if (dropped.exit_code !== 0) {
          assert.equal(dropped.interrupted, false);
          assert.match(dropped.stderr, /already applied; drop cannot undo changes/);
          assert.equal(status.run.overlay.state, "applied");
          assert.equal(status.filesystem.changed_files, 0);
        }
      }
      if (process.platform === "linux") {
        assert.ok(!(await readFile("/proc/self/mountinfo", "utf8")).includes(root));
        for (const pid of (await readdir("/proc")).filter((name) => /^\d+$/.test(name) && Number(name) !== process.pid)) {
          try {
            if ((await stat(`/proc/${pid}`)).uid !== process.getuid()) continue;
            assert.ok(!(await readFile(`/proc/${pid}/cmdline`, "utf8")).includes(root), `owned process ${pid} remains`);
          } catch (error) {
            if (!["ENOENT", "ESRCH", "EACCES"].includes(error.code)) throw error;
          }
        }
      }
      return true;
    });
  });
  const setup = await track(async (signal) => {
    root = await mkdtemp(path.join(tmpdir(), "plugin-project-identity-"));
    root = await realpath(root);
    options = { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(root, "state") };
    const projects = [path.join(root, "first"), path.join(root, "second")];
    for (const project of projects) await mkdir(project);
    const temporary = path.join(root, "tmp");
    await mkdir(temporary);
    signal.throwIfAborted();
    process.env.TMPDIR = temporary;
    temporaryChanged = true;
    const alias = path.join(root, "project-window");
    await symlink(projects[0], alias);
    return { projects, alias };
  }, undefined, 15_000);
  t.diagnostic(`Project identity evidence: ${root}`);
  const [first, second] = setup.projects;
  const runs = [];
  for (const [index, project] of setup.projects.entries()) {
    attempts.push(project);
    runs.push(ok(await track((signal) => new Backend(index ? project : setup.alias, options).run({
      argv: ["sh", "-c", `printf 'project ${index}\\n' > shared.txt; printf 'retained ${index}\\n' > scratch.txt`],
      timeout_seconds: 5,
    }, signal), undefined, 15_000)));
    assert.equal(runs[index].project, project);
  }
  assert.notEqual(runs[0].selector, runs[1].selector);
  const reopened = setup.projects.map((project) => client(project));
  for (let index = 0; index < 2; index++) {
    const own = runs[index].selector;
    const foreign = runs[1 - index].selector;
    assert.equal(JSON.parse(ok(await reopened[index].operate("status", { selector: own })).stdout).live, false);
    assert.match(ok(await reopened[index].operate("review", { selector: own })).stdout, /shared\.txt/);
    for (const action of ["status", "review", "preview", "apply", "drop"]) {
      await assert.rejects(reopened[index].operate(action, {
        selector: foreign, ...(["preview", "apply"].includes(action) ? { paths: ["shared.txt"] } : {}),
      }), /does not belong to this project/);
    }
    for (const selector of ["last", setup.projects[index], `${own}/../stage`]) {
      await assert.rejects(reopened[index].operate("status", { selector }), /exact absolute stage selector/);
    }
    for (const selected of [`../${path.basename(setup.projects[1 - index])}/shared.txt`, path.join(setup.projects[1 - index], "shared.txt")]) {
      await assert.rejects(reopened[index].operate("apply", { selector: own, paths: [selected] }), /relative paths/);
    }
  }
  // Both hosts keep the original project/task despite another active window.
  for (const [index, kind] of ["pi", "dsh"].entries()) {
    const host = approvalHost(kind, index ? second : setup.alias, client);
    const input = { selector: runs[index].selector, paths: ["shared.txt"] };
    const operation = startApproval(host, input);
    try {
      const request = await operation.entered();
      assert.ok(request.reason.includes(setup.projects[index]));
      assert.ok(request.reason.includes(runs[index].selector));
      input.selector = runs[1 - index].selector;
      host.moveProject(setup.projects[1 - index]);
      host.approve();
      await operation.result;
      assert.equal(await readFile(path.join(setup.projects[index], "shared.txt"), "utf8"), `project ${index}\n`);
      await assert.rejects(stat(path.join(setup.projects[index], "scratch.txt")), { code: "ENOENT" });
      if (!index) await assert.rejects(stat(path.join(second, "shared.txt")), { code: "ENOENT" });
    } finally {
      await operation.close();
    }
  }
  // A user can repoint a project-window symlink while a dialog is open.
  // The old task may become unavailable through that window, never redirected.
  for (const kind of ["pi", "dsh"]) {
    const host = approvalHost(kind, setup.alias, client);
    const operation = startApproval(host, { selector: runs[0].selector, paths: ["scratch.txt"] });
    let replaced = false;
    try {
      await operation.entered();
      await rm(setup.alias);
      replaced = true;
      await symlink(second, setup.alias);
      host.approve();
      assert.match((await operation.settled)?.message || "", /does not belong to this project/);
      for (const project of setup.projects) {
        await assert.rejects(stat(path.join(project, "scratch.txt")), { code: "ENOENT" });
      }
    } finally {
      await operation.close();
      if (replaced) {
        await rm(setup.alias, { force: true });
        await symlink(first, setup.alias);
      }
    }
  }
  // A renamed/disappeared project cannot silently redirect a pending approval.
  for (const kind of ["pi", "dsh"]) {
    const host = approvalHost(kind, first, client);
    const operation = startApproval(host, { selector: runs[0].selector, paths: ["scratch.txt"] });
    const renamed = path.join(root, "first-renamed");
    let moved = false;
    try {
      await operation.entered();
      await rename(first, renamed);
      moved = true;
      host.moveProject(second);
      host.approve();
      assert.match((await operation.settled)?.message || "", /ENOENT/);
      await assert.rejects(stat(path.join(renamed, "scratch.txt")), { code: "ENOENT" });
      await assert.rejects(stat(path.join(second, "scratch.txt")), { code: "ENOENT" });
    } finally {
      await operation.close();
      if (moved) await rename(renamed, first);
    }
    assert.match(ok(await client(setup.alias).operate("review", { selector: runs[0].selector })).stdout, /scratch\.txt/);
  }
  // Only rename our completed fixture directories; never edit native staged data.
  for (const directory of [path.dirname(runs[0].selector), options.stateDir]) {
    const parked = `${directory}-parked`;
    let moved = false;
    let linked = false;
    try {
      await rename(directory, parked);
      moved = true;
      await symlink(parked, directory);
      linked = true;
      await assert.rejects(client(first).operate("status", { selector: runs[0].selector }), /must not be a symlink|not a stage owned/);
    } finally {
      if (linked) await rm(directory);
      if (moved) await rename(parked, directory);
    }
    ok(await client(first).operate("status", { selector: runs[0].selector }));
  }
  const applied = calls.filter((call) => call.action === "apply");
  assert.equal(applied.length, 2, "only the two explicit successful approvals reached native apply");
  assert.deepEqual(applied.map((call) => [call.result.project, call.result.selector, call.input.paths]), [
    [first, runs[0].selector, ["shared.txt"]],
    [second, runs[1].selector, ["shared.txt"]],
  ]);
  for (const call of applied) {
    const result = JSON.parse(ok(call.result).stdout);
    assert.equal(result.source.stage, call.result.selector);
    assert.deepEqual(result.selected.map((item) => item.path), ["shared.txt"]);
    assert.equal(result.execution.applied, 1);
  }
});
