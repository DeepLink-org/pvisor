"""Safety and failure handling for the opt-in real-VM application runner."""

import errno
import importlib.util
import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from tests.fixtures import workspace

spec = importlib.util.spec_from_file_location(
    "application_chaos", Path(__file__).parent / "podman/application_chaos.py"
)
chaos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chaos)


class ApplicationChaosTests(unittest.TestCase):
    def test_removed_scope_allows_only_missing_or_revoked_device(self):
        for number in (errno.ENOENT, errno.ENODEV):
            with mock.patch.object(chaos, "scope_state", side_effect=OSError(number, "removed")):
                self.assertIsNone(chaos.maybe_scope_state(Path("/owned-scope")))
        with mock.patch.object(
            chaos, "scope_state", side_effect=PermissionError(errno.EACCES, "denied")
        ):
            with self.assertRaises(PermissionError):
                chaos.maybe_scope_state(Path("/owned-scope"))

    def runner(self, directory):
        return chaos.ApplicationChaos(
            SimpleNamespace(
                output=directory, seed=11, seconds=60, binary="/unused", image=chaos.IMAGE
            )
        )

    def test_resource_limits_reject_oom_swap_and_budget_events(self):
        state = {"peak": chaos.LIMIT, "swap": 0, "events": {"oom": 0, "oom_kill": 0, "max": 0}}
        chaos.verify_state(state)
        for change in (
            {"peak": chaos.LIMIT + 1},
            {"swap": 1},
            {"events": {"oom": 1}},
            {"events": {"oom_kill": 1}},
            {"events": {"max": 1}},
        ):
            with self.subTest(change=change), self.assertRaises(AssertionError):
                chaos.verify_state({**state, **change})

    def test_vm_scope_routes_to_test_owned_parent_slice(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            runner.args.product_slice = "pvisor-soak-owned.slice"
            child = mock.Mock(returncode=0, pid=123)
            child.communicate.return_value = (b"ok", b"")
            with mock.patch.object(chaos.subprocess, "Popen", return_value=child) as spawn:
                self.assertEqual(runner.call("run", scoped="owned"), (b"ok", b""))
            argv = spawn.call_args.args[0]
            self.assertIn("--slice=pvisor-soak-owned.slice", argv)
            self.assertIn("MemoryMax=512M", argv)
            self.assertIn("MemorySwapMax=0", argv)

    def test_parallel_observes_every_failure(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            first = mock.Mock(side_effect=ValueError("first"))
            second = mock.Mock(side_effect=ValueError("second"))
            with self.assertRaisesRegex(RuntimeError, "first.*second"):
                runner.parallel(first, second)
            first.assert_called_once()
            second.assert_called_once()

    def test_cleanup_attempts_all_owned_resources_after_failure(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            calls = []

            def call(*arguments, **kwargs):
                calls.append(arguments)
                if arguments[-1] == runner.web:
                    raise RuntimeError("owned removal denied")
                return b"", b""

            with mock.patch.object(runner, "call", side_effect=call):
                with self.assertRaisesRegex(RuntimeError, "owned removal denied"):
                    runner.cleanup()
            self.assertEqual([c[-1] for c in calls[:4]], runner.names)
            self.assertEqual(
                calls[4:6],
                [("network", "rm", "-f", runner.network), ("network", "rm", "-f", runner.isolated)],
            )
            self.assertTrue(json.loads((directory / "cleanup.json").read_text())["errors"])

    def test_cleanup_rejects_remaining_container(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with mock.patch.object(
                runner, "call", side_effect=[(b"", b"")] * 6 + [(b"remaining\n", b"")]
            ):
                with self.assertRaises(RuntimeError):
                    runner.cleanup()

    def test_http_corruption_is_saved_and_rejected(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            response = mock.Mock(status=200)
            response.read.return_value = b"truncated"
            response.getheader.return_value = "9"
            client = mock.Mock()
            client.getresponse.return_value = response
            with mock.patch.object(chaos.http.client, "HTTPConnection", return_value=client):
                with self.assertRaises(AssertionError):
                    runner.http(b"expected content")
            self.assertEqual(next(directory.glob("http-failure-*.bin")).read_bytes(), b"truncated")
            self.assertFalse(json.loads((directory / "http.jsonl").read_text())["verified"])
            client.close.assert_called_once()

    def test_eof_inside_headers_is_not_a_successful_empty_body(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            response = mock.Mock(status=200)
            response.getheader.return_value = None
            response.getheaders.return_value = []
            client = mock.Mock()
            client.getresponse.return_value = response
            with mock.patch.object(chaos.http.client, "HTTPConnection", return_value=client):
                with self.assertRaisesRegex(chaos.http.client.HTTPException, "incomplete"):
                    runner.http()
            response.read.assert_not_called()
            client.close.assert_called_once()
            self.assertEqual(len(list(directory.glob("http-incomplete-*.json"))), 1)

    def test_kill_target_must_match_scope_store_and_role(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            (directory / "cgroup.procs").write_text("123\n456\n")
            real_read = Path.read_bytes

            def read(path):
                if path == Path("/proc/123/cmdline"):
                    return (
                        b"pvisor\0--pvisor-internal-docker-vm\0"
                        + str(runner.store).encode()
                        + b"\0"
                    )
                if path == Path("/proc/456/cmdline"):
                    return b"pvisor\0--pvisor-internal-docker-vm\0/another-store\0"
                return real_read(path)

            with mock.patch.object(Path, "read_bytes", read):
                self.assertEqual(runner.victim(directory, "vm"), 123)
                with self.assertRaises(AssertionError):
                    runner.victim(directory, "owner")

    def test_background_connection_failure_outside_fault_is_rejected(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with mock.patch.object(runner, "http", side_effect=ConnectionResetError("reset")):
                runner.background_http()
            self.assertRegex(runner.monitor_error[0], "outside fault window")
            self.assertFalse(
                json.loads((directory / "background-http.jsonl").read_text())["expected_fault"]
            )

    def test_background_fault_epoch_accepts_only_injected_disconnect(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)

            def request():
                with runner.expected_fault():
                    pass
                runner.stop_monitor.set()
                raise ConnectionResetError("injected reset")

            with mock.patch.object(runner, "http", side_effect=request):
                runner.background_http()
            self.assertEqual(runner.monitor_error, [])
            self.assertTrue(
                json.loads((directory / "background-http.jsonl").read_text())["expected_fault"]
            )

    def test_background_corruption_is_rejected_even_during_fault(self):
        with workspace() as (directory, _):
            runner = self.runner(directory)
            with (
                runner.expected_fault(),
                mock.patch.object(runner, "http", return_value=b"corruption"),
            ):
                runner.background_http()
            self.assertRegex(runner.monitor_error[0], "corrupted")


if __name__ == "__main__":
    unittest.main()
