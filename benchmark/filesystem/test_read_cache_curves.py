"""Offline read-cache protocol tests; no VM or benchmark payload is executed."""

import copy
import csv
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location("read_cache_contract", HERE / "read_cache_curves.py")
READ = importlib.util.module_from_spec(spec)
spec.loader.exec_module(READ)


def evidence():
    protocol = dict(cells=READ.cells())
    count = len(protocol["cells"])
    ready = dict(
        schema=1,
        cells=count,
        rounds=3,
        passes=5,
        buffer_bytes=READ.MIB,
        verification="inline_memcmp_repeated_1m",
    )
    done = dict(schema=1, rows=count * 3, passes=count * 15, verified=True)
    rows = []
    for round_ in range(1, 4):
        for cell in protocol["cells"]:
            passes = [dict(pass_=p) for p in range(1, 6)]
            for p in passes:
                p["pass"] = p.pop("pass_")
                p.update(
                    bytes=cell["bytes"],
                    open_ns=1,
                    read_check_ns=7,
                    close_ns=2,
                    total_ns=10,
                    correctness=True,
                )
            rows.append(
                dict(
                    schema=1,
                    id=cell["id"],
                    round=round_,
                    bytes=cell["bytes"],
                    passes=passes,
                    total_ns=60,
                    pass_ns_sum=50,
                    correctness=True,
                )
            )
    return protocol, rows, ready, done


