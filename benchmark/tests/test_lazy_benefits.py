"""OCI whiteouts and exact proxy bytes must not silently change the oracle."""

import hashlib
import http.client
import http.server
import io
import json
import sys
import tarfile
import threading
import unittest
from pathlib import Path

from tests.fixtures import workspace

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "storage"))
import lazy_benefits as subject  # noqa: E402


class OciOracleContracts(unittest.TestCase):
    def layer(self, root, entries):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w:gz") as archive:
            for name, payload, kind in entries:
                member = tarfile.TarInfo(name)
                if kind == "symlink":
                    member.type = tarfile.SYMTYPE
                    member.linkname = payload
                    archive.addfile(member)
                else:
                    member.size = len(payload)
                    archive.addfile(member, io.BytesIO(payload))
        data = stream.getvalue()
        digest = hashlib.sha256(data).hexdigest()
        (root / digest).write_bytes(data)
        return {"digest": "sha256:" + digest}

    def test_layer_whiteouts_opaque_directories_and_symlinks_match_mmap_workset(self):
        with workspace() as (root, _):
            prefix = subject.PYTHON_TREE.lstrip("/")
            layers = [
                self.layer(
                    root,
                    [
                        (prefix + "/gone", b"old", "regular"),
                        (prefix + "/package/old", b"old", "regular"),
                        (prefix + "/stay", b"first", "regular"),
                    ],
                ),
                self.layer(
                    root,
                    [
                        (prefix + "/.wh.gone", b"", "regular"),
                        (prefix + "/package/.wh..wh..opq", b"", "regular"),
                        (prefix + "/package/new", b"new", "regular"),
                        (prefix + "/stay", b"second", "regular"),
                        (prefix + "/alias", "stay", "symlink"),
                    ],
                ),
            ]
            expected = hashlib.sha256()
            for name, payload in [("package/new", b"new"), ("stay", b"second")]:
                expected.update(
                    (subject.PYTHON_TREE + "/" + name).encode()
                    + b"\0"
                    + hashlib.sha256(payload).hexdigest().encode()
                    + b"\n"
                )
            oracle = subject.tree_oracle({"source": str(root), "manifest": {"layers": layers}})
            self.assertEqual(oracle, (2, 9, expected.hexdigest()))
            self.assertNotIn(
                "EXPECTED_ORACLE", subject.MMAP_PROBE.replace("EXPECTED_ORACLE", repr(oracle))
            )

    def test_json_commands_pin_phase_and_require_daemon_only_for_remote(self):
        directory = Path(subject.__file__).parent
        for phase in ["storage", "runtime", "remote"]:
            command = json.loads((directory / f"lazy_{phase}.json").read_text())[phase]
            self.assertIn(f"--phase {phase}", command)
            self.assertEqual("--daemon @DAEMON@" in command, phase == "remote")

    def test_registry_counts_get_bodies_but_not_head_and_retains_404_bodies(self):
        with workspace() as (root, _):
            blob = root / "blob"
            blob.write_bytes(b"complete OCI body")
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), subject.Registry)
            server.guard = threading.Lock()
            server.objects = {"sha256:known": (blob, "application/octet-stream")}
            server.rows_path = root / "requests.jsonl"
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                for method, path in [
                    ("HEAD", "/v2/a/blobs/sha256:known"),
                    ("GET", "/v2/a/blobs/sha256:known"),
                    ("GET", "/v2/a/blobs/missing"),
                ]:
                    client = http.client.HTTPConnection(*server.server_address, timeout=2)
                    client.request(method, path)
                    response = client.getresponse()
                    payload = response.read()
                    client.close()
                    if method == "HEAD":
                        self.assertEqual(payload, b"")
                    elif "known" in path:
                        self.assertEqual(payload, blob.read_bytes())
                    else:
                        self.assertEqual(response.status, 404)
                        self.assertIn(b"BLOB_UNKNOWN", payload)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)
            rows = [json.loads(line) for line in server.rows_path.read_text().splitlines()]
            self.assertEqual([r["status"] for r in rows], [200, 200, 404])
            self.assertEqual(rows[0]["response_bytes"], 0)
            self.assertEqual(rows[1]["response_bytes"], len(blob.read_bytes()))
            self.assertEqual(rows[1]["body_sha256"], hashlib.sha256(blob.read_bytes()).hexdigest())
            self.assertGreater(rows[2]["response_bytes"], 0)
