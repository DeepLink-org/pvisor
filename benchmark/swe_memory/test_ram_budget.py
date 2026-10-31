import json
import tempfile
import unittest
from pathlib import Path

import arrival_batch as b
from arrival_common import GIB


class RAMBudget(unittest.TestCase):
    def group(self, d, mem=34091302912, cpu="5975000 100000", swap=0):
        p = Path(d)
        for k, v in {"memory.max": mem, "cpu.max": cpu, "memory.swap.max": swap}.items():
            (p / k).write_text(str(v))
        return p

    def test_combined_envelope(self):
        plan = json.loads((Path(__file__).parent / "protocol.json").read_text())
        self.assertEqual(plan["condition_ram_bytes"] + plan["supervisor_ram_bytes"], 32 * GIB)
        self.assertEqual(plan["condition_cpu_percent"] + plan["supervisor_cpu_percent"], 6000)
        with tempfile.TemporaryDirectory() as d:
            b.budget(self.group(d))

    def test_wrong_caps_rejected(self):
        for mem, cpu, swap in [
            (400 * GIB, "5975000 100000", 0),
            (34091302912, "6000000 100000", 0),
            (34091302912, "5975000 100000", 1),
        ]:
            with tempfile.TemporaryDirectory() as d:
                with self.assertRaises(AssertionError):
                    b.budget(self.group(d, mem, cpu, swap))

    def test_memory_pressure_not_mistaken_for_queue_density(self):
        s = dict(
            meminfo="MemAvailable: 200000000 kB",
            ssd_free=300 * GIB,
            hdd_free=1000 * GIB,
            total=dict(memory_current=31 * GIB, memory_events={"oom": 0}, swap_current=0),
            oom_initial=0,
            supervisor=dict(swap_current=0, memory_events={"oom": 0}),
        )
        self.assertTrue(b.resource_guard(s))
        s["total"]["memory_events"]["oom"] = 1
        with self.assertRaises(AssertionError):
            b.resource_guard(s)
        s["total"]["memory_events"]["oom"] = 0
        s["supervisor"]["memory_events"]["oom"] = 1
        with self.assertRaises(AssertionError):
            b.resource_guard(s)
