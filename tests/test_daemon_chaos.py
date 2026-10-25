"""Resource and ownership guards for the opt-in real-SDK chaos runner."""

import importlib.util
import json
import os
import unittest
from pathlib import Path
from unittest import mock

from tests.fixtures import workspace

spec = importlib.util.spec_from_file_location(
    "daemon_chaos", Path(__file__).with_name("daemon_chaos.py")
)
chaos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chaos)


class DaemonChaosTests(unittest.TestCase):
    def test_cleanup_ownership_requires_the_exact_nonempty_campaign_marker(self):
        self.assertTrue(
            chaos.owned_sandbox({"ownership": "ours"}, {"metadata": {"pvisor-chaos-owner": "ours"}})
        )
        for cfg, record in [
            ({"ownership": "ours"}, {"metadata": {"pvisor-chaos-owner": "theirs"}}),
            ({"ownership": "ours"}, {"id": "foreign"}),
            ({"ownership": "ours"}, {"metadata": None}),
            ({"ownership": "ours"}, {"metadata": []}),
            ({}, {"metadata": {"pvisor-chaos-owner": ""}}),
        ]:
            self.assertFalse(chaos.owned_sandbox(cfg, record))

    def test_private_api_rejects_insecure_or_nonempty_endpoints(self):
        denied = chaos.urllib.error.HTTPError("http://local", 401, "unauthorized", {}, None)
        with mock.patch.object(chaos, "api", side_effect=[(200, {"items": []}), denied]):
            chaos.verify_private_api({"key": "ours"})
        for replies in [
            [(200, {"items": [{"id": "foreign"}]})],
            [(200, {"items": []}), (200, {"items": []})],
        ]:
            with (
                mock.patch.object(chaos, "api", side_effect=replies),
                self.assertRaises(AssertionError),
            ):
                chaos.verify_private_api({"key": "ours"})

    def test_revocation_exception_never_masks_live_identity_authentication_or_corruption(self):
        for deleting, retired in [({"deleted"}, set()), (set(), {"deleted"})]:
            self.assertTrue(
                chaos.expected_reader_error(False, 401, "deleted", deleting, retired, True)
            )
            self.assertFalse(
                chaos.expected_reader_error(False, 401, "live", deleting, retired, True)
            )
            self.assertFalse(
                chaos.expected_reader_error(False, 401, "deleted", deleting, retired, False)
            )
            self.assertFalse(
                chaos.expected_reader_error(False, None, "deleted", deleting, retired, True)
            )

    def guest(self, group, name="pvisor-sb-owned", pids=None):
        child = group / name
        child.mkdir()
        for field, value in {
            "cgroup.procs": str(os.getpid()) if pids is None else pids,
            "memory.max": str(256 * 1024**2),
            "memory.swap.max": "0",
            "cpu.max": "100000 100000",
            "pids.max": "512",
            "pids.current": "12",
            "pids.events": "max 0\n",
            "memory.current": "1234",
            "memory.peak": "2345",
            "memory.events": "max 0\noom 0\noom_kill 0\n",
        }.items():
            (child / field).write_text(value)
        return child

    def test_live_guests_require_exact_limits_and_no_resource_events(self):
        with workspace() as (group, _):
            child = self.guest(group)
            (group / "api").mkdir()
            self.assertEqual(chaos.observe_guests(group)[child.name]["memory"], 1234)
            for field, value in [
                ("memory.max", "max"),
                ("memory.swap.max", "1"),
                ("cpu.max", "max 100000"),
                ("pids.max", "max"),
                ("pids.current", "513"),
                ("pids.events", "max 1\n"),
                ("memory.events", "oom_kill 1\n"),
            ]:
                original = (child / field).read_text()
                (child / field).write_text(value)
                with self.subTest(field=field), self.assertRaises(AssertionError):
                    chaos.observe_guests(group)
                (child / field).write_text(original)

    def test_empty_uninstalled_group_is_not_live_but_is_retained_for_cleanup_check(self):
        with workspace() as (group, _):
            child = self.guest(group, pids="")
            (child / "memory.max").write_text("max")
            self.assertEqual(chaos.observe_guests(group), {child.name: {"pids": []}})

    def test_fifth_live_guest_is_rejected(self):
        with workspace() as (group, _):
            for index in range(5):
                self.guest(group, name=f"pvisor-sb-{index}")
            with self.assertRaises(AssertionError):
                chaos.observe_guests(group)

    def test_api_crash_refuses_a_foreign_process_and_closes_pidfd(self):
        with workspace() as (root, _):
            (root / "api-pid.json").write_text(json.dumps({"pid": 123}))
            with (
                mock.patch.object(chaos.os, "pidfd_open", return_value=42, create=True),
                mock.patch.object(chaos.os, "close") as close,
                mock.patch.object(chaos.signal, "pidfd_send_signal", create=True) as kill,
                mock.patch.object(Path, "read_bytes", return_value=b"/foreign\0--state\0/other\0"),
            ):
                with self.assertRaises(AssertionError):
                    chaos.crash_api(
                        {"output": str(root), "binary": "/owned", "state": "/owned-state"}
                    )
                kill.assert_not_called()
                close.assert_called_once_with(42)

    def test_injected_exit_is_recorded_before_signalling_the_verified_pidfd(self):
        with workspace() as (root, _):
            (root / "api-pid.json").write_text(json.dumps({"pid": 123}))

            def signal(descriptor, number):
                self.assertEqual(descriptor, 42)
                self.assertEqual(number, chaos.signal.SIGKILL)
                entry = json.loads((root / "expected-api-exits.jsonl").read_text())
                self.assertEqual(entry["pid"], 123)

            with (
                mock.patch.object(chaos.os, "pidfd_open", return_value=42, create=True),
                mock.patch.object(chaos.os, "close") as close,
                mock.patch.object(
                    chaos.signal, "pidfd_send_signal", side_effect=signal, create=True
                ),
                mock.patch.object(
                    Path, "read_bytes", return_value=b"/owned\0--state\0/owned-state\0"
                ),
            ):
                chaos.crash_api({"output": str(root), "binary": "/owned", "state": "/owned-state"})
                close.assert_called_once_with(42)


if __name__ == "__main__":
    unittest.main()
