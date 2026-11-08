import assert from "node:assert/strict";
import test from "node:test";
import { formatPreview, summarizePreview } from "../summary.mjs";
import { nativePreview } from "./summary-fixture.mjs";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";

const input = { selector: "/private/task/stage", paths: ["src"] };
const entry = (path, kind = "modified", more = {}) => ({ path, kind, old_type: "file", new_type: "file", ...more });
const summary = (overrides = {}, result = {}) => summarizePreview({ ...nativePreview(input, "/project", overrides), ...result }, input, "/project");

test("summary separates exact selection, required dependencies and other staged candidates", () => {
  const selected = [
    entry("src/new.txt", "added", { old_type: null, size_bytes: 20 }),
    entry("src/old.txt", "deleted", { new_type: null }),
    entry("src/code.txt"),
    entry("src/type", "type_changed", { new_type: "symlink" }),
    entry("src/dir", "opaque", { old_type: "directory", new_type: "directory" }),
  ];
  const required = [entry("src", "added", { new_type: "directory", old_type: null })];
  const pending = [entry("scratch.txt", "added")];
  const value = summary({
    selected, required, candidates: [...selected, ...required, ...pending],
    remaining: { in_scope: 1, outside_scope: 3 },
  });
  const text = formatPreview(value);
  assert.equal(value.can_confirm, true);
  assert.deepEqual(value.pending, pending);
  for (const expected of [
    "Selected changes: 5", 'Add "src/new.txt"', 'Delete "src/old.txt"',
    'Change "src/code.txt"', 'Change type "src/type"', 'Opaque directory change "src/dir"',
    "Additional required changes (also applied): 1",
    "Other staged candidates (NOT applied by this selection): 1",
    "in project 1; outside scope 3", "--path 'src' --dry-run --diff",
  ]) assert.ok(text.includes(expected), expected);
  assert.ok(!text.includes("--all"));
});

test("binary and mode-only candidates stay factual without invented content or test claims", () => {
  const text = formatPreview(summary({
    selected: [entry("image.bin", "modified", { size_bytes: 120, mode: 0o100755 }), entry("mode-only")],
  }));
  assert.match(text, /mode 100755 \(reported, not a mode delta\)/);
  assert.match(text, /Content\/binary classification, previous modes, semantic purpose and test results are not provided/);
  assert.match(text, /content or metadata-only/);
  assert.doesNotMatch(text, /tests passed|binary file changed|mode changed from|lines added/i);
});

test("empty and no-op selection never becomes all pending", () => {
  for (const status of ["ready", "no_changes"]) {
    const value = summary({ status, selected: [], required: [], candidates: [entry("unselected")] });
    assert.equal(value.selected.length, 0);
    assert.equal(value.pending.length, 1);
    assert.match(formatPreview(value), /no-op, not an all-pending apply/);
  }
});

test("blocked/conflicting preview is readable without claiming an empty plan", () => {
  const value = summary({
    status: "blocked", selected: [], required: [], blockers: ["target changed after staging at src/code"],
    remaining: { in_scope: null, outside_scope: null },
  }, { exit_code: 3 });
  assert.equal(value.can_confirm, false);
  assert.equal(value.selected, null);
  assert.match(formatPreview(value), /Selected changes: unknown/);
  assert.match(formatPreview(value), /target changed after staging/);
  assert.match(formatPreview(value), /in project unknown; outside scope unknown/);
  assert.doesNotMatch(formatPreview(value), /No matching staged changes/);
});

test("malformed, truncated, interrupted, wrong identity or unsupported data fail closed", () => {
  for (const result of [
    { stdout: "preview" }, { stdout: "{}" }, { truncated: true }, { interrupted: true }, { signal: "SIGTERM" },
    { exit_code: 7 },
  ]) assert.equal(summary({}, result).can_confirm, false, JSON.stringify(result));
  for (const override of [
    { schema_version: 2 }, { mode: "execute" }, { source: { stage: "/other" } },
    { scope: { kind: "rootfs", target: "/project" } }, { scope: { kind: "project", target: "/other" } },
    { selected: [{ path: "x", kind: "future" }] }, { remaining: { in_scope: -1, outside_scope: 0 } },
  ]) assert.equal(summary(override).can_confirm, false, JSON.stringify(override));
});

test("non-string action kinds cannot be coerced into supported actions in any inventory", () => {
  for (const field of ["selected", "required", "candidates"]) {
    for (const kind of [["modified"], ["added"], [], null, 0, { toString: "modified" }]) {
      const value = summary({ [field]: [entry("x", kind)] });
      assert.equal(value.can_confirm, false, `${field}: ${JSON.stringify(kind)}`);
      assert.match(value.issue, /unsupported or incomplete/);
    }
  }
});

