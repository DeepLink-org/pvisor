import json
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import lowering_writeback_guard as guard


class DiskGuardTests(unittest.TestCase):
    def test_admission_rejects_before_launch(self):
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(
                guard.shutil,
                "disk_usage",
                return_value=SimpleNamespace(free=guard.ADMISSION_BYTES - 1),
            ),
        ):
            calls = []
            with self.assertRaises(RuntimeError):
                guard.guarded(lambda: calls.append(1), Path(name) / "disk.jsonl")
            self.assertEqual(calls, [])

    def test_healthy_observations_keep_result(self):
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(guard.shutil, "disk_usage", return_value=SimpleNamespace(free=8 << 30)),
        ):
            log = Path(name) / "disk.jsonl"
            self.assertEqual(guard.guarded(lambda: 42, log), 42)
            self.assertEqual(json.loads(log.read_text().splitlines()[0])["event"], "admission")

    def test_critical_reserve_interrupts_only_own_coordinator(self):
        values = [SimpleNamespace(free=8 << 30), SimpleNamespace(free=0)]
        with (
            tempfile.TemporaryDirectory() as name,
            patch.object(guard.shutil, "disk_usage", side_effect=values),
            patch.object(guard.os, "kill") as kill,
        ):
            with self.assertRaises(RuntimeError):
                guard.guarded(lambda: time.sleep(0.02), Path(name) / "disk.jsonl")
            kill.assert_called_once_with(guard.os.getpid(), guard.signal.SIGINT)


if __name__ == "__main__":
    unittest.main()
