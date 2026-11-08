import assert from "node:assert/strict";
import test from "node:test";
import { approvalOperation, pendingApproval } from "./concurrent-approval-fixture.mjs";

for (const kind of ["pi", "dsh"]) {
  test(`concurrent approval ${kind}: native mutation failure is surfaced once without retry`, { timeout: 5000 }, async () => {
    const calls = [];
    let changed = false;
    const input = Object.freeze({ selector: "/owned/stage", paths: Object.freeze(["a.txt", "b.txt"]) });
    const host = pendingApproval(kind, "/project", () => ({
      project: async () => "/project",
      async operate(action, args) {
        calls.push({ action, args });
        if (action === "preview") return { exit_code: 0, stdout: "approved a.txt and b.txt", stderr: "" };
        assert.equal(changed, true);
        return { exit_code: 3, stdout: "", stderr: "target changed after staging at b.txt; refusing to overwrite concurrent changes" };
      },
    }));
    const operation = approvalOperation(host, input);
    try {
      const request = await operation.ready;
      assert.match(request.reason || request.message, /approved a.txt and b.txt/);
      assert.deepEqual(calls.map((call) => call.action), ["preview"]);
      changed = true; // Another explicit operation completed while approval was pending.
      host.release();
      await assert.rejects(operation.settled, /target changed after staging at b\.txt; refusing to overwrite concurrent changes/);
      assert.deepEqual(calls.map((call) => call.action), ["preview", "apply"]);
      assert.deepEqual(calls[1].args, input);
      assert.equal(host.requests.length, 1);
    } finally {
      await operation.close();
    }
  });

  test(`concurrent approval ${kind}: preview failure settles readiness without entering approval`, { timeout: 5000 }, async () => {
    const calls = [];
    const host = pendingApproval(kind, "/project", () => ({
      project: async () => "/project",
      async operate(action) {
        calls.push(action);
        return { exit_code: 3, stdout: "", stderr: "deterministic early preview failure" };
      },
    }));
    const operation = approvalOperation(host, { selector: "/owned/stage", paths: ["a.txt"] });
    try {
      await assert.rejects(operation.ready, /deterministic early preview failure/);
      await assert.rejects(operation.settled, /deterministic early preview failure/);
      assert.deepEqual(calls, ["preview"]);
      assert.deepEqual(host.requests, []);
    } finally {
      await operation.close();
    }
  });
}
