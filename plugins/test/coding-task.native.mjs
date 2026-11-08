import assert from "node:assert/strict";
import { readFile, readdir, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { approvalHost, approvalOperation } from "./approval-fixture.mjs";
import { codingFixture, ok } from "./coding-task-fixture.mjs";

const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";
const baseline = "def add(a, b):\n    return a - b\n";
const repaired = "def add(a, b):\n    return a + b\n";

async function absent(f, name) {
  await assert.rejects(stat(path.join(f.project, name)), { code: "ENOENT" });
}

async function accept(t, f, selector, paths, kind) {
  const host = approvalHost(kind, f.project, () => f.client);
  const operation = approvalOperation(host, "apply", { selector, paths }, t.signal);
  try {
    const request = await operation.entered();
    assert.ok(request.reason.includes(selector));
    for (const name of paths) assert.ok(request.reason.includes(name));
    host.approve();
    await operation.result;
    assert.equal(await operation.settled, null);
  } finally {
    await operation.close();
  }
}

for (const fail of [false, true]) {
  test(`coding task: Python unittest ${fail ? "failure retains candidates without retry" : "repair accepts source and drops generated caches"}`, { timeout: 30_000 }, async (t) => {
    const f = await codingFixture(t);
    await writeFile(path.join(f.project, "calc.py"), baseline);
    await writeFile(path.join(f.project, "test_calc.py"), "import unittest\nfrom calc import add\n\nclass TestAdd(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n");
    const run = await f.run({
      argv: [python, "-c", `
import json, os, pathlib, subprocess, sys
assert sys.stdin.read(1) == "", "task must receive EOF, not interactive input"
home = pathlib.Path(os.environ["HOME"])
assert str(home) != ${JSON.stringify(process.env.HOME)}
for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "CODEX_HOME"]:
    assert pathlib.Path(os.environ[key]).is_relative_to(home), key
pathlib.Path("calc.py").write_text(${JSON.stringify(fail ? "def add(a, b):\n    return a * b\n" : repaired)})
result = subprocess.run([sys.executable, "-m", "unittest", "-v"], capture_output=True, text=True)
print(result.stdout, end="", flush=True)
print(result.stderr, end="", file=sys.stderr, flush=True)
pathlib.Path("test-results.log").write_text(result.stdout + result.stderr)
assert pathlib.Path("__pycache__").is_dir()
print("TASK_ENV=" + json.dumps({"private_home": True, "xdg_private": True}), flush=True)
print("TASK_TEST_EXIT=" + str(result.returncode), flush=True)
sys.exit(result.returncode)
`],
      timeout_seconds: 10,
    });
    assert.equal(run.exit_code, fail ? 1 : 0, JSON.stringify(run));
    assert.equal(run.signal, null);
    assert.equal(run.interrupted, false);
    assert.equal(run.truncated, false);
    assert.equal(run.stage, run.selector);
    assert.equal(run.project, f.project);
    assert.match(run.stderr, fail ? /FAILED \(failures=1\)/ : /Ran 1 test[\s\S]*OK/);
    assert.match(run.stdout, new RegExp(`TASK_TEST_EXIT=${fail ? 1 : 0}`));
    assert.match(run.stdout, /"private_home": true, "xdg_private": true/);
    assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), baseline);
    await absent(f, "__pycache__");
    await absent(f, "test-results.log");
    const status = JSON.parse(ok(await f.operate("status", { selector: run.selector })).stdout);
    assert.equal(status.state, fail ? "failed" : "completed");
    assert.equal(status.live, false);
    const review = ok(await f.operate("review", { selector: run.selector }));
    assert.match(review.stdout, /calc\.py/);
    assert.match(review.stdout, /__pycache__/);
    assert.match(review.stdout, /test-results\.log/);
    if (!fail) {
      await accept(t, f, run.selector, ["calc.py"], "pi");
      assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), repaired);
      const remaining = ok(await f.operate("review", { selector: run.selector }));
      assert.match(remaining.stdout, /test-results\.log/);
    }
    ok(await f.operate("drop", { selector: run.selector }));
    assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), fail ? baseline : repaired);
    await absent(f, "__pycache__");
    await absent(f, "test-results.log");
    assert.equal((await readdir(path.join(f.root, "state"))).length, 1, "no automatic retry");
  });
}

