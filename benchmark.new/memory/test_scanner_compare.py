"""Independent checks for rejecting incomplete memory comparison evidence."""

import copy
import importlib.util
import json
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "scanner_compare", Path(__file__).with_name("scanner_compare.py")
)
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


def cohort():
    rows = []
    for r in range(5):
        for pattern in subject.PATTERNS:
            for arm, pss in [("off", 200), ("ksm", 170), ("adaptive", 160)]:
                rows.append(
                    dict(
                        round=r,
                        pattern=pattern,
                        arm=arm,
                        status="successful",
                        metrics={
                            phase: dict(pss_mib=pss + r) for phase in ("ready", "cow25", "cow100")
                        },
                    )
                )
    return dict(complete=True, preflight=False, samples=5, arms=subject.ARMS, attempts=rows)


class ComparisonTests(unittest.TestCase):
    def test_prepare_quotes_paths_and_refuses_to_overwrite_evidence(self):
        with tempfile.TemporaryDirectory(prefix="pv-scan-") as temporary:
            root = Path(temporary)
            command = [
                sys.executable,
                subject.__file__,
                "--prepare",
                "--build",
                str(root / "build space"),
                "--rootfs",
                str(root / "root$(false)"),
                "--firmware",
                str(root / "fw"),
                "--output",
                str(root / "f"),
            ]
            completed = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            path = root / "f.input.json"
            before = path.read_bytes()
            expanded = shlex.split(json.loads(before)["paired-memory"][2:])
            self.assertEqual(expanded[expanded.index("--rootfs") + 1], str(root / "root$(false)"))
            self.assertEqual(expanded[expanded.index("--build") + 1], str(root / "build space"))
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(path.read_bytes(), before)

    def test_disabled_or_tuned_ksm_is_not_the_default_parameter_comparison(self):
        files = dict(
            run="1",
            pages_to_scan="100",
            sleep_millisecs="20",
            smart_scan="1",
            merge_across_nodes="1",
            use_zero_pages="0",
            max_page_sharing="256",
            advisor_mode="[none] scan-time",
        )
        subject.require_default_ksm(dict(files=files))
        for key, value in [
            ("run", "0"),
            ("pages_to_scan", "1000"),
            ("smart_scan", "0"),
            ("advisor_mode", "none [scan-time]"),
        ]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                subject.require_default_ksm(dict(files=files | {key: value}))

    def test_paired_saving_direction_and_exact_constant_interval(self):
        result = subject.analyze(cohort())
        pair = next(
            x
            for x in result["comparisons"]
            if x["pattern"] == "repeated"
            and x["phase"] == "ready"
            and x["arm"] == "adaptive"
            and x["reference"] == "ksm"
        )
        self.assertEqual(pair["saved_mib_median"], 10)
        self.assertEqual(pair["ci95"], [10, 10])
        self.assertTrue(pair["detected"])

    def test_missing_failed_and_duplicate_samples_are_rejected(self):
        for kind in ("missing", "failed", "duplicate", "incomplete"):
            with self.subTest(kind=kind):
                report = copy.deepcopy(cohort())
                if kind == "missing":
                    report["attempts"].pop()
                if kind == "failed":
                    report["attempts"][0]["status"] = "failed"
                if kind == "duplicate":
                    report["attempts"][-1] = report["attempts"][0]
                if kind == "incomplete":
                    report["complete"] = False
                with self.assertRaises(ValueError):
                    subject.analyze(report)

    def test_interval_crossing_zero_is_not_a_detected_difference(self):
        interval = subject.paired_interval([-20, -10, 0, 10, 20])
        self.assertLessEqual(interval[0], 0)
        self.assertGreaterEqual(interval[1], 0)


if __name__ == "__main__":
    unittest.main()