class ReadCacheContracts(unittest.TestCase):
    def test_plan_covers_ram_boundary_and_conditions_change_only_root_data(self):
        cells = READ.cells()
        self.assertEqual([c["size_mib"] for c in cells], [1, 16, 64, 256, 384, 448, 512, 768])
        self.assertEqual(len({c["id"] for c in cells}), 8)
        self.assertTrue(any(c["size_mib"] > 512 for c in cells))
        self.assertEqual(
            READ.CONDITIONS,
            {
                "native": {},
                "off": {"PVISOR_VM_CACHE_EXPERIMENT": "off"},
                "on": {"PVISOR_VM_CACHE_EXPERIMENT": "root-data"},
            },
        )
        for cell in cells:
            self.assertEqual(cell["bytes"], cell["size_mib"] * READ.MIB)
            self.assertEqual(Path(cell["file"]).name, cell["file"])

    def test_complete_three_round_five_pass_inventory_is_valid(self):
        READ.validate_rows(*evidence())

    def test_missing_duplicate_or_changed_pass_and_round_are_rejected(self):
        for mutation in (
            "missing-row",
            "duplicate-row",
            "wrong-round",
            "missing-pass",
            "wrong-pass",
            "wrong-bytes",
            "bad-clock",
            "bad-full-clock",
            "incorrect",
        ):
            protocol, rows, ready, done = evidence()
            if mutation == "missing-row":
                rows.pop()
            elif mutation == "duplicate-row":
                rows.append(copy.deepcopy(rows[0]))
            elif mutation == "wrong-round":
                rows[0]["round"] = 4
            elif mutation == "missing-pass":
                rows[0]["passes"].pop()
            elif mutation == "wrong-pass":
                rows[0]["passes"][0]["pass"] = 2
            elif mutation == "wrong-bytes":
                rows[0]["passes"][0]["bytes"] -= 1
            elif mutation == "bad-clock":
                rows[0]["passes"][0]["read_check_ns"] = -1
            elif mutation == "bad-full-clock":
                rows[0]["total_ns"] = 49
            else:
                rows[0]["passes"][0]["correctness"] = False
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                READ.validate_rows(protocol, rows, ready, done)

    def test_ready_done_must_match_full_five_pass_inventory(self):
        for target, field, value in (
            ("ready", "passes", 1),
            ("ready", "buffer_bytes", 768 * READ.MIB),
            ("done", "passes", 15),
            ("done", "verified", False),
        ):
            protocol, rows, ready, done = evidence()
            (ready if target == "ready" else done)[field] = value
            with self.subTest(target=target, field=field), self.assertRaises(ValueError):
                READ.validate_rows(protocol, rows, ready, done)

    def test_fixture_hashes_full_contents_and_rejects_same_length_corruption(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fixture"
            manifest = READ.make_fixture(root, sizes=(1, 2))
            READ.verify_fixture(root, manifest)
            self.assertEqual(len(manifest["files"]), 3)
            self.assertEqual(
                manifest["files"]["cache-input/files/file-2m.bin"]["bytes"], 2 * READ.MIB
            )
            pattern = (root / "cache-input/pattern.bin").read_bytes()
            with (root / "cache-input/files/file-2m.bin").open("rb") as stream:
                self.assertEqual(stream.read(READ.MIB), pattern)
                self.assertEqual(stream.read(READ.MIB), pattern)
            victim = root / "cache-input/files/file-1m.bin"
            victim.chmod(0o644)
            with victim.open("r+b") as stream:
                stream.write(bytes([pattern[0] ^ 1]))
            with self.assertRaisesRegex(ValueError, "immutable fixture changed"):
                READ.verify_fixture(root, manifest)

    def test_measure_wrapper_emits_one_checked_json_value_for_semspec(self):
        for invalid in (False, True):
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temporary:
                batch = Path(temporary)
                protocol = dict(
                    benchmark_id="B-FS-ENG",
                    fixture=str(batch / "fixture"),
                    fixture_manifest={},
                    artifacts={},
                    orders=[dict(pair=0, conditions=["native", "off", "on"])],
                )
                (batch / "protocol.json").write_text(json.dumps(protocol))

                def completed(command, **kwargs):
                    READ.save(
                        batch / "status.json",
                        dict(
                            valid=not invalid,
                            trials=3,
                            failed=int(invalid),
                            excluded=0,
                            fixture_verified=True,
                        ),
                    )
                    READ.save(
                        batch / "host-postverification.json", dict(verified=True, full_sha256=True)
                    )
                    return SimpleNamespace(returncode=0)

                stdout = io.StringIO()
                with (
                    patch.object(READ, "verify_artifacts"),
                    patch.object(READ, "verify_fixture"),
                    patch.object(READ.subprocess, "run", side_effect=completed),
                    redirect_stdout(stdout),
                ):
                    if invalid:
                        with self.assertRaisesRegex(ValueError, "completion receipt"):
                            READ.measure(batch)
                        self.assertEqual(stdout.getvalue(), "")
                    else:
                        READ.measure(batch)
                        # json.loads rejects trailing non-whitespace/multiple JSON values.
                        receipt = json.loads(stdout.getvalue())
                        self.assertIs(receipt["valid"], True)
                        self.assertEqual(receipt["trials"], 3)
                        self.assertEqual(receipt["failed"], 0)
                        self.assertEqual(receipt["benchmark_id"], "B-FS-ENG")

    def test_analysis_aggregates_bytes_over_time_and_bootstraps_independent_pairs(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            all_cells, original, _, _ = evidence()
            selected_cell = all_cells["cells"][0]
            original = [row for row in original if row["id"] == selected_cell["id"]]
            protocol = dict(
                benchmark_id="B-FS-ENG",
                profiling=False,
                cells=[selected_cell],
                conditions={"off": {}, "on": {}},
                samples=2,
                resident_rounds=3,
                seed=42,
                timing="synthetic read/check pass clocks",
                memory="synthetic",
                orders=[dict(pair=pair, conditions=["off", "on"]) for pair in (0, 1)],
            )
            records = []
            for pair, condition, factor in (
                (0, "off", 1),
                (0, "on", 0.5),
                (1, "off", 2),
                (1, "on", 4),
            ):
                rows = copy.deepcopy(original)
                for row, base in zip(rows, (1_000_000_000, 2_000_000_000, 4_000_000_000)):
                    duration = int(base * factor)
                    for pass_ in row["passes"]:
                        pass_.update(
                            open_ns=1, read_check_ns=duration - 2, close_ns=1, total_ns=duration
                        )
                    row.update(pass_ns_sum=5 * duration, total_ns=5 * duration + 1000)
                records.append(dict(pair=pair, condition=condition, valid=True, rows=rows))
            READ.save(batch / "protocol.json", protocol)
            READ.save(batch / "records.json", records)
            READ.save(batch / "host-postverification.json", dict(verified=True, full_sha256=True))
            report = READ.analyze(batch, plots=False)
            with (batch / "analysis/environment_points.csv").open() as stream:
                points = list(csv.DictReader(stream))
            first = next(
                p
                for p in points
                if p["pair"] == "0" and p["condition"] == "off" and p["reuse"] == "first"
            )
            self.assertAlmostEqual(float(first["throughput_mib_s"]), 3 / 7)
            self.assertNotAlmostEqual(float(first["throughput_mib_s"]), (1 + 0.5 + 0.25) / 3)
            self.assertEqual(int(first["bytes_sum"]), 3 * READ.MIB)
            self.assertEqual(int(first["total_ns_sum"]), 7_000_000_000)
            self.assertEqual(len(report["paired_ratio_summary"]), 2)
            for comparison in report["paired_ratio_summary"]:
                self.assertEqual(comparison["pairs"], [0, 1])
                self.assertEqual(comparison["n"], 2)
                self.assertAlmostEqual(comparison["median"], 1.25)
                self.assertAlmostEqual(comparison["ci_low"], 0.5)
                self.assertAlmostEqual(comparison["ci_high"], 2)
            self.assertEqual(len(report["round_paired_ratio_summary"]), 3)
            for comparison in report["round_paired_ratio_summary"]:
                self.assertEqual(comparison["pairs"], [0, 1])
                self.assertEqual(comparison["n"], 2, "resident rounds must not create N6")
                self.assertAlmostEqual(comparison["median"], 1.25)
            with (batch / "analysis/cache_rounds.csv").open() as stream:
                round_points = list(csv.DictReader(stream))
            self.assertEqual(len(round_points), 12)
            self.assertEqual({point["round"] for point in round_points}, {"1", "2", "3"})
            self.assertEqual(
                report["analysis_source"]["script_sha256"], READ.sha(HERE / "read_cache_curves.py")
            )
            self.assertEqual(report["input_sha256"]["records"], READ.sha(batch / "records.json"))
            self.assertTrue((batch / "analysis/paired_ratio_summary.csv").is_file())
            self.assertIn("not cold first-ever", report["reuse_labels"]["first"])

    def test_diagnostic_analysis_outputs_counts_without_timing_tables(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            all_cells, rows, _, _ = evidence()
            cell = all_cells["cells"][0]
            rows = [row for row in rows if row["id"] == cell["id"]]
            protocol = dict(
                benchmark_id="B-FS-DIAG",
                profiling=True,
                cells=[cell],
                conditions={"off": {}, "on": {}},
                samples=1,
                resident_rounds=3,
                timing="diagnostic only",
                memory="synthetic",
                orders=[dict(pair=0, conditions=["off", "on"])],
            )
            records = []
            for condition in ("off", "on"):
                folder = batch / "trials" / f"000-{condition}"
                folder.mkdir(parents=True)
                phases = [
                    dict(
                        component="virtio-fs-protocol",
                        id=cell["id"] + f"::p{pass_}",
                        round=round_,
                        measurements={"READ": {"calls": int(condition == "off" or pass_ == 1)}},
                    )
                    for round_ in range(1, 4)
                    for pass_ in range(1, 6)
                ]
                READ.save(folder / "diagnostics.json", dict(phases=phases))
                records.append(
                    dict(pair=0, condition=condition, valid=True, rows=copy.deepcopy(rows))
                )
            READ.save(batch / "protocol.json", protocol)
            READ.save(batch / "records.json", records)
            READ.save(batch / "host-postverification.json", dict(verified=True, full_sha256=True))
            report = READ.analyze(batch, plots=False)
            self.assertIs(report["performance_samples"], False)
            self.assertEqual(report["summary"], [])
            self.assertEqual(report["paired_ratio_summary"], [])
            self.assertEqual(
                next(
                    row["median"]
                    for row in report["request_summary"]
                    if row["condition"] == "on" and row["reuse"] == "repeat"
                ),
                0,
            )
            self.assertEqual(len(report["request_round_summary"]), 6)
            self.assertTrue(all(row["n"] == 1 for row in report["request_round_summary"]))
            for name in (
                "curve_summary.csv",
                "paired_ratios.csv",
                "paired_ratio_summary.csv",
                "raw_passes.csv",
            ):
                self.assertFalse((batch / "analysis" / name).exists(), name)
            self.assertTrue((batch / "analysis/read_requests.csv").is_file())


if __name__ == "__main__":
    unittest.main()