test("coding task: nested git observes atomic edits and stages only intended source", { timeout: 30_000 }, async (t) => {
  const f = await codingFixture(t);
  await writeFile(path.join(f.project, "calc.py"), baseline);
  await writeFile(path.join(f.project, "obsolete.txt"), "remove in candidate\n");
  await writeFile(path.join(f.project, "rename-me.txt"), "rename in candidate\n");
  ok(await f.command("/usr/bin/git", ["init", "--quiet"]));
  ok(await f.command("/usr/bin/git", ["add", "calc.py", "obsolete.txt", "rename-me.txt"]));
  const index = await readFile(path.join(f.project, ".git/index"));
  const config = await readFile(path.join(f.project, ".git/config"));
  const run = ok(await f.run({
    argv: [python, "-c", `
import os, pathlib, subprocess
p = pathlib.Path
p("calc.py.tmp").write_text(${JSON.stringify(repaired)})
os.replace("calc.py.tmp", "calc.py")
os.chmod("calc.py", 0o755)
os.rename("rename-me.txt", "renamed.txt")
p("obsolete.txt").unlink()
p("build.log").write_text("disposable build log\\\\n")
def git(*args):
    return subprocess.check_output(["/usr/bin/git", *args], text=True)
print("WORKTREE_DIFF_START", flush=True)
print(git("diff", "--", "calc.py"), flush=True)
print("STATUS_START", flush=True)
print(git("status", "--porcelain=v1", "--untracked-files=all"), flush=True)
subprocess.run(["/usr/bin/git", "add", "--", "calc.py"], check=True)
print("AFTER_ADD_DIFF=" + repr(git("diff", "--", "calc.py")), flush=True)
print("CACHED_DIFF_START", flush=True)
print(git("diff", "--cached", "--", "calc.py"), flush=True)
`],
    timeout_seconds: 10,
  }));
  assert.match(run.stdout, /-    return a - b/);
  assert.match(run.stdout, /\+    return a \+ b/);
  assert.match(run.stdout, /AM calc\.py/);
  assert.match(run.stdout, /AD obsolete\.txt/);
  assert.match(run.stdout, /\?\? renamed\.txt/);
  assert.match(run.stdout, /AFTER_ADD_DIFF=''/);
  assert.match(run.stdout, /CACHED_DIFF_START[\s\S]*\+    return a \+ b/);
  assert.deepEqual(await readFile(path.join(f.project, ".git/index")), index);
  assert.deepEqual(await readFile(path.join(f.project, ".git/config")), config);
  assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), baseline);
  assert.equal((await stat(path.join(f.project, "calc.py"))).mode & 0o111, 0);
  await absent(f, "renamed.txt");
  await accept(t, f, run.selector, ["calc.py", "rename-me.txt", "renamed.txt"], "dsh");
  assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), repaired);
  assert.equal((await stat(path.join(f.project, "calc.py"))).mode & 0o111, 0o111);
  await absent(f, "rename-me.txt");
  assert.equal(await readFile(path.join(f.project, "renamed.txt"), "utf8"), "rename in candidate\n");
  assert.equal(await readFile(path.join(f.project, "obsolete.txt"), "utf8"), "remove in candidate\n");
  await absent(f, "build.log");
  const remaining = ok(await f.operate("review", { selector: run.selector }));
  assert.match(remaining.stdout, /obsolete\.txt/);
  assert.match(remaining.stdout, /build\.log/);
  assert.match(remaining.stdout, /\.git\/index/);
  ok(await f.operate("drop", { selector: run.selector }));
  assert.deepEqual(await readFile(path.join(f.project, ".git/index")), index);
  assert.deepEqual(await readFile(path.join(f.project, ".git/config")), config);
  assert.equal(await readFile(path.join(f.project, "calc.py"), "utf8"), repaired);
  assert.equal(await readFile(path.join(f.project, "obsolete.txt"), "utf8"), "remove in candidate\n");
  await absent(f, "build.log");
  assert.equal((await readdir(path.join(f.root, "state"))).length, 1);
});

test("coding task: bounded binary and UTF-8 tool output preserves nonzero result", { timeout: 30_000 }, async (t) => {
  const f = await codingFixture(t);
  const run = await f.run({
    argv: [python, "-c", `
import pathlib, sys
assert sys.stdin.read(1) == ""
pathlib.Path("diagnostic.txt").write_text("candidate from failed tool\\n")
sys.stdout.buffer.write("tool: λ\\0\\n".encode() + b"x" * 270000)
sys.stderr.buffer.write(b"tool stderr\\n" + b"y" * 270000)
sys.exit(2)
`],
    timeout_seconds: 10,
  });
  assert.equal(run.exit_code, 2);
  assert.equal(run.signal, null);
  assert.equal(run.interrupted, false);
  assert.equal(run.truncated, true);
  assert.ok(run.stdout.startsWith("tool: λ\0\n"));
  assert.equal(Buffer.byteLength(run.stdout), 256 * 1024);
  assert.equal(Buffer.byteLength(run.stderr), 256 * 1024);
  assert.match(run.stderr, /tool stderr/);
  await absent(f, "diagnostic.txt");
  const status = JSON.parse(ok(await f.operate("status", { selector: run.selector })).stdout);
  assert.equal(status.state, "failed");
  assert.equal(status.live, false);
  assert.match(ok(await f.operate("review", { selector: run.selector })).stdout, /candidate from failed tool/);
  ok(await f.operate("drop", { selector: run.selector }));
  await absent(f, "diagnostic.txt");
});
