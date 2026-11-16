"""Fixture helpers and read-only auditing of historical filesystem evidence."""

import csv
import importlib.util
import json
import shlex
import sys
import unittest
from copy import deepcopy
from pathlib import Path

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "filesystem"))
SPEC = importlib.util.spec_from_file_location(
    "filesystem_archive_audit", ROOT / "filesystem/audit.py"
)
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)
from common import digest, save  # noqa: E402
from filesystem.historical import (  # noqa: E402
    FIELDS,
    FIXTURE_SCOPE,
    ROWS,
    RUNS,
    SCOPE,
    WARMUP,
    command,
    summarize,
)
from update import publish, validate_batch  # noqa: E402
from workload import execute, prepare_fixture, validate_fixture  # noqa: E402


def frozen_filesystem_batch(batch, modes=("metadata", "read")):
    """Freeze synthetic original-format observations, without sampling a subject."""
    batch.mkdir(parents=True)
    prepare_fixture(batch / "fixture")
    template, resolved = {}, {}
    evidence = dict(
        format=1, cwd=str(batch), template=str(batch / "in.json"), cells=[], commands=[], rows=[]
    )
    for mode in modes:
        label = "native-" + mode
        expression = shlex.join(
            ["python3", str(ROOT / "filesystem/fs_timing.py"), str(batch), mode]
        )
        template[label] = "$ " + expression
        samples = []
        phases = (
            [("preflight", 0)]
            + [("warmup", i) for i in range(WARMUP)]
            + [("sample", i) for i in range(RUNS)]
        )
        for i, (phase, index) in enumerate(phases):
            duration = (index + 1) * 1_000_000 if phase == "sample" else 500_000_000
            start = (i + 1) * 1_000_000_000
            save(
                batch / "trials" / mode / f"{phase}-{index:03d}.json",
                dict(
                    command=command(batch, mode),
                    phase=phase,
                    index=index,
                    start_ns=start,
                    end_ns=start + duration,
                    duration_ns=duration,
                    exit_code=0,
                    stdout=json.dumps(dict(workload=mode, correctness="passed")),
                    stderr="",
                ),
            )
            if phase == "sample":
                samples.append(duration)
        result = summarize(samples)
        save(
            batch / f"{mode}.samples.json",
            dict(
                engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result
            ),
        )
        resolved[label] = result
        evidence["commands"].append(
            dict(kind="script-row", command=expression, raw=result, value=result)
        )
        evidence["rows"].append(
            dict(row=label, command=expression, expression=expression, columns=list(FIELDS))
        )
    save(batch / "in.json", template)
    save(batch / "in.bench.json", evidence)
    save(batch / "in.render.json", resolved)
    save(batch / "harness/source.json", {"historical": True})
    manifest = dict(
        status="prepared",
        batch=batch.name,
        benchmark_id="B-FS-TOOLS",
        role="user-facing",
        entry="filesystem/tools.md",
        metric_scope=SCOPE,
        fixture_scope=FIXTURE_SCOPE,
        runs=RUNS,
        warmup=WARMUP,
        rows={row: row for row in template},
        conditions=list(template),
        cwd=str(batch),
        measurement_engine="python-perf_counter_ns",
        python_version="historical-test",
        input_sha256=digest(batch / "in.json"),
        artifacts={str(batch / "fixture/fixture.json"): digest(batch / "fixture/fixture.json")},
        harness={"source.json": digest(batch / "harness/source.json")},
    )
    save(batch / "manifest.json", manifest)
    return manifest, evidence, resolved, template


