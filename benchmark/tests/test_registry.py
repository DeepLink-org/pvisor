"""Criterion registration/retirement contracts, without product measurements."""

import csv
import json
import os
import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def registry():
    with (ROOT / "registry.csv").open() as stream:
        reader = csv.DictReader(stream)
        rows = list(reader)
        return reader.fieldnames, rows


class CriterionRegistrationContracts(unittest.TestCase):
    def test_archive_has_no_measurement_api(self):
        archive = runpy.run_path(str(ROOT / "archive.py"))
        for name in ("repeat", "capture", "command_trial"):
            self.assertNotIn(name, archive)

    def test_all_registered_ids_entries_and_readiness_remain_explicit(self):
        fields, rows = registry()
        self.assertEqual(fields, ["benchmark_id", "role", "entry", "status", "implementation"])
        self.assertEqual(len(rows), 67)
        self.assertEqual(len({row["entry"] for row in rows}), 67)
        expected_ids = {
            line.split("：")[0].removeprefix("### ")
            for line in (ROOT / "REGISTRY.md").read_text().splitlines()
            if line.startswith("### B-")
        } - {"B-OVERVIEW / B-METHOD", "B-COMPARE", "B-CLUSTER"}
        self.assertEqual({row["benchmark_id"] for row in rows}, expected_ids)
        self.assertEqual(sum(row["status"] == "criterion-rust" for row in rows), 34)
        self.assertEqual(
            sum(row["status"] == "requires-single-condition-command" for row in rows), 33
        )
        for row in rows:
            with self.subTest(entry=row["entry"]):
                self.assertIn(row["role"], ("user-facing", "engineering A/B", "diagnostic"))
                entry = ROOT / row["entry"]
                self.assertTrue(entry.is_file(), entry)
                if row["status"] == "criterion-rust":
                    implementation = ROOT / row["implementation"]
                    self.assertTrue(implementation.is_file(), implementation)
                    self.assertEqual(implementation.suffix, ".rs")
                    self.assertTrue(
                        implementation.resolve().is_relative_to((ROOT / "criterion/src").resolve())
                    )
                else:
                    self.assertEqual(row["status"], "requires-single-condition-command")
                    self.assertEqual(row["implementation"], "")
                    self.assertEqual(entry.suffix, ".json")
                    table = json.loads(entry.read_text())
                    self.assertTrue(table)
                    self.assertTrue(
                        all(
                            isinstance(cells, dict)
                            and cells
                            and all(value is None for value in cells.values())
                            for cells in table.values()
                        )
                    )

    def test_all_active_descriptors_point_to_rust_not_python_commands(self):
        _, rows = registry()
        for row in rows:
            if row["status"] != "criterion-rust":
                continue
            with self.subTest(entry=row["entry"]):
                entry = ROOT / row["entry"]
                text = entry.read_text()
                self.assertNotIn("<!--", text)
                if entry.suffix == ".json":
                    descriptor = json.loads(text)
                    self.assertEqual(descriptor["engine"], "criterion")
                    self.assertEqual(descriptor["entry"], row["entry"])
                    self.assertEqual(
                        (entry.parent / descriptor["implementation"]).resolve(),
                        (ROOT / row["implementation"]).resolve(),
                    )
                    self.assertFalse(
                        any(
                            isinstance(value, str) and value.lstrip().startswith("$ ")
                            for value in descriptor.values()
                        )
                    )
                    self.assertNotIn("$ python", text)
                    self.assertNotIn("command_timing.py", text)
                else:
                    self.assertEqual(entry.suffix, ".md")
                    self.assertIn("Criterion", text)
                    self.assertIn("criterion/", text)

    def test_shared_rust_entry_owns_registry_and_safe_failure(self):
        source = (ROOT / "criterion/src/main.rs").read_text()
        self.assertIn('include_str!("../../registry.csv")', source)
        for alias in ('"smoke"', '"nightly"', '"replay"'):
            self.assertIn(alias, source)
        self.assertIn("requires-single-condition-command", source)
        self.assertIn("unprepared", source)
        self.assertIn("std::process::exit(1)", source)
        self.assertNotIn('Command::new("python', source)
        self.assertNotIn("command_timing.py", source)

    def test_no_python_measurement_dispatcher_or_timer_shims(self):
        for name in ("run.py", "command_timing.py", "filesystem/fs_timing.py"):
            self.assertFalse((ROOT / name).exists(), name)


@unittest.skipUnless(
    os.environ.get("PVISOR_BENCH_TEST_BINARY"),
    "optional prebuilt Criterion CLI; no product benchmark is run",
)
class NativeCriterionEntryContracts(unittest.TestCase):
    def invoke(self, *arguments, cwd):
        binary = Path(os.environ["PVISOR_BENCH_TEST_BINARY"]).resolve(strict=True)
        environment = os.environ.copy()
        for name in ("PVISOR_BENCH_BINARY", "PVISOR_BENCH_OUTPUT"):
            environment.pop(name, None)
        return subprocess.run(
            [str(binary), *arguments],
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_list_is_exact_registry_and_has_no_side_effects(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.invoke("--list", cwd=directory)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, (ROOT / "registry.csv").read_text())
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_unknown_and_unprepared_fail_before_subject_or_output_creation(self):
        for entry, message in (
            ("not-a-real-entry", "unknown"),
            ("network/network.json", "unprepared"),
        ):
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "must-not-exist"
                result = self.invoke(entry, "--output", str(output), cwd=directory)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr.lower())
                self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
