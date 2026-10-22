"""Safety contracts for the opt-in chaos runner; no VM or registry is needed."""

import importlib.util
import signal
import subprocess
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from tests.fixtures import workspace

spec = importlib.util.spec_from_file_location(
    "podman_chaos", Path(__file__).parent / "podman/chaos.py"
)
chaos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chaos)


class ChaosSafetyTests(unittest.TestCase):
    def runner(self, directory):
        return chaos.Chaos(
            SimpleNamespace(seed=1, binary="unused", rounds=6, seconds=30, max_load=5), directory
        )

    def test_pressure_must_be_parseable(self):
        with (
            mock.patch.object(chaos.os, "getloadavg", return_value=(1.5, 2, 3)),
            mock.patch.object(chaos.subprocess, "run") as run,
        ):
            run.return_value.stdout = "System-wide memory free percentage: 64%\n"
            self.assertEqual(chaos.host_sample(), {"load1": 1.5, "memory_free_percent": 64})
            run.return_value.stdout = "unknown output"
            with self.assertRaisesRegex(RuntimeError, "cannot read"):
                chaos.host_sample()

    def test_pressure_aborts_before_workload(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with (
                mock.patch.object(
                    chaos, "host_sample", return_value={"load1": 1, "memory_free_percent": 19}
                ),
                mock.patch.object(chaos.subprocess, "run") as run,
                mock.patch.object(runner, "call") as call,
            ):
                run.return_value.stdout = ""
                with self.assertRaisesRegex(RuntimeError, "host pressure stop"):
                    runner.run()
                call.assert_not_called()

    def test_cleanup_attempts_every_owned_name_after_error(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with mock.patch.object(
                runner, "call", side_effect=[RuntimeError("permission denied"), (b"", b"")]
            ) as call:
                with self.assertRaisesRegex(RuntimeError, "permission denied"):
                    runner.cleanup()
                self.assertEqual([c.args[-1] for c in call.call_args_list], runner.names)

    def test_cleanup_requires_empty_listing(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with mock.patch.object(
                runner, "call", side_effect=[(b"", b""), (b"", b""), (b"remaining", b"")]
            ):
                with self.assertRaises(AssertionError):
                    runner.cleanup()

    def test_timeout_kills_and_reaps_only_its_cli_group(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            process = mock.MagicMock()
            process.pid = 12345
            process.communicate.side_effect = [subprocess.TimeoutExpired("pvisor", 1), (b"", b"")]
            process.__enter__.return_value = process
            with (
                mock.patch.object(chaos.subprocess, "Popen", return_value=process),
                mock.patch.object(chaos.os, "killpg") as kill,
            ):
                with self.assertRaisesRegex(RuntimeError, "timed out"):
                    runner.call("exec", "chaos-a", "sleep", "600")
                kill.assert_called_once_with(12345, signal.SIGKILL)
                self.assertEqual(process.communicate.call_count, 2)

    def test_parallel_observes_all_failures(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            first = mock.Mock(side_effect=RuntimeError("first"))
            second = mock.Mock(side_effect=RuntimeError("second"))
            with self.assertRaisesRegex(RuntimeError, "first; second"):
                runner.parallel(first, second)
            first.assert_called_once()
            second.assert_called_once()
