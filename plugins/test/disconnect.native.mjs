import assert from "node:assert/strict";
import { readdir, stat } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { Backend } from "../backend.mjs";
import { resolveRuntime } from "../runtime.mjs";
import { discoverTask, disconnectFixture } from "./disconnect-fixture.mjs";

function ok(result) {
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  return result;
}

for (const fault of ["active EOF", "truncated frame EOF", "server SIGKILL", "client pipe loss"]) {
  test(`DOC-PLUGINS-13 ${fault}: recover a lost spawn response without rerun or acceptance`, { timeout: 45_000 }, async (t) => {
    const binary = await resolveRuntime(process.env.PVISOR_PLUGIN_BINARY);
    let disposed = false;
    const f = await disconnectFixture(t, binary, {
      async reconcile({ project, stateDir, until }) {
        if (disposed) return true;
        // Closing MCP is not evidence that its native child stopped. On any
        // assertion/startup failure, reconcile before touching the fixture.
        let ids;
        try { ids = await readdir(stateDir); }
        catch (error) { if (error.code === "ENOENT") return true; throw error; }
        if (!ids.length) return true;
        const cleanup = new Backend(project, { binary, stateDir });
        const deadline = Date.now() + 20_000;
        const remainingSignal = () => {
          const remaining = deadline - Date.now();
          assert.ok(remaining > 0, "native cleanup reconciliation deadline exceeded");
          // Capability probes and the actual operation share this call budget.
          // The process supervisor can additionally need its bounded kill/drain
          // grace, but must not fall back to its ordinary command deadline.
          return AbortSignal.timeout(Math.min(2000, remaining));
        };
        const selector = await until(async () => {
          const candidate = await discoverTask(stateDir, project);
          if (!candidate) return false;
          const observed = await cleanup.operate("status", { selector: candidate }, remainingSignal());
          if (observed.exit_code !== 0) return false;
          const status = JSON.parse(observed.stdout);
          return !status.live && ["cancelled", "failed", "completed"].includes(status.state) ? candidate : false;
        }, "cleanup needs a terminal native receipt; preserve live/uncertain work", 20_000);
        ok(await cleanup.operate("drop", { selector }, remainingSignal()));
        return true;
      },
    });
    const backend = new Backend(f.project, { binary, stateDir: f.stateDir });
    f.send(2, "tools/call", {
      name: "pvisor_spawn",
      arguments: {
        argv: ["sh", "-c", "printf 'fixed\\n' > solution.txt; test \"$(cat solution.txt)\" = fixed; printf 'tests passed\\n' > test-result.txt; sleep 12"],
        timeout_seconds: 15,
      },
    });
    // No response exists yet. This is explicit operator discovery of private
    // records, not a task-list API or an assumption that 'last' names this task.
    let selector;
    await f.until(async () => {
      selector = await discoverTask(f.stateDir, f.project);
      if (!selector) return false;
      try { await stat(path.join(selector, "run.json")); }
      catch (error) { if (error.code === "ENOENT") return false; throw error; }
      const observed = await backend.operate("status", { selector });
      if (observed.exit_code !== 0) return false;
      const status = JSON.parse(observed.stdout);
      return status.state === "running" && status.live &&
        status.filesystem?.sample_paths.includes("test-result.txt");
    }, "native staged write and test must finish before fault");
    assert.ok(!f.responses.some((response) => response.id === 2), "spawn response must still be outstanding");
    for (const action of ["preview", "apply", "drop"]) {
      const input = action === "drop" ? { selector } : { selector, paths: ["solution.txt"] };
      const refused = await backend.operate(action, input);
      assert.notEqual(refused.exit_code, 0, `${action} must refuse a live Session: ${JSON.stringify(refused)}`);
    }
    if (fault === "active EOF") f.child.stdin.end();
    else if (fault === "truncated frame EOF") f.child.stdin.end('{"jsonrpc":"2.0","id":3,"method":"tools/call"');
    else if (fault === "server SIGKILL") f.child.kill("SIGKILL");
    else {
      f.child.stdout.destroy();
      f.child.stdin.destroy();
    }
    const reopened = new Backend(f.project, { binary, stateDir: f.stateDir });
    const final = await f.until(async () => {
      const status = JSON.parse(ok(await reopened.operate("status", { selector })).stdout);
      // A status sampled during owner shutdown can be transiently unknown.
      // Wait for the receipt; do not use that earlier sample as authority for
      // a later operation, which independently reconciles native ownership.
      return !status.live && ["cancelled", "failed", "completed"].includes(status.state) ? status : false;
    }, "native owner must reconcile after transport loss", 20_000);
    t.diagnostic(`${fault}: reconciled state=${final.state}, selector=${selector}`);
    if (fault === "server SIGKILL") {
      assert.ok(["cancelled", "failed", "completed"].includes(final.state), JSON.stringify(final));
    } else {
      assert.equal(final.state, "cancelled", "graceful transport loss must cancel, not wait for natural completion");
    }
    assert.match(ok(await reopened.operate("review", { selector })).stdout, /solution\.txt/);
    assert.match(ok(await reopened.operate("review", { selector })).stdout, /test-result\.txt/);
    await assert.rejects(stat(path.join(f.project, "solution.txt")), { code: "ENOENT" });
    await assert.rejects(stat(path.join(f.project, "test-result.txt")), { code: "ENOENT" });
    assert.equal((await readdir(f.stateDir)).length, 1);
    ok(await reopened.operate("preview", { selector, paths: ["solution.txt"] }));
    ok(await reopened.operate("drop", { selector }));
    disposed = true;
    const [code, signal] = await f.closed;
    if (fault === "server SIGKILL") assert.equal(signal, "SIGKILL");
    else assert.equal(code, 0, `graceful transport loss must not crash: ${signal}`);
    if (fault === "truncated frame EOF") {
      assert.ok(f.responses.some((response) => response.error?.code === -32700));
    }
  });
}
