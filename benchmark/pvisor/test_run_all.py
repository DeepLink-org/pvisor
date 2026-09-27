#!/usr/bin/env python3
"""Checks for the one-command benchmark setup and partial-result report."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import run_all


class OneClickBenchmarkTests(unittest.TestCase):
    def test_prepared_rootfs_contains_only_selected_programs_and_libraries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            rootfs = Path(directory)
            copied = run_all.prepare_rootfs(rootfs, Path("/bin/true"))
            self.assertIn("/bin/sh", copied)
            self.assertTrue((rootfs / "bin/sh").is_file())
            self.assertTrue((rootfs / "usr/bin/sleep").is_file())
            self.assertTrue((rootfs / "opt/persisting/pvisor").is_file())
            self.assertFalse((rootfs / "etc/shadow").exists())

    def test_unavailable_vm_is_reported_while_direct_result_is_kept(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "report"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(Path(run_all.__file__)),
                    "--no-build",
                    "--pvisor",
                    "/bin/true",
                    "--output",
                    str(output),
                    "--cases",
                    "direct,vm",
                    "--warmups",
                    "0",
                    "--samples",
                    "1",
                    "--resource-hold-ms",
                    "50",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads((output / "startup.json").read_text())
            self.assertIn("direct", report["cases"])
            self.assertIn("vm", report["skipped_cases"])
            self.assertIn("Skipped cases", (output / "startup.md").read_text())


if __name__ == "__main__":
    unittest.main()
