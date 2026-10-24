"""Analysis contracts use synthetic rows only; no subject or benchmark launch."""

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import workloads_v2_analysis as A  # noqa: E402 - import after module-loading setup


def fixture(ids=("M06", "S01")):
    protocol = dict(
        comparison="guest-writeback",
        samples=3,
        resident_rounds=3,
        conditions={"baseline": {"env": {}}, "candidate": {"env": {}}},
        inventory={"selected_cell_ids": list(ids)},
        catalog=[
            dict(
                id=id_,
                name=id_ + " synthetic",
                layer="scenario" if id_[0] == "S" else "mechanism",
                parameters={},
            )
            for id_ in ids
        ],
    )
    records = []
    for case in ids:
        for pair in range(3):
            for arm in A.ARMS:
                factor = 1 if arm == "baseline" else [0.5, 0.8, 1.2][pair]
                rows = [
                    dict(
                        id=case,
                        round=r,
                        work_ns=int(1000000 * r * factor),
                        completed_ns=int(2000000 * r * factor),
                        final_sync_ns=int(1000000 * r * factor),
                        bytes_written=A.MIB,
                        bytes_read=0,
                        write_ops=1024,
                        read_ops=0,
                        metadata_ops=0,
                        correctness=True,
                    )
                    for r in range(1, 4)
                ]
                records.append(
                    dict(
                        case_id=case,
                        condition=arm,
                        pair=pair,
                        valid=True,
                        host_verified=True,
                        ready_ms=40,
                        exit_after_done_ms=5,
                        resource={"cpu_usage_usec_delta": 1000000},
                        rows=rows,
                    )
                )
    return protocol, records


class WorkloadAnalysisContracts(unittest.TestCase):
    def test_rounds_remain_dependent_and_all_three_pairs_are_reported(self):
        protocol, records = fixture()
        result = A.summarize(protocol, records)
        row = next(
            c
            for c in result["comparisons"]
            if c["case_id"] == "M06"
            and c["scope"] == "sum_three_rounds"
            and c["metric"] == "completed_ns"
        )
        self.assertEqual(row["independent_pairs"], 3)
        self.assertEqual(row["dependent_rounds"], 3)
        self.assertEqual(row["pair_ids"], [0, 1, 2])
        self.assertEqual(row["paired_ratios"], [0.5, 0.8, 1.2])
        self.assertEqual(row["median_ratio"], 0.8)
        self.assertEqual(row["baseline_median_ms"], 12)
        self.assertAlmostEqual(row["candidate_median_ms"], 9.6)
        self.assertEqual(row["ratio_ci_low"], 0.5)
        self.assertEqual(row["ratio_ci_high"], 1.2)
        self.assertEqual(len(result["comparisons"]), 2 * 4 * 2)
        self.assertEqual(len(result["raw_rows"]), 2 * 6 * 3)
        self.assertEqual(len(result["primary_workflows"]), 1)
        self.assertIsNone(result["statistical_scope"]["aggregate_score"])
        self.assertEqual(result, A.summarize(protocol, records))

    def test_sum_precedes_ratio_never_median_of_round_ratios(self):
        protocol, records = fixture(("M06",))
        for record in records:
            if record["condition"] == "candidate":
                for row in record["rows"]:
                    row["work_ns"] = row["completed_ns"] = (
                        1000000 if row["round"] < 3 else 100000000
                    )
                    row["final_sync_ns"] = 0
        result = A.summarize(protocol, records)
        row = next(
            c
            for c in result["comparisons"]
            if c["scope"] == "sum_three_rounds" and c["metric"] == "completed_ns"
        )
        self.assertEqual(row["paired_ratios"], [102 / 12] * 3)
        self.assertNotEqual(row["median_ratio"], 1 / 4)

    def test_incomplete_case_retains_observations_without_partial_contrast(self):
        protocol, records = fixture()
        records.pop(0)
        result = A.summarize(protocol, records)
        self.assertEqual(result["status"], "incomplete_cases_retained")
        self.assertEqual(result["incomplete_case_ids"], ["M06"])
        self.assertEqual(result["complete_case_ids"], ["S01"])
        self.assertEqual(len(result["raw_rows"]), 33)
        self.assertTrue(all(c["case_id"] == "S01" for c in result["comparisons"]))
        self.assertTrue(any("missing" in f["reason"] for f in result["failures"]))
        records[0]["host_verified"] = False
        records[0]["error"] = "byte mismatch"
        failed = A.summarize(protocol, records)
        self.assertTrue(any("byte mismatch" in f["reason"] for f in failed["failures"]))
        self.assertEqual(len(failed["raw_rows"]), len(result["raw_rows"]))

    def test_invalid_clocks_or_oracles_cannot_supply_performance(self):
        protocol, original = fixture(("M06",))
        for field, value in [
            ("work_ns", 0),
            ("completed_ns", -1),
            ("work_ns", float("nan")),
            ("correctness", False),
            ("round", None),
            ("bytes_written", -1),
        ]:
            with self.subTest(field=field, value=value):
                records = copy.deepcopy(original)
                records[0]["rows"][0][field] = value
                result = A.summarize(protocol, records)
                self.assertEqual(result["comparisons"], [])
                self.assertTrue(result["failures"])
        records = copy.deepcopy(original)
        records[0]["rows"][0]["completed_ns"] = records[0]["rows"][0]["work_ns"] - 1
        self.assertEqual(A.summarize(protocol, records)["comparisons"], [])

    def test_duplicate_or_unexpected_environments_rejected(self):
        protocol, records = fixture()
        with self.assertRaisesRegex(ValueError, "duplicate"):
            A.summarize(protocol, records + [records[0]])
        records[0]["case_id"] = "M99"
        with self.assertRaisesRegex(ValueError, "unexpected"):
            A.summarize(protocol, records)
        protocol["samples"] = 5
        with self.assertRaisesRegex(ValueError, "N3"):
            A.summarize(protocol, [])

    def test_unknown_resources_and_absent_work_are_null_not_zero(self):
        protocol, records = fixture(("M06",))
        result = A.summarize(protocol, records)
        self.assertTrue(
            all(env["sampled_memory_current_peak"] is None for env in result["environments"])
        )
        self.assertTrue(
            all(point["read_completed_mib_s"] is None for point in result["observations"])
        )
        self.assertTrue(all(point["write_completed_mib_s"] > 0 for point in result["observations"]))
        self.assertNotIn("job_ms", result["raw_rows"][0])

    def test_derived_files_preserve_sources_and_refuse_overwrite(self):
        protocol, records = fixture()
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary) / ".data" / "synthetic"
            batch.mkdir(parents=True)
            (batch / "protocol.json").write_text(json.dumps(protocol))
            (batch / "records.json").write_text(json.dumps(records))
            hashes = [A.sha(batch / name) for name in ("protocol.json", "records.json")]
            result = A.analyze(batch, plots=False)
            self.assertEqual(
                hashes, [A.sha(batch / name) for name in ("protocol.json", "records.json")]
            )
            self.assertEqual(result["plots"], [])
            self.assertEqual(result["provenance"]["records_sha256"], hashes[1])
            for name in (
                "comparisons.csv",
                "levels.csv",
                "observations.csv",
                "environments.csv",
                "failures.csv",
                "raw_rows.csv",
                "report.md",
                "summary.json",
            ):
                self.assertTrue((batch / "analysis" / name).is_file())
            with self.assertRaises(FileExistsError):
                A.analyze(batch, plots=False)
            with self.assertRaisesRegex(ValueError, ".data"):
                A.analyze(batch, Path(temporary) / "public", plots=False)


if __name__ == "__main__":
    unittest.main()
