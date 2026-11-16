"""Run the actual CLI adapter against an owned executable fixture."""

import os
import subprocess
import sys
import unittest

from tests.fixtures import ROOT, workspace


class PodmanContractsTests(unittest.TestCase):
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
                [
                    sys.executable,
                    str(ROOT / "tests/podman/command.py"),
                    "exec",
                    "literal ; $argument",
                ],
                env={**os.environ, "PVISOR_BINARY": str(binary)},
                input=b"hello\n",
                capture_output=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 23)
            self.assertEqual(result.stdout, b"hello\n")
            self.assertEqual(result.stderr, b"separate error\n")
