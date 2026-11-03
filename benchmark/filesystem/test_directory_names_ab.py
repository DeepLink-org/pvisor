"""Offline contracts for the sequential SDK sampler; no VM required."""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "directory_names_ab", Path(__file__).with_name("directory_names_ab.py")
)
SUBJECT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECT)


class DirectoryNamesContracts(unittest.TestCase):
    def test_macos_does_not_claim_linux_host_limits(self):
        with patch.object(SUBJECT.sys, "platform", "darwin"):
            self.assertEqual(SUBJECT.host_cpus(), [])
            self.assertEqual(SUBJECT.firmware_name(), "libkrunfw.5.dylib")
            self.assertEqual(SUBJECT.host_budget(["driver"], "unit", []), ["driver"])
            with patch.object(SUBJECT.subprocess, "run") as run:
                SUBJECT.stop_trial("unit")
                run.assert_not_called()

    def test_linux_keeps_existing_budget_and_firmware(self):
        with patch.object(SUBJECT.sys, "platform", "linux"):
            self.assertEqual(SUBJECT.firmware_name(), "libkrunfw.so.5")
            command = SUBJECT.host_budget(["driver"], "unit", [2, 3])
            self.assertEqual(command[0], "systemd-run")
            self.assertEqual(command[-4:], ["taskset", "-c", "2,3", "driver"])
            self.assertIn("MemorySwapMax=0", command)

    def test_copyup_verification_requires_exactly_one_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, "missing or ambiguous"):
                SUBJECT.verify_copyup_fixture(root)
            for name in ("one", "two"):
                path = root / name / "stable-lower"
                path.mkdir(parents=True)
                (path / "copy-1").touch()
            with self.assertRaisesRegex(ValueError, "missing or ambiguous"):
                SUBJECT.verify_copyup_fixture(root)

    def test_copyup_verification_checks_every_chunk_and_exact_lengths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = root / "fixture"
            for name in ("stable-lower", "stable-upper"):
                path = fixture / name
                path.mkdir(parents=True)
                for round_ in (1, 2, 3):
                    with (path / f"copy-{round_}").open("wb") as file:
                        file.truncate(32 << 20)
            with patch.object(SUBJECT, "copyup_expected_block", return_value=bytes(1 << 20)):
                SUBJECT.verify_copyup_fixture(root)
                last = fixture / "stable-upper/copy-3"
                with last.open("r+b") as file:
                    file.seek((32 << 20) - 1)
                    file.write(b"x")
                with self.assertRaisesRegex(ValueError, "byte mismatch"):
                    SUBJECT.verify_copyup_fixture(root)
                with last.open("r+b") as file:
                    file.truncate((32 << 20) + 1)
                with self.assertRaisesRegex(ValueError, "size"):
                    SUBJECT.verify_copyup_fixture(root)

    def test_failed_trial_retains_owned_workspace_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "work").mkdir()
            manifest = {"experiment": "directory-names", "cpus": [0, 1]}
            result = type("Result", (), {"returncode": 1})()
            with patch.object(SUBJECT.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(ValueError, "trial failed"):
                    SUBJECT.trial(root, manifest, 0, "directory-pages", 0)
            scratch = root / "work/directory-pages-0-0-time"
            self.assertTrue(scratch.is_dir())
            command = SUBJECT.json.loads(
                (root / "directory-pages-0-0-time.command.json").read_text()
            )
            self.assertIn(f"TMPDIR={scratch}", command)
            if SUBJECT.sys.platform == "linux":
                self.assertIn("MemorySwapMax=0", command)
            else:
                self.assertNotIn("systemd-run", command)
            self.assertIn("PVISOR_VM_FS_DIRECTORY_NAMES_CACHE=0", command)
            self.assertIn("PVISOR_VM_FS_PREPARED_COPY_UP=0", command)


if __name__ == "__main__":
    unittest.main()
