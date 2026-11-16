"""Real Linux subreaper, pidfd, output capture and cleanup regressions."""

import json
import os
import sys
import unittest

from tests import stage_chaos as chaos
from tests.fixtures import workspace


class SafetyTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_subreaper_cleans_detached_adopted_child_without_group_kill(self):
        with workspace() as (root, _):
            child = (
                "import os,time; os.setsid(); print('DETACHED',os.getpid(),flush=True); "
                "os.close(1); os.close(2); time.sleep(60)"
            )
            parent = (
                "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c',"
                + repr(child)
                + "], stdout=subprocess.PIPE, text=True); "
                "print(child.stdout.readline(), end='', flush=True)"
            )
            result = chaos.run_bounded(
                [sys.executable, "-c", parent], root, {"PATH": "/usr/bin:/bin"}, 3
            )
            self.assertFalse(result["timed_out"])
            self.assertTrue(result["cleanup"]["complete"])
            pid = int(result["stdout"].split("DETACHED ")[1].split()[0])
            self.assertIsNone(chaos.proc_identity(pid))
            sent = [item for item in result["cleanup"]["signals"] if item["pid"] == pid]
            self.assertTrue(sent)
            self.assertTrue(all(item["starttime"] > 0 for item in sent))

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_pid_starttime_guard_refuses_stale_and_unowned_identities(self):
        with workspace() as (root, _):
            scope = chaos.Descendants()
            command = None
            try:
                command = chaos.WatchedCommand(
                    [
                        sys.executable,
                        "-c",
                        "import time; print('ready',flush=True); time.sleep(60)",
                    ],
                    root,
                    {"PATH": "/usr/bin:/bin"},
                    scope,
                )
                stale = dict(command.identity, starttime=command.identity["starttime"] + 1)
                self.assertFalse(scope.send(stale, chaos.signal.SIGKILL))
                self.assertIsNone(command.process.poll())
                own = chaos.proc_identity(os.getpid())
                with self.assertRaises(AssertionError):
                    scope.send(own, chaos.signal.SIGTERM)
                self.assertTrue(scope.cleanup(timeout=3)["complete"])
                command.finish(chaos.time.monotonic() + 1)
            finally:
                scope.cleanup(timeout=3)
                if command:
                    command.close()
                scope.close()

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_watched_files_actually_cap_large_output(self):
        with workspace() as (root, _):
            scope = chaos.Descendants()
            command = None
            try:
                command = chaos.WatchedCommand(
                    [
                        sys.executable,
                        "-c",
                        "import os; os.write(1,b'x'*100000); os.write(2,b'y'*100000)",
                    ],
                    root,
                    {"PATH": "/usr/bin:/bin"},
                    scope,
                    root / "log",
                )
                result = command.finish(chaos.time.monotonic() + 3)
                self.assertFalse(result["timed_out"])
                self.assertIsNone(result["json"])
                for name in ("stdout", "stderr"):
                    self.assertEqual(result["bytes"][name], 100000)
                    self.assertTrue(result["truncated"][name])
                    self.assertEqual(len(result[name]), chaos.LIMIT)
                    self.assertEqual((root / ("log." + name)).stat().st_size, chaos.LIMIT)
            finally:
                self.assertTrue(scope.cleanup(timeout=3)["complete"])
                if command:
                    command.close()
                scope.close()

    @unittest.skipUnless(sys.platform == "linux", "Linux subreaper/pidfd contract")
    def test_bounded_command_json_validation_and_timeout(self):
        with workspace() as (root, _):

            def run(script, timeout=3):
                return chaos.run_bounded(
                    [sys.executable, "-c", script], root, {"PATH": "/usr/bin:/bin"}, timeout
                )

            obj = {"schema_version": 1, "selected": [], "status": "ready"}
            result = run("print(" + repr(json.dumps(obj)) + ")")
            self.assertEqual(chaos.apply_object(result, 0, "ready"), obj)
            with self.assertRaises(AssertionError):
                chaos.apply_object(result, 3)
            self.assertIsNone(run("print('not json')")["json"])
            result = run("import time; time.sleep(10)", timeout=0.1)
            self.assertTrue(result["timed_out"])
            self.assertTrue(result["cleanup"]["complete"])
            self.assertIsNone(chaos.proc_identity(result["pid_identity"]["pid"]))
