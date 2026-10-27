"""Safety contracts for the opt-in CLI chaos runner; no VM is launched."""

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from tests.fixtures import workspace

spec = importlib.util.spec_from_file_location(
    "cli_chaos", Path(__file__).parent / "podman/cli_chaos.py"
)
chaos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(chaos)
audit_spec = importlib.util.spec_from_file_location(
    "audit_cli_chaos", Path(__file__).parent / "podman/audit_cli_chaos.py"
)
audit_cli = importlib.util.module_from_spec(audit_spec)
audit_spec.loader.exec_module(audit_cli)


class CliChaosSafetyTests(unittest.TestCase):
    def test_optimized_python_cannot_disable_the_runtime_or_audit_oracles(self):
        for script in ["cli_chaos.py", "audit_cli_chaos.py"]:
            output = subprocess.run(
                [sys.executable, "-O", str(Path(__file__).parent / "podman" / script), "--help"],
                capture_output=True,
                timeout=5,
            )
            self.assertNotEqual(output.returncode, 0)
            self.assertIn(b"requires assertions enabled", output.stderr)

    def runner(self, root):
        binary = root / "bin/pvisor"
        binary.parent.mkdir()
        binary.symlink_to(sys.executable)
        binary.with_name("pvisor-daemon").write_bytes(b"fingerprint-only")
        return chaos.Campaign(
            SimpleNamespace(
                output=root / "evidence",
                binary=binary,
                image="unused",
                seconds=60,
                seed=1,
                interval=1,
            )
        )

    def test_wrong_exit_and_both_streams_remain_available_for_diagnosis(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with self.assertRaises(AssertionError):
                runner.call(
                    "-c",
                    "import sys;sys.stdout.buffer.write(b'out\\x00');sys.stderr.write('err');sys.exit(7)",
                )
            receipt = runner.root / "cmd-000000"
            self.assertEqual((receipt / "stdout").read_bytes(), b"out\x00")
            self.assertEqual((receipt / "stderr").read_bytes(), b"err")
            self.assertEqual(json.loads((receipt / "exit.json").read_text())["code"], 7)

    def test_non_linux_construction_keeps_pure_command_oracles_available(self):
        original = Path.exists
        with workspace() as (root, _):
            with mock.patch.object(
                Path,
                "exists",
                lambda path: False if path == Path("/proc/self/cgroup") else original(path),
            ):
                runner = self.runner(root)
            self.assertIsNone(json.loads((runner.root / "manifest.json").read_text())["cgroup"])
            runner.call(
                "-c",
                "import sys;sys.stdout.buffer.write(sys.stdin.buffer.read());sys.stderr.write('stream-error')",
                data=b"ok\n",
                out=b"ok\n",
            )

    def test_timeout_remains_failure_even_if_zero_is_an_allowed_exit(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with self.assertRaises(AssertionError):
                runner.call("-c", "import time;time.sleep(30)", timeout=0.1, code=(0, -9))
            receipt = json.loads((runner.root / "cmd-000000/exit.json").read_text())
            self.assertTrue(receipt["timed_out"])

    def test_matching_exit_cannot_hide_byte_corruption(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            with self.assertRaises(AssertionError):
                runner.call("-c", "print('wrong')", out=b"correct\n")

    def test_scenarios_are_not_shadowed_by_fixture_state(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            for name in [
                "streams",
                "environment",
                "volume",
                "restart",
                "manager",
                "logs",
                "batch",
                "disconnected_client",
                "transient",
                "invalid",
            ]:
                self.assertTrue(callable(getattr(runner, name)), name)

    def test_audit_rechecks_raw_bytes_instead_of_trusting_passed_flag(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.call(
                "-c",
                "import sys;sys.stdout.buffer.write(sys.stdin.buffer.read());sys.stderr.write('stream-error')",
                data=b"ok\n",
                out=b"ok\n",
            )
            chaos.save(
                runner.root / "outcome.json",
                {
                    "passed": True,
                    "failure": None,
                    "cleanup_errors": [],
                    "commands": 1,
                    "rounds": 1,
                    "counts": {"streams": 1},
                    "elapsed": 1,
                    "paused_samples": 0,
                },
            )
            chaos.save(runner.root / "cleanup.json", {"errors": []})
            chaos.save(
                runner.root / "limits.json",
                {
                    "memory.max": str(1536 * 1024**2),
                    "memory.swap.max": "0",
                    "cpu.max": "150000 100000",
                    "pids.max": "384",
                },
            )
            (runner.root / "resources.jsonl").write_text(
                json.dumps(
                    {
                        "available": 10 * 1024**3,
                        "load1": 1,
                        "cgroup": {
                            "memory.peak": "1024",
                            "memory.swap.current": "0",
                            "memory.events": "max 0\noom 0\noom_kill 0",
                        },
                    }
                )
                + "\n"
            )
            self.assertTrue(audit_cli.audit(runner.root)["passed"])
            # Legacy receipts have an input checksum but no explicit output
            # checksum; the known cat contract still provides an independent oracle.
            receipt = runner.root / "cmd-000000/command.json"
            command = json.loads(receipt.read_text())
            command["argv"] = [
                "exec",
                "-i",
                "owned",
                "sh",
                "-c",
                "cat; printf stream-error >&2; exit 0",
            ]
            command["expected_stdout_sha256"] = None
            chaos.save(receipt, command)
            self.assertEqual(audit_cli.audit(runner.root)["cat_roundtrip_hashes_rechecked"], 1)
            (runner.root / "cmd-000000/stdout").write_bytes(b"corrupt\n")
            with self.assertRaises(AssertionError):
                audit_cli.audit(runner.root)


if __name__ == "__main__":
    unittest.main()
