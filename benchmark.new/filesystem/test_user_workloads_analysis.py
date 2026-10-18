"""Protect environment-level statistics and failure visibility in user tables."""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "user_workloads_analysis", Path(__file__).with_name("user_workloads_analysis.py")
)
ANALYSIS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANALYSIS)


def protocol(backends=("native",)):
    return dict(
        catalog=[dict(id="M01", name="lookup", layer="mechanism", parameters={})],
        backends=[dict(id=backend, label=backend) for backend in backends],
        samples=3,
        resident_rounds=3,
    )


def environment(backend, pair, values=(1, 1, 1)):
    return dict(
        case_id="M01",
        backend=backend,
        pair=pair,
        ready_ns=(pair + 1) * 1000000,
        returncode=0,
        valid=True,
        host_verified=True,
        rows=[
            dict(
                id="M01",
                round=r,
                correctness=True,
                work_ns=value * 1000000,
                final_sync_ns=0,
                completed_ns=value * 1000000,
            )
            for r, value in enumerate(values, 1)
        ],
    )


class UserWorkloadsAnalysisTests(unittest.TestCase):
    def test_environment_sum_precedes_median(self):
        records = [
            environment("native", pair, values)
            for pair, values in enumerate([(1, 1, 100), (1, 100, 1), (100, 1, 1)])
        ]
        summary = ANALYSIS.summarize(protocol(), records)
        total = next(
            row
            for row in summary["levels"]
            if row["scope"] == "sum_three_rounds" and row["metric"] == "completed_ns"
        )
        self.assertEqual(total["median_ms"], 102)
        self.assertEqual(total["n"], 3)
        self.assertEqual(summary["counts"]["observed_round_rows"], 9)

    def test_failure_does_not_suppress_another_mode_or_discard_raw_rows(self):
        records = [
            environment(backend, pair) for backend in ("native", "host") for pair in range(3)
        ]
        records[-1]["returncode"] = 7
        records[-1]["error"] = "intentional synthetic failure"
        summary = ANALYSIS.summarize(protocol(("native", "host")), records)
        self.assertEqual(summary["status"], "incomplete_cells_retained")
        self.assertEqual({row["backend"] for row in summary["levels"]}, {"native"})
        self.assertEqual(len(summary["raw_rows"]), 18)
        self.assertTrue(any("failure" in row["reason"] for row in summary["failures"]))

    def test_missing_environment_never_looks_like_a_faster_zero_sample(self):
        records = [environment("native", pair) for pair in range(2)]
        summary = ANALYSIS.summarize(protocol(), records)
        self.assertEqual(summary["levels"], [])
        self.assertEqual(summary["counts"]["observed_environments"], 2)
        self.assertTrue(any("missing" in failure["reason"] for failure in summary["failures"]))

    def test_inconsistent_completion_clock_is_not_publishable(self):
        records = [environment("native", pair) for pair in range(3)]
        records[0]["rows"][0]["completed_ns"] += 1
        summary = ANALYSIS.summarize(protocol(), records)
        self.assertEqual(summary["levels"], [])
        self.assertTrue(any("differs" in failure["reason"] for failure in summary["failures"]))

    def test_ready_keeps_independent_environment_count(self):
        summary = ANALYSIS.summarize(protocol(), [environment("native", pair) for pair in range(3)])
        self.assertEqual(len(summary["ready"]), 1)
        self.assertEqual(summary["ready"][0]["n"], 3)
        self.assertEqual(summary["ready"][0]["median_ms"], 2)
        self.assertEqual(summary["ready"][0]["case_id"], "M01")

    def test_one_based_inventory_matches_runner(self):
        spec = protocol()
        spec["sample_index_base"] = 1
        summary = ANALYSIS.summarize(spec, [environment("native", pair) for pair in (1, 2, 3)])
        self.assertEqual(summary["counts"]["defects"], 0)
        self.assertEqual(summary["statistical_scope"]["sample_ids"], [1, 2, 3])

    def test_duplicate_environment_fails_instead_of_overweighting_sample(self):
        records = [environment("native", pair) for pair in range(3)]
        with self.assertRaisesRegex(ValueError, "duplicate environment"):
            ANALYSIS.summarize(protocol(), records + [records[0]])

    def test_reference_modes_are_fresh_observations_and_missing_mode_has_no_median(self):
        modes = (
            "native",
            "host",
            "staged",
            "vm",
            "vm-writeback",
            "docker",
            "fc-system",
            "qemu",
            "qemu-microvm",
        )
        records = [environment(backend, pair) for backend in modes for pair in range(3)]
        summary = ANALYSIS.summarize(protocol(modes), records[:-1])
        self.assertEqual(summary["counts"]["expected_environments"], 27)
        self.assertEqual(summary["counts"]["complete_cells"], 8)
        self.assertEqual(summary["backends"], list(modes))
        self.assertNotIn("qemu-microvm", {row["backend"] for row in summary["levels"]})
        self.assertEqual(summary["counts"]["observed_round_rows"], 78)


if __name__ == "__main__":
    unittest.main()
