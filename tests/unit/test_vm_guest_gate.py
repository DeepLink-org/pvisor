"""Paired confirmation for the VM guest regression gate."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci/vm_guest_gate.py"


class VmGuestGateTests(unittest.TestCase):
    def run_gate(
        self, status, *, kind="guest", paired=1.68, lower=None, paired_passed=True, write=True
    ):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        comparison = root / "comparison.json"
        comparison.write_text(
            json.dumps(
                {
                    "passed": False,
                    "threshold_pct": 15,
                    "rows": [
                        {
                            "condition": "linux-tmpfs-shuffled-overwrite",
                            "passed": False,
                            "change_pct": 27.25,
                        }
                    ],
                }
            )
        )
        (root / "compare.status").write_text(f"{status}\n")
        summary = root / "paired-diagnostic/linux-tmpfs-shuffled-overwrite/summary.json"
        if write:
            summary.parent.mkdir(parents=True)
            summary.write_text(
                json.dumps(
                    {
                        "condition": "linux-tmpfs-shuffled-overwrite",
                        "blocks": 16,
                        "median_paired_change_pct": paired,
                        "lower_paired_change_pct": paired if lower is None else lower,
                        "paired_passed": paired_passed,
                    }
                )
            )
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                str(SCRIPT),
                "--kind",
                kind,
                "--status-file",
                str(root / "compare.status"),
                "--comparison",
                str(comparison),
                "--paired",
                str(root / "paired-diagnostic"),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        return result, comparison

    def test_passing_sequential_gate_does_not_need_paired_data(self):
        result, _ = self.run_gate(0, write=False)
        self.assertEqual(result.returncode, 0)

    def test_paired_replay_confirms_a_sequential_outlier(self):
        result, comparison = self.run_gate(1)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(comparison.read_text())
        self.assertTrue(report["gate_passed"])
        self.assertTrue(report["rows"][0]["paired_confirmed"])
        self.assertEqual(report["paired_confirmation"]["blocks"], 16)
        sequential = json.loads(comparison.with_suffix(".sequential.json").read_text())
        self.assertNotIn("gate_passed", sequential)
        self.assertNotIn("paired_confirmed", sequential["rows"][0])

    def test_lower_bound_confirms_a_noisy_median(self):
        result, comparison = self.run_gate(1, paired=30.0, lower=7.0, paired_passed=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(comparison.read_text())
        self.assertTrue(report["paired_confirmation"]["lower_bounds"])

    def test_paired_replay_rejects_a_real_regression(self):
        result, comparison = self.run_gate(1, paired=20.0, paired_passed=False)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("gate_passed", json.loads(comparison.read_text()))

    def test_missing_paired_summary_fails_closed(self):
        result, _ = self.run_gate(1, write=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("invalid paired confirmation", result.stderr)

    def test_startup_failure_has_no_paired_confirmation(self):
        result, _ = self.run_gate(1, kind="startup")
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()
