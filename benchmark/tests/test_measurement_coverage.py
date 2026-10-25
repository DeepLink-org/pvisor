"""Coverage follows the newest complete protocol, while retaining partial results."""

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location(
    "measurement_coverage", ROOT / "measurement_coverage.py"
)
COVERAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COVERAGE)


class FilesystemCoverageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="filesystem-coverage-synthetic-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.patch = mock.patch.object(COVERAGE, "REPO", self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def receipt(self, modes, *, state="partial"):
        prefix = "filesystem-layered-references" if modes == 9 else "filesystem-layered"
        path = self.root / "docs/overrides/assets/benchmarks" / prefix / "provenance.json"
        output = COVERAGE.csv_path(self.root, f"{prefix}-levels.csv")
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(b"synthetic test data\n")
        evidence = dict(
            benchmark_id="B-FS-TOOLS",
            role="user-facing",
            state=state,
            batch=prefix,
            counts=dict(cases=19, backends=modes, expected_environments=19 * modes * 3),
            modes=[
                dict(id=backend)
                for backend in [
                    "native",
                    "host",
                    "staged",
                    "vm",
                    "vm-writeback",
                    "docker",
                    "fc-system",
                    "qemu",
                    "qemu-microvm",
                ][:modes]
            ],
            outputs={
                str(output.relative_to(self.root)): hashlib.sha256(output.read_bytes()).hexdigest()
            },
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(evidence))
        return path, output, evidence

    def test_nine_mode_cohort_takes_precedence_and_retains_partial_state(self):
        self.receipt(5, state="passed")
        self.receipt(9)
        evidence = COVERAGE.filesystem_publication()
        self.assertEqual(evidence["counts"]["expected_environments"], 513)
        self.assertEqual(evidence["state"], "partial")
        self.assertEqual(evidence["batch"], "filesystem-layered-references")

    def test_five_mode_receipt_remains_available_when_references_not_published(self):
        self.receipt(5)
        evidence = COVERAGE.filesystem_publication()
        self.assertEqual(evidence["counts"]["expected_environments"], 285)
        self.assertEqual(evidence["kind"], "layered")

    def test_modified_new_csv_cannot_silently_fall_back_to_old_results(self):
        self.receipt(5)
        _, output, _ = self.receipt(9)
        output.write_bytes(b"modified synthetic data\n")
        with self.assertRaisesRegex(ValueError, "export changed"):
            COVERAGE.filesystem_publication()

    def test_nine_mode_receipt_requires_nine_actual_mode_identities(self):
        path, _, evidence = self.receipt(9)
        evidence["modes"][-1]["id"] = "qemu"
        path.write_text(json.dumps(evidence))
        with self.assertRaisesRegex(ValueError, "mode inventory differs"):
            COVERAGE.filesystem_publication()


class LazyStageCoverageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="lazy-stage-coverage-synthetic-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        patch = mock.patch.object(COVERAGE, "REPO", self.root)
        patch.start()
        self.addCleanup(patch.stop)

    def receipt(self):
        outputs = {}
        for name in ("summary", "comparisons"):
            path = COVERAGE.csv_path(self.root, f"lazy-stage-{name}.csv")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"synthetic publication fixture\n")
            outputs[str(path.relative_to(self.root))] = hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
        evidence = dict(
            benchmark_id="B-LAZY-STARTUP",
            role="user-facing",
            state="passed",
            batch="synthetic-stage",
            scope="same-host-staged-lower-readonly",
            counts=dict(formal_rows=120, samples_per_cell=30, warmup_rows=12),
            outputs=outputs,
        )
        receipt = self.root / "docs/overrides/assets/benchmarks/lazy-stage.provenance.json"
        receipt.parent.mkdir(parents=True, exist_ok=True)
        receipt.write_text(json.dumps(evidence))
        return receipt, evidence

    def test_absent_publication_preserves_historical_coverage(self):
        self.assertIsNone(COVERAGE.lazy_stage_publication())

    def test_audited_same_executor_publication_is_recognized(self):
        self.receipt()
        evidence = COVERAGE.lazy_stage_publication()
        self.assertEqual(evidence["counts"]["formal_rows"], 120)
        self.assertEqual(evidence["scope"], "same-host-staged-lower-readonly")

    def test_modified_shared_csv_cannot_retain_measured_status(self):
        self.receipt()
        COVERAGE.csv_path(self.root, "lazy-stage-summary.csv").write_bytes(b"changed\n")
        with self.assertRaisesRegex(ValueError, "lazy stage export changed"):
            COVERAGE.lazy_stage_publication()

    def test_partial_trial_count_cannot_be_published_as_the_full_cohort(self):
        receipt, evidence = self.receipt()
        evidence["counts"]["formal_rows"] = 119
        receipt.write_text(json.dumps(evidence))
        with self.assertRaisesRegex(ValueError, "invalid same-executor"):
            COVERAGE.lazy_stage_publication()


if __name__ == "__main__":
    unittest.main()
