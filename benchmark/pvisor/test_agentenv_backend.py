"""Failure/identity/lifecycle contracts for the optional AgentENV adapter."""

import copy
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

import agentenv_backend as backend
import agentenv_guest as guest


def payload():
    return dict(
        schema=1,
        correctness="passed",
        inputs_verified=True,
        input_manifest_sha256="a" * 64,
        guest_cpus=2,
        calls=[
            dict(tool=tool, call=call, operation_ms=1.0, child_ms=2.0, correctness="passed")
            for call in (1, 2)
            for tool in guest.TOOLS
        ],
    )


class Sandbox:
    sandbox_id = "owned-test-id"

    def __init__(self, value=None, exit_code=0, execute_error=None, cleanup_error=None):
        self.value = payload() if value is None else value
        self.exit_code = exit_code
        self.execute_error = execute_error
        self.cleanup_error = cleanup_error
        self.killed = 0
        self.commands = SimpleNamespace(run=self.run)

    def run(self, command, **kwargs):
        if self.execute_error:
            raise self.execute_error
        return SimpleNamespace(
            stdout=guest.PREFIX + json.dumps(self.value) + "\n",
            stderr="guest log",
            exit_code=self.exit_code,
        )

    def kill(self, **kwargs):
        self.killed += 1
        if self.cleanup_error:
            raise self.cleanup_error
        return True


class AgentenvTests(unittest.TestCase):
    def run_fake(self, sandbox):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "run"
            row = backend.run_trial(lambda **kwargs: sandbox, "fixed-template", output, "a" * 64)
            self.assertEqual(json.loads((output / "report.json").read_text()), row)
            csv_exists = (output / "per-call.csv").exists()
            return row, csv_exists

    def test_complete_guest_result_and_owned_cleanup(self):
        sandbox = Sandbox()
        row, csv_exists = self.run_fake(sandbox)
        self.assertEqual(row["state"], "passed")
        self.assertTrue(csv_exists)
        self.assertEqual(sandbox.killed, 1)
        self.assertFalse(row["formal_comparison_eligible"])

    def test_nonzero_exit_has_no_success_csv(self):
        sandbox = Sandbox(exit_code=1)
        row, csv_exists = self.run_fake(sandbox)
        self.assertEqual(row["state"], "failed")
        self.assertFalse(csv_exists)
        self.assertEqual(sandbox.killed, 1)

    def test_timeout_still_cleans_up_without_leaking_exception_body(self):
        sandbox = Sandbox(execute_error=TimeoutError("secret-token"))
        row, csv_exists = self.run_fake(sandbox)
        self.assertEqual(row["state"], "failed")
        self.assertEqual(sandbox.killed, 1)
        self.assertFalse(csv_exists)
        self.assertNotIn("secret-token", json.dumps(row))

    def test_cleanup_failure_invalidates_success(self):
        row, csv_exists = self.run_fake(Sandbox(cleanup_error=RuntimeError("secret")))
        self.assertEqual(row["state"], "failed")
        self.assertFalse(csv_exists)
        self.assertFalse(row["destroy_acknowledged"])

    def test_unknown_create_retains_reconciliation_token(self):
        def factory(**kwargs):
            raise TimeoutError("secret-token")

        with tempfile.TemporaryDirectory() as directory:
            row = backend.run_trial(factory, "template", Path(directory) / "run", "a" * 64)
            self.assertTrue(row["creation_state_unknown"])
            self.assertEqual(row["reconcile_metadata_token"], row["run_token"])
            self.assertNotIn("secret-token", json.dumps(row))

    def test_incorrect_cpu_count_rejected(self):
        value = payload()
        value["guest_cpus"] = 4
        row, _ = self.run_fake(Sandbox(value))
        self.assertEqual(row["state"], "failed")

    def test_missing_duplicate_and_reordered_calls_rejected(self):
        original = payload()
        for rows in (
            original["calls"][:-1],
            original["calls"] + original["calls"][:1],
            list(reversed(original["calls"])),
        ):
            with self.subTest(rows=rows):
                value = dict(original, calls=rows)
                with self.assertRaises(ValueError):
                    backend.validate_result(guest.PREFIX + json.dumps(value), "a" * 64, 2)

    def test_nan_negative_and_inverted_timing_rejected(self):
        for operation, child in ((float("nan"), 2), (1, float("inf")), (-1, 2), (3, 2), (True, 2)):
            value = copy.deepcopy(payload())
            value["calls"][0].update(operation_ms=operation, child_ms=child)
            with self.subTest(operation=operation, child=child), self.assertRaises(ValueError):
                backend.validate_result(guest.PREFIX + json.dumps(value), "a" * 64, 2)

    def test_wrong_manifest_rejected(self):
        with self.assertRaises(ValueError):
            backend.validate_result(guest.PREFIX + json.dumps(payload()), "b" * 64, 2)

    def test_duplicate_final_result_rejected(self):
        line = guest.PREFIX + json.dumps(payload()) + "\n"
        with self.assertRaises(ValueError):
            backend.validate_result(line + line, "a" * 64, 2)

    def test_endpoint_does_not_send_plaintext_key_off_host(self):
        for url in (
            "http://example.com",
            "http://user:secret@localhost",
            "https://example.com/?token=secret",
            "file:///tmp/a",
        ):
            with self.subTest(url=url), self.assertRaises(ValueError):
                backend.validate_endpoint(url)
        self.assertEqual(
            backend.validate_endpoint("http://127.0.0.1:18080/"), "http://127.0.0.1:18080"
        )

    def test_generated_symlink_is_not_followed_or_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            target.mkdir()
            (target / "keep").write_text("original")
            link = Path(directory) / "written"
            link.symlink_to(target, target_is_directory=True)
            with self.assertRaises(ValueError):
                guest.remove_generated(link)
            self.assertEqual((target / "keep").read_text(), "original")

    def test_guest_manifest_detects_byte_and_mode_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "file"
            path.write_bytes(b"original")
            path.chmod(0o640)
            manifest = {"file": dict(mode=0o640, sha256=hashlib.sha256(b"original").hexdigest())}
            guest.verify_inputs(manifest, root)
            path.chmod(0o600)
            with self.assertRaises(ValueError):
                guest.verify_inputs(manifest, root)
            path.chmod(0o640)
            path.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                guest.verify_inputs(manifest, root)

    def test_manifest_rejects_escape(self):
        for name in ("/etc/passwd", "../outside"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                guest.verify_inputs({name: {}}, Path("/tmp"))

    def test_existing_output_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "keep").write_text("original")
            with self.assertRaises(FileExistsError):
                backend.run_trial(lambda **kwargs: Sandbox(), "template", output, "a" * 64)
            self.assertEqual((output / "keep").read_text(), "original")


if __name__ == "__main__":
    unittest.main()
