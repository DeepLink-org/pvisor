"""Real-process checks of the owned HTTP fixture, not product acceptance."""

import http.client
import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from urllib.parse import urlsplit


class RepositoryFixtureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="lazy-repository-fixture-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.objects = self.root / "objects"
        self.objects.mkdir()
        (self.objects / "data").write_bytes(b"0123456789\x00\xff")
        (self.objects / "empty").write_bytes(b"")
        (self.root / "outside").write_bytes(b"private")
        (self.objects / "escape").symlink_to(self.root / "outside")
        self.ready = self.root / "ready.json"
        self.requests = self.root / "requests.jsonl"
        self.deny = self.root / "deny"
        errors = (self.root / "stderr").open("wb")
        self.addCleanup(errors.close)
        self.process = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name("repository.py")),
                str(self.objects),
                "--ready",
                str(self.ready),
                "--requests",
                str(self.requests),
                "--deny",
                str(self.deny),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=errors,
        )
        self.addCleanup(self.stop)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if self.ready.exists():
                try:
                    self.url = urlsplit(json.loads(self.ready.read_text())["url"])
                    return
                except json.JSONDecodeError:
                    pass
            self.assertIsNone(self.process.poll(), (self.root / "stderr").read_text())
            time.sleep(0.01)
        self.fail("fixture readiness deadline")

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)

    def get(self, path, range_header=None):
        connection = http.client.HTTPConnection(self.url.hostname, self.url.port, timeout=2)
        try:
            connection.request("GET", path, headers={"Range": range_header} if range_header else {})
            response = connection.getresponse()
            body = response.read()
            self.assertEqual(int(response.getheader("Content-Length")), len(body))
            return response.status, response.getheader("Content-Range"), body
        finally:
            connection.close()

    def test_whole_and_boundary_ranges_have_exact_bytes_and_lengths(self):
        self.assertEqual(self.get("/data"), (200, None, b"0123456789\x00\xff"))
        for requested, wanted_range, wanted_body in [
            ("bytes=0-0", "bytes 0-0/12", b"0"),
            ("bytes=10-11", "bytes 10-11/12", b"\x00\xff"),
            ("bytes=10-99", "bytes 10-11/12", b"\x00\xff"),
            ("bytes=11-", "bytes 11-11/12", b"\xff"),
        ]:
            with self.subTest(requested=requested):
                self.assertEqual(self.get("/data", requested), (206, wanted_range, wanted_body))
        rows = [json.loads(line) for line in self.requests.read_text().splitlines()]
        self.assertEqual(len(rows), 5)
        self.assertEqual(rows[-1], {"path": "/data", "range": "bytes=11-"})

    def test_invalid_and_empty_ranges_fail_without_successful_bytes(self):
        for value in ("bytes=12-13", "bytes=4-3", "bytes=-2", "bytes=0-1,3-4", "items=0-1"):
            with self.subTest(value=value):
                self.assertEqual(self.get("/data", value), (416, "bytes */12", b""))
        self.assertEqual(self.get("/empty"), (200, None, b""))
        self.assertEqual(self.get("/empty", "bytes=0-0"), (416, "bytes */0", b""))

    def test_missing_escape_and_faults_are_errors_and_recovery_is_exact(self):
        self.assertEqual(self.get("/absent"), (404, None, b""))
        for path in ("/../outside", "/%2e%2e/outside", "/escape"):
            with self.subTest(path=path):
                self.assertEqual(self.get(path), (403, None, b""))
        self.deny.touch()
        self.assertEqual(self.get("/data", "bytes=0-0"), (503, None, b""))
        self.deny.unlink()
        self.assertEqual(self.get("/data", "bytes=0-0"), (206, "bytes 0-0/12", b"0"))


if __name__ == "__main__":
    unittest.main()
