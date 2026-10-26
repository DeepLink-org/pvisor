"""Run the actual arrival loop with a deterministic denied admission; no VM starts."""

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

import arrival_batch as b


class LoopTest(unittest.TestCase):
    def test_fifo_expiry_is_not_vm_launch_or_success(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "batches").mkdir()
            sup = root / "sup"
            sup.mkdir()
            (sup / "memory.max").write_text(str(256 * 2**20))
            (sup / "cpu.max").write_text("25000 100000")
            (root / "advance-progress.json").write_text('{"status":"all-calibration-passed"}')
            plan = dict(
                condition_bytes=0,
                headroom_bytes=8 * 2**30,
                boot_bytes={"x": 2**30},
                startup_reservation_s=30,
                max_snapshot_age_s=3,
                memory_psi_avg10_max=1,
                max_queue_s=120,
            )
            (root / "admission-plan.json").write_text(json.dumps(plan))
            spec = root / "spec.json"
            spec.write_text(
                json.dumps(
                    dict(
                        id="arrival-test",
                        arrivals=[dict(index=0, at_s=0, task="x"), dict(index=1, at_s=1, task="x")],
                        arrival_duration_s=10,
                        drain_timeout_s=300,
                        job_timeout_s=100,
                        protective_worker_limit=256,
                    )
                )
            )
            tick = [0.0]
            orig = Path.read_text

            def read(p, *args, **kw):
                if str(p) == "/proc/self/cgroup":
                    return "0::/user.slice/" + b.SLICE + "/test.service\n"
                if str(p).startswith("/proc/"):
                    return "MemAvailable: 999999999 kB\n" if str(p) == "/proc/meminfo" else ""
                return orig(p, *args, **kw)

            snap = dict(
                memory_current=0,
                memory_events={"oom": 0},
                swap_current=0,
                memory_stat=dict(
                    file=0, inactive_file=0, file_mapped=0, file_dirty=0, file_writeback=0, shmem=0
                ),
                pressure={"memory": "some avg10=0 total=0"},
            )
            with (
                patch.object(b, "ROOT", root),
                patch.object(b, "SSD", root),
                patch.object(b, "service_group", return_value=sup),
                patch.object(b, "budget"),
                patch.object(b, "cgroup_snapshot", return_value=snap),
                patch.object(b, "resource_guard", return_value=True),
                patch.object(
                    b.os, "sched_getaffinity", create=True, return_value=b.cpus(b.HELPER_CPUS)
                ),
                patch.object(Path, "read_text", read),
                patch.object(b.time, "monotonic", side_effect=lambda: tick[0]),
                patch.object(b.time, "monotonic_ns", side_effect=lambda: int(tick[0] * 1e9)),
                patch.object(
                    b.time, "sleep", side_effect=lambda dt: tick.__setitem__(0, tick[0] + dt)
                ),
                patch.object(b.concurrent.futures, "ProcessPoolExecutor", return_value=MagicMock()),
                patch.object(b.subprocess, "Popen") as start,
            ):
                self.assertEqual(b.run(spec, "baseline"), 0)
                start.assert_not_called()
            p = json.loads((root / "batches/arrival-test-baseline/progress.json").read_text())
            self.assertEqual(p["status"], "completed-with-rejections")
            self.assertEqual(p["launched"], 0)
            self.assertEqual(p["completed"], [])
            self.assertEqual([x["index"] for x in p["rejected"]], [0, 1])
            self.assertTrue(all(x["queue_s"] > 120 for x in p["rejected"]))
            self.assertEqual(p["unlaunched"], [])


if __name__ == "__main__":
    unittest.main()