test("untrusted/control/non-UTF-8 paths are data, not instructions or conflated identities", () => {
  const a = entry("lossy", "modified", { path_bytes: [255] });
  const b = entry("lossy", "modified", { path_bytes: [254] });
  const value = summary({
    selected: [a, entry('file\nAPPROVE\u001b[2J\u202e.txt')],
    candidates: [a, b],
  });
  const text = formatPreview(value);
  assert.equal(value.pending.length, 1);
  assert.match(text, /non-UTF-8 identity/);
  assert.ok(text.includes("file\\nAPPROVE\\u001b[2J\\u202e.txt"));
  assert.doesNotMatch(text, /[\u001b\u202e]/);
});

test("every directional control is escaped in labels and suppresses runnable commands", () => {
  for (const point of [0x061c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069]) {
    const control = String.fromCodePoint(point);
    const escaped = `\\u${point.toString(16).padStart(4, "0")}`;
    for (const field of ["selector", "paths"]) {
      const controlled = { selector: input.selector, paths: [...input.paths] };
      if (field === "selector") controlled.selector = `/private/task${control}/stage`;
      else controlled.paths = [`src${control}/file.txt`];
      const text = formatPreview(summarizePreview(nativePreview(controlled), controlled, "/project"));
      assert.ok(text.includes(escaped), `${field}: ${escaped} label must be escaped`);
      assert.ok(!text.includes(control), `${field}: ${escaped} must not be emitted literally`);
      assert.match(text, /Review commands omitted \(long or control-containing paths\)/);
      assert.doesNotMatch(text, /^pvisor (review|apply) /m);
    }
  }
});

test("long lists and labels are bounded and disclose hidden entries; commands quote apostrophes", () => {
  const many = Array.from({ length: 200 }, (_, n) => entry(`${n}-${"x".repeat(2000)}`));
  const value = summary({ selected: many, candidates: many });
  const text = formatPreview(value);
  assert.ok(text.length < 24000, text.length);
  assert.match(text, /188 more entries not shown/);
  assert.match(text, /label truncated/);
  const blocked = formatPreview(summary({ status: "blocked", blockers: Array(100).fill("x".repeat(2000)) }, { exit_code: 3 }));
  assert.ok(blocked.length < 24000, blocked.length);
  assert.match(blocked, /88 more entries not shown/);
  const quoted = { selector: "/private/it's stage", paths: ["it's.txt"] };
  const command = formatPreview(summarizePreview(nativePreview(quoted), quoted, "/project"));
  assert.ok(command.includes("'it'\\''s.txt'"));
});

test("realistic ancestor conflict shows native blocker, known inventory and unknown selected/remaining", () => {
  const value = summary({
    status: "blocked", selected: [], required: [],
    candidates: [entry("experiments/pvisor-case/scratch.txt", "modified", { size_bytes: 14, mode: 420 })],
    remaining: { in_scope: null, outside_scope: null },
    blockers: ["overlay apply failed: target changed after staging at /home/operator/project/experiments/pvisor-case; refusing to overwrite concurrent changes"],
  }, { exit_code: 3 });
  const text = formatPreview(value);
  assert.equal(value.can_confirm, false);
  assert.match(text, /Native candidate inventory: 1 entries/);
  assert.match(text, /Change "experiments\/pvisor-case\/scratch\.txt"/);
  assert.match(text, /Selected changes: unknown/);
  assert.match(text, /in project unknown/);
  assert.match(text, /target changed after staging at \/home\/operator\/project\/experiments\/pvisor-case/);
  assert.doesNotMatch(text, /No matching staged changes/);
});

for (const kind of ["pi", "dsh"]) {
  for (const bad of ["malformed", "malformed-kind", "truncated", "blocked"]) {
    test(`${kind}: ${bad} preview never enters approval or executes apply`, { timeout: 5000 }, async (t) => {
      const calls = [];
      const host = approvalHost(kind, "/project", () => ({
        project: async () => "/project",
        operate: async (action, args) => {
          calls.push(action);
          if (bad === "malformed") return { ...nativePreview(args), stdout: "preview" };
          if (bad === "malformed-kind") return nativePreview(args, "/project", { selected: [entry("src/file", ["modified"])] });
          if (bad === "truncated") return { ...nativePreview(args), truncated: true };
          return { ...nativePreview(args, "/project", { status: "blocked", blockers: ["native conflict"] }), exit_code: 3 };
        },
      }));
      const operation = approvalOperation(host, "apply", input, t.signal);
      try {
        await assert.rejects(operation.entered());
        assert.ok(await operation.settled);
        assert.deepEqual(calls, ["preview"]);
      } finally {
        await operation.close();
      }
    });
  }
}
