"""Pure-Python contracts for the post-collection B-FS-ENG analysis.

These fixtures are isolated synthetic test inputs, not benchmark observations.
No subject, VM, matplotlib, benchmark plot or published result is produced.
"""

from __future__ import annotations

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "filesystem_curves_analysis_test_subject", Path(__file__).with_name("curves_analysis.py")
)
assert SPEC is not None and SPEC.loader is not None
ANALYSIS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANALYSIS)


def protocol(samples=1):
    return {
        "benchmark_id": "B-FS-ENG",
        "role": "engineering A/B",
        "resident_rounds": 3,
        "samples": samples,
        "baseline_condition": "sync",
        "conditions": {
            "sync": {"env": {"PVISOR_VM_FS_HOST_WRITEBACK": "0"}},
            "async": {"env": {"PVISOR_VM_FS_HOST_WRITEBACK": "1"}},
        },
        "cells": [
            {
                "id": "block-1k",
                "family": "block",
                "x": 1,
                "workload": "stream",
                "block_bytes": 1024,
                "workers": 1,
                "bytes_per_worker": ANALYSIS.MIB,
                "sync_every_ops": 0,
            }
        ],
    }


def record(plan, pair, condition, times=(1_000_000, 2_000_000, 6_000_000)):
    cell = plan["cells"][0]
    rows = []
    for round_no, total in enumerate(times, 1):
        rows.append(
            {
                **{
                    key: cell[key]
                    for key in (
                        "id",
                        "workload",
                        "block_bytes",
                        "workers",
                        "bytes_per_worker",
                        "sync_every_ops",
                    )
                },
                "round": round_no,
                "bytes": ANALYSIS.MIB,
                "accepted_ns": total // 2,
                "applied_ns": total // 10,
                "fsync_ns": total // 5,
                "close_ns": total // 10,
                "dirsync_ns": total // 10,
                "total_ns": total,
                "correctness": True,
                "periodic_fsyncs": 7,
            }
        )
    return {
        "pair": pair,
        "condition": condition,
        "rows": rows,
        "valid": True,
        "returncode": 0,
        "ready_ms": 4.0,
        "job_ms": 20.0,
        "sampled_memory_current_peak": 2 * ANALYSIS.MIB,
        "resource": {"memory.peak": "999999", "cpu.stat": "usage_usec 123"},
    }


def point(pair, condition, throughput):
    return {
        "pair": str(pair),
        "condition": condition,
        "id": "block-1k",
        "family": "block",
        "workload": "stream",
        "x": 1,
        "throughput_mib_s": throughput,
        "accepted_mib_s": throughput * 2,
        **{phase + "_ms": 1.0 for phase in ANALYSIS.PHASES},
    }


