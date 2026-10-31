"""Offline tests for launch schedule, resource guard and single-job analysis extraction."""

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import arrival_batch as b
import arrival_common as c


class Harness(unittest.TestCase):
    def test_schedule_repeatability_and_mix(self):
        a = c.schedule(6, 600, 42)
        self.assertEqual(a, c.schedule(6, 600, 42))
        self.assertNotEqual(a, c.schedule(6, 600, 43))
        for i in range(0, len(a) - 2, 3):
            self.assertEqual({x["task"] for x in a[i : i + 3]}, set(c.TASKS))
        self.assertEqual(a[0]["at_s"], 0)
        self.assertLess(a[-1]["at_s"], 600)
        self.assertTrue(all(5 <= v["at_s"] - u["at_s"] <= 15 for u, v in zip(a, a[1:])))

    def test_higher_arrival_rate_and_disjoint_placement(self):
        self.assertGreater(len(c.schedule(12, 600, 42)), len(c.schedule(6, 600, 42)))
        self.assertFalse(c.cpus(c.VM_CPUS) & c.cpus(c.HELPER_CPUS))
        self.assertEqual(len(c.cpus(c.VM_CPUS)), 56)

    def test_guards_and_admission(self):
        s = dict(
            meminfo="MemAvailable: 200000000 kB\n",
            ssd_free=300 * c.GIB,
            hdd_free=1000 * c.GIB,
            total=dict(memory_current=50 * c.GIB, memory_events={"oom": 0}, swap_current=0),
            oom_initial=0,
        )
        self.assertTrue(b.resource_guard(s))
        s["total"]["memory_current"] = 370 * c.GIB
        self.assertFalse(b.resource_guard(s))
        s["total"]["swap_current"] = 1
        with self.assertRaises(AssertionError):
            b.resource_guard(s)
        s["total"]["swap_current"] = 0
        s["ssd_free"] = 99 * c.GIB
        with self.assertRaises(AssertionError):
            b.resource_guard(s)
        s["ssd_free"] = 300 * c.GIB
        s["total"]["memory_events"]["oom"] = 1
        with self.assertRaises(AssertionError):
            b.resource_guard(s)

    def test_never_release_failed_job(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)
            with (
                patch.object(b, "analyze_job", return_value={"passed": False}),
                patch.object(b, "inactive") as inactive,
            ):
                with self.assertRaises(AssertionError):
                    b.finish(p)
                inactive.assert_not_called()
                self.assertTrue((p / "analysis.json").exists())

    def test_phase_tracker_handles_partial_writes(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)
            tracker = c.PhaseTracker(p)

            def append(e):
                with (p / "events.jsonl").open("a") as f:
                    f.write(json.dumps(e) + "\n")

            append({"event": "vm_start"})
            append({"event": "trial.agent_start"})
            append({"event": "model_request_start"})
            a = tracker.update()
            self.assertEqual(a["vm"], 1)
            self.assertEqual(a["model_wait"], 1)
            with (p / "events.jsonl").open("a") as f:
                f.write('{"event":"model_request_')
            self.assertEqual(tracker.update()["model_wait"], 1)
            with (p / "events.jsonl").open("a") as f:
                f.write('end"}\n')
            self.assertEqual(tracker.update()["model_wait"], 0)
            append({"event": "vm_stop"})
            self.assertEqual(tracker.update()["vm"], 0)

    def test_timeout_requires_identical_position_and_phase(self):
        from trace_checks import compare_timeouts

        def events(flags):
            return (
                [{"event": "trial.agent_start", "host_monotonic_ns": 1}]
                + [
                    {"event": "tool_end", "host_monotonic_ns": i + 2, "timed_out": v}
                    for i, v in enumerate(flags)
                ]
                + [{"event": "trial.agent_end", "host_monotonic_ns": 100}]
            )

        source = events([False, True, False])
        self.assertTrue(compare_timeouts(source, source)["passed"])
        self.assertFalse(compare_timeouts(events([True, False, False]), source)["passed"])
        self.assertFalse(compare_timeouts(events([False, True, True]), source)["passed"])
        self.assertFalse(compare_timeouts(events([False, True]), source)["passed"])
        self.assertFalse(
            compare_timeouts(
                source + [{"event": "tool_end", "host_monotonic_ns": 101, "timed_out": True}],
                source,
            )["passed"]
        )

    def test_unavailable_io_is_not_reported_as_zero(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)
            for name, text in {
                "memory.current": "1",
                "cpu.stat": "usage_usec 1",
                "memory.events": "oom 0",
                "memory.swap.current": "0",
                "memory.stat": "anon 1",
                "cpu.pressure": "some avg10=0",
                "memory.pressure": "some avg10=0",
                "io.pressure": "some avg10=0",
            }.items():
                (p / name).write_text(text)
            r = c.cgroup_snapshot(p)
            self.assertIsNone(r["io"])
            self.assertFalse(r["io_accounting_available"])
            (p / "memory.current").unlink()
            with self.assertRaises(FileNotFoundError):
                c.cgroup_snapshot(p)


if __name__ == "__main__":
    unittest.main()
