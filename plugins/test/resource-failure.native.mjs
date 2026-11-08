import assert from "node:assert/strict";
import { readFile, readdir, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { codingFixture, ok } from "./coding-task-fixture.mjs";

const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";
const baseline = "def answer():\n    return 0\n";
const candidate = "def answer():\n    return 42\n";

// These are voluntary limits in the task's Python process AFTER imports, not
// plugin quotas. No limit is installed in Node, the CLI or the native owner.
// Handled exceptions deliberately exit nonzero; no signal mapping is assumed.
const faults = [
  {
    name: "address space", exit: 71, marker: "MemoryError AS=134217728 request=268435456",
    body: `
limit = 128 * 1024 * 1024
resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
assert resource.getrlimit(resource.RLIMIT_AS) == (limit, limit)
try:
    allocation = bytearray(256 * 1024 * 1024)
except MemoryError:
    print("FAULT_REACHED MemoryError AS=134217728 request=268435456", flush=True)
else:
    raise AssertionError("virtual allocation unexpectedly succeeded")
`,
  },
  {
    name: "file descriptors", exit: 72, marker: "EMFILE NOFILE=24",
    body: `
resource.setrlimit(resource.RLIMIT_NOFILE, (24, 24))
assert resource.getrlimit(resource.RLIMIT_NOFILE) == (24, 24)
owned = []
reached = False
try:
    for attempt in range(32):
        try:
            owned.append(os.open("partial.txt", os.O_RDONLY))
        except OSError as error:
            assert error.errno == errno.EMFILE, repr(error)
            reached = True
            break
finally:
    for descriptor in owned:
        os.close(descriptor)
assert reached, "bounded descriptor attempts did not reach EMFILE"
print("FAULT_REACHED EMFILE NOFILE=24 owned=" + str(len(owned)), flush=True)
`,
  },
  {
    name: "file size", exit: 73, marker: "EFBIG FSIZE=8192 bytes=8192",
    body: `
# Ignore SIGXFSZ in this process only so Linux returns observable EFBIG.
signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
resource.setrlimit(resource.RLIMIT_FSIZE, (8192, 8192))
assert resource.getrlimit(resource.RLIMIT_FSIZE) == (8192, 8192)
descriptor = os.open("bounded-output.txt", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
try:
    assert os.write(descriptor, b"x" * 8192) == 8192
    try:
        os.write(descriptor, b"y" * 8192)
    except OSError as error:
        assert error.errno == errno.EFBIG, repr(error)
    else:
        raise AssertionError("write beyond file-size limit unexpectedly succeeded")
    assert os.fstat(descriptor).st_size == 8192
finally:
    os.close(descriptor)
print("FAULT_REACHED EFBIG FSIZE=8192 bytes=8192", flush=True)
`,
  },
];

async function absent(f, name) {
  await assert.rejects(stat(path.join(f.project, name)), { code: "ENOENT" });
}

async function hostUnchanged(f) {
  assert.equal(await readFile(path.join(f.project, "answer.py"), "utf8"), baseline);
  await absent(f, "partial.txt");
  await absent(f, "bounded-output.txt");
}

function bounded(result) {
  assert.equal(result.signal, null, JSON.stringify(result));
  assert.equal(result.interrupted, false, JSON.stringify(result));
  assert.equal(result.truncated, false, JSON.stringify(result));
  assert.ok(Buffer.byteLength(result.stdout) + Buffer.byteLength(result.stderr) < 4096);
}

// One selected parent, four awaited serial children; each child's fixture hook
// joins operations, reconciles terminal state and drops before the next starts.
// Recovery intentionally uses a DIFFERENT fixture/project, not a second task
// in codingFixture, whose one-attempt guard must remain intact.
test("resource failure Linux: retained candidates and fresh-session recovery", {
  skip: process.platform !== "linux", timeout: 110_000,
}, async (t) => {
  const supervisorLimits = await readFile("/proc/self/limits", "utf8");
  let previousProject;
  for (const fault of faults) {
    await t.test(`handled ${fault.name} failure`, { timeout: 25_000 }, async (c) => {
      const f = await codingFixture(c, { runMilliseconds: 20_000 });
      previousProject = f.project;
      await writeFile(path.join(f.project, "answer.py"), baseline);
      const run = await f.run({
        argv: [python, "-B", "-c", `
import errno, os, pathlib, resource, signal, sys
signal.alarm(12)
pathlib.Path("answer.py").write_text(${JSON.stringify(candidate)})
pathlib.Path("partial.txt").write_text("partial work before controlled resource failure\\n")
print("FAULT_READY ${fault.name}", flush=True)
${fault.body}
sys.exit(${fault.exit})
`],
        timeout_seconds: 15,
      });
      bounded(run);
      assert.equal(run.exit_code, fault.exit, JSON.stringify(run));
      assert.ok(run.stdout.includes(`FAULT_READY ${fault.name}`), run.stdout);
      assert.ok(run.stdout.includes(`FAULT_REACHED ${fault.marker}`), run.stdout);
      assert.equal(run.selector, run.stage);
      assert.equal(run.project, f.project);
      c.diagnostic(JSON.stringify({
        selector: run.selector, exit: run.exit_code, signal: run.signal,
        stdout: run.stdout, stderr: run.stderr,
      }));
      await hostUnchanged(f);
      const status = JSON.parse(ok(await f.operate("status", { selector: run.selector })).stdout);
      assert.equal(status.state, "failed");
      assert.equal(status.live, false);
      const review = ok(await f.operate("review", { selector: run.selector }));
      assert.match(review.stdout, /answer\.py/);
      assert.match(review.stdout, /partial work before controlled resource failure/);
      if (fault.name === "file size") assert.match(review.stdout, /bounded-output\.txt/);
      const preview = ok(await f.operate("preview", {
        selector: run.selector, paths: ["answer.py", "partial.txt"],
      }));
      assert.equal(preview.summary.can_confirm, true, JSON.stringify(preview.summary));
      await hostUnchanged(f);
      assert.match(ok(await f.operate("review", { selector: run.selector })).stdout,
        /partial work before controlled resource failure/);
      assert.equal((await readdir(path.join(f.root, "state"))).length, 1, "no automatic retry");
      assert.ok(f.calls.every(({ action }) => !["apply", "drop"].includes(action)),
        "observations and preview must not accept/discard partial work");
      // Explicit operator disposal, only after terminal/nonlive observation.
      ok(await f.operate("drop", { selector: run.selector }));
      await hostUnchanged(f);
      assert.equal(await readFile("/proc/self/limits", "utf8"), supervisorLimits);
    });
  }
  await t.test("healthy command in a fresh different-project Session", { timeout: 25_000 }, async (c) => {
    const f = await codingFixture(c, { runMilliseconds: 20_000 });
    assert.notEqual(f.project, previousProject);
    const run = ok(await f.run({
      argv: [python, "-B", "-c", `
import os, pathlib, signal
signal.alarm(12)
with open("healthy.txt", "w") as output:
    output.write("healthy fresh Session\\n")
print("HEALTHY_FRESH_SESSION", flush=True)
`],
      timeout_seconds: 15,
    }));
    bounded(run);
    assert.match(run.stdout, /HEALTHY_FRESH_SESSION/);
    await absent(f, "healthy.txt");
    const status = JSON.parse(ok(await f.operate("status", { selector: run.selector })).stdout);
    assert.equal(status.state, "completed");
    assert.equal(status.live, false);
    assert.match(ok(await f.operate("review", { selector: run.selector })).stdout,
      /healthy fresh Session/);
    ok(await f.operate("drop", { selector: run.selector }));
    await absent(f, "healthy.txt");
    assert.equal((await readdir(path.join(f.root, "state"))).length, 1);
    assert.equal(await readFile("/proc/self/limits", "utf8"), supervisorLimits);
    c.diagnostic(JSON.stringify({ selector: run.selector, exit: run.exit_code, signal: run.signal }));
  });
});
