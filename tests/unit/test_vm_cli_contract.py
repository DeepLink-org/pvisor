"""VM driver arguments, evidence and result oracles, without a VM or child process."""

import io
import json
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

from tests import vm_cli_contract as contracts
from tests.fixtures import workspace


class VmCliContractTests(unittest.TestCase):
    def setUp(self):
        # platform.platform() may spawn `uname -p` on a cold interpreter.
        # Host metadata is not part of these argument/evidence contracts.
        self.enterContext(
            patch.object(contracts.platform, "platform", return_value="unit-platform")
        )

    def test_invalid_parameters_fail_before_launch_or_evidence_creation(self):
        for options in [
            ["--rounds", "0"],
            ["--rounds", "-1"],
            ["--timeout", "0"],
            ["--timeout", "-1"],
            ["--timeout", "nan"],
            ["--timeout", "inf"],
            ["--timeout=-inf"],
            ["--seed", "-1"],
            ["--seed", str(2**64)],
            ["--seed", str(2**64 - 1), "--rounds", "2"],
        ]:
            with self.subTest(options=options), workspace() as (directory, _):
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
                    *options,
                ]
                with (
                    patch.object(sys, "argv", arguments),
                    patch.object(contracts, "invoke") as invoke,
                    redirect_stderr(io.StringIO()),
                ):
                    with self.assertRaises(SystemExit) as error:
                        contracts.main()
                self.assertEqual(error.exception.code, 2)
                invoke.assert_not_called()
                self.assertFalse(output.exists())

    def test_uint64_seed_endpoints_are_forwarded_without_overflow(self):
        for seed, rounds in [(0, 1), (2**64 - 1, 1), (2**64 - 2, 2)]:
            with self.subTest(seed=seed, rounds=rounds), workspace() as (directory, _):
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
                    str(rounds),
                    "--seed",
                    str(seed),
                ]

                # Supply only the guest's file protocol; this checks the driver's
                # iteration/argv/oracle path, not real virtualization.
                def guest_result(command, case, timeout):
                    cpus, actual_seed, code = map(int, command[-3:])
                    (case / "root/work").mkdir()
                    (case / "root/result").write_bytes(
                        f"vm-contract-v1 cpus={cpus} seed={actual_seed} exit={code}\n".encode()
                    )
                    return code, 0.01

                with (
                    patch.object(sys, "argv", arguments),
                    patch.object(contracts, "invoke", side_effect=guest_result) as invoke,
                    redirect_stdout(io.StringIO()),
                ):
                    contracts.main()
                report = json.loads((output / "report.json").read_text())
                self.assertTrue(report["passed"])
                self.assertEqual(invoke.call_count, 4 * rounds)
                self.assertEqual(
                    {case["seed"] for case in report["cases"]}, set(range(seed, seed + rounds))
                )

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
