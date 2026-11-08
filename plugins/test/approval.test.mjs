import assert from "node:assert/strict";
import test from "node:test";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";

const success = { exit_code: 0, interrupted: false, stdout: "preview of src.txt", stderr: "" };

for (const kind of ["pi", "dsh"]) {
  test(`${kind}: approval retains initiating project, exact selector and explicit paths`, { timeout: 5000 }, async (t) => {
    const calls = [];
    const host = approvalHost(kind, "/first", (project) => ({
      project: async () => project,
      operate: async (action, input) => {
        calls.push({ project, action, input: structuredClone(input) });
        return success;
      },
    }));
    const raw = { selector: "/first-stage", paths: ["src.txt"] };
    const operation = approvalOperation(host, "apply", raw, t.signal);
    try {
      const request = await operation.entered();
      assert.match(request.reason, /\/first/);
      assert.match(request.reason, /\/first-stage/);
      assert.match(request.reason, /src\.txt/);
      assert.match(request.reason, /preview of src\.txt/);
      raw.selector = "/second-stage";
      raw.paths.splice(0, 1, "unreviewed.txt");
      host.moveProject("/second");
      host.approve();
      await operation.result;
      assert.deepEqual(calls, ["preview", "apply"].map((action) => ({
        project: "/first", action, input: { selector: "/first-stage", paths: ["src.txt"] },
      })));
    } finally {
      await operation.close();
    }
  });

  test(`${kind}: rejected and failed approval callbacks never apply or drop`, { timeout: 5000 }, async (t) => {
    for (const action of ["apply", "drop"]) {
      for (const outcome of ["rejected", "cancelled", "unavailable", "error", ...(kind === "dsh" ? ["abort"] : [])]) {
        const calls = [];
        const host = approvalHost(kind, "/project", () => ({
          project: async () => "/project",
          operate: async (operation) => { calls.push(operation); return success; },
        }));
        const operation = approvalOperation(host, action, { selector: "/stage", ...(action === "apply" ? { paths: ["src.txt"] } : {}) }, t.signal);
        try {
          await operation.entered();
          if (outcome === "error") host.gate.fail(new Error("approval service offline"));
          else if (outcome === "abort") { host.controller.abort(); host.approve(); }
          else host.reject(outcome);
          const error = await operation.settled;
          if (kind === "dsh" || outcome === "error") assert.ok(error, outcome);
          assert.deepEqual(calls, action === "apply" ? ["preview"] : [], `${action}: ${outcome}`);
        } finally {
          await operation.close();
        }
      }
    }
  });

  test(`${kind}: an early preview failure settles the approval wait and teardown`, { timeout: 5000 }, async (t) => {
    const calls = [];
    const host = approvalHost(kind, "/project", () => ({
      project: async () => "/project",
      operate: async (action) => {
        calls.push(action);
        return { ...success, exit_code: 3, stderr: "injected early preview failure" };
      },
    }));
    const operation = approvalOperation(host, "apply", { selector: "/stage", paths: ["src.txt"] }, t.signal);
    try {
      await assert.rejects(operation.entered(), /injected early preview failure/);
      assert.ok(await operation.settled);
      assert.deepEqual(calls, ["preview"]);
    } finally {
      await operation.close();
    }
  });
}
