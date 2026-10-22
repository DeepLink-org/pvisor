"""Check imported source integrity and the CLI adapter without a container runtime."""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parent / "podman"
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
        # Inherited settings from the old dual-engine runner cannot invoke Docker.
        self.assertEqual(
            adapter.command_prefix(
                {"PVISOR_TEST_ENGINE": "docker", "DOCKER_BINARY": "/unused/docker"}
            ),
            ["pvisor"],
        )

    def test_adapter_preserves_arguments_streams_stdin_and_exit(self):
        with workspace() as (directory, _):
            binary = directory / "fake engine"
            binary.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                "assert sys.argv[1:] == ['exec', 'literal ; $argument']\n"
                "sys.stdout.write(sys.stdin.read())\n"
                "sys.stderr.write('separate error\\n')\n"
                "sys.exit(23)\n"
            )
            binary.chmod(0o755)
            result = subprocess.run(
                [sys.executable, str(ROOT / "command.py"), "exec", "literal ; $argument"],
                env={**os.environ, "PVISOR_BINARY": str(binary)},
                input=b"hello\n",
                capture_output=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 23)
            self.assertEqual(result.stdout, b"hello\n")
            self.assertEqual(result.stderr, b"separate error\n")

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
