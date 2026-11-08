import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { formatPreview } from "../summary.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";

test("filename selection: literal leading dash and explicit controlled-name directory", { timeout: 60_000 }, async (t) => {
  const f = await codingFixture(t);
  const directory = "reviewer's 文件";
  const files = ["--json", "-notes.txt", "space name.txt", "café.txt", "it's.txt",
    `${directory}/line\nbreak.txt`, `${directory}/direction\u202e.txt`, "scratch.txt"];
  const run = ok(await f.run({
    argv: [process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3", "-c",
      "import json,pathlib,sys\nfor name in json.loads(sys.argv[1]):\n p=pathlib.Path(name); p.parent.mkdir(parents=True,exist_ok=True); p.write_text('candidate:'+name,encoding='utf-8')",
      JSON.stringify(files)],
    timeout_seconds: 10,
  }));
  const selector = run.selector;
  const native = (args) => f.command(process.env.PVISOR_PLUGIN_BINARY, args);
  ok(await f.operate("status", { selector }));
  // Deterministic parser evidence, independent of adapter translation. Both
  // calls are read-only and use the exact owned stage, never a user's stage.
  const separated = await native(["apply", selector, "--json", "--dry-run", "--path", "--json"]);
  assert.equal(separated.interrupted, false);
  assert.equal(separated.signal, null);
  if (separated.exit_code === 0) {
    assert.deepEqual(JSON.parse(separated.stdout).selected.map((entry) => entry.path), ["--json"]);
  } else {
    assert.equal(separated.exit_code, 2, JSON.stringify(separated));
    assert.match(separated.stderr, /value.*--path|--path.*value/);
  }
  t.diagnostic(`Separated --path diagnostic exit: ${separated.exit_code}; attached form must select the exact file`);
  const attached = ok(await native(["apply", selector, "--json", "--dry-run", "--path=--json"]));
  assert.deepEqual(JSON.parse(attached.stdout).selected.map((entry) => entry.path), ["--json"]);

  const first = ok(await f.operate("preview", { selector, paths: ["--json"] }));
  assert.equal(first.summary.can_confirm, true);
  assert.deepEqual(first.summary.selected.map((entry) => entry.path), ["--json"]);
  assert.equal(first.summary.required.length, 0);
  assert.equal(first.summary.candidates, files.length + 1); // Explicit new directory entry.
  assert.equal(first.summary.pending.length, files.length);
  const text = formatPreview(first.summary);
  assert.match(text, /Selected changes: 1/);
  assert.ok(text.includes("--path='--json' --dry-run --diff"));
  assert.ok(text.includes("line\\nbreak.txt"));
  assert.ok(text.includes("direction\\u202e.txt"));
  assert.doesNotMatch(text, /\u202e/);
  for (const name of files) await assert.rejects(readFile(path.join(f.project, name)), { code: "ENOENT" });
  ok(await f.operate("apply", { selector, paths: ["--json"] }));
  assert.equal(await readFile(path.join(f.project, "--json"), "utf8"), "candidate:--json");

  const missing = ok(await f.operate("preview", { selector, paths: ["absent.txt"] }));
  assert.equal(missing.summary.selected.length, 0);
  assert.match(formatPreview(missing.summary), /no-op, not an all-pending apply/);
  const controlled = `${directory}/line\nbreak.txt`;
  const newline = ok(await f.operate("preview", { selector, paths: [controlled] }));
  assert.deepEqual(newline.summary.selected.map((entry) => entry.path), [controlled]);
  assert.match(formatPreview(newline.summary), /Review commands omitted/);
  assert.doesNotMatch(formatPreview(newline.summary), /^pvisor (review|apply) /m);
  const second = ok(await f.operate("preview", { selector, paths: [directory] }));
  assert.deepEqual(second.summary.selected.map((entry) => entry.path).sort(),
    [directory, ...files.filter((name) => name.startsWith(`${directory}/`))].sort());
  assert.equal(second.summary.required.length, 0);
  assert.equal(second.summary.pending.length, 5);
  assert.ok(formatPreview(second.summary).includes("--path='reviewer'\\''s 文件'"));
  ok(await f.operate("apply", { selector, paths: [directory] }));
  ok(await f.operate("drop", { selector }));
  for (const name of files) {
    if (name === "--json" || name.startsWith(`${directory}/`)) {
      assert.equal(await readFile(path.join(f.project, name), "utf8"), `candidate:${name}`);
    } else await assert.rejects(readFile(path.join(f.project, name)), { code: "ENOENT" });
  }
});