class FilesystemContracts(unittest.TestCase):
    def test_interpreter_alias_requires_same_bin_directory_binary_and_arguments(self):
        with workspace() as (root, resources):
            binary = root / "interpreter"
            binary.touch()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            python, python3 = bin_dir / "python", bin_dir / "python3"
            python.symlink_to(binary)
            python3.symlink_to(binary)
            expected = [str(python), "workload.py", "metadata", "fixture"]
            self.assertTrue(AUDIT._same_trial_command(expected, expected))
            self.assertTrue(AUDIT._same_trial_command([str(python3), *expected[1:]], expected))
            other_bin = root / "other-bin"
            other_bin.mkdir()
            other_python = other_bin / "python3"
            other_python.symlink_to(binary)
            other_binary = bin_dir / "other-interpreter"
            other_binary.touch()
            for altered in (
                [str(other_python), *expected[1:]],
                [str(other_binary), *expected[1:]],
                [str(python3), "other-workload.py", *expected[2:]],
                [*expected, "--extra"],
                ["python3", *expected[1:]],
            ):
                with self.subTest(command=altered):
                    self.assertFalse(AUDIT._same_trial_command(altered, expected))

    def test_fixture_validation_rejects_modified_sizes_contents_and_inventory(self):
        with workspace() as (root, resources):
            fixture = root / "fixture"
            prepare_fixture(fixture)
            validate_fixture(fixture)
            for mode in ("metadata", "read"):
                self.assertEqual(execute(mode, fixture), dict(workload=mode, correctness="passed"))
            path = fixture / "fixture.json"
            original = path.read_bytes()
            for key, value in (("files", 1000), ("payload_bytes", 16 * 1024**2)):
                path.write_text(json.dumps(json.loads(original) | {key: value}))
                with self.assertRaisesRegex(ValueError, "configuration changed"):
                    validate_fixture(fixture)
            path.write_bytes(original)
            tree_file = fixture / "tree/d00/f000000.txt"
            original = tree_file.read_bytes()
            tree_file.write_bytes(b"x" * len(original))
            with self.assertRaisesRegex(ValueError, "content changed"):
                validate_fixture(fixture)
            tree_file.write_bytes(original)
            extra = fixture / "tree/extra"
            extra.touch()
            with self.assertRaisesRegex(ValueError, "inventory changed"):
                validate_fixture(fixture)
            extra.unlink()
            with (fixture / "payload.bin").open("r+b") as stream:
                stream.write(b"corrupted")
            with self.assertRaisesRegex(ValueError, "payload changed"):
                validate_fixture(fixture)
            with self.assertRaisesRegex(ValueError, "payload differs"):
                execute("read", fixture)

    def test_statistics_use_nanoseconds_and_sample_standard_deviation(self):
        self.assertEqual(
            summarize([1_000_000, 3_000_000, 5_000_000]), dict(zip(FIELDS, (3, 3, 3, 2, 1, 5)))
        )
        for samples in ([1], [True, 2], [0, 2], [1.5, 2]):
            with self.subTest(samples=samples), self.assertRaises(ValueError):
                summarize(samples)

    def test_archive_publication_is_idempotent_and_failed_audit_preserves_outputs(self):
        with workspace() as (root, resources):
            batch = root / ".data/batch"
            frozen_filesystem_batch(batch)
            manifest, table, results = validate_batch(batch)
            self.assertEqual(set(results), set(ROWS))
            self.assertEqual(len(list(batch.rglob("payload.bin"))), 1)
            self.assertTrue(
                all(row["N"] == RUNS and row["Median ms"] == 15.5 for row in table.values())
            )
            csv_path, markdown = root / "result.csv", root / "table.md"
            render = publish(batch, csv_path, markdown)
            before = {path: path.read_bytes() for path in (csv_path, render, markdown)}
            publish(batch, csv_path, markdown)
            self.assertEqual({path: path.read_bytes() for path in before}, before)
            with csv_path.open() as stream:
                self.assertTrue(
                    all(
                        row["measurement_engine"] == "python-perf_counter_ns"
                        for row in csv.DictReader(stream)
                    )
                )
            path = batch / "trials/read/sample-000.json"
            record = json.loads(path.read_text())
            save(path, record | {"duration_ns": record["duration_ns"] + 1})
            with self.assertRaisesRegex(ValueError, "monotonic duration"):
                publish(batch, csv_path, markdown)
            self.assertEqual({path: path.read_bytes() for path in before}, before)

    def test_archive_rejects_trial_changes_including_preflight_and_warmup(self):
        with workspace() as (root, resources):
            batch = root / "batch"
            args = frozen_filesystem_batch(batch, ("metadata",))
            for phase in ("preflight", "warmup", "sample"):
                path = batch / f"trials/metadata/{phase}-000.json"
                original = path.read_bytes()
                for key, value in (
                    ("command", ["arbitrary"]),
                    ("phase", "wrong"),
                    ("index", 99),
                    ("exit_code", 7),
                    ("stdout", '{"correctness":"failed"}'),
                    ("duration_ns", 1),
                    ("start_ns", True),
                    ("end_ns", 0),
                    ("error", "timeout with captured output"),
                ):
                    with self.subTest(phase=phase, key=key):
                        save(path, json.loads(original) | {key: value})
                        with self.assertRaises(ValueError):
                            AUDIT.audit_filesystem(batch, *args)
                path.write_bytes(original)
            extra = batch / "trials/metadata/sample-999.json"
            extra.write_text("{}")
            with self.assertRaisesRegex(ValueError, "missing filesystem"):
                AUDIT.audit_filesystem(batch, *args)
            extra.unlink()
            (batch / "trials/metadata/warmup-000.json").unlink()
            with self.assertRaisesRegex(ValueError, "missing filesystem"):
                AUDIT.audit_filesystem(batch, *args)

    def test_archive_rejects_argv_protocol_mapping_and_statistics_tampering(self):
        with workspace() as (root, resources):
            batch = root / "batch"
            args = frozen_filesystem_batch(batch, ("read",))
            for position, key, value in (
                (0, "measurement_engine", "criterion-rust"),
                (0, "runs", 29),
                (0, "fixture_scope", "different"),
                (1, "rows", []),
                (3, "native-read", args[3]["native-read"] + " --payload-mib 16"),
                (3, "native-read", "$ printf 1"),
                (2, "native-read", args[2]["native-read"] | {"Median ms": 0}),
            ):
                altered = deepcopy(args)
                altered[position][key] = value
                with self.subTest(position=position, key=key), self.assertRaises(ValueError):
                    AUDIT.audit_filesystem(batch, *altered)
            source = batch / "harness/source.json"
            original = source.read_bytes()
            source.write_text('{"historical": false}')
            with self.assertRaisesRegex(ValueError, "frozen harness changed"):
                validate_batch(batch)
            source.write_bytes(original)
            path = batch / "read.samples.json"
            raw = json.loads(path.read_text())
            raw["result"]["Median ms"] += 1
            save(path, raw)
            with self.assertRaisesRegex(ValueError, "raw samples or statistics"):
                AUDIT.audit_filesystem(batch, *args)


if __name__ == "__main__":
    unittest.main()
