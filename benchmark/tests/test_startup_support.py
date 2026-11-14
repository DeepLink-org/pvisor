"""Contracts for replay startup helpers; no root, systemd or VM required."""

import asyncio
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "replay"))
import startup_support as s


class CheckedExecTests(unittest.TestCase):
    def test_check_failure_blocks_payload(self):
        with patch.object(s.subprocess, "run", side_effect=subprocess.CalledProcessError(7, "check")), patch.object(s.os, "execv") as execute:
            with self.assertRaises(subprocess.CalledProcessError):
                s.checked_exec("/check.py", ["unit"], ["/payload"])
            execute.assert_not_called()

    def test_exact_argv_and_exec_failure_propagates(self):
        with patch.object(s.subprocess, "run") as check, patch.object(s.os, "execv", side_effect=PermissionError) as execute:
            with self.assertRaises(PermissionError):
                s.checked_exec("/check.py", ["unit"], ["/payload", "a b", "$HOME"])
            check.assert_called_once_with([sys.executable, "-B", "/check.py", "unit"], check=True)
            execute.assert_called_once_with("/payload", ["/payload", "a b", "$HOME"])

    def test_reject_relative_paths_before_check(self):
        with patch.object(s.subprocess, "run") as check:
            for script, command in [("check.py", ["/payload"]), ("/check.py", []), ("/check.py", ["payload"])]:
                with self.assertRaises(ValueError):
                    s.checked_exec(script, [], command)
            check.assert_not_called()

    def test_real_check_then_exec(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            check = root / "check.py"
            check.write_text("import os,pathlib,sys\npathlib.Path(sys.argv[1]).write_text(str(os.getppid()))\n")
            receipt = root / "checked"
            code = "import os,pathlib,sys,json; print(json.dumps([os.getpid(), int(pathlib.Path(sys.argv[1]).read_text()),sys.argv[2]]))"
            result = subprocess.run([sys.executable, s.__file__, "--check-script", str(check), "--check-arg", str(receipt), "--", sys.executable, "-c", code, str(receipt), "a b;$HOME"], check=True, capture_output=True, text=True)
            pid, checked_parent, literal = json.loads(result.stdout)
            self.assertEqual(pid, checked_parent)  # payload replaces wrapper
            self.assertEqual(literal, "a b;$HOME")


class SamplerTests(unittest.IsolatedAsyncioTestCase):
    async def test_slow_read_does_not_block_tools_or_event_loop(self):
        entered, release = threading.Event(), threading.Event()
        def collect():
            entered.set()
            if not release.wait(5):
                raise TimeoutError("test did not release collector")
            return threading.current_thread().name
        async with s.ResourceSampler(1) as sampler:
            read = asyncio.create_task(sampler.read(collect))
            try:
                self.assertTrue(await asyncio.to_thread(entered.wait, 2))
                self.assertEqual(await asyncio.wait_for(asyncio.to_thread(lambda: 42), 2), 42)
                self.assertFalse(read.done())
            finally:
                release.set()
            self.assertTrue((await read).startswith("replay-metrics"))

    async def test_errors_and_context_lifecycle(self):
        sampler = s.ResourceSampler()
        with self.assertRaises(RuntimeError):
            await sampler.read(lambda: 1)
        async with sampler:
            with self.assertRaisesRegex(OSError, "missing metric"):
                await sampler.read(lambda: (_ for _ in ()).throw(OSError("missing metric")))
        with self.assertRaises(RuntimeError):
            await sampler.read(lambda: 1)
        with self.assertRaises(RuntimeError):
            await sampler.__aenter__()

    async def test_cancellation_drains_inflight_read_before_exit(self):
        entered, release, drained = threading.Event(), threading.Event(), threading.Event()
        exited = asyncio.Event()
        def collect():
            entered.set()
            if not release.wait(5):
                raise TimeoutError("test did not release collector")
            drained.set()
        async def owner():
            try:
                async with s.ResourceSampler(1) as sampler:
                    await sampler.read(collect)
            finally:
                exited.set()
        task = asyncio.create_task(owner())
        try:
            self.assertTrue(await asyncio.to_thread(entered.wait, 2))
            task.cancel()
            await asyncio.sleep(0)
            task.cancel()  # cancellation while draining must also preserve ordering
            await asyncio.sleep(0)
            self.assertFalse(exited.is_set())
        finally:
            release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(drained.is_set())
        self.assertTrue(exited.is_set())

    def test_worker_validation(self):
        for workers in [0, -1, True, 1.5]:
            with self.assertRaises(ValueError):
                s.ResourceSampler(workers)


if __name__ == "__main__":
    unittest.main()
