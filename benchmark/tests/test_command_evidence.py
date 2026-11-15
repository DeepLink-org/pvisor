"""Retained command evidence audits; no Python benchmark subject is executed."""

import csv
import importlib.util
import io
import json
import shlex
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import common as COMMON  # noqa: E402
from archive import summarize  # noqa: E402
from update import display_table, publish, validate_batch  # noqa: E402


class RetainedCommandAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="retained command evidence ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.batch = self.root / ".data/batch"
        self.csv = self.root / "results.csv"
        self.md = self.root / "results.md"
        self.make_archive(self.batch)

    def make_archive(self, batch, names=("checked",)):
        """Construct recorded evidence, never a measuring runner or fake execution."""
        batch.mkdir(parents=True)
        artifact = batch / "subject"
        artifact.write_text("frozen bytes, not an executable\n")
        harness = batch / "harness/observer.py"
        harness.parent.mkdir()
        harness.write_text("# retained observer bytes\n")
        table, resolved, commands, records, rows = {}, {}, {}, [], []
        for condition in names:
            # These historical strings are verified as data, not dispatched.
            argv = ["python3", str(ROOT / "command_timing.py"), str(batch), condition]
            command = shlex.join(argv)
            table[condition] = "$ " + command
            config = dict(
                argv=[str(artifact), "--condition", condition],
                cwd=str(self.root),
                env={"FIXTURE": "0"},
                prepare=["/never-execute/hook", "prepare"],
                conclude=["/never-execute/hook", "conclude"],
                cleanup=["/never-execute/hook", "cleanup"],
                expected_stdout="prepared\n",
                timeout=30,
            )
            commands[condition] = config
            values = [1_000_000, 3_000_000]
            result = summarize(values)
            resolved[condition] = result
            for phase, index, duration in (
                ("warmup", 0, 2_000_000),
                ("sample", 0, values[0]),
                ("sample", 1, values[1]),
            ):
                COMMON.save(
                    batch / "command-trials" / condition / f"{phase}-{index:04d}.json",
                    dict(
                        condition=condition,
                        phase=phase,
                        index=index,
                        command=config["argv"],
                        environment=config["env"],
                        exit_code=0,
                        stdout="prepared\n",
                        stderr="",
                        start_ns=100,
                        end_ns=100 + duration,
                        duration_ns=duration,
                        hooks=[
                            dict(action=action, command=config[action], exit_code=0)
                            for action in ("prepare", "conclude", "cleanup")
                        ],
                    ),
                )
            COMMON.save(
                batch / f"{condition}.samples.json",
                dict(
                    engine="python-perf_counter_ns",
                    unit="nanosecond",
                    samples=values,
                    result=result,
                ),
            )
            records.append(dict(command=command, kind="script-row", raw=result, value=result))
            rows.append(
                dict(row=condition, command=command, expression=command, columns=list(result))
            )
        COMMON.save(batch / "in.json", table)
        COMMON.save(batch / "commands.json", commands)
        COMMON.save(batch / "in.render.json", resolved)
        COMMON.save(
            batch / "in.bench.json",
            dict(
                format=1,
                cwd=str(self.root),
                template=str(batch / "in.json"),
                commands=records,
                rows=rows,
                cells=[],
            ),
        )
        COMMON.save(
            batch / "manifest.json",
            dict(
                batch=batch.name,
                entry="process/retained-fixture.json",
                status="prepared",
                benchmark_id="B-PROCESS",
                role="engineering A/B",
                metric_scope=COMMON.METRIC_SCOPE,
                measurement_engine="python-perf_counter_ns",
                python_version="retained-test-fixture",
                platform="retained-test-host",
                cpu_affinity=[0, 1],
                runs=2,
                warmup=1,
                cwd=str(self.root),
                conditions=list(names),
                rows={name: name for name in names},
                artifacts={str(artifact): COMMON.digest(artifact)},
                harness={"observer.py": COMMON.digest(harness)},
                input_sha256=COMMON.digest(batch / "in.json"),
            ),
        )

    def test_all_statistics_share_one_export_and_publication_is_idempotent(self):
        manifest, resolved, results = validate_batch(self.batch)
        evidence = json.loads((self.batch / "in.bench.json").read_text())
        self.assertEqual(manifest["status"], "passed")
        self.assertEqual(len(evidence["commands"]), 1)
        self.assertEqual(len(evidence["rows"]), 1)
        self.assertEqual(evidence["cells"], [])
        self.assertEqual(resolved["checked"]["N"], 2)
        self.assertEqual(resolved["checked"], results["checked"]["statistics"])
        publish(self.batch, self.csv, self.md)
        exported = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        self.assertEqual(float(exported[0]["Median ms"]), resolved["checked"]["Median ms"])
        self.assertIn('{{ table("results.render.json") }}', self.md.read_text())
        original = self.csv.read_bytes(), self.md.read_bytes()
        publish(self.batch, self.csv, self.md)
        self.assertEqual(original, (self.csv.read_bytes(), self.md.read_bytes()))

    def test_historical_process_gate_validates_all_four_conditions_and_failed_manifest(self):
        spec = importlib.util.spec_from_file_location(
            "retained_process_gate", ROOT / "process/compare.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        batch = self.batch.with_name("four-conditions")
        self.make_archive(batch, names=("native", "host-run", "status", "review"))
        evidence = batch / "in.bench.json"
        table, regressions = module.compare(evidence, evidence, 15)
        self.assertEqual(regressions, 0)
        self.assertEqual(table.count("within threshold"), 4)
        manifest = json.loads((batch / "manifest.json").read_text())
        manifest["status"] = "failed"
        COMMON.save(batch / "manifest.json", manifest)
        with self.assertRaises(ValueError):
            module.compare(evidence, None, 15)

    def test_failed_payload_hooks_timeout_or_artifact_change_cannot_publish(self):
        trial = self.batch / "command-trials/checked/warmup-0000.json"
        original = trial.read_bytes()
        original_artifact = (self.batch / "subject").read_bytes()
        for field, value in (
            ("exit_code", 7),
            ("error", "command timed out"),
            ("cleanup_error", "cleanup failed"),
            ("hooks", []),
        ):
            with self.subTest(field=field):
                record = json.loads(original)
                record[field] = value
                COMMON.save(trial, record)
                with self.assertRaises(ValueError):
                    publish(self.batch, self.csv, self.md)
                self.assertFalse(self.csv.exists())
                trial.write_bytes(original)
        (self.batch / "subject").write_text("changed")
        with self.assertRaisesRegex(ValueError, "artifact changed"):
            publish(self.batch, self.csv, self.md)
        (self.batch / "subject").write_bytes(original_artifact)

    def test_every_correctness_hook_is_checked_for_warmups_and_samples(self):
        for trial in (self.batch / "command-trials/checked").glob("*.json"):
            original = trial.read_bytes()
            for index in range(3):
                with self.subTest(trial=trial.name, hook=index):
                    record = json.loads(original)
                    record["hooks"][index]["exit_code"] = 1
                    COMMON.save(trial, record)
                    with self.assertRaisesRegex(ValueError, "correctness hook"):
                        validate_batch(self.batch)
                    trial.write_bytes(original)

    def test_tampered_trials_samples_and_mapping_are_rejected(self):
        original = {p: p.read_bytes() for p in self.batch.rglob("*.json")}
        cases = [
            ("command-trials/checked/sample-0000.json", "exit_code", 7),
            ("command-trials/checked/sample-0000.json", "duration_ns", 1),
            ("command-trials/checked/warmup-0000.json", "index", 5),
            ("checked.samples.json", "unit", "second"),
            ("in.bench.json", "cells", [{"altered": True}]),
        ]
        for name, key, value in cases:
            with self.subTest(name=name, key=key):
                path = self.batch / name
                data = json.loads(path.read_text())
                data[key] = value
                COMMON.save(path, data)
                with self.assertRaises(ValueError):
                    validate_batch(self.batch)
                for path, contents in original.items():
                    path.write_bytes(contents)
        path = self.batch / "in.bench.json"
        data = json.loads(path.read_text())
        data["rows"][0]["row"] = "wrong"
        COMMON.save(path, data)
        with self.assertRaisesRegex(ValueError, "row matrix"):
            validate_batch(self.batch)
        path.write_bytes(original[path])
        (self.batch / "command-trials/checked/sample-0000.json").unlink()
        with self.assertRaisesRegex(ValueError, "trials"):
            validate_batch(self.batch)

    def test_tampered_resolved_values_and_frozen_harness_cannot_publish(self):
        path = self.batch / "in.render.json"
        original = path.read_bytes()
        data = json.loads(original)
        data["checked"]["Median ms"] = 0
        COMMON.save(path, data)
        with self.assertRaisesRegex(ValueError, "resolved row"):
            publish(self.batch, self.csv, self.md)
        path.write_bytes(original)
        (self.batch / "harness/observer.py").write_text("changed")
        with self.assertRaisesRegex(ValueError, "frozen harness changed"):
            publish(self.batch, self.csv, self.md)
        self.assertFalse(self.csv.exists())

    def test_passed_export_checksum_prevents_rewriting_published_results(self):
        publish(self.batch, self.csv, self.md)
        original = self.csv.read_bytes(), self.md.read_bytes()
        for name in ("in.render.json", "in.bench.json", "commands.json", "checked.samples.json"):
            with self.subTest(name=name):
                path = self.batch / name
                contents = path.read_bytes()
                path.write_bytes(contents + b" ")
                with self.assertRaisesRegex(ValueError, "checksum"):
                    publish(self.batch, self.csv, self.md)
                self.assertEqual(original, (self.csv.read_bytes(), self.md.read_bytes()))
                path.write_bytes(contents)

    def test_schema_mismatch_preserves_public_results_and_raw_is_protected(self):
        self.csv.write_text("legacy,value\nold,1\n")
        self.md.write_text("historical\n")
        original = self.csv.read_bytes(), self.md.read_bytes()
        with self.assertRaisesRegex(ValueError, "schema differs"):
            publish(self.batch, self.csv, self.md)
        self.assertEqual(original, (self.csv.read_bytes(), self.md.read_bytes()))
        with self.assertRaisesRegex(ValueError, "outside raw evidence"):
            publish(self.batch, self.root / "other.csv", self.batch / "in.json")

    def test_new_batch_appends_without_rewriting_previous_rows(self):
        publish(self.batch, self.csv, self.md)
        first = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        second = self.batch.with_name("second")
        self.make_archive(second)
        publish(second, self.csv, self.md)
        combined = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        self.assertEqual(combined[: len(first)], first)
        self.assertEqual(len(combined), 2)

    def test_incomplete_conditions_and_failed_manifests_are_rejected(self):
        path = self.batch / "manifest.json"
        original = json.loads(path.read_text())
        for change in (
            {"status": "failed"},
            {"conditions": []},
            {"measurement_engine": "criterion"},
        ):
            with self.subTest(change=change):
                COMMON.save(path, original | change)
                with self.assertRaises(ValueError):
                    validate_batch(self.batch)
                COMMON.save(path, original)


class PresentationTests(unittest.TestCase):
    def test_public_summary_excludes_harnesses_from_both_directory_names(self):
        artifacts = {
            "/repo/benchmark/startup/timing.json": "current-harness",
            "/repo/benchmark.new/startup/timing.json": "historical-harness",
            "/repo/benchmark/.data/cohort/inputs/rootfs.img": "input-bytes",
        }
        self.assertEqual(COMMON.artifact_summary(artifacts), {"inputs/rootfs.img": "input-bytes"})

    def test_display_format_changes_only_presentation(self):
        table = {"native": {"N": 60, "Median ms": 1.234567, "note": "unchanged", "Min ms": None}}
        self.assertEqual(
            display_table(table),
            {"native": {"N": 60, "Median ms": "1.23", "note": "unchanged", "Min ms": None}},
        )
        self.assertEqual(table["native"]["Median ms"], 1.234567)

    def test_historical_statistics_keep_original_values_and_reject_invalid_durations(self):
        self.assertEqual(
            summarize([1_000_000, 3_000_000]),
            {
                "N": 2,
                "Median ms": 2.0,
                "Mean ms": 2.0,
                "SD ms": 2**0.5,
                "Min ms": 1.0,
                "Max ms": 3.0,
            },
        )
        for samples in ([1], [0, 1], [True, 2], [1.2, 2]):
            with self.subTest(samples=samples), self.assertRaises(ValueError):
                summarize(samples)


if __name__ == "__main__":
    unittest.main()
