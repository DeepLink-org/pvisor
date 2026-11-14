"""Local HTTP contract tests for the runner; these do not replace VM deployment."""

import base64
import hashlib
import http.cookiejar
import json
import secrets
import tempfile
import threading
import unittest
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from unittest.mock import Mock

from deploy import Deployment


class RunnerMinioBuildContracts(unittest.TestCase):
    def fixture(self, root):
        binary = root / "minio-binary"
        binary.write_bytes(b"test fixture, never executed")
        license_file = root / "LICENSE"
        license_file.write_text("test license")
        manifest = root / "build.json"
        manifest.write_text(
            json.dumps(
                {
                    "schema": 1,
                    "binary_path": str(binary),
                    "license_path": str(license_file),
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "source_tag": "test",
                    "source_url": "https://example.invalid",
                    "source_sha256": "test",
                    "go_mod_sha256": "test",
                    "go_version": "test",
                }
            )
        )
        deployment = Deployment(Path("/not-executed"), root, "minio", minio_manifest=manifest)
        deployment.command = Mock()
        deployment.compose = Mock()
        store = deployment.root / "store"
        store.mkdir()
        (store / "images.json").write_text("[]")
        return deployment, binary

    def test_local_build_pulls_only_the_proxy_and_records_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            deployment, binary = self.fixture(Path(temporary))
            deployment.pull()
            self.assertEqual(
                [call.args for call in deployment.compose.call_args_list],
                [("config", "-q"), ("pull", "nginx")],
            )
            self.assertEqual(
                deployment.command.call_args.args[0][:3],
                ["build", "-t", "pvisor-local-minio:source"],
            )
            self.assertEqual(
                (deployment.root / "minio-image/minio").read_bytes(), binary.read_bytes()
            )
            self.assertEqual(deployment.result["minio_source_build"]["source_tag"], "test")

    def test_changed_binary_is_rejected_before_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            deployment, binary = self.fixture(Path(temporary))
            binary.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                deployment.pull()
            deployment.command.assert_not_called()

    def test_cleanup_rejects_orphaned_volume_data_even_after_successful_down(self):
        with tempfile.TemporaryDirectory() as temporary:
            deployment, _ = self.fixture(Path(temporary))
            deployment.compose.return_value.returncode = 0
            orphan = deployment.root / "store/volumes/orphan"
            orphan.mkdir(parents=True)
            (orphan / "dump.rdb").write_bytes(b"retained test data")
            deployment.cleanup()
            self.assertEqual(deployment.result["cleanup"], "fail")
            self.assertEqual(deployment.result["outcome"], "fail")
            self.assertEqual(deployment.result["remaining_volume_directories"], ["orphan"])
            self.assertTrue(
                (orphan / "dump.rdb").exists(), "cleanup must not blindly delete unknown data"
            )

    def test_cleanup_accepts_empty_container_and_volume_stores(self):
        with tempfile.TemporaryDirectory() as temporary:
            deployment, _ = self.fixture(Path(temporary))
            deployment.compose.return_value.returncode = 0
            deployment.cleanup()
            self.assertEqual(deployment.result["cleanup"], "pass")
            self.assertEqual(deployment.result["remaining_volume_directories"], [])


class RunnerInitializationContracts(unittest.TestCase):
    def deployment(self, records):
        deployment = Deployment.__new__(Deployment)
        deployment.compose = Mock()
        deployment.records = Mock(return_value=records)
        return deployment

    def record(self, service, state="running", exit_code=None):
        return {
            "id": service + "-id",
            "compose": {"service": service},
            "state": state,
            "exit_code": exit_code,
        }

    def test_only_explicit_successful_init_jobs_may_be_exited(self):
        records = [self.record("init", "exited", 0), self.record("app")]
        deployment = self.deployment(records)
        self.assertEqual(deployment.up(completed=("init",)), {"init-id", "app-id"})
        with self.assertRaises(AssertionError):
            deployment.up()

    def test_failed_missing_or_still_running_init_is_not_ready(self):
        for records in (
            [self.record("init", "exited", 1), self.record("app")],
            [self.record("app")],
            [self.record("init"), self.record("app")],
            [self.record("init", "exited", 0), self.record("app", "exited", 0)],
        ):
            with self.subTest(records=records), self.assertRaises(AssertionError):
                self.deployment(records).up(completed=("init",))


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, body, headers=None):
        self.send_response(code)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/cookie":
            self.reply(200, b"ok", {"Set-Cookie": "proof=1; Path=/"})
        elif self.path == "/auth":
            authenticated = (
                self.headers.get("Cookie") == "proof=1"
                or self.headers.get("Authorization") == self.server.authorization
            )
            self.reply(200 if authenticated else 401, b"ok" if authenticated else b"denied")
        elif self.path == "/missing":
            self.reply(404, self.server.secret.encode())
        else:
            self.reply(200, bytes(range(256)))

    def do_POST(self):
        self.reply(200, self.rfile.read(int(self.headers["Content-Length"])))

    def do_PUT(self):
        self.reply(201, self.rfile.read(int(self.headers["Content-Length"])))

    def do_DELETE(self):
        self.reply(204, b"")


class RunnerHTTPContracts(unittest.TestCase):
    def setUp(self):
        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.server.secret = secrets.token_urlsafe(16)
        credentials = f"test-user:{self.server.secret}".encode()
        self.server.authorization = "Basic " + base64.b64encode(credentials).decode()
        self.worker = threading.Thread(target=self.server.serve_forever)
        self.worker.start()
        self.deployment = Deployment.__new__(Deployment)
        self.deployment.base = f"http://127.0.0.1:{self.server.server_port}"
        self.deployment.secret_values = [self.server.secret]
        self.deployment.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
        )

    def tearDown(self):
        self.server.shutdown()
        self.worker.join(timeout=3)
        self.server.server_close()
        self.assertFalse(self.worker.is_alive())

    def test_binary_body_and_explicit_methods_are_not_text_decoded(self):
        payload = bytes(range(256)) * 4
        status, _, received = self.deployment.request("/binary", payload, method="PUT", binary=True)
        self.assertEqual(status, 201)
        self.assertEqual(received, payload)
        self.assertEqual(self.deployment.request("/binary", binary=True)[2], bytes(range(256)))
        self.assertEqual(self.deployment.request("/binary", method="DELETE")[0], 204)

    def test_existing_json_and_form_requests_keep_their_contract(self):
        payload = {"args": [20, 22], "text": "with spaces"}
        self.assertEqual(json.loads(self.deployment.request("/echo", payload)[2]), payload)
        self.assertEqual(
            self.deployment.request("/echo", {"text": "a b"}, form=True)[2], "text=a+b"
        )

    def test_anonymous_checks_do_not_reuse_authenticated_cookies(self):
        self.deployment.request("/cookie")
        self.assertEqual(self.deployment.request("/auth")[0], 200)
        self.assertEqual(
            self.deployment.request("/auth", use_cookies=False, allow_error=True)[0], 401
        )
        self.assertEqual(
            self.deployment.request(
                "/auth", use_cookies=False, auth=("test-user", self.server.secret)
            )[0],
            200,
        )

    def test_expected_errors_are_explicit_and_other_errors_are_redacted(self):
        self.assertEqual(self.deployment.request("/missing", allow_error=True)[0], 404)
        with self.assertRaises(RuntimeError) as caught:
            self.deployment.request("/missing")
        self.assertIn("HTTP 404", str(caught.exception))
        self.assertIn("[REDACTED]", str(caught.exception))
        self.assertNotIn(self.server.secret, str(caught.exception))


if __name__ == "__main__":
    unittest.main()
