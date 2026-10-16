"""Trust-boundary checks for the guest-cache experiment, without hardware."""

import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import guest_cache as bench


class GuestCacheTests(unittest.TestCase):
    def payload(self, replace=None):
        rows = [{"phase": p, "ms": 1.0} for p in bench.PHASES]
        if replace is not None:
            rows[0]["ms"] = replace
        return (
            "\n".join("CACHE_RESULT " + json.dumps(row) for row in rows) + "\nCACHE_PASS all\n"
        ).encode()

    def test_complete_finite_phases_only(self):
        self.assertEqual(set(bench.results(self.payload(), "all")), set(bench.PHASES))
        for invalid in [True, -1, float("nan"), float("inf")]:
            with self.assertRaises(ValueError):
                bench.results(self.payload(invalid), "all")
        for invalid in [
            b"CACHE_PASS all\n",
            self.payload() + b"CACHE_PASS all\n",
            self.payload().replace(b"stat_hot", b"stat_cold"),
        ]:
            with self.assertRaises(ValueError):
                bench.results(invalid, "all")

    def test_profile_mode_cannot_supply_timing_rows(self):
        self.assertEqual(bench.results(b"CACHE_PASS open\n", "open"), {})
        with self.assertRaises(ValueError):
            bench.results(self.payload(), "open")

    def test_firmware_override_reaches_both_runners(self):
        result = SimpleNamespace(returncode=0, stdout=b"CACHE_PASS empty\n", stderr=b"")
        for cohort in ("sdk", "cli"):
            with (
                self.subTest(cohort=cohort),
                patch.object(Path, "mkdir"),
                patch.object(bench, "run_owned_probe", return_value=result) as run,
                patch.object(Path, "read_text", return_value="{}"),
                patch("reference_baselines.validate_bundle_execution"),
            ):
                bench.run_one(
                    Path("/prepared"),
                    Path("/output"),
                    cohort,
                    1,
                    "empty",
                    False,
                    Path("/baseline/libkrunfw.5.dylib"),
                )
                command, _, env, _ = run.call_args.args
                self.assertEqual(env["PVISOR_VM_GUEST_CACHE"], "1")
                if cohort == "sdk":
                    self.assertIn("/baseline/libkrunfw.5.dylib", command)
                else:
                    self.assertEqual(command[command.index("--vm-library-dir") + 1], "/baseline")

    def test_cli_cannot_silently_select_another_library(self):
        with self.assertRaises(ValueError):
            bench.run_one(
                Path("/prepared"),
                Path("/output"),
                "cli",
                1,
                "empty",
                False,
                Path("/baseline/other.dylib"),
            )

    def test_incomplete_pair_is_rejected(self):
        with self.assertRaises(ValueError):
            bench.analyze(
                {
                    "cohorts": ["sdk"],
                    "samples": 1,
                    "rows": [
                        {
                            "round": 0,
                            "cohort": "sdk",
                            "arm": 0,
                            "times": dict.fromkeys(bench.PHASES, 1),
                        }
                    ],
                }
            )


if __name__ == "__main__":
    unittest.main()
