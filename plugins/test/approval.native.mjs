import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Backend } from "../backend.mjs";
import { approvalGate, approvalHost, approvalOperation } from "./approval-fixture.mjs";

function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  return result;
}

async function fixture(t) {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "plugin-approval-")));
  const project = path.join(root, "first");
  const other = path.join(root, "second");
  const options = { binary: process.env.PVISOR_PLUGIN_BINARY, stateDir: path.join(root, "state") };
  const controller = new AbortController();
  const signal = AbortSignal.any([controller.signal, t.signal]);
  const owned = [];
  const attempts = [];
  const calls = [];
  const client = (cwd) => {
    const native = new Backend(cwd, options);
    function track(work) {
      const pending = Promise.resolve().then(work);
      owned.push(pending);
      pending.catch(() => {});
      return pending;
    }
    return {
      project: () => native.project(),
      run(input) {
        // Register before starting, even if no run result/selector is returned.
        attempts.push(cwd);
        return track(() => native.run({ ...input, timeout_seconds: 5 }, signal));
      },
      operate(action, input, callerSignal) {
        const selected = structuredClone(input);
        return track(async () => {
          const result = await native.operate(action, input, callerSignal ? AbortSignal.any([signal, callerSignal]) : signal);
          calls.push({ action, input: selected, result });
          return result;
        });
      },
    };
  };
  const backend = client(project);
  t.after(async () => {
    controller.abort(new Error("Native approval fixture teardown"));
    await Promise.allSettled(owned);
    try {
      // Failed/interrupted runs need discovery too. Missing records/receipts
      // are uncertainty, not permission to delete the fixture.
      let ids;
      try { ids = await readdir(options.stateDir); }
      catch (error) {
        if (error.code !== "ENOENT" || attempts.length) throw error;
        ids = [];
      }
      assert.ok(ids.length >= attempts.length, "an attempted task has no recoverable record");
      const deadline = Date.now() + 20_000;
      for (const id of ids) {
        assert.match(id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
        const directory = path.join(options.stateDir, id);
        assert.equal(await realpath(directory), directory);
        const record = JSON.parse(await readFile(path.join(directory, "task.json"), "utf8"));
        assert.equal(record.schema_version, 1);
        assert.equal(record.task, id);
        assert.ok(attempts.includes(record.project), "unexpected task project");
        const selector = path.join(directory, "stage");
        await stat(path.join(selector, "run.json"));
        const native = new Backend(record.project, options);
        const cleanupSignal = () => {
          const remaining = deadline - Date.now();
          assert.ok(remaining > 0, "native reconciliation deadline exceeded");
          return AbortSignal.timeout(Math.min(2000, remaining));
        };
        while (true) {
          const observed = ok(await native.operate("status", { selector }, cleanupSignal()));
          const status = JSON.parse(observed.stdout);
          if (!status.live && ["cancelled", "failed", "completed"].includes(status.state)) break;
          await delay(20, undefined, { signal: cleanupSignal() });
        }
        ok(await native.operate("drop", { selector }, cleanupSignal()));
      }
    } catch (error) {
      throw new Error(`Preserved fixture ${root}: ${error.message}`, { cause: error });
    }
    await rm(root, { recursive: true });
  });
  await mkdir(project);
  await mkdir(other);
  return { root, project, other, options, backend, client, calls };
}

async function absent(project, name) {
  await assert.rejects(stat(path.join(project, name)), { code: "ENOENT" });
}

