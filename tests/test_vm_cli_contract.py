"""Oracles and bounded process supervision without a VM."""

import json
import sys
import unittest
from unittest.mock import patch

from tests import vm_cli_contract as contracts
from tests.fixtures import workspace


class VmCliContractTests(unittest.TestCase):
    def test_matching_exit_without_result_is_not_success(self):
        with workspace() as (directory, _):
            with self.assertRaises(FileNotFoundError):
                contracts.verify(directory, 1, 1729, 0, 0)
            with self.assertRaisesRegex(AssertionError, "guest exit"):
                contracts.verify(directory, 1, 1729, 7, 0)

    def test_result_readonly_and_cleanup_are_all_checked(self):
        with workspace() as (directory, _):
            (directory / "root/work").mkdir(parents=True)
            (directory / "readonly").mkdir()
            (directory / "root/result").write_bytes(b"vm-contract-v1 cpus=2 seed=42 exit=7\n")
            (directory / "readonly/fixture").write_bytes(b"immutable")
            contracts.verify(directory, 2, 42, 7, 7)
            (directory / "readonly/fixture").write_bytes(b"changed")
            with self.assertRaisesRegex(AssertionError, "read-only"):
                contracts.verify(directory, 2, 42, 7, 7)
            (directory / "readonly/fixture").write_bytes(b"immutable")
            (directory / "root/work/leaked").touch()
            with self.assertRaisesRegex(AssertionError, "unexpected entries"):
                contracts.verify(directory, 2, 42, 7, 7)

    def test_timeout_fails_and_reaps_child(self):
        with workspace() as (directory, _):
            with self.assertRaisesRegex(RuntimeError, "deadline exceeded"):
                contracts.invoke(
                    [sys.executable, "-c", "import time; time.sleep(60)"], directory, 0.1
                )

    def test_existing_evidence_is_preserved_and_workload_never_starts(self):
        with workspace() as (directory, _):
            guest = directory / "guest"
            guest.write_bytes(b"fixture")
            output = directory / "evidence"
            output.mkdir()
            receipt = output / "previous-result"
            receipt.write_bytes(b"retained evidence")
            arguments = [
                "vm_cli_contract",
                "--binary",
                sys.executable,
                "--guest",
                str(guest),
                "--output",
                str(output),
            ]
            with patch.object(sys, "argv", arguments), patch.object(contracts, "invoke") as invoke:
                with self.assertRaises(FileExistsError):
                    contracts.main()
            invoke.assert_not_called()
            self.assertEqual(receipt.read_bytes(), b"retained evidence")
            self.assertEqual(list(output.iterdir()), [receipt])

    def test_failure_retains_report_and_command(self):
        with workspace() as (directory, _):
            guest = directory / "guest"
            guest.write_bytes(b"fixture")
            output = directory / "evidence"
            arguments = [
                "vm_cli_contract",
                "--binary",
                sys.executable,
                "--guest",
                str(guest),
                "--output",
                str(output),
                "--rounds",
                "1",
            ]
            with (
                patch.object(sys, "argv", arguments),
                patch.object(contracts, "invoke", return_value=(0, 0.01)),
            ):
                with self.assertRaises(FileNotFoundError):
                    contracts.main()
            report = json.loads((output / "report.json").read_text())
            self.assertFalse(report["passed"])
            self.assertFalse(report["cases"][0]["passed"])
            self.assertEqual(report["cases"][0]["command"][1], "vm-run")
