import assert from "node:assert/strict";
import test from "node:test";
import { structuredReview } from "../review.mjs";
import { failed } from "../backend.mjs";

function fixture() {
  const changes = [
    { path: "src", kind: "added", new_type: "directory" },
    { path: "src/a", kind: "modified", old_type: "file", new_type: "file", size_bytes: 4 },
    { path: "gone", kind: "deleted", old_type: "file" },
    { path: "link", kind: "added", new_type: "symlink", path_bytes: [108, 105, 110, 107] },
  ];
  const bundle = {
    schema_version: 6, run: { run_id: "run", execution_id: "exec" },
    filesystem: { target: "/project", changes, state: "staged" },
    review_context: { session_id: "run", execution_id: "exec", workspace_generation: 0 },
    handoff: {
      schema_version: 1, project_root: "/project", classification_available: true,
      project_changes: changes, environment_changes: [], unclassified_changes: [],
      unsupported_project_changes: [changes[3]],
      counts: { project: 4, environment: 0, unclassified: 0, unsupported_project: 1 },
    },
  };
  const decode = () => structuredReview({
    exit_code: 0, interrupted: false, truncated: false, stdout: JSON.stringify(bundle),
  }, "/project");
  return { bundle, decode };
}

test("review preserves native entries and scope, counting directories separately and deleted old types", () => {
  const { bundle, decode } = fixture();
  const value = decode();
  assert.equal(value.review_status, "complete");
  assert.deepEqual(value.changes, bundle.filesystem.changes);
  assert.deepEqual(value.counts, { total: 4, file: 2, directory: 1, symlink: 1, other: 0, unknown: 0 });
  assert.equal(value.in_scope, 4);
  assert.equal(value.outside_scope, 0);
});

test("unclassified scope is null, not zero; empty current inventories remain valid", () => {
  const { bundle, decode } = fixture();
  bundle.handoff.classification_available = false;
  bundle.handoff.unclassified_changes = bundle.filesystem.changes;
  bundle.handoff.project_changes = [];
  bundle.handoff.unsupported_project_changes = [];
  bundle.handoff.counts = { project: null, environment: null, unsupported_project: null, unclassified: 4 };
  assert.equal(decode().in_scope, null);
  assert.equal(decode().unclassified, 4);
  bundle.filesystem.changes = [];
  bundle.handoff.unclassified_changes = [];
  bundle.handoff.counts.unclassified = 0;
  assert.deepEqual(decode().changes, []);
});

test("unknown, foreign, malformed and incomplete inventories fail closed rather than reporting no changes", () => {
  for (const mutate of [
    (b) => { b.state = "unknown"; },
    (b) => { b.filesystem.target = "/foreign"; },
    (b) => { b.review_context.execution_id = "foreign"; },
    (b) => { b.schema_version = 5; },
    (b) => { delete b.filesystem.changes; },
    (b) => { b.handoff.counts.project = 0; },
    (b) => { b.handoff.project_changes = []; b.handoff.counts.project = 0; },
    (b) => { b.filesystem.changes[0].kind = "bogus"; },
  ]) {
    const { bundle, decode } = fixture();
    mutate(bundle);
    const result = decode();
    assert.equal(result.review_status, "unavailable");
    assert.equal(result.changes, null);
    assert.equal(failed({ exit_code: 0, ...result }), true);
  }
  for (const override of [{ truncated: true }, { interrupted: true }, { exit_code: 1 }, { stdout: "invalid" }]) {
    assert.equal(structuredReview({ exit_code: 0, stdout: "{}", ...override }, "/project").review_status, "unavailable");
  }
});
