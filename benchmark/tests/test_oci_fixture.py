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
import oci_fixture as subject  # noqa: E402


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

    def test_json_entries_locate_the_rust_criterion_implementation(self):
        directory = Path(subject.__file__).parent
        for phase in ["storage", "runtime", "remote"]:
            with self.subTest(phase=phase):
                locator = json.loads((directory / f"lazy_{phase}.json").read_text())
                self.assertEqual(
                    locator,
                    {
                        "engine": "criterion",
                        "entry": f"storage/lazy_{phase}.json",
                        "implementation": "../criterion/src/storage/mod.rs",
                        "config_env": "PVISOR_STORAGE_CONFIG",
                    },
                )
                implementation = (directory / locator["implementation"]).resolve()
                self.assertTrue(implementation.is_file())
                self.assertEqual(implementation.suffix, ".rs")
                self.assertIn("pub fn register(c: &mut Criterion)", implementation.read_text())

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
            responses = {}
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
                    responses[method, path] = payload
                    if method == "HEAD":
                        self.assertEqual(response.status, 200)
                        self.assertEqual(payload, b"")
                    elif "known" in path:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(payload, blob.read_bytes())
                    else:
                        self.assertEqual(response.status, 404)
                        self.assertIn(b"BLOB_UNKNOWN", payload)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)
            rows = [json.loads(line) for line in server.rows_path.read_text().splitlines()]
            self.assertCountEqual(
                [(row["method"], row["path"], row["status"]) for row in rows],
                [
                    ("HEAD", "/v2/a/blobs/sha256:known", 200),
                    ("GET", "/v2/a/blobs/sha256:known", 200),
                    ("GET", "/v2/a/blobs/missing", 404),
                ],
            )
            # Handler completion can reorder evidence even for sequential clients.
            # Match every row to its actual request, not its scheduling position.
            for row in rows:
                payload = responses[row["method"], row["path"]]
                self.assertEqual(row["response_bytes"], len(payload))
                self.assertEqual(
                    row["body_sha256"],
                    hashlib.sha256(payload).hexdigest() if row["method"] == "GET" else None,
                )
            missing = next(row for row in rows if row["path"].endswith("/missing"))
            self.assertGreater(missing["response_bytes"], 0)
