#!/usr/bin/env python3
"""Focused checks for the startup harness without sandbox privileges."""

from __future__ import annotations

import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path

MODULE = Path(__file__).with_name("startup.py")
SPEC = importlib.util.spec_from_file_location("pvisor_startup", MODULE)
assert SPEC is not None and SPEC.loader is not None
STARTUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STARTUP)


class StartupBenchmarkTests(unittest.TestCase):
    def test_host_option_effects_use_paired_nearest_reference(self) -> None:
        def row(latency: float, cpu: float, rss: int) -> dict:
            return {
                "elapsed_ms": latency,
                "cpu_ms": cpu,
                "peak_tree_rss_bytes": rss,
            }

        compared = STARTUP.comparisons(
            {
                "host": [row(10, 5, 100), row(20, 8, 200)],
                "host_stage": [row(13, 6, 110), row(19, 9, 190)],
                "host_safe": [row(18, 9, 140), row(25, 12, 230)],
                "host_safe_net_deny_all": [row(20, 10, 150), row(40, 20, 270)],
            }
        )
        self.assertEqual(compared["host_safe"]["reference_case"], "host_stage")
        self.assertEqual(compared["host_safe"]["paired_latency_delta_ms_p50"], 5.5)
        self.assertEqual(compared["host_safe_net_deny_all"]["reference_case"], "host_safe")
        self.assertEqual(compared["host_safe_net_deny_all"]["paired_rss_delta_bytes_p50"], 25)

    @unittest.skipUnless(sys.platform == "linux", "occupancy sampler requires /proc")
    def test_trial_measures_completion_and_resource_occupancy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            result = STARTUP.run_trial(
                "direct",
                ["{workload}"],
                phase="occupancy",
                workload=["/bin/sh", "-c", "sleep 0.05"],
                scratch=scratch,
                interval_ms=1,
                timeout_s=2,
                verify_pvisor=False,
                keep=False,
            )
            self.assertGreaterEqual(result["elapsed_ms"], 40)
            self.assertGreater(result["peak_tree_rss_bytes"], 0)
            self.assertGreaterEqual(result["peak_processes"], 1)
            self.assertEqual(list(scratch.iterdir()), [])

    def test_pvisor_trial_requires_completed_bundle_and_expected_isolation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            shim = scratch / "shim.py"
            shim.write_text(
                "import json, os, pathlib, sys\n"
                "root = pathlib.Path(os.environ['PVISOR_RUN_HOME']) / 'run-test'\n"
                "root.mkdir()\n"
                "(root / 'run-bundle.json').write_text(json.dumps({'run': {"
                "'state': 'completed', 'exit_code': 0, 'executor': "
                "{'isolation': 'virtual_machine'}}}))\n"
            )
            result = STARTUP.run_trial(
                "vm",
                [sys.executable, str(shim), "{workload}"],
                phase="startup",
                workload=STARTUP.WORKLOAD,
                scratch=scratch,
                interval_ms=1,
                timeout_s=2,
                verify_pvisor=True,
                keep=False,
            )
            self.assertEqual(result["observed_isolation"], "virtual_machine")
            shim.write_text(shim.read_text().replace("virtual_machine", "host_process"))
            with (
                redirect_stderr(io.StringIO()),
                self.assertRaisesRegex(RuntimeError, "observed isolation"),
            ):
                STARTUP.run_trial(
                    "vm",
                    [sys.executable, str(shim), "{workload}"],
                    phase="startup",
                    workload=STARTUP.WORKLOAD,
                    scratch=scratch,
                    interval_ms=1,
                    timeout_s=2,
                    verify_pvisor=True,
                    keep=False,
                )

    def test_adapter_requires_argv_workload_and_unique_case(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "adapter.json"
            path.write_text(
                json.dumps(
                    {
                        "schema": "pvisor-startup-adapter/v1",
                        "cases": [
                            {"name": "docker", "command": ["docker", "run", "image", "{workload}"]}
                        ],
                    }
                )
            )
            self.assertEqual(STARTUP.load_adapter(path)["docker"][-1], "{workload}")
            path.write_text(path.read_text().replace("{workload}", "true"))
            with self.assertRaisesRegex(ValueError, "workload"):
                STARTUP.load_adapter(path)
