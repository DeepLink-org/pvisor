"""Offline contracts for documentation-workload retest analysis."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "docs_retest_analysis", Path(__file__).with_name("docs_retest_analysis.py")
)
SUBJECT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECT)


class RetestContracts(unittest.TestCase):
    def test_identical_clocks_detect_no_gain(self):
        pairs = [{"a": {"ns": 100}, "b": {"ns": 100}} for _ in range(30)]
        result = SUBJECT.comparison(pairs, "ns", "a", "b")
        self.assertFalse(result["detected"])
        self.assertEqual(result["bootstrap_95_percent"], [0, 0])

    def test_gain_and_regression_preserve_sign(self):
        pairs = [{"a": {"ns": 100}, "b": {"ns": 80}} for _ in range(30)]
        self.assertAlmostEqual(SUBJECT.comparison(pairs, "ns", "a", "b")["reduction_percent"], 20)
        self.assertAlmostEqual(SUBJECT.comparison(pairs, "ns", "b", "a")["reduction_percent"], -25)

    def test_distribution_retains_slow_cluster(self):
        result = SUBJECT.clusters([100_000_000] * 27 + [140_000_000] * 3)
        self.assertEqual(result, [{"n": 27, "median_ms": 100}, {"n": 3, "median_ms": 140}])

    def test_invalid_clocks_are_not_silently_dropped(self):
        for value in (0, -1, True, 1.5):
            with self.assertRaises(ValueError):
                SUBJECT.comparison([{"a": {"ns": 100}, "b": {"ns": value}}], "ns", "a", "b")

    def test_rootfs_inventory_rejects_partial_and_duplicate_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            batch = root / "main"
            batch.mkdir()
            (root / "status.json").write_text(json.dumps({"valid": True, "failed_cells": []}))
            (root / "plan.json").write_text(
                json.dumps({"samples": 1, "batches": {"main": str(batch)}})
            )
            (batch / "protocol.json").write_text(
                json.dumps({"cases": ["S01"], "backends": [{"id": "vm"}]})
            )
            row = {
                "case_id": "S01",
                "backend": "vm",
                "revision": "main",
                "pair": 1,
                "valid": True,
                "host_verified": True,
                "ready_ns": 100,
                "rows": [{"round": n, "completed_ns": 10} for n in (1, 2, 3)],
            }
            (root / "records.json").write_text(json.dumps([row]))
            result = SUBJECT.rootfs_levels(root)
            self.assertEqual(result["S01/vm/main"]["completed_ms"]["n"], 1)
            for records in ([], [row, row], [{**row, "host_verified": False}]):
                (root / "records.json").write_text(json.dumps(records))
                with self.assertRaises(ValueError):
                    SUBJECT.rootfs_levels(root)


if __name__ == "__main__":
    unittest.main()
