import asyncio
import pathlib
import sys
import time
import types
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "replay"))
from wait_timing import measured_wait

ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "uksm"))


class WaitTests(unittest.IsolatedAsyncioTestCase):
    async def test_sleep_budget_is_not_reduced_by_maintenance(self):
        events = []

        def emit(op, phase, **kw):
            events.append((phase, time.monotonic(), kw))

        def pause():
            time.sleep(0.02)

        b = types.SimpleNamespace(enter_wait=pause, leave_wait=lambda: None)

        async def call(fn):
            return await asyncio.to_thread(fn)

        await measured_wait(b, 0.04, "wait-0", call, emit)
        phases = [x[0] for x in events]
        self.assertEqual(
            phases,
            [
                "pause_submitted",
                "pause_worker_begin",
                "pause_worker_end",
                "pause_returned",
                "sleep_begin",
                "sleep_returned",
                "resume_submitted",
                "resume_worker_begin",
                "resume_worker_end",
                "resume_returned",
            ],
        )
        self.assertGreaterEqual(events[5][1] - events[4][1], 0.035)
        self.assertGreaterEqual(events[-1][1] - events[0][1], 0.055)

    async def test_pause_failure_does_not_claim_sleep_or_resume(self):
        events = []

        def pause():
            raise RuntimeError("pause failed")

        b = types.SimpleNamespace(enter_wait=pause, leave_wait=lambda: events.append("bad-resume"))

        async def call(fn):
            return await asyncio.to_thread(fn)

        with self.assertRaises(RuntimeError):
            await measured_wait(b, 0, "w", call, lambda op, phase, **kw: events.append(phase))
        self.assertEqual(events, ["pause_submitted", "pause_worker_begin", "pause_worker_error"])


if __name__ == "__main__":
    unittest.main()
