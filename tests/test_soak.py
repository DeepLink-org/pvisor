"""Failure evidence and timeout safety for the opt-in real-VM soak."""

import importlib.util
import json
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from tests.fixtures import workspace

spec = importlib.util.spec_from_file_location("soak", Path(__file__).parent / "podman/soak.py")
soak = importlib.util.module_from_spec(spec)
spec.loader.exec_module(soak)


class SoakSafetyTests(unittest.TestCase):
    def test_missing_pidfd_support_rejects_before_pulling_or_launching(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with mock.patch.object(soak.os, "pidfd_open", None, create=True), mock.patch.object(
                runner, "pv"
            ) as pv:
                with self.assertRaisesRegex(RuntimeError, "pidfd support"):
                    runner.main()
                pv.assert_not_called()

    def runner(self, root):
        return soak.Soak(
            SimpleNamespace(
                output=root / "evidence", binary=Path(sys.executable), seconds=60, seed=1
            )
        )

    def test_nonzero_exit_keeps_both_streams_and_receipt(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with self.assertRaisesRegex(RuntimeError, "exit=7"):
                runner.call(
                    "probe",
                    [
                        sys.executable,
                        "-c",
                        "import sys;sys.stdout.buffer.write(b'out\\x00');sys.stderr.write('err');sys.exit(7)",
                    ],
                )
            command = runner.output / "cmd-000000-probe"
            self.assertEqual((command / "stdout").read_bytes(), b"out\x00")
            self.assertEqual((command / "stderr").read_bytes(), b"err")
            receipt = json.loads((command / "exit.json").read_text())
            self.assertEqual(receipt["code"], 7)
            self.assertFalse(receipt["timed_out"])

    def test_timeout_is_failure_even_if_process_ignores_term(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with self.assertRaisesRegex(RuntimeError, "timeout=True"):
                runner.call(
                    "timeout",
                    [
                        sys.executable,
                        "-c",
                        "import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(30)",
                    ],
                    timeout=0.1,
                )
            receipt = json.loads((runner.output / "cmd-000000-timeout/exit.json").read_text())
            self.assertTrue(receipt["timed_out"])
            self.assertLess(receipt["code"], 0)

    def test_failure_remains_in_status_after_subsequent_success(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.record("first", ValueError("content corruption"))
            runner.record("second")
            status = json.loads((runner.output / "status.json").read_text())
            self.assertEqual(len(status["failures"]), 1)
            self.assertIn("content corruption", status["failures"][0]["error"])
            self.assertEqual(status["counts"], {"first": 1, "second": 1})

    def test_successful_auto_removed_workload_needs_no_racy_cleanup_lookup(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.images["fixture"] = "fixture@sha256:owned"
            with mock.patch.object(runner, "pv", return_value=b"ok") as call:
                runner.run_image("probe", "fixture", ["true"], b"ok")
            self.assertEqual(call.call_count, 1)

    def test_failed_workload_cleans_only_its_exact_owned_name(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.images["fixture"] = "fixture@sha256:owned"
            with mock.patch.object(
                runner, "pv", side_effect=[RuntimeError("run failed"), b""]
            ) as call:
                with self.assertRaisesRegex(RuntimeError, "run failed"):
                    runner.run_image("probe", "fixture", ["true"], b"ok")
            launch, cleanup = call.call_args_list
            name = launch.args[launch.args.index("--name") + 1]
            self.assertEqual(cleanup.args, ("probe", "rm", "-f", name))
            self.assertEqual(cleanup.kwargs, {"missing_ok": True})

    def test_independent_workers_have_distinct_names_even_with_the_same_clock(self):
        with workspace() as (root, _):
            first = self.runner(root / "first")
            second = self.runner(root / "second")
            names = []
            with mock.patch.object(soak.time, "time_ns", return_value=0):
                for runner in (first, second):
                    runner.images["fixture"] = "fixture@sha256:owned"
                    with mock.patch.object(runner, "pv", return_value=b"ok") as call:
                        runner.run_image("probe", "fixture", ["true"], b"ok")
                    names.append(call.call_args.args[call.call_args.args.index("--name") + 1])
            self.assertNotEqual(*names)

    def test_name_conflict_never_removes_the_preexisting_instance(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.images["fixture"] = "fixture@sha256:owned"
            evidence = root / "conflict"
            evidence.mkdir()
            (evidence / "stderr").write_text(
                "pvisor: container name already in use: soak-probe-collision\n"
            )
            failure = soak.CommandFailure(evidence, 125, 0, False)
            with mock.patch.object(
                soak.uuid, "uuid4", return_value=SimpleNamespace(hex="collision")
            ):
                with mock.patch.object(runner, "pv", side_effect=failure) as call:
                    with self.assertRaises(soak.CommandFailure):
                        runner.run_image("probe", "fixture", ["true"], b"ok")
            self.assertEqual(call.call_count, 1)


if __name__ == "__main__":
    unittest.main()