for (const kind of ["pi", "dsh"]) {
  test(`DOC-PLUGINS-14 ${kind}: a host edit during approval refuses writeback and preserves exact scope`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    await writeFile(path.join(f.project, "src.txt"), "baseline\n");
    const run = ok(await f.backend.run({ argv: ["sh", "-c", "cat src.txt; printf 'proposal\\n' > src.txt; printf 'scratch\\n' > scratch.txt"] }));
    const other = f.client(f.other);
    const second = ok(await other.run({ argv: ["sh", "-c", "printf 'other\\n' > other.txt"] }));
    const host = approvalHost(kind, f.project, f.client);
    const raw = { selector: run.selector, paths: ["src.txt"] };
    const operation = approvalOperation(host, "apply", raw, t.signal);
    try {
      const request = await operation.entered();
      assert.ok(request.reason.includes(f.project));
      assert.ok(request.reason.includes(run.selector));
      assert.ok(request.reason.includes("src.txt"));
      // Preview succeeded. A real editor now changes the original, while a
      // caller revises its own input and the active host switches projects.
      await writeFile(path.join(f.project, "src.txt"), "editor change\n");
      raw.selector = second.selector;
      raw.paths.splice(0, 1, "other.txt");
      host.moveProject(f.other);
      host.approve();
      assert.ok(await operation.settled, "native apply must reject the post-preview conflict");
      const applies = f.calls.filter((call) => call.action === "apply");
      assert.equal(applies.length, 1, "approval must reach native apply exactly once");
      const conflict = applies[0];
      assert.deepEqual(conflict.input, { selector: run.selector, paths: ["src.txt"] });
      assert.equal(conflict.result.selector, run.selector);
      assert.equal(conflict.result.project, f.project);
      assert.equal(conflict.result.interrupted, false);
      assert.equal(conflict.result.exit_code, 3, JSON.stringify(conflict.result));
      const diagnostic = JSON.parse(conflict.result.stdout);
      assert.equal(diagnostic.status, "blocked");
      assert.equal(diagnostic.source.stage, run.selector);
      assert.deepEqual(diagnostic.blockers, [
        `overlay apply failed: target changed after staging at ${path.join(f.project, "src.txt")}; refusing to overwrite concurrent changes`,
      ]);
      assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"), "editor change\n");
      await absent(f.project, "scratch.txt");
      await absent(f.other, "other.txt");
      assert.match(ok(await f.backend.operate("review", { selector: run.selector })).stdout, /src\.txt/);
      assert.match(ok(await other.operate("review", { selector: second.selector })).stdout, /other\.txt/);
      // Recovery is explicit: restore the baseline and ask again, not an
      // adapter retry or a conflict override.
      await writeFile(path.join(f.project, "src.txt"), "baseline\n");
      host.moveProject(f.project);
      host.gate = approvalGate();
      const retry = approvalOperation(host, "apply", { selector: run.selector, paths: ["src.txt"] }, t.signal);
      try {
        await retry.entered();
        host.approve();
        await retry.result;
      } finally {
        await retry.close();
      }
      assert.equal(await readFile(path.join(f.project, "src.txt"), "utf8"), "proposal\n");
      await absent(f.project, "scratch.txt");
      await absent(f.other, "other.txt");
    } finally {
      await operation.close();
    }
  });

  test(`DOC-PLUGINS-15 ${kind}: rejected, cancelled and errored approvals leave native candidates recoverable`, { timeout: 60_000 }, async (t) => {
    const f = await fixture(t);
    const run = ok(await f.backend.run({ argv: ["sh", "-c", "printf 'proposal\\n' > candidate.txt"] }));
    for (const action of ["apply", "drop"]) {
      for (const outcome of ["rejected", "cancelled", "unavailable", "error", ...(kind === "dsh" ? ["abort"] : [])]) {
        const host = approvalHost(kind, f.project, f.client);
        const operation = approvalOperation(host, action, { selector: run.selector, ...(action === "apply" ? { paths: ["candidate.txt"] } : {}) }, t.signal);
        try {
          await operation.entered();
          if (outcome === "error") host.gate.fail(new Error("approval service offline"));
          else if (outcome === "abort") { host.controller.abort(); host.approve(); }
          else host.reject(outcome);
          const error = await operation.settled;
          if (kind === "dsh" || outcome === "error") assert.ok(error, `${action}: ${outcome}`);
          await absent(f.project, "candidate.txt");
          const reopened = f.client(f.project);
          assert.match(ok(await reopened.operate("review", { selector: run.selector })).stdout, /candidate\.txt/);
          ok(await reopened.operate("preview", { selector: run.selector, paths: ["candidate.txt"] }));
        } finally {
          await operation.close();
        }
      }
    }
  });
}