class AggregationTests(unittest.TestCase):
    def test_dependent_rounds_are_summed_before_throughput_and_are_one_environment(self):
        plan = protocol()
        records = [record(plan, 0, name) for name in plan["conditions"]]
        points, raw, failures, environments = ANALYSIS.collect(plan, records)
        self.assertFalse(failures)
        self.assertEqual((len(points), len(raw), len(environments)), (2, 6, 2))
        # 3 MiB / 9 ms, not the average of 1 MiB/1 ms, /2 ms and /6 ms.
        self.assertAlmostEqual(points[0]["throughput_mib_s"], 3 / 0.009)
        self.assertAlmostEqual(points[0]["total_ms"], 3.0)
        self.assertAlmostEqual(points[0]["accepted_ms"], 1.5)
        self.assertNotAlmostEqual(points[0]["throughput_mib_s"], (1000 + 500 + 1000 / 6) / 3)
        summaries, comparisons, ratios = ANALYSIS.summarize(plan, points)
        self.assertTrue(all(row["throughput_mib_s"]["n"] == 1 for row in summaries))
        self.assertEqual(comparisons[0]["n"], 1)
        self.assertEqual(len(ratios), 1)

    def test_bootstrap_keeps_the_same_independent_pair_index_for_both_arms(self):
        plan = protocol(samples=5)
        baseline = [1, 10, 100, 1000, 10000]
        points = [point(index, "sync", value) for index, value in enumerate(baseline)]
        # Reverse record ordering to ensure matching uses pair identity, not position.
        points += [point(index, "async", baseline[index] * 2) for index in reversed(range(5))]
        _, comparisons, ratios = ANALYSIS.summarize(plan, points)
        estimate = comparisons[0]
        self.assertEqual(estimate["n"], 5)
        self.assertEqual([row["throughput_ratio"] for row in ratios], [2.0] * 5)
        self.assertEqual(
            (estimate["median"], estimate["ci_low"], estimate["ci_high"]), (2.0, 2.0, 2.0)
        )
        self.assertEqual(estimate["pairs"], ["0", "1", "2", "3", "4"])
        self.assertFalse(estimate["distribution"]["testable"])

    def test_missing_failed_and_duplicate_environments_remain_explicit(self):
        plan = protocol(samples=2)
        records = [record(plan, pair, arm) for pair in range(2) for arm in plan["conditions"]]
        points, _, failures, _ = ANALYSIS.collect(plan, records[:-1])
        self.assertEqual(len(points), 3)
        self.assertEqual(len(failures), 1)
        self.assertIn("missing", failures[0]["reasons"][0])
        summaries, comparisons, _ = ANALYSIS.summarize(plan, points)
        self.assertEqual(
            {row["condition"]: row["throughput_mib_s"]["n"] for row in summaries},
            {"sync": 2, "async": 1},
        )
        self.assertEqual(comparisons[0]["n"], 1)

        failed = copy.deepcopy(records)
        failed[0]["valid"] = False
        points, raw, failures, _ = ANALYSIS.collect(plan, failed)
        self.assertEqual((len(points), len(raw), len(failures)), (3, 12, 1))
        points, raw, failures, _ = ANALYSIS.collect(plan, records + [records[0]])
        self.assertEqual((len(points), len(raw), len(failures)), (3, 15, 2))

    def test_payload_fields_and_phase_boundaries_are_checked_and_raw_fields_retained(self):
        plan = protocol()
        records = [record(plan, 0, arm) for arm in plan["conditions"]]
        points, raw, failures, _ = ANALYSIS.collect(plan, records)
        self.assertEqual(raw[0]["periodic_fsyncs"], 7)
        self.assertEqual(json.loads(raw[0]["raw_row_json"])["round"], 1)
        self.assertEqual(points[0]["rounds"], 3)
        self.assertFalse(failures)
        for field, value in (
            ("workers", 99),
            ("correctness", False),
            ("total_ns", 99),
            ("fsync_ns", -1),
        ):
            with self.subTest(field=field):
                broken = copy.deepcopy(records)
                broken[0]["rows"][0][field] = value
                points, retained, failures, _ = ANALYSIS.collect(plan, broken)
                self.assertEqual((len(points), len(retained), len(failures)), (1, 6, 1))
                self.assertEqual(retained[0][field], value)

    def test_profiling_never_calls_timing_plot_renderer(self):
        plan = protocol()
        plan.update(profiling=True, benchmark_id="B-FS-DIAG", role="diagnostic")
        records = [record(plan, 0, arm) for arm in plan["conditions"]]
        with tempfile.TemporaryDirectory(prefix="curves-analysis-unit-") as directory:
            batch = Path(directory)
            (batch / "protocol.json").write_text(json.dumps(plan))
            (batch / "records.json").write_text(json.dumps(records))
            before = (batch / "records.json").read_bytes()
            with mock.patch.object(
                ANALYSIS, "render_plots", side_effect=AssertionError("timing renderer called")
            ) as render:
                result = ANALYSIS.analyze(batch, batch / "analysis", plots=True)
            render.assert_not_called()
            self.assertEqual(result["benchmark"], "B-FS-DIAG")
            self.assertEqual(result["plot_status"], "diagnostic_only")
            self.assertEqual(
                (result["curves"], result["comparisons"], result["plots"]), ([], [], [])
            )
            self.assertEqual((batch / "records.json").read_bytes(), before)
            self.assertTrue((batch / "analysis/raw_rows.csv").is_file())
            self.assertTrue((batch / "analysis/environment_resources.csv").is_file())

    def test_analysis_artifacts_expose_failures_n5_limit_and_versioned_source(self):
        plan = protocol()
        with tempfile.TemporaryDirectory(prefix="curves-analysis-unit-") as directory:
            batch = Path(directory)
            (batch / "protocol.json").write_text(json.dumps(plan))
            (batch / "records.json").write_text(json.dumps([record(plan, 0, "sync")]))
            result = ANALYSIS.analyze(batch, batch / "derived", plots=False)
            self.assertEqual(result["status"], "incomplete_or_failed")
            self.assertEqual(result["counts"]["failed_environments"], 1)
            self.assertEqual(len(result["source"]["analysis_sha256"]), 64)
            self.assertIn(
                "N5 cannot test", result["statistical_scope"]["separated_distributions"]["N5_limit"]
            )
            self.assertTrue((batch / "derived/failures.csv").is_file())
            self.assertEqual(result["analysis_schema"], 3)


