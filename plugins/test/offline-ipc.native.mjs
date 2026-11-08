import assert from "node:assert/strict";
import { createServer, Socket } from "node:net";
import { readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { codingFixture, ok } from "./coding-task-fixture.mjs";

const python = process.env.PVISOR_PLUGIN_TEST_PYTHON || "/usr/bin/python3";
const canary = "round18-fixture-only-canary\n";
const prefix = "offline IPC Linux:";

// These assertions describe Linux's private network namespace, not macOS's
// Seatbelt loopback policy. Each selected case owns exactly one native Session.
async function runUnittest(t, source) {
  const f = await codingFixture(t);
  await writeFile(path.join(f.project, "test_offline.py"), source);
  const run = ok(await f.run({
    argv: [python, "-B", "-m", "unittest", "-v", "test_offline"],
    timeout_seconds: 10,
  }));
  assert.match(run.stderr, /Ran 1 test[\s\S]*OK/);
  assert.match(run.stdout, /ROUND18_OK/);
  assert.ok(Buffer.byteLength(run.stdout) + Buffer.byteLength(run.stderr) < 4096);
  await assert.rejects(stat(path.join(f.project, "result.txt")), { code: "ENOENT" });
  const status = JSON.parse(ok(await f.operate("status", { selector: run.selector })).stdout);
  assert.equal(status.state, "completed");
  assert.equal(status.live, false);
  assert.match(ok(await f.operate("review", { selector: run.selector })).stdout, /result\.txt/);
  ok(await f.operate("drop", { selector: run.selector }));
  await assert.rejects(stat(path.join(f.project, "result.txt")), { code: "ENOENT" });
  assert.equal(await readFile(path.join(f.project, "test_offline.py"), "utf8"), source);
}

test(`${prefix} tempfile subprocess and owned Unix socketpair`, {
  skip: process.platform !== "linux", timeout: 30_000,
}, async (t) => {
  await runUnittest(t, `
import os, pathlib, socket, subprocess, sys, tempfile, unittest

class Offline(unittest.TestCase):
    def test_temp_and_ipc(self):
        home = pathlib.Path(os.environ["HOME"])
        for key in ("XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
            self.assertTrue(pathlib.Path(os.environ[key]).is_relative_to(home), key)
        with tempfile.TemporaryDirectory(prefix="round18-") as directory:
            target = pathlib.Path(directory) / "fixture.txt"
            target.write_text("fixture-only")
            child = subprocess.run(
                [sys.executable, "-B", "-c",
                 "import pathlib,sys; print(pathlib.Path(sys.argv[1]).read_text())",
                 str(target)], capture_output=True, text=True, timeout=2, check=True)
            self.assertEqual(child.stdout, "fixture-only\\n")
            left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
            with left, right:
                left.settimeout(2)
                right.settimeout(2)
                left.sendall(b"fixture-only")
                self.assertEqual(right.recv(32), b"fixture-only")
        self.assertFalse(pathlib.Path(directory).exists())
        pathlib.Path("result.txt").write_text("tempfile subprocess socketpair passed\\n")
        print("ROUND18_OK tempfile subprocess socketpair", flush=True)
`);
});

test(`${prefix} same sandbox local TCP server and client`, {
  skip: process.platform !== "linux", timeout: 30_000,
}, async (t) => {
  await runUnittest(t, `
import pathlib, socket, threading, unittest

class Offline(unittest.TestCase):
    def test_private_loopback(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        worker = None
        failures = []
        def cleanup():
            server.close()
            if worker is not None:
                worker.join(3)
                self.assertFalse(worker.is_alive(), "owned server thread remains")
        self.addCleanup(cleanup)
        server.settimeout(2)
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        def serve():
            try:
                connection, _ = server.accept()
                with connection:
                    connection.settimeout(2)
                    self.assertEqual(connection.recv(32), b"fixture-only")
                    connection.sendall(b"private-loopback")
            except BaseException as error:
                failures.append(error)
        worker = threading.Thread(target=serve)
        worker.start()
        with socket.create_connection(server.getsockname(), timeout=2) as client:
            client.sendall(b"fixture-only")
            self.assertEqual(client.recv(32), b"private-loopback")
        worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(failures, [])
        pathlib.Path("result.txt").write_text("private loopback passed\\n")
        print("ROUND18_OK private loopback", flush=True)
`);
});

test(`${prefix} host fixture loopback is unreachable`, {
  skip: process.platform !== "linux", timeout: 30_000,
}, async (t) => {
  const sockets = new Set();
  let requests = 0;
  const server = createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    socket.setTimeout(2000, () => socket.destroy());
    requests += 1;
    if (requests > 2) socket.destroy();
    else socket.end(canary);
  });
  // Register before bind/setup; destroy only sockets accepted by this server.
  t.after(async () => {
    await Promise.all([...sockets].map((socket) => new Promise((resolve) => {
      if (socket.closed) return resolve();
      socket.once("close", resolve);
      socket.destroy();
    })));
    if (server.listening) {
      await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  const control = await new Promise((resolve, reject) => {
    const client = new Socket();
    sockets.add(client);
    client.once("close", () => sockets.delete(client));
    const chunks = [];
    client.on("data", (chunk) => chunks.push(chunk));
    client.once("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    client.once("error", reject);
    client.setTimeout(2000, () => client.destroy(new Error("fixture positive control timeout")));
    client.connect({ host: "127.0.0.1", port });
  });
  assert.equal(control, canary);
  assert.equal(requests, 1);
  await runUnittest(t, `
import errno, pathlib, socket, unittest

class Offline(unittest.TestCase):
    def test_host_loopback_denied(self):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as client:
            client.settimeout(2)
            try:
                client.connect(("127.0.0.1", ${port}))
            except OSError as error:
                self.assertIn(error.errno, (errno.ECONNREFUSED, errno.ENETUNREACH,
                                           errno.EHOSTUNREACH, errno.EACCES, errno.EPERM))
            else:
                self.fail("sandbox connected to the fixture-owned host listener")
        pathlib.Path("result.txt").write_text("host fixture unreachable\\n")
        print("ROUND18_OK host fixture unreachable", flush=True)
`);
  assert.equal(requests, 1, "sandbox must not reach the host fixture");
});
