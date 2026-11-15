"""Bounded evidence and native console contracts without booting a VM."""

import os
import sys
import unittest

from tests import windows_container_contract as contracts
from tests.fixtures import workspace


class WindowsContainerContractTests(unittest.TestCase):
    def test_cleanup_or_a_failed_command_cannot_be_reported_as_a_pass(self):
        good = {"passed": True, "commands": [{"passed": True}]}
        contracts.verify_report(good)
        for bad in [
            dict(good, passed=False),
            dict(good, commands=[]),
            dict(good, commands=[{"passed": False}]),
            dict(good, cleanup_errors=["owner did not release its lease"]),
        ]:
            with self.subTest(bad=bad), self.assertRaises(AssertionError):
                contracts.verify_report(bad)

    def test_diagnostics_are_bounded_and_do_not_copy_disks_or_blobs(self):
        with workspace() as (directory, _):
            store, output = directory / "store", directory / "evidence"
            instance = store / "instances" / ("a" * 32)
            instance.mkdir(parents=True)
            (instance / "record.json").write_bytes(b'{"phase":"starting"}')
            (instance / "boot.stderr").write_bytes(b"prefix-" + b"failure" * 100)
            (instance / "root.ext4").write_bytes(b"raw disk must not be copied")
            (store / "blobs").mkdir()
            (store / "blobs/payload").write_bytes(b"image bytes")
            items = contracts.snapshot_store(store, output, limit=32)
            self.assertEqual(len(items), 2)
            for item in items:
                path = output / item["instance"] / item["file"]
                self.assertEqual(path.read_bytes(), (instance / item["file"]).read_bytes()[-32:])
                self.assertLessEqual(path.stat().st_size, 32)
            truncated = next(item for item in items if item["file"] == "boot.stderr")
            self.assertGreater(truncated["offset"], 0)
            self.assertFalse((output / instance.name / "root.ext4").exists())
            self.assertFalse(any(path.name == "root.ext4" for path in output.rglob("*")))
            self.assertFalse((output / "blobs").exists())
            self.assertEqual((instance / "root.ext4").read_bytes(), b"raw disk must not be copied")

    def test_missing_stores_and_invalid_limits_do_not_create_evidence(self):
        with workspace() as (directory, _):
            output = directory / "evidence"
            self.assertEqual(contracts.snapshot_store(directory / "missing", output), [])
            self.assertFalse(output.exists())
            with self.assertRaises(ValueError):
                contracts.snapshot_store(directory / "missing", output, limit=0)

    @unittest.skipUnless(os.name == "nt", "native ConPTY requires Windows")
    def test_conpty_attaches_console_handles_and_preserves_child_exit(self):
        from tests.windows_pty import Pty

        probe = (
            "import sys; "
            "assert sys.stdin.isatty() and sys.stdout.isatty() and sys.stderr.isatty(); "
            "print('CONSOLE-READY',flush=True); "
            "line=sys.stdin.readline().strip(); "
            "assert line=='literal input',repr(line); "
            "print('CONSOLE-DONE',flush=True); sys.exit(7)"
        )
        with Pty([sys.executable, "-c", probe], dict(os.environ)) as console:
            console.expect(b"CONSOLE-READY", timeout=15)
            console.send(b"literal input\r")
            console.expect(b"CONSOLE-DONE", timeout=15)
            self.assertEqual(console.wait(timeout=10), 7)
