"""Contracts for the shared experiment runtime, independent of product daemons."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

_spec = importlib.util.spec_from_file_location(
    "semspec_runtime", Path(__file__).with_name("runtime.py")
)
RUNTIME = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(RUNTIME)


class RuntimeContracts(unittest.TestCase):
    def test_counts_are_validated_before_side_effects_and_warmups_are_not_samples(self):
        calls = []
        self.assertEqual(
            RUNTIME.repeat(2, 3, lambda *args: calls.append(args) or len(calls)), [3, 4, 5]
        )
        self.assertEqual(
            calls, [("warmup", 0), ("warmup", 1), ("sample", 0), ("sample", 1), ("sample", 2)]
        )
        for warmup, runs in [(-1, 2), (0, 1), (True, 3), (0, 2.0)]:
            with self.assertRaises(ValueError):
                RUNTIME.repeat(warmup, runs, lambda *args: self.fail("invalid plan executed"))

    def test_ready_and_exit_share_statistics_without_changing_labels(self):
        self.assertEqual(
            RUNTIME.summarize([1_000_000, 3_000_000]),
            dict(zip(RUNTIME.FIELDS, (2, 2, 2, 2**0.5, 1, 3))),
        )
        pairs = RUNTIME.summarize_pairs([dict(ready_ns=1, exit_ns=3), dict(ready_ns=2, exit_ns=4)])
        self.assertEqual(pairs["N"], 2)
        self.assertGreater(pairs["Exit mean ms"], pairs["Ready mean ms"])
        for samples in [[True, 2], [0, 1], [1], [-1, 2]]:
            with self.assertRaises(ValueError):
                RUNTIME.summarize(samples)

    def test_cleanup_runs_even_after_failed_prepare_subject_or_conclusion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for failed in ["prepare", "subject", "conclude", "validate"]:
                with self.subTest(failed=failed):
                    config = dict(
                        argv=["subject"],
                        cwd=temporary,
                        timeout=1,
                        prepare=["prepare"],
                        conclude=["conclude"],
                        cleanup=["cleanup"],
                    )
                    calls = []

                    def execute(argv, **kwargs):
                        calls.append(argv[0])
                        return subprocess.CompletedProcess(
                            argv, int(argv[0] == failed), "out", "err"
                        )

                    def validate(record):
                        if failed == "validate":
                            raise ValueError("incorrect output")

                    with patch.object(RUNTIME.subprocess, "run", side_effect=execute):
                        with self.assertRaises(ValueError):
                            RUNTIME.command_trial(
                                root / "trial.json", "row", "warmup", 0, config, validate
                            )
                    self.assertEqual(calls[-1], "cleanup")
                    record = json.loads((root / "trial.json").read_text())
                    self.assertIn("error", record)
                    self.assertEqual(record["hooks"][-1]["action"], "cleanup")

    def test_cleanup_failure_cannot_erase_primary_failure_or_publish_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "trial.json"
            config = dict(argv=["subject"], cwd=temporary, timeout=1, cleanup=["cleanup"])
            with patch.object(
                RUNTIME.subprocess, "run", return_value=subprocess.CompletedProcess([], 5, "", "")
            ):
                with self.assertRaisesRegex(ValueError, "subject failed"):
                    RUNTIME.command_trial(path, "row", "sample", 0, config)
            record = json.loads(path.read_text())
            self.assertIn("subject failed", record["error"])
            self.assertIn("cleanup failed", record["cleanup_error"])

    def test_plan_keeps_warmup_failure_and_no_statistics(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = dict(
                id="row",
                argv=[sys.executable, "-c", "print('wrong')"],
                cwd=temporary,
                timeout=1,
                warmup=1,
                runs=2,
                expected_stdout="right\n",
            )
            result = RUNTIME.measure_plan(config, Path(temporary) / "trials")
            self.assertEqual(result["status"], "FAIL")
            self.assertIsNone(result["result"])
            self.assertEqual(len(result["trials"]), 1)
            self.assertEqual(result["trials"][0]["phase"], "warmup")
            self.assertEqual(result["trials"][0]["stdout"], "wrong\n")

    def test_plan_retains_timeout_output_and_never_calls_it_a_sample(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = dict(
                id="row",
                argv=[
                    sys.executable,
                    "-c",
                    "import time; print('partial',flush=True); time.sleep(10)",
                ],
                cwd=temporary,
                timeout=0.05,
                warmup=0,
                runs=2,
            )
            result = RUNTIME.measure_plan(config, Path(temporary) / "trials")
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["samples"], [])
            self.assertEqual(result["trials"][0]["stdout"], "partial\n")
            self.assertIsNone(result["trials"][0]["exit_code"])


if __name__ == "__main__":
    unittest.main()
