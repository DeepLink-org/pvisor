"""Exercise the real FIFO loop beyond120s; no VM or model calls."""

import concurrent.futures
import json
import multiprocessing
import os
import pickle
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

import arrival_batch as b


def spawn_probe():
    return os.getpid(), b.finish.__module__


class DrainTests(unittest.TestCase):
    def test_unlimited_queue_does_not_expire(self):
        from admission import expired

        self.assertFalse(expired(dict(at_s=0), 10**9, dict(max_queue_s=None)))

    def test_real_spawn_finish_and_error(self):
        self.assertIs(pickle.loads(pickle.dumps(b.finish)), b.finish)
        with concurrent.futures.ProcessPoolExecutor(
            max_workers=1, mp_context=multiprocessing.get_context("spawn")
        ) as p:
            pid, module = p.submit(spawn_probe).result(timeout=30)
            self.assertNotEqual(pid, os.getpid())
            self.assertEqual(module, "arrival_batch")
            with self.assertRaisesRegex(ValueError, "Missing job start record"):
                p.submit(b.finish, Path("/nonexistent-drain-test-job")).result(timeout=30)
            self.assertEqual(p.submit(spawn_probe).result(timeout=30)[0], pid)

    def test_real_loop_waits_then_admits_fifo_without_rejection(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for f in ["batches", "logs", "jobs", "sup"]:
                (root / f).mkdir()
            sup = root / "sup"
            (sup / "memory.max").write_text(str(256 * 2**20))
            (sup / "cpu.max").write_text("25000 100000")
            (root / "advance-progress.json").write_text('{"status":"all-calibration-passed"}')
            plan = dict(
                condition_bytes=32 * 2**30,
                headroom_bytes=8 * 2**30,
                boot_bytes={"x": 2**30},
                startup_reservation_s=30,
                max_snapshot_age_s=3,
                memory_psi_avg10_max=1,
                max_queue_s=None,
            )
            (root / "admission-plan.json").write_text(json.dumps(plan))
            spec = root / "spec.json"
            spec.write_text(
                json.dumps(
                    dict(
                        id="arrival-drain-test",
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
            launches = []

            def read(p, *a, **kw):
                if str(p) == "/proc/self/cgroup":
                    return "0::/user.slice/" + b.SLICE + "/test.service\n"
                if str(p).startswith("/proc/"):
                    return "MemAvailable: 999999999 kB\n" if str(p) == "/proc/meminfo" else ""
                return orig(p, *a, **kw)

            def snapshot(_):
                return dict(
                    memory_current=(31 if tick[0] < 130 else 1) * 2**30,
                    memory_events={"oom": 0},
                    swap_current=0,
                    memory_stat=dict(
                        file=0,
                        inactive_file=0,
                        file_mapped=0,
                        file_dirty=0,
                        file_writeback=0,
                        shmem=0,
                    ),
                    pressure={"memory": "some avg10=0 total=0"},
                )

            def launch(args, **kwargs):
                launches.append((args[-1], tick[0]))
                p = MagicMock()
                p.pid = 100 + len(launches)
                p.poll.return_value = 0
                p.returncode = 0
                return p

            executor = MagicMock()

            def submit(fn, job):
                f = concurrent.futures.Future()
                f.set_result(dict(task="x", agent_s=1))
                return f

            executor.submit.side_effect = submit
            tracker = MagicMock()
            tracker.update.return_value = {}
            with (
                patch.object(b, "ROOT", root),
                patch.object(b, "SSD", root),
                patch.object(b, "service_group", return_value=sup),
                patch.object(b, "budget"),
                patch.object(b, "cgroup_snapshot", side_effect=snapshot),
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
                patch.object(b.concurrent.futures, "ProcessPoolExecutor", return_value=executor),
                patch.object(b.subprocess, "Popen", side_effect=launch),
                patch.object(b, "PhaseTracker", return_value=tracker),
                patch.object(b.admission, "observe"),
            ):
                self.assertEqual(b.run(spec, "baseline"), 0)
            p = json.loads((root / "batches/arrival-drain-test-baseline/progress.json").read_text())
            self.assertEqual(p["status"], "completed")
            self.assertEqual(p["launched"], 2)
            self.assertEqual(len(p["completed"]), 2)
            self.assertEqual(p["rejected"], [])
            self.assertEqual(p["unlaunched"], [])
            self.assertEqual([x[0][-5:] for x in launches], ["00000", "00001"])
            self.assertTrue(all(t > 120 for _, t in launches))
            self.assertGreater(p["admission_holds"], 0)


if __name__ == "__main__":
    unittest.main()