class DistributionTests(unittest.TestCase):
    def test_registered_thresholds_retain_cluster_members_and_withhold_global_summary(self):
        values = [1.0, 1.0, 1.0, 1.5, 1.7, 1.9]
        result = ANALYSIS.describe(values, bootstrap=True, labels=list("abcdef"))
        self.assertTrue(result["separated"])
        self.assertIsNone(result["median"])
        self.assertIsNone(result["mean"])
        self.assertIsNone(result["ci_low"])
        self.assertIsNone(result["ci_high"])
        clusters = result["distribution"]["clusters"]
        self.assertEqual([cluster["n"] for cluster in clusters], [3, 3])
        self.assertEqual([cluster["fraction"] for cluster in clusters], [0.5, 0.5])
        self.assertEqual([cluster["median"] for cluster in clusters], [1.0, 1.7])
        self.assertEqual(clusters[0]["pairs"], ["a", "b", "c"])
        self.assertEqual(
            sorted(value for cluster in clusters for value in cluster["values"]), values
        )

    def test_both_gap_and_median_ratio_and_three_per_side_are_required(self):
        for values in (
            [1.0, 1.1, 1.2, 1.7, 2.0, 2.2],  # median ratio passes; adjacent gap fails
            [1.0, 1.0, 1.0, 1.5, 1.6, 1.65],  # gap passes; median ratio fails
            [1.0, 1.0, 1.0, 1.0, 1.0, 100.0],  # isolated outlier is not a cluster
        ):
            with self.subTest(values=values):
                result = ANALYSIS.describe(values)
                self.assertFalse(result["separated"])
                self.assertIsNotNone(result["median"])
        result = ANALYSIS.describe([1, 1, 50, 50, 50])
        self.assertFalse(result["separated"])
        self.assertFalse(result["distribution"]["testable"])
        self.assertEqual(result["distribution"]["reason"], "N_lt_6_rule_not_testable")

    def test_separated_input_arms_withhold_ranking_even_with_constant_paired_ratio(self):
        plan = protocol(samples=6)
        baseline = [1.0, 1.1, 1.2, 3.0, 3.1, 3.2]
        points = [point(index, "sync", value) for index, value in enumerate(baseline)]
        points += [point(index, "async", value * 2) for index, value in enumerate(baseline)]
        summaries, comparisons, ratios = ANALYSIS.summarize(plan, points)
        self.assertTrue(all(row["throughput_mib_s"]["separated"] for row in summaries))
        self.assertEqual(len(ratios), 6)
        self.assertEqual({row["throughput_ratio"] for row in ratios}, {2.0})
        self.assertTrue(comparisons[0]["ranking_withheld"])
        self.assertEqual(comparisons[0]["separated_arms"], ["sync", "async"])
        self.assertIsNone(comparisons[0]["median"])
        self.assertIsNone(comparisons[0]["ci_low"])

    def test_ratio_distribution_can_withhold_ranking_without_a_separated_arm(self):
        plan = protocol(samples=6)
        points = [point(index, "sync", index + 1) for index in range(6)]
        points += [point(index, "async", 6 - index) for index in range(6)]
        summaries, comparisons, ratios = ANALYSIS.summarize(plan, points)
        # Both arms contain the same dense 1..6 values, but reversed pairing
        # produces two groups of ratios. The ratio check must run independently.
        self.assertFalse(any(row["throughput_mib_s"]["separated"] for row in summaries))
        self.assertEqual(len(ratios), 6)
        self.assertTrue(comparisons[0]["separated"])
        self.assertTrue(comparisons[0]["ranking_withheld"])
        self.assertEqual(comparisons[0]["separated_arms"], [])
        self.assertIsNone(comparisons[0]["median"])
        self.assertEqual(len(comparisons[0]["distribution"]["clusters"]), 2)


