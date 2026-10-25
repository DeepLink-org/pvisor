"""Conventional B-LAZY-ENG tests; no VM, build, namespace or benchmark launch."""

import contextlib
import hashlib
import json
import os
import socket
import socketserver
import struct
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))


import lazy_image_v2 as harness


def encode(value):
    # Deliberate noncanonical whitespace: forwarding must preserve original bytes.
    raw = json.dumps(value, indent=1).encode()
    return struct.pack("!I", len(raw)) + raw


def envelope(version, op="ping", **fields):
    return dict(version=version, token="test-token", request=dict(op=op, **fields))


@contextlib.contextmanager
def proxy_with(replies):
    """A framed fake service, not a real cache/benchmark workload."""
    seen = []
    failures = []

    class Upstream(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(2)
            try:
                for wire in replies:
                    request = harness.frame(self.request, clean_eof=True)
                    if request is None:
                        return
                    seen.append(request[1])
                    # Split writes exercise exact-length reads, including raw bodies.
                    for offset in range(0, len(wire), 7):
                        self.request.sendall(wire[offset : offset + 7])
                    if request[1]["version"] == 1:
                        return
            except Exception as error:
                failures.append(error)

    upstream = harness.TCPServer(("127.0.0.1", 0), Upstream)
    proxy = harness.TCPServer(("127.0.0.1", 0), harness.CacheProxy)
    proxy.upstream = upstream.server_address[1]
    proxy.counts = harness.Counts()
    for server in (upstream, proxy):
        threading.Thread(target=server.serve_forever, daemon=True).start()
    client = socket.create_connection(proxy.server_address, timeout=2)
    try:
        yield client, proxy.counts, seen, failures
    finally:
        client.close()
        for server in (proxy, upstream):
            server.shutdown()
            server.server_close()


def wait_rows(counts, number):
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        snapshot = counts.snapshot()
        if len(snapshot["rows"]) >= number or snapshot["errors"]:
            return snapshot
        time.sleep(0.005)
    raise AssertionError("proxy did not finish bounded test exchange")


class ProxyTests(unittest.TestCase):
    def test_v1_closes_after_one_frame_and_preserves_json(self):
        reply = encode(dict(status="ready"))
        with proxy_with([reply]) as (client, counts, seen, failures):
            client.sendall(encode(envelope(1)))
            self.assertEqual(harness.exact(client, len(reply)), reply)
            self.assertEqual(client.recv(1), b"")
            snapshot = wait_rows(counts, 1)
            self.assertEqual(snapshot["connections"], 1)
            self.assertEqual(snapshot["errors"], [])
            self.assertEqual(snapshot["rows"][0]["version"], 1)
            self.assertEqual(snapshot["rows"][0]["op"], "ping")
            self.assertEqual(snapshot["rows"][0]["response_bytes"], len(reply))
            self.assertEqual(len(seen), 1)
            self.assertEqual(failures, [])

    def test_v2_multiple_frames_raw_body_and_application_error(self):
        body = b'\x00\x00\x00\x11{"status":"ready"}\x00\xffraw'
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        replies = [
            encode(dict(status="ready")),
            encode(dict(status="data", length=len(body), sha256=digest)) + body,
            encode(dict(status="error", code="not_found", message="missing")),
            encode(dict(status="ready")),
        ]
        requests = [
            envelope(2),
            envelope(2, "read", path=[102, 111, 111], length=100, offset=0),
            envelope(2, "stat", path=[120]),
            envelope(2),
        ]
        with proxy_with(replies) as (client, counts, seen, failures):
            for request, reply in zip(requests, replies):
                client.sendall(encode(request))
                self.assertEqual(harness.exact(client, len(reply)), reply)
                # Snapshot must return while the persistent client is still open.
                snapshot = wait_rows(counts, len(seen))
                self.assertEqual(snapshot["errors"], [])
            snapshot = wait_rows(counts, 4)
            self.assertEqual(snapshot["connections"], 1)
            self.assertEqual([r["op"] for r in snapshot["rows"]], ["ping", "read", "stat", "ping"])
            self.assertEqual({r["connection_id"] for r in snapshot["rows"]}, {1})
            row = snapshot["rows"][1]
            self.assertEqual(row["path"], [102, 111, 111])
            self.assertEqual(row["content_bytes"], len(body))
            self.assertEqual(row["server_body_sha256"], digest)
            self.assertEqual(row["forwarded_body_sha256"], digest)
            self.assertGreaterEqual(row["elapsed_ms"], 0)
            self.assertEqual(snapshot["rows"][2]["content_bytes"], 0)
            self.assertEqual(seen, requests)
            self.assertEqual(failures, [])

    def test_file_and_metadata_data_accounting_is_separate(self):
        body = b"\x00\xffindex-or-file-payload"
        data = (
            encode(
                dict(
                    status="data",
                    length=len(body),
                    sha256="sha256:" + hashlib.sha256(body).hexdigest(),
                )
            )
            + body
        )
        ready = encode(dict(status="ready"))
        error = encode(dict(status="error", code="not_found", message="missing"))
        exchanges = [
            ("read", data, len(body), 0),
            ("metadata", data, 0, len(body)),
            ("stat", data, 0, 0),
            ("read", ready, 0, 0),
            ("metadata", error, 0, 0),
        ]
        with proxy_with([reply for _, reply, _, _ in exchanges]) as (client, counts, _, failures):
            for number, (op, reply, content, metadata) in enumerate(exchanges, 1):
                client.sendall(encode(envelope(2, op)))
                self.assertEqual(harness.exact(client, len(reply)), reply)
                row = wait_rows(counts, number)["rows"][-1]
                self.assertEqual(row["content_bytes"], content)
                self.assertEqual(row["metadata_bytes"], metadata)
                self.assertEqual(row["response_bytes"], len(reply))
            snapshot = counts.snapshot()
            self.assertEqual(snapshot["errors"], [])
            self.assertEqual(sum(r["content_bytes"] for r in snapshot["rows"]), len(body))
            self.assertEqual(sum(r["metadata_bytes"] for r in snapshot["rows"]), len(body))
            self.assertEqual(
                sum(r["response_bytes"] for r in snapshot["rows"]),
                sum(len(reply) for _, reply, _, _ in exchanges),
            )
            self.assertEqual(failures, [])

    def test_partial_response_is_rejected_and_recorded(self):
        cases = [
            b"\x00\x00",
            struct.pack("!I", 20) + b"{",
            encode(dict(status="data", length=8, sha256="sha256:unused")) + b"abc",
        ]
        for reply in cases:
            with self.subTest(reply=reply), proxy_with([reply]) as (client, counts, _, _):
                client.sendall(encode(envelope(2, "read", path=[], length=8, offset=0)))
                self.assertEqual(client.recv(1), b"")
                snapshot = wait_rows(counts, 1)
                self.assertTrue(snapshot["errors"])
                self.assertEqual(snapshot["rows"], [])

    def test_hash_mismatch_rejects_complete_body(self):
        reply = encode(dict(status="data", length=3, sha256="sha256:wrong")) + b"abc"
        with proxy_with([reply]) as (client, counts, _, _):
            client.sendall(encode(envelope(2, "read", path=[], length=3, offset=0)))
            self.assertEqual(client.recv(1), b"")
            self.assertIn("hash mismatch", wait_rows(counts, 1)["errors"][0]["error"])

    def test_active_requests_not_idle_connections_control_drain(self):
        counts = harness.Counts()
        counts.connections = 4
        self.assertEqual(counts.snapshot(timeout=0.01)["connections"], 4)
        counts.active = 1
        with self.assertRaises(TimeoutError):
            counts.snapshot(timeout=0.01)

    def test_frame_limits_and_partial_request(self):
        left, right = socket.socketpair()
        try:
            right.sendall(struct.pack("!I", harness.MAX_FRAME + 1))
            with self.assertRaises(ValueError):
                harness.frame(left)
        finally:
            left.close()
            right.close()
        with proxy_with([encode(dict(status="ready"))]) as (client, counts, _, _):
            client.sendall(b"\x00\x00")
            client.shutdown(socket.SHUT_WR)
            self.assertEqual(client.recv(1), b"")
            self.assertTrue(wait_rows(counts, 1)["errors"])


class ChrootTests(unittest.TestCase):
    def test_bind_plan_canonicalizes_directories_and_preserves_usrmerge_links(self):
        aliases = {
            "/bin": "usr/bin",
            "/sbin": "usr/sbin",
            "/lib": "usr/lib",
            "/lib64": "/usr/lib64",
        }
        canonical = {name: Path("/" + target.lstrip("/")) for name, target in aliases.items()}
        with (
            patch.object(Path, "exists", autospec=True, return_value=True),
            patch.object(
                Path,
                "resolve",
                autospec=True,
                side_effect=lambda path, **kwargs: canonical.get(str(path), path),
            ),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(
                Path, "is_symlink", autospec=True, side_effect=lambda path: str(path) in aliases
            ),
            patch.object(harness.os, "readlink", side_effect=lambda path: aliases[str(path)]),
        ):
            plan = harness.chroot_bind_plan()
        self.assertEqual(set(plan["binds"]), {"/home", "/usr", "/etc", "/dev", "/proc"})
        self.assertEqual({row["path"]: row["target"] for row in plan["symlinks"]}, aliases)

    def test_bind_plan_rejects_whole_host_root_or_private_tmp(self):
        for unsafe in (Path("/"), Path("/tmp/host-tree")):
            with (
                self.subTest(unsafe=unsafe),
                patch.object(Path, "resolve", autospec=True, return_value=unsafe),
                patch.object(Path, "is_dir", autospec=True, return_value=True),
            ):
                with self.assertRaisesRegex(RuntimeError, "unsafe chroot bind source"):
                    harness.chroot_bind_plan()

    def make_launch_fixture(self, base):
        root = base / "private-root"
        root.mkdir(mode=0o700)
        (root / "tmp").mkdir()
        (root / "tmp").chmod(0o1777)
        frozen = base / "frozen"
        frozen.mkdir()
        for name in ("pvisor", "pvisor-daemon"):
            (frozen / name).write_bytes(("frozen exact bytes: " + name).encode())
        return root, frozen

    def test_launch_copies_are_identical_and_owned_private_paths_are_valid(self):
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            binaries = harness.copy_launch_binaries(frozen, root)
            harness.validate_launch_tree(root)
            harness.verify_launch_binaries(binaries, root)
            for name, record in binaries.items():
                copied = root / "tmp/lazy-binaries" / name
                self.assertEqual(copied.read_bytes(), (frozen / name).read_bytes())
                self.assertEqual(record["launch_path"], "/tmp/lazy-binaries/" + name)
                self.assertEqual(record["frozen_sha256"], record["launch_sha256"])
                self.assertEqual(copied.stat().st_mode & 0o7777, 0o700)
            (root / "tmp/lazy-binaries").chmod(0o777)
            with self.assertRaisesRegex(RuntimeError, "unsafe private launch ancestor"):
                harness.validate_launch_tree(root)

    def test_corrupted_copy_or_changed_launch_binary_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            with patch.object(
                harness.shutil,
                "copyfile",
                side_effect=lambda source, destination: destination.write_bytes(b"corrupt"),
            ):
                with self.assertRaisesRegex(RuntimeError, "copy hash mismatch"):
                    harness.copy_launch_binaries(frozen, root)
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            binaries = harness.copy_launch_binaries(frozen, root)
            (root / "tmp/lazy-binaries/pvisor").write_bytes(b"changed after copy")
            with self.assertRaisesRegex(RuntimeError, "launch/frozen binary changed"):
                harness.verify_launch_binaries(binaries, root)

    def test_unbound_evidence_path_refused_before_construction(self):
        with (
            patch.object(
                harness, "chroot_bind_plan", return_value=dict(binds=["/home"], symlinks=[])
            ),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(harness.tempfile, "mkdtemp") as create,
            patch.object(harness.subprocess, "run") as mount,
        ):
            with self.assertRaisesRegex(RuntimeError, "cannot preserve evidence/workspace path"):
                harness.enter_private_chroot(Path("/elsewhere/evidence"), Path("/home/workspace"))
            create.assert_not_called()
            mount.assert_not_called()

    def test_pivot_root_utility_invocation(self):
        root = Path("/tmp/private-root")
        with (
            patch.object(harness.shutil, "which", return_value="/usr/sbin/pivot_root"),
            patch.object(harness.subprocess, "run") as run,
        ):
            method = harness.perform_pivot_root(root)
        run.assert_called_once_with(
            ["/usr/sbin/pivot_root", str(root), str(root / ".old-root")], check=True, timeout=10
        )
        self.assertIn("utility", method)

    def test_pivot_root_libc_fallback_without_architecture_specific_syscall_number(self):
        root = Path("/tmp/private-root")
        pivot = Mock(return_value=0)
        with (
            patch.object(harness.shutil, "which", return_value=None),
            patch.object(
                harness.ctypes, "CDLL", return_value=SimpleNamespace(pivot_root=pivot)
            ) as load,
        ):
            self.assertEqual(harness.perform_pivot_root(root), "libc pivot_root")
        load.assert_called_once_with(None, use_errno=True)
        pivot.assert_called_once_with(os.fsencode(root), os.fsencode(root / ".old-root"))
        self.assertEqual(pivot.argtypes, [harness.ctypes.c_char_p, harness.ctypes.c_char_p])
        self.assertEqual(pivot.restype, harness.ctypes.c_int)

    def test_unsupported_or_denied_pivot_root_is_environment_gap_not_chroot_fallback(self):
        root = Path("/tmp/private-root")
        cases = [SimpleNamespace(), SimpleNamespace(pivot_root=Mock(return_value=-1))]
        for libc in cases:
            with (
                self.subTest(libc=libc),
                patch.object(harness.shutil, "which", return_value=None),
                patch.object(harness.ctypes, "CDLL", return_value=libc),
                patch.object(harness.ctypes, "get_errno", return_value=1),
                patch.object(harness.os, "chroot") as chroot,
            ):
                with self.assertRaisesRegex(
                    harness.ContainmentEnvironmentGap, "pivot_root environment gap"
                ):
                    harness.perform_pivot_root(root)
                chroot.assert_not_called()
        with (
            patch.object(harness.shutil, "which", return_value="/usr/sbin/pivot_root"),
            patch.object(
                harness.subprocess,
                "run",
                side_effect=harness.subprocess.CalledProcessError(1, ["pivot_root"]),
            ),
        ):
            with self.assertRaises(harness.ContainmentEnvironmentGap):
                harness.perform_pivot_root(root)

    def test_construction_mounts_and_pivot_are_mocked_and_receipt_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            host_home = base / "host-home"
            output = host_home / "evidence"
            cwd = host_home / "workspace"
            (output / "frozen/bin").mkdir(parents=True)
            cwd.mkdir()
            for name in ("pvisor", "pvisor-daemon"):
                (output / "frozen/bin" / name).write_bytes(("same artifact: " + name).encode())
            private_root = base / "owned-root"
            private_root.mkdir(mode=0o700)
            sources = [str(host_home), "/usr", "/etc", "/dev", "/proc"]
            plan = dict(binds=sources, symlinks=[dict(path="/bin", target="usr/bin")])
            original_validate = harness.validate_launch_tree
            original_verify = harness.verify_launch_binaries
            original_read_text = Path.read_text
            operations = []

            def run(command, **kwargs):
                operations.append(("run", command))

            def pivot(root):
                operations.append(("pivot_root", root))
                return "mock pivot_root"

            def change_directory(path):
                operations.append(("chdir", path))

            def validate(root):
                original_validate(private_root if root == Path("/") else root)

            def verify(binaries, root=Path("/")):
                original_verify(binaries, private_root if root == Path("/") else root)

            def read_text(path, *args, **kwargs):
                if path == Path("/proc/self/mountinfo"):
                    return "mock private chroot mountinfo\n"
                return original_read_text(path, *args, **kwargs)

            with (
                patch.object(harness, "chroot_bind_plan", return_value=plan),
                patch.object(harness.tempfile, "mkdtemp", return_value=str(private_root)),
                patch.object(harness.subprocess, "run", side_effect=run) as mount,
                patch.object(harness, "perform_pivot_root", side_effect=pivot) as root_switch,
                patch.object(harness.os, "chroot") as chroot,
                patch.object(harness.os, "chdir", side_effect=change_directory) as chdir,
                patch.object(harness, "validate_launch_tree", side_effect=validate),
                patch.object(harness, "verify_launch_binaries", side_effect=verify),
                patch.object(Path, "read_text", autospec=True, side_effect=read_text),
            ):
                receipt = harness.enter_private_chroot(output, cwd)
            chroot.assert_not_called()
            root_switch.assert_called_once_with(private_root)
            self.assertEqual(
                [call.args[0] for call in chdir.call_args_list], [private_root, "/", cwd]
            )
            self.assertEqual(
                operations[-5:],
                [
                    ("chdir", private_root),
                    ("pivot_root", private_root),
                    ("chdir", "/"),
                    ("run", ["umount", "-l", "/.old-root"]),
                    ("chdir", cwd),
                ],
            )
            self.assertEqual(mount.call_count, len(sources) + 2)
            self.assertEqual(
                mount.call_args_list[0].args[0],
                ["mount", "--bind", str(private_root), str(private_root)],
            )
            self.assertEqual(mount.call_args_list[0].kwargs, dict(check=True, timeout=10))
            self.assertEqual(mount.call_args_list[-1].args[0], ["umount", "-l", "/.old-root"])
            self.assertEqual(mount.call_args_list[-1].kwargs, dict(check=True, timeout=10))
            for call, source in zip(mount.call_args_list[1:-1], sources):
                self.assertEqual(
                    call.args[0],
                    ["mount", "--rbind", source, str(private_root / source.lstrip("/"))],
                )
                self.assertEqual(call.kwargs, dict(check=True, timeout=10))
            self.assertEqual(os.readlink(private_root / "bin"), "usr/bin")
            self.assertTrue(receipt["entered"])
            self.assertEqual(receipt["root_switch"], "pivot_root")
            self.assertEqual(receipt["pivot_root_method"], "mock pivot_root")
            self.assertTrue(receipt["root_self_bind"])
            self.assertTrue(receipt["old_root_detached"])
            self.assertEqual((private_root / ".old-root").stat().st_mode & 0o7777, 0o700)
            self.assertTrue(receipt["evidence_path_unchanged"])
            self.assertEqual(receipt["cwd"], str(cwd))
            self.assertEqual(len(receipt["binds"]), len(sources))
            self.assertTrue(all(bind["recursive"] for bind in receipt["binds"]))
            self.assertEqual(json.loads((output / "containment-chroot.json").read_text()), receipt)
            for name in ("pvisor", "pvisor-daemon"):
                record = receipt["binaries"][name]
                self.assertEqual(record["launch_path"], "/tmp/lazy-binaries/" + name)
                self.assertEqual(record["launch_sha256"], record["frozen_sha256"])
