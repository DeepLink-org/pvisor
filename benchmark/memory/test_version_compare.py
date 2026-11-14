"""Conventional tests for strict version pairing and regression evidence gates."""

import copy
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "version_compare", Path(__file__).with_name("version_compare.py")
)
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


def cases(differences=(5, 5, 5, 5, 5)):
    result = []
    for n, difference in enumerate(differences):
        for pattern in subject.PATTERNS:
            for version in ("A", "B"):
                row = dict(round=n, pattern=pattern, version=version, arm="pvisor-physical")
                row.update(
                    {
                        metric: 100 + n + (difference if version == "B" else 0)
                        for metric in subject.METRICS
                    }
                )
                row["sustained_drop"] = {
                    "reached": True,
                    "crossing_interval_seconds": [1, 1.05] if version == "A" else [1.2, 1.25],
                }
                result.append(row)
    return result


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


def fake_artifacts(root):
    helper = root / "common/pool_memory_case.rs"
    helper.parent.mkdir()
    helper.write_bytes(b"common entry")
    prepared = dict(prepared=True, variants={})
    for version in ("A", "B"):
        folder = root / version
        inputs = folder / "inputs"
        data = {
            "probe": version.encode(),
            "daemon": version.encode(),
            "worker": b"worker",
            "worker.rs": b"source",
            "root/worker": b"worker",
            "bundle/kernel.bin": b"kernel",
            "bundle/kernel.json": b"metadata",
        }
        for name, content in data.items():
            path = inputs / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        for name, content in (
            ("source.tar.gz", b"frozen source"),
            ("source-files.json", b"[]"),
            ("guest-supervisor", b"guest"),
        ):
            (folder / name).write_bytes(content)
        receipt = dict(
            embedded_fw=True,
            target="x86_64-unknown-linux-musl",
            profile="release",
            rustc="same compiler",
            zig="same zig",
            common_helper_sha256=subject.digest(helper),
            hashes={name: subject.digest(inputs / name) for name in data},
            source_archive_sha256=subject.digest(folder / "source.tar.gz"),
            source_files_sha256=subject.digest(folder / "source-files.json"),
            guest_supervisor_sha256=subject.digest(folder / "guest-supervisor"),
        )
        write(inputs / "receipt.json", receipt)
        prepared["variants"][version] = receipt
    write(root / "preparation.json", prepared)
    return prepared


class VersionPairTests(unittest.TestCase):
    def test_schedule_is_frozen_and_pairs_are_adjacent(self):
        plan = subject.schedule(5, False, 17)
        self.assertEqual(plan, subject.schedule(5, False, 17))
        self.assertNotEqual(plan, subject.schedule(5, False, 18))
        self.assertEqual(len(plan), 30)
        for a, b in zip(plan[::2], plan[1::2]):
            self.assertEqual(a["pair"], b["pair"])
            self.assertEqual({a["version"], b["version"]}, {"A", "B"})
        self.assertEqual(len({(r["round"], r["pattern"], r["version"]) for r in plan}), 30)

    def test_preflight_keeps_off_controls_and_cannot_replace_formal_rounds(self):
        plan = subject.schedule(1, True, 17)
        self.assertEqual(len(plan), 12)
        self.assertEqual({r["arm"] for r in plan}, {"pvisor-off", "pvisor-physical"})
        for rounds, preflight in ((4, False), (0, False), (2, True), (0, True)):
            with self.assertRaises(ValueError):
                subject.schedule(rounds, preflight, 17)

    def test_paired_cost_increase_and_decrease_have_explicit_direction(self):
        for delta, interpretation in ((5, "higher-cost"), (-5, "lower-cost")):
            result = subject.paired_summary(cases((delta,) * 5), 5)
            self.assertEqual(len(result), len(subject.PATTERNS) * len(subject.METRICS))
            for row in result:
                self.assertEqual(row["paired_differences"], [delta] * 5)
                self.assertEqual(row["paired_bootstrap_95"], [delta, delta])
                self.assertEqual(row["interpretation"], interpretation)

    def test_crossing_zero_is_not_proof_of_no_regression(self):
        result = subject.paired_summary(cases((-2, -1, 0, 1, 2)), 5)
        self.assertTrue(all(r["interpretation"] == "inconclusive" for r in result))

    def test_missing_duplicates_wrong_modes_and_bad_values_are_rejected(self):
        for failure in ("missing", "duplicate", "mode", "nan", "negative"):
            with self.subTest(failure=failure):
                rows = cases()
                if failure == "missing":
                    rows.pop()
                elif failure == "duplicate":
                    rows[-1] = copy.deepcopy(rows[0])
                elif failure == "mode":
                    rows[0]["arm"] = "pvisor-off"
                elif failure == "nan":
                    rows[0]["charged_mib"] = float("nan")
                else:
                    rows[0]["charged_mib"] = -1
                with self.assertRaises(ValueError):
                    subject.paired_summary(rows, 5)

    def test_crossing_difference_keeps_sampled_uncertainty(self):
        result = subject.paired_crossings(cases(), 5)
        for row in result:
            self.assertEqual(row["censored_pairs"], 0)
            self.assertAlmostEqual(row["median_difference_bracket"][0], 0.15)
            self.assertAlmostEqual(row["median_difference_bracket"][1], 0.25)

    def test_missed_crossing_remains_censored(self):
        rows = cases()
        rows[0]["sustained_drop"] = dict(reached=False, crossing_interval_seconds=None)
        first = subject.paired_crossings(rows, 5)[0]
        self.assertEqual(first["censored_pairs"], 1)
        self.assertIsNone(first["median_difference_bracket"])
        self.assertIsNone(first["conservative_bootstrap_95"])
        self.assertIsNone(first["paired_interval_differences"][0])

    def test_historical_and_contended_reports_cannot_be_promoted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for report in (
                dict(complete=True, preflight=True),
                dict(complete=False, diagnostic_complete=True, benchmark_eligible=False),
                dict(schema=subject.SCHEMA, complete=False),
            ):
                write(root / "report.json", report)
                with self.assertRaisesRegex(ValueError, "strict version"):
                    subject.audit(root)


