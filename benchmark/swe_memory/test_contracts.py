"""Prepared-input, reporting and process-entry contracts; no VM starts."""

import concurrent.futures
import json
import multiprocessing
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import report
import run_pair
from verify_inputs import image_identity


def import_worker():
    import worker

    return os.getpid(), callable(worker.main)


class Contracts(unittest.TestCase):
    def test_worker_import_is_safe_in_real_spawn_without_arguments_or_manifest(self):
        with concurrent.futures.ProcessPoolExecutor(
            max_workers=1, mp_context=multiprocessing.get_context("spawn")
        ) as pool:
            pid, callable_main = pool.submit(import_worker).result(timeout=20)
        self.assertNotEqual(pid, os.getpid())
        self.assertTrue(callable_main)

    def test_worker_requires_explicit_context_before_accessing_artifacts(self):
        env = dict(os.environ)
        env.pop("PVISOR_SWE_CONTEXT", None)
        p = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("worker.py"))],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("Explicit context", p.stderr)
        self.assertNotIn("UnboundLocalError", p.stderr)

    def test_runtime_timeout_stops_service_not_only_systemd_run_client(self):
        c = dict(unit="pvisor-test.service", slice="pvisor-test.slice", helper_cpus="0,1")
        with patch.object(
            run_pair.subprocess,
            "run",
            side_effect=[subprocess.TimeoutExpired("systemd-run", 4400), None],
        ) as call:
            with self.assertRaises(subprocess.TimeoutExpired):
                run_pair.execute(Path("/test/context.json"), c, "arrival_batch.py")
        self.assertEqual(
            call.call_args_list[1].args[0], ["systemctl", "--user", "stop", "pvisor-test.service"]
        )
        cmd = call.call_args_list[0].args[0]
        self.assertIn("RuntimeMaxSec=4200", cmd)
        self.assertFalse(any("OFFLOAD_LOCK" in x for x in cmd))

    def test_image_identity_detects_modes_links_content_and_xattrs(self):
        # macOS Python does not expose Linux xattr APIs. Mock only that OS
        # boundary; inode, hardlink, symlink and permission checks use real files.
        for name, value in [("listxattr", []), ("getxattr", b"")]:
            mocked = patch.object(os, name, create=True, return_value=value)
            mocked.start()
            self.addCleanup(mocked.stop)
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            contexts = []
            for arm in ("b", "o"):
                path = root / arm
                image = path / "images/task"
                image.mkdir(parents=True)
                f = image / "file"
                f.write_text("same")
                (image / "link").symlink_to("file")
                os.link(f, image / "hardlink")
                (path / "frozen.json").write_text(
                    json.dumps({"files": {str(f): "a" * 64, str(image / "hardlink"): "a" * 64}})
                )
                contexts.append(dict(root=str(path), image_roots=[str(image)]))
            self.assertEqual(image_identity(contexts[0]), image_identity(contexts[1]))
            changed = Path(contexts[1]["image_roots"][0]) / "file"
            changed.chmod(0o600)
            self.assertNotEqual(image_identity(contexts[0]), image_identity(contexts[1]))
            changed.chmod((Path(contexts[0]["image_roots"][0]) / "file").stat().st_mode)
            (changed.parent / "link").unlink()
            (changed.parent / "link").symlink_to("different")
            self.assertNotEqual(image_identity(contexts[0]), image_identity(contexts[1]))

    def test_protocol_preserves_queue_execution_and_budget(self):
        p = run_pair.PROTOCOL
        self.assertIsNone(p["admission_plan"]["max_queue_s"])
        self.assertEqual(p["condition_ram_bytes"] + p["supervisor_ram_bytes"], 32 * 2**30)
        self.assertEqual(p["condition_cpu_percent"] + p["supervisor_cpu_percent"], 6000)
        self.assertEqual(p["arrival_spec"]["job_timeout_s"], 2400)
        self.assertEqual(
            p["arrival_spec"]["drain_timeout_s"] + p["arrival_spec"]["arrival_duration_s"], 4200
        )
        self.assertEqual(len(p["arrival_spec"]["arrivals"]), 60)
        self.assertEqual(p["selected_offload_after_ms"], 50)

    def test_relative_changes_include_zero_baseline(self):
        self.assertEqual(report.relative_change(100, 116), "+16.0%")
        self.assertEqual(report.relative_change(100, 86.2), "-13.8%")
        self.assertEqual(report.relative_change(0, 0), "0.0%")
        self.assertEqual(report.relative_change(0, 1), "N/A (zero baseline)")

    def fixture(self):
        samples = [
            dict(
                elapsed_s=t,
                host_monotonic_ns=t * 10**9,
                total=dict(memory_current=2**30),
                supervisor=dict(memory_current=2**30),
                phases=dict(agent=2, model_wait=1, tool_exec=1),
                queued=3,
            )
            for t in range(0, 1801, 2)
        ]
        q = dict(p50=10, p95=20, maximum=30)
        r = dict(
            offline_batch_passed=True,
            safety_audit_passed=True,
            verified=60,
            jobs=[{}] * 60,
            rejected_count=0,
            combined_memory_gib_s=3600,
            total_wall_s=1800,
            throughput_per_min_including_drain=2,
            queue_wait_s=q,
            verified_task_latency_s=q,
        )
        return r, samples

    def test_total_integral_includes_coordinator_but_not_average_memory_row(self):
        r, s = self.fixture()
        values = {row[0]: row[1] for row in report.metrics(r, s)}
        self.assertEqual(values["Total memory-time integral (GiB min)"], 60)
        self.assertEqual(values["Mean active agents, common first 30 min"], 2)
        self.assertFalse(any("average" in label.lower() or "P99" in label for label in values))

    def test_report_rejects_partial_failed_or_inconsistent_pair(self):
        for field, value in [
            ("verified", 59),
            ("rejected_count", 1),
            ("offline_batch_passed", False),
            ("safety_audit_passed", False),
            ("combined_memory_gib_s", 3599),
        ]:
            r, s = self.fixture()
            r[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                report.metrics(r, s)

    def test_report_rejects_gaps_and_overlapping_phases(self):
        r, s = self.fixture()
        s.pop(3)
        s.pop(3)
        s.pop(3)
        s.pop(3)
        s.pop(3)
        with self.assertRaises(ValueError):
            report.metrics(r, s)
        r, s = self.fixture()
        s[4]["phases"]["model_wait"] = 2
        with self.assertRaises(ValueError):
            report.metrics(r, s)


if __name__ == "__main__":
    unittest.main()
