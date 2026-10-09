"""A skipped or failed backend must not yield a successful A/B summary."""

import unittest

from filesystem_ab import WORKLOADS, summarize, validate_binaries
from reference_baselines import validate_bundle_execution


class FilesystemAbTests(unittest.TestCase):
    def test_declared_rootless_staging_requires_exact_isolation_and_kernel_boundaries(self):
        bundle = {
            "run": {
                "state": "completed",
                "exit_code": 0,
                "executor": {"isolation": "rootless_process"},
            },
            "safety": {
                "filesystem_changes_staged": True,
                "filesystem_non_bypassable": True,
                "filesystem_read_non_bypassable": True,
                "filesystem_write_non_bypassable": True,
            },
        }
        with self.assertRaises(AssertionError):
            validate_bundle_execution(bundle, "pvisor-staged")
        validate_bundle_execution(bundle, "pvisor-staged", "rootless_process")
        for boundary in (
            "filesystem_non_bypassable",
            "filesystem_read_non_bypassable",
            "filesystem_write_non_bypassable",
        ):
            bundle["safety"][boundary] = False
            with self.assertRaises(AssertionError):
                validate_bundle_execution(bundle, "pvisor-staged", "rootless_process")
            bundle["safety"][boundary] = True
        bundle["run"]["executor"]["isolation"] = "host_process"
        with self.assertRaises(AssertionError):
            validate_bundle_execution(bundle, "pvisor-staged", "rootless_process")
        validate_bundle_execution(bundle, "pvisor-staged")

    def test_stale_build_or_wrong_manifest_cannot_enter_the_ab_run(self):
        with self.assertRaisesRegex(ValueError, "identical hashes"):
            validate_binaries({"baseline": "same", "candidate": "same"})
        hashes = {"baseline": "old", "candidate": "new"}
        validate_binaries(hashes, {"binary_sha256": hashes})
        with self.assertRaisesRegex(ValueError, "manifest does not match"):
            validate_binaries(hashes, {"binary_sha256": {"baseline": "old", "candidate": "other"}})

    def test_incomplete_matrix_cannot_publish_percentiles(self):
        cells = [("baseline", "pvisor-vm"), ("candidate", "pvisor-vm")]
        rows = [{"variant": "baseline", "backend": "pvisor-vm"}]
        with self.assertRaisesRegex(ValueError, "incomplete sample matrix"):
            summarize(rows, cells, 1)

    def test_warmed_samples_are_separate_from_completion_time(self):
        cells = [("baseline", "pvisor-vm"), ("candidate", "pvisor-vm")]
        rows = [
            {
                "variant": variant,
                "backend": backend,
                "completion_ms": 100 + duration,
                "result": {"filesystem": {mode: {"worker_ms": duration} for mode in WORKLOADS}},
            }
            for variant, backend in cells
            for duration in (10, 30)
        ]
        result = summarize(rows, cells, 2)
        assert result["candidate/pvisor-vm"]["n"] == 2
        assert result["baseline/pvisor-vm"]["timings_ms"]["git"]["p50"] == 20
        assert result["baseline/pvisor-vm"]["timings_ms"]["completion_ms"]["p50"] == 120
