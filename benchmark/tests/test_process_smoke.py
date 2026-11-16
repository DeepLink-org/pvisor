"""Rust process migration guards and read-only historical comparison contracts."""

import csv
import importlib.util
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))


def comparison():
    spec = importlib.util.spec_from_file_location(
        "retained_process_compare", ROOT / "process/compare.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ProcessCriterionContracts(unittest.TestCase):
    def test_profiles_use_registered_rust_implementations(self):
        with (ROOT / "registry.csv").open() as stream:
            rows = {row["entry"]: row for row in csv.DictReader(stream)}
        for name in ("smoke", "nightly"):
            with self.subTest(name=name):
                entry = f"process/{name}.md"
                row = rows[entry]
                self.assertEqual(row["benchmark_id"], "B-PROCESS")
                self.assertEqual(row["role"], "engineering A/B")
                self.assertEqual(row["status"], "criterion-rust")
                implementation = ROOT / row["implementation"]
                self.assertTrue(implementation.is_file())
                text = (ROOT / entry).read_text()
                self.assertIn("Criterion", text)
                self.assertNotIn("<!--", text)
                self.assertFalse((ROOT / f"process/{name}.json").exists())
                self.assertFalse((ROOT / f"process/{name}.commands.json").exists())

    def test_process_implementation_keeps_controls_and_native_bundle_validation(self):
        source = (ROOT / "criterion/src/process.rs").read_text()
        for required in (
            '"native"',
            '"host-run"',
            '"status"',
            '"review"',
            '"--no-agent-defaults"',
            '"--overlaynet"',
            '"off"',
            '"capture"',
            '"PVISOR_RUN_HOME"',
            '"PVISOR_STARTUP_TIMING"',
            '"PVISOR_FS_PROFILE"',
            '"run-bundle.json"',
            '"completed"',
            '"exit_code"',
            "SamplingMode::Flat",
            "iter_custom",
            "measured_total",
            "bundle_success_is_not_inferred_from_process_exit",
        ):
            self.assertIn(required, source)
        # Sampling/profile behavior belongs to Rust's contract tests. These
        # guards prevent restoring the old executable Markdown/Python pipeline.
        self.assertNotIn("command_timing.py", source)
        self.assertNotIn('Command::new("python', source)


class HistoricalProcessComparisonContracts(unittest.TestCase):
    def setUp(self):
        self.module = comparison()
        self.values = {name: 1.0 for name in ("native", "host-run", "status", "review")}
        self.manifest = dict(
            runs=10,
            warmup=2,
            measurement_engine="python-perf_counter_ns",
            python_version="historical",
            platform="historical",
            cpu_affinity=[0, 1],
            conditions=list(self.values),
            metric_scope="historical",
            harness={"frozen.py": "bytes"},
        )

    def test_different_sampling_clock_host_or_harness_cannot_be_compared(self):
        for key, altered in (
            ("runs", 50),
            ("warmup", 10),
            ("measurement_engine", "criterion"),
            ("python_version", "different"),
            ("platform", "different"),
            ("cpu_affinity", [2, 3]),
            ("conditions", ["native"]),
            ("metric_scope", "different"),
            ("harness", {"other": "bytes"}),
        ):
            with (
                self.subTest(key=key),
                patch.object(
                    self.module,
                    "read",
                    side_effect=[
                        (self.values, self.manifest),
                        (self.values, self.manifest | {key: altered}),
                    ],
                ),
                self.assertRaises(ValueError),
            ):
                self.module.compare(
                    Path("candidate/in.bench.json"), Path("baseline/in.bench.json"), 15
                )

    def test_threshold_counts_all_four_conditions_and_candidate_only_remains_explicit(self):
        with patch.object(
            self.module,
            "read",
            side_effect=[
                ({name: 1.16 for name in self.values}, self.manifest),
                (self.values, self.manifest),
            ],
        ):
            report, regressions = self.module.compare(
                Path("candidate/in.bench.json"), Path("baseline/in.bench.json"), 15
            )
        self.assertEqual(regressions, 4)
        self.assertEqual(report.count("| regression |"), 4)
        with patch.object(self.module, "read", return_value=(self.values, self.manifest)):
            report, regressions = self.module.compare(Path("candidate/in.bench.json"), None, 15)
        self.assertEqual(regressions, 0)
        self.assertEqual(report.count("candidate-only"), 4)

    def test_read_requires_successful_complete_process_archive(self):
        with (
            patch.object(
                self.module.UPDATE,
                "validate_batch",
                return_value=({"benchmark_id": "B-PROCESS"}, {}, {"native": {}}),
            ),
            self.assertRaisesRegex(ValueError, "successful complete"),
        ):
            self.module.read(Path("retained/in.bench.json"))
        with self.assertRaisesRegex(ValueError, "evidence file"):
            self.module.read(Path("retained/in.render.json"))


if __name__ == "__main__":
    unittest.main()
