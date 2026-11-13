import asyncio
import json
import pathlib
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "replay"))
from resource_sampler import ResourceSampler, collect_to_log

ROOT = pathlib.Path(__file__).resolve().parent


class SamplingTests(unittest.IsolatedAsyncioTestCase):
    async def test_blocking_read_does_not_block_loop(self):
        entered = threading.Event()
        release = threading.Event()

        def collect():
            entered.set()
            release.wait(3)
            return 17

        async with ResourceSampler(1) as s:
            task = asyncio.create_task(s.read(collect))
            while not entered.is_set():
                await asyncio.sleep(0.001)
            ticks = 0
            for _ in range(10):
                await asyncio.sleep(0.002)
                ticks += 1
            self.assertFalse(task.done())
            self.assertEqual(ticks, 10)
            release.set()
            self.assertEqual(await task, 17)

    async def test_cancel_drains_before_return(self):
        entered = threading.Event()
        release = threading.Event()
        finished = threading.Event()

        def collect():
            entered.set()
            release.wait(3)
            finished.set()

        async with ResourceSampler(1) as s:
            t = asyncio.create_task(s.read(collect))
            while not entered.is_set():
                await asyncio.sleep(0.001)
            t.cancel()
            await asyncio.sleep(0.02)
            self.assertFalse(t.done())
            release.set()
            with self.assertRaises(asyncio.CancelledError):
                await t
            self.assertTrue(finished.is_set())

    async def test_failure_is_not_a_zero_sample(self):
        def broken():
            raise PermissionError("cannot read smaps")

        async with ResourceSampler() as s:
            with self.assertRaises(PermissionError):
                await s.read(broken)
            with self.assertRaises(RuntimeError):
                s.check()

    async def test_workers_bounded_and_not_tool_executor(self):
        release = threading.Event()
        lock = threading.Lock()
        count = [0, 0]
        names = []

        def collect():
            with lock:
                count[0] += 1
                count[1] = max(count)
                names.append(threading.current_thread().name)
            release.wait(3)
            with lock:
                count[0] -= 1

        async with ResourceSampler(2) as s:
            tasks = [asyncio.create_task(s.read(collect)) for _ in range(12)]
            while count[1] < 2:
                await asyncio.sleep(0.001)
            self.assertEqual(await asyncio.to_thread(lambda: 42), 42)
            release.set()
            await asyncio.gather(*tasks)
        self.assertEqual(count[1], 2)
        self.assertTrue(all(n.startswith("resource-sampler") for n in names))

    async def test_timing_and_file_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "samples.jsonl"
            now = time.monotonic_ns()
            async with ResourceSampler() as s:
                await s.read(
                    collect_to_log, lambda r, g: {"memory": 123}, None, "vm", path, now, now - 10**7
                )
            row = json.loads(path.read_text())
            self.assertEqual(row["memory"], 123)
            self.assertGreaterEqual(row["sampling"]["schedule_lag_s"], 0.01)
            self.assertGreaterEqual(row["sampling"]["queue_s"], 0)


if __name__ == "__main__":
    unittest.main()