class ArtifactTests(unittest.TestCase):
    def test_prepare_creates_only_an_unsent_execution_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_artifacts(root)
            output = root / "measurement"
            argv = [
                "version_compare",
                "--artifacts",
                str(root),
                "--output",
                str(output),
                "--preflight",
                "--prepare",
            ]
            with (
                patch.object(subject.sys, "argv", argv),
                patch.object(subject.sys, "stdout", io.StringIO()),
                patch.object(subject, "execution") as execute,
            ):
                self.assertEqual(subject.main(), 0)
                execute.assert_not_called()
            self.assertFalse(output.exists())
            command = json.loads(output.with_suffix(".input.json").read_text())["version-pairs"]
            self.assertIn("--preflight", command)
            self.assertIn("--rounds 1", command)

    def test_formal_sampling_requires_a_matched_audited_preflight(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_artifacts(root)
            output = root / "measurement"
            argv = ["version_compare", "--artifacts", str(root), "--output", str(output)]
            with (
                patch.object(subject.sys, "argv", argv),
                patch.object(subject.sys, "stderr", io.StringIO()),
                patch.object(subject, "execution") as execute,
            ):
                with self.assertRaises(SystemExit):
                    subject.main()
                execute.assert_not_called()
            self.assertFalse(output.exists())

    def test_matched_controls_are_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_artifacts(root)
            self.assertEqual(set(subject.artifacts(root)), {"A", "B"})

    def test_fw_toolchain_and_supervisor_mismatches_are_rejected(self):
        for failure in ("fw", "compiler", "supervisor", "helper"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                prepared = fake_artifacts(root)
                receipt = prepared["variants"]["B"]
                if failure == "fw":
                    path = root / "B/inputs/bundle/kernel.bin"
                    path.write_bytes(b"different kernel")
                    receipt["hashes"]["bundle/kernel.bin"] = subject.digest(path)
                elif failure == "compiler":
                    receipt["rustc"] = "different compiler"
                elif failure == "supervisor":
                    path = root / "B/guest-supervisor"
                    path.write_bytes(b"different supervisor")
                    receipt["guest_supervisor_sha256"] = subject.digest(path)
                else:
                    (root / "common/pool_memory_case.rs").write_bytes(b"different helper")
                write(root / "B/inputs/receipt.json", receipt)
                write(root / "preparation.json", prepared)
                with self.assertRaises(ValueError):
                    subject.artifacts(root)

    def test_changed_artifacts_and_incomplete_preparation_are_rejected(self):
        for failure in ("probe", "source", "prepared"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                prepared = fake_artifacts(root)
                if failure == "probe":
                    (root / "A/inputs/probe").write_bytes(b"changed")
                elif failure == "source":
                    (root / "A/source.tar.gz").write_bytes(b"changed")
                else:
                    prepared["prepared"] = False
                    write(root / "preparation.json", prepared)
                with self.assertRaises(ValueError):
                    subject.artifacts(root)


if __name__ == "__main__":
    unittest.main()
