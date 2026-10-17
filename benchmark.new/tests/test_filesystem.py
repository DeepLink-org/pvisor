"""Native filesystem rows validate fixed inputs, trials and Python timing evidence."""

import csv
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "filesystem"))


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


FILESYSTEM = module("filesystem_prepare_tests", ROOT / "filesystem/prepare.py")
AUDIT = module("filesystem_audit_tests", ROOT / "filesystem/audit.py")
from fs_timing import (  # noqa: E402
    FIELDS,
    FIXTURE_SCOPE,
    RUNS,
    SCOPE,
    WARMUP,
    measure,
    run_trial,
    summarize,
)
from update import publish, validate_batch  # noqa: E402
from workload import execute, prepare_fixture, validate_fixture  # noqa: E402

SEMSPEC = Path(os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec"))


class FilesystemContracts(unittest.TestCase):
    def test_interpreter_alias_requires_same_bin_directory_binary_and_remaining_arguments(self):
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

    def test_modified_command_or_extra_arguments_are_rejected_before_preparation(self):
        table = json.loads((ROOT / "filesystem/tools.json").read_text())
        for expression in (
            table["native-read"] + " --payload-mib 16",
            "$ python3 arbitrary.py @BATCH@",
            "$ hyperfine --runs 30 true",
        ):
            with self.subTest(expression=expression), workspace() as (root, resources):
                path = root / "in.json"
                path.write_text(json.dumps(table | {"native-read": expression}))
                with self.assertRaisesRegex(ValueError, "fixed metadata/read"):
                    FILESYSTEM.validate_template(path)

    def test_fixture_validation_rejects_modified_sizes_contents_and_inventory(self):
        with workspace() as (root, resources):
            fixture = root / "fixture"
            prepare_fixture(fixture)
            validate_fixture(fixture)
            self.assertEqual(
                execute("metadata", fixture), dict(workload="metadata", correctness="passed")
            )
            self.assertEqual(execute("read", fixture), dict(workload="read", correctness="passed"))
            path = fixture / "fixture.json"
            original = path.read_bytes()
            for key, value in (("files", 1000), ("payload_bytes", 16 * 1024**2)):
                declared = json.loads(original)
                declared[key] = value
                path.write_text(json.dumps(declared))
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

    def test_failed_sample_keeps_diagnostics_and_cannot_write_a_summary(self):
        with workspace() as (root, resources):
            batch = FILESYSTEM.prepare(
                ROOT / "filesystem/tools.json", root / ".data/batch", row="native-read"
            )
            with patch(
                "fs_timing.run_trial", side_effect=[1, 1, 1, ValueError("incorrect output")]
            ):
                with self.assertRaisesRegex(ValueError, "incorrect output"):
                    measure(batch, "read")
            self.assertFalse((batch / "read.samples.json").exists())
            with patch(
                "fs_timing.subprocess.run",
                return_value=subprocess.CompletedProcess([], 7, "failed", "diagnostic"),
            ):
                with self.assertRaisesRegex(ValueError, "subject failed"):
                    run_trial(batch, "read", "sample", 0)
            failed = json.loads((batch / "trials/read/sample-000.json").read_text())
            self.assertEqual(failed["exit_code"], 7)
            self.assertEqual(failed["stderr"], "diagnostic")
            self.assertIn("error", failed)

    def test_timeout_retains_captured_output(self):
        with workspace() as (root, resources):
            timeout = subprocess.TimeoutExpired(
                ["subject"], 30, output=b"partial", stderr=b"failed"
            )
            with patch("fs_timing.subprocess.run", side_effect=timeout):
                with self.assertRaises(subprocess.TimeoutExpired):
                    run_trial(root, "metadata", "preflight", 0)
            record = json.loads((root / "trials/metadata/preflight-000.json").read_text())
            self.assertEqual(record["stdout"], "partial")
            self.assertIn("error", record)


@unittest.skipUnless(SEMSPEC.is_file(), "built semspec required")
class FilesystemIntegration(unittest.TestCase):
    def bench(self, batch):
        result = subprocess.run(
            [str(SEMSPEC), "bench", "--cwd", str(batch), str(batch / "in.json")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_two_rows_share_one_fixture_and_publish_only_audited_samples(self):
        with workspace() as (root, resources):
            batch = FILESYSTEM.prepare(ROOT / "filesystem/tools.json", root / ".data/batch")
            self.bench(batch)
            for mode in ("metadata", "read"):
                self.assertEqual(
                    len(list((batch / "trials" / mode).glob("*.json"))), 1 + WARMUP + RUNS
                )
            self.assertEqual(len(list(batch.rglob("payload.bin"))), 1)
            with self.assertRaisesRegex(ValueError, "reuse a measured"):
                measure(batch, "read")
            manifest, table, results = validate_batch(batch)
            self.assertEqual(manifest["metric_scope"], SCOPE)
            self.assertEqual(manifest["fixture_scope"], FIXTURE_SCOPE)
            self.assertEqual(manifest["measurement_engine"], "python-perf_counter_ns")
            self.assertEqual(set(results), {"native-metadata", "native-read"})
            self.assertTrue(all(row["N"] == RUNS for row in table.values()))
            csv_path, markdown = root / "result.csv", root / "table.md"
            render = publish(batch, csv_path, markdown)
            before = {path: path.read_bytes() for path in (csv_path, render)}
            publish(batch, csv_path, markdown)
            self.assertEqual(csv_path.read_bytes(), before[csv_path])
            with csv_path.open() as stream:
                rows = list(csv.DictReader(stream))
            self.assertTrue(
                all(row["measurement_engine"] == "python-perf_counter_ns" for row in rows)
            )
            sample = batch / "trials/read/sample-000.json"
            record = json.loads(sample.read_text())
            record["duration_ns"] += 1
            sample.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "monotonic duration"):
                publish(batch, csv_path, markdown)
            self.assertEqual({path: path.read_bytes() for path in before}, before)

    def test_single_row_preserves_fixed_workload_and_rejects_tampered_statistics(self):
        with workspace() as (root, resources):
            batch = FILESYSTEM.prepare(
                ROOT / "filesystem/tools.json", root / ".data/batch", row="native-metadata"
            )
            self.assertEqual(list(json.loads((batch / "in.json").read_text())), ["native-metadata"])
            self.bench(batch)
            path = batch / "metadata.samples.json"
            raw = json.loads(path.read_text())
            raw["result"]["Median ms"] += 1
            path.write_text(json.dumps(raw))
            with self.assertRaisesRegex(ValueError, "raw samples or statistics"):
                validate_batch(batch)
            self.assertEqual(
                json.loads((batch / "manifest.json").read_text())["status"], "prepared"
            )


if __name__ == "__main__":
    unittest.main()