class ProtocolAxesTests(unittest.TestCase):
    def test_phase_breakdown_uses_additive_means_and_preserves_individual_totals(self):
        points = []
        for pair, (accepted, fsync) in enumerate(((1, 100), (100, 1), (1, 1))):
            points.append(
                {
                    "id": "block-1k",
                    "condition": "async",
                    "pair": pair,
                    "accepted_ms": accepted,
                    "applied_ms": 0,
                    "fsync_ms": fsync,
                    "close_ms": 0,
                    "dirsync_ms": 0,
                    "total_ms": accepted + fsync,
                }
            )
        points.append({**points[0], "id": "block-4k"})
        rows = ANALYSIS.phase_breakdown(points)
        self.assertEqual(len(rows), 1)  # Selection is fixed, independent of measurements.
        row = rows[0]
        self.assertEqual(row["n"], 3)
        self.assertEqual(row["individual_total_ms"], [101, 101, 2])
        self.assertEqual(row["pairs"], [0, 1, 2])
        self.assertEqual(row["accepted_mean_ms"], 34)
        self.assertEqual(row["fsync_mean_ms"], 34)
        self.assertEqual(
            sum(row[phase + "_mean_ms"] for phase in ANALYSIS.PHASES[:-1]), row["total_mean_ms"]
        )

    def test_numeric_order_units_and_explicit_axis_contract(self):
        cells = {
            "large": {"family": "block", "x": 128, "block_bytes": 128 * 1024},
            "small": {"family": "block", "x": 4, "block_bytes": 4 * 1024},
            "medium": {"family": "block", "x": 16, "block_bytes": 16 * 1024},
        }
        axes = ANALYSIS.curve_axes(cells, cells.keys())
        self.assertEqual(axes["ordered_cells"], ["small", "medium", "large"])
        self.assertEqual(axes["label"], "Write block size (KiB)")
        self.assertEqual(axes["scale"], "log2")
        self.assertEqual(axes["tick_labels"], ["4", "16", "128"])
        cells["small"].update(x_label="Explicit bytes", x_scale="linear")
        axes = ANALYSIS.curve_axes(cells, cells.keys())
        self.assertEqual((axes["label"], axes["scale"]), ("Explicit bytes", "linear"))

    def test_barrier_zero_categorical_and_read_working_set_axes(self):
        barriers = {"end": {"family": "barrier", "x": 0}, "each": {"family": "barrier", "x": 1}}
        axes = ANALYSIS.curve_axes(barriers, barriers.keys())
        self.assertEqual(axes["scale"], "symlog")
        self.assertIn("0 = final only", axes["label"])
        categorical = {"a": {"family": "case", "x": "alpha"}, "b": {"family": "case", "x": "beta"}}
        axes = ANALYSIS.curve_axes(categorical, categorical.keys())
        self.assertFalse(axes["numeric"])
        self.assertEqual(axes["xvalue"], {"a": 0, "b": 1})
        reads = {
            "r": {"family": "readset", "x": 8, "bytes_per_worker": 8 * ANALYSIS.MIB, "workers": 1}
        }
        axes = ANALYSIS.curve_axes(reads, reads.keys())
        self.assertEqual(axes["label"], "Read working set (MiB)")

    def test_missing_parameter_factors_are_inferred_and_explicit_protocol_wins(self):
        plan = protocol()
        plan["conditions"].update(
            {
                "capacity-4m": {"env": {}},
                "batch-256k": {"env": {}},
                "delay-100us": {"env": {}},
                "async-old": {"env": {}},
                "unmerged": {"env": {}},
            }
        )
        factors, source = ANALYSIS.condition_factors(plan)
        self.assertTrue(source.startswith("inferred"))
        self.assertEqual(factors["capacity-4m"]["capacity_mib"], 4)
        self.assertEqual(factors["batch-256k"]["batch_kib"], 256)
        self.assertEqual(factors["delay-100us"]["delay_us"], 100)
        self.assertTrue(factors["async-old"]["qualification_lock"])
        self.assertEqual((factors["unmerged"]["chunks"], factors["unmerged"]["delay_us"]), (1, 0))
        plan["conditions"]["capacity-4m"]["env"]["PVISOR_VM_FS_HOST_WRITEBACK_CONFIG"] = (
            '{"capacity_mib": 8}'
        )
        self.assertEqual(ANALYSIS.condition_factors(plan)[0]["capacity-4m"]["capacity_mib"], 8)
        explicit = {"async": {"capacity_mib": 16}}
        plan["condition_factors"] = explicit
        self.assertEqual(ANALYSIS.condition_factors(plan), (explicit, "protocol"))


if __name__ == "__main__":
    unittest.main()
