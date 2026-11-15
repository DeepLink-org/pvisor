"""Imported source integrity and command-adapter policy; processes are mocked."""

import hashlib
import importlib.util
import json
import subprocess
import sys
import unittest
from unittest import mock

from tests.fixtures import ROOT as PROJECT_ROOT

ROOT = PROJECT_ROOT / "tests/podman"
spec = importlib.util.spec_from_file_location("podman_command", ROOT / "command.py")
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class PodmanContractsTests(unittest.TestCase):
    def test_upstream_inventory_and_contents(self):
        expected = json.loads((ROOT / "upstream-sha256.json").read_text())
        actual = {
            path.relative_to(ROOT / "upstream").as_posix(): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in (ROOT / "upstream").rglob("*")
            if path.is_file()
        }
        self.assertEqual(actual, expected)
        for suite in ("system", "e2e"):
            self.assertTrue(any(name.startswith(f"test/{suite}/") for name in expected))

    def test_pvisor_default_and_literal_binary_path(self):
        self.assertEqual(adapter.command_prefix({}), ["pvisor"])
        self.assertEqual(
            adapter.command_prefix({"PVISOR_BINARY": "/a path/pvisor"}),
            ["/a path/pvisor"],
        )
        # Only PVISOR_BINARY may select the executable.
        self.assertEqual(
            adapter.command_prefix(
                {"PVISOR_TEST_ENGINE": "docker", "DOCKER_BINARY": "/unused/docker"}
            ),
            ["pvisor"],
        )

    def test_timeout_kills_process_group_and_returns_failure(self):
        process = mock.MagicMock()
        process.pid = 12345
        process.wait.side_effect = [subprocess.TimeoutExpired("engine", 120), 0]
        process.__enter__.return_value = process
        with (
            mock.patch.object(sys, "argv", ["command.py", "run"]),
            mock.patch.object(adapter.subprocess, "Popen", return_value=process),
            mock.patch.object(adapter.os, "killpg") as kill,
            mock.patch.object(sys, "stderr"),
        ):
            self.assertEqual(adapter.main(), 124)
        kill.assert_called_once_with(12345, adapter.signal.SIGKILL)
