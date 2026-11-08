import assert from "node:assert/strict";
import test from "node:test";
import { fileReview } from "../file-review.mjs";

const file = (path, extra = {}) => ({ path, kind: "modified", old_type: "file", new_type: "file", ...extra });
const review = (changes, text, extra = {}) => ({
  review_status: "complete", changes, diff_status: "separate_observation",
  stdout: `Native review summary\nProject diff (environment changes: use review --json)\n${text}`, ...extra,
});

test("per-file review counts validated hunks, including spaces and several hunks/files", () => {
  const files = fileReview(review([file("a file.py"), file("b")],
    "--- a/a file.py\n+++ b/a file.py\n@@ -1,2 +1,3 @@\n same\n-old\n+new\n+added\n" +
    "@@ -9 +10 @@\n-was\n+now\n--- a/b\n+++ b/b\n@@ -0,0 +1 @@\n+hello\n"));
  assert.deepEqual(files.map((item) => [item.additions, item.deletions]), [[3, 2], [1, 0]]);
  assert.ok(files.every((item) => item.diff_status === "text"));
  assert.ok(!files[0].diff.includes("Native review summary"));
});

test("header-looking file contents stay inside their hunk and cannot forge another file's diff", () => {
  const files = fileReview(review([file("a"), file("victim")],
    "--- a/a\n+++ b/a\n@@ -1 +1 @@\n--- a/victim\n+++ b/victim\n"));
  assert.equal(files[0].diff_status, "text");
  assert.equal(files[0].additions, 1);
  assert.equal(files[0].deletions, 1);
  assert.equal(files[1].diff_status, "unavailable");
  assert.equal(files[1].additions, null);
});

test("binary/directory/skipped entries never manufacture zero line counts", () => {
  const files = fileReview(review([file("binary"), file("folder", { new_type: "directory" }), file("large")],
    '"binary": binary (text diff unavailable)\n"large": content diff skipped (unsafe, unreadable, special or large file)\n'));
  assert.deepEqual(files.map((item) => item.diff_status), ["binary", "directory", "unavailable"]);
  assert.ok(files.every((item) => item.additions === null && item.deletions === null));
});

test("truncated or incomplete text cannot produce complete addition/deletion totals", () => {
  const diff = "--- a/a\n+++ b/a\n@@ -1 +1,2 @@\n-old\n+new\n";
  assert.equal(fileReview(review([file("a")], diff))[0].diff_status, "partial");
  const truncated = fileReview(review([file("a")], diff + "  … diff output truncated at 256 bytes\n"));
  assert.equal(truncated[0].additions, null);
  assert.equal(truncated[0].deletions, null);
  const transport = fileReview(review([file("a")], diff, { diff_status: "truncated" }));
  assert.equal(transport[0].diff_status, "partial");
  assert.equal(transport[0].additions, null);
  assert.equal(fileReview({ review_status: "unavailable", changes: [] }), null);
});

test("ambiguous/control/non-UTF-8 names and duplicate labels stay unavailable", () => {
  const text = "--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
  for (const entries of [[file("a\nname"), file("a")], [file("a", { path_bytes: [255] })]]) {
    assert.ok(fileReview(review(entries, text)).every((item) => item.additions === null));
  }
  const duplicated = fileReview(review([file("a")], text + text));
  assert.equal(duplicated[0].diff_status, "unavailable");
  assert.equal(duplicated[0].additions, null);
});

test("only native project-classified changes get a file presentation", () => {
  const value = review([file("project"), file("environment")], "");
  value.handoff = { project_changes: [value.changes[0]] };
  assert.deepEqual(fileReview(value).map((item) => item.path), ["project"]);
});
