import importlib.util
import json
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "firmware_stages", Path(__file__).resolve().parents[1] / "startup/firmware_stages.py"
)
stages = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stages)


def mark(stage, ns):
    span = dict(
        pid=42,
        schema=1,
        epoch="vm-runtime",
        clock="host_monotonic_process_relative",
        stage=stage,
        start_ns=ns,
        outcome="ok",
    )
    return b"pvisor-startup-profile " + json.dumps(span).encode() + b"\n"


class FirmwareStages(unittest.TestCase):
    def test_only_same_process_complete_interval(self):
        start = mark("vm.runtime_configure", 20)
        end = mark("vmm.console_ack", 30)
        self.assertEqual(stages.console_interval(start + end, 42), 10)
        self.assertIsNone(stages.console_interval(start, 42))
        for log, pid in [
            (start + end, 43),
            (start + end * 2, 42),
            (start + mark("vmm.console_ack", 19), 42),
        ]:
            with self.subTest(log=log, pid=pid), self.assertRaises(ValueError):
                stages.console_interval(log, pid)
