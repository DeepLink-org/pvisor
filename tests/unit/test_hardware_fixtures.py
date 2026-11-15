"""Hardware fixture admission and response contracts, without running probes."""

import os
import subprocess
import unittest
from unittest import mock

from tests.fixtures import workspace
from tests.hardware import support

MODULES = [
    "tests.hardware.hvf_cold_restore",
    "tests.hardware.vm_snapshot_state",
    "tests.hardware.linux_cold_restore",
    "tests.hardware.environment_snapshot",
]


class HardwareFixtureTests(unittest.TestCase):
    def test_explicit_hardware_selection_fails_instead_of_skipping_missing_capabilities(self):
        for module in MODULES:
            with self.subTest(module=module):
                suite = unittest.TestLoader().loadTestsFromName(module)
                self.assertEqual(suite.countTestCases(), 1)
                result = unittest.TestResult()
                with (
                    mock.patch.object(
                        support, "require_hvf", side_effect=RuntimeError("HVF unavailable")
                    ),
                    mock.patch.object(support, "binary") as binary,
                ):
                    suite.run(result)
                self.assertFalse(result.wasSuccessful())
                self.assertEqual(len(result.errors), 1)
                self.assertEqual(result.skipped, [])
                binary.assert_not_called()

    def test_hardware_prerequisites_and_artifacts_are_explicit(self):
        with mock.patch.object(support.platform, "system", return_value="Linux"):
            with self.assertRaisesRegex(RuntimeError, "Apple Silicon"):
                support.require_hvf()
        with workspace() as (directory, resources):
            firmware = directory / "firmware"
            firmware.mkdir()
            guest = directory / "debug/probes/guest_linux"
            guest.parent.mkdir(parents=True)
            guest.write_bytes(b"guest")
            resources.enter_context(
                mock.patch.dict(
                    os.environ,
                    {
                        "CARGO_TARGET_DIR": str(directory),
                        "PVISOR_CASE_VM_LIBRARY_DIR": str(firmware),
                        "PVISOR_VM_PROBE_BINARY": str(directory / "missing"),
                    },
                )
            )
            with self.assertRaises(FileNotFoundError):
                support.binary()
            with self.assertRaisesRegex(RuntimeError, "firmware directory"):
                support.guest_and_firmware()
            (firmware / "libkrunfw.5.dylib").write_bytes(b"firmware")
            self.assertEqual(support.guest_and_firmware(), (guest.resolve(), firmware.resolve()))

    def test_probe_exit_and_error_content_are_both_asserted(self):
        support.assert_response(self, subprocess.CompletedProcess([], 0, "ok", ""))
        support.assert_response(
            self, subprocess.CompletedProcess([], 1, "", "bad schema"), "schema"
        )
        for result, error in [
            (subprocess.CompletedProcess([], 1, "", "unexpected"), None),
            (subprocess.CompletedProcess([], 0, "", "schema"), "schema"),
            (subprocess.CompletedProcess([], 1, "", "unrelated"), "schema"),
            (subprocess.CompletedProcess([], 0, "", "panicked"), None),
        ]:
            with self.subTest(result=result, error=error), self.assertRaises(AssertionError):
                support.assert_response(self, result, error)
