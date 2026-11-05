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

    def runner(self, root, *, scenario=None):
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
                scenario=scenario,
            )
        )

    def test_focused_scenarios_are_explicit_and_default_profile_stays_complete(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            self.assertEqual(runner.scenario_names, list(chaos.SCENARIOS))
            self.assertIsNone(json.loads((runner.root / "manifest.json").read_text())["scenarios"])
        with workspace() as (root, _):
            names = ["manager", "restart", "volume"]
            runner = self.runner(root, scenario=names)
            self.assertEqual(runner.scenario_names, names)
            self.assertEqual(
                json.loads((runner.root / "manifest.json").read_text())["scenarios"], names
            )
        for names in [[], ["manager", "manager"], ["unsupported"]]:
            with self.subTest(names=names), workspace() as (root, _):
                with self.assertRaises(ValueError):
                    self.runner(root, scenario=names)
                self.assertFalse((root / "evidence").exists())

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

    def test_owned_evidence_is_synced_before_cache_advice_and_keeps_exact_bytes(self):
        with workspace() as (root, _):
            path = root / "evidence"
            data = b"raw\x00evidence\xff\n"
            calls = []
            with (
                mock.patch.object(chaos.os, "POSIX_FADV_DONTNEED", 4, create=True),
                mock.patch.object(
                    chaos.os,
                    "fsync",
                    side_effect=lambda fd: calls.append(("sync", path.read_bytes())),
                ),
                mock.patch.object(
                    chaos.os,
                    "posix_fadvise",
                    side_effect=lambda *args: calls.append(("advice", args[1:], path.read_bytes())),
                    create=True,
                ),
            ):
                chaos.write_evidence(path, data)
                self.assertEqual(
                    calls,
                    [("sync", data), ("advice", (0, 0, chaos.os.POSIX_FADV_DONTNEED), data)],
                )
            self.assertEqual(path.read_bytes(), data)

    def test_evidence_cache_advice_failure_is_not_silently_accepted(self):
        with workspace() as (root, _):
            path = root / "evidence"
            with (
                mock.patch.object(chaos.os, "POSIX_FADV_DONTNEED", 4, create=True),
                mock.patch.object(
                    chaos.os,
                    "posix_fadvise",
                    side_effect=OSError("advice failed"),
                    create=True,
                ),
                self.assertRaisesRegex(OSError, "advice failed"),
            ):
                chaos.write_evidence(path, b"retained")
            self.assertEqual(path.read_bytes(), b"retained")

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

    def test_health_waits_for_application_marker_before_exact_counter_oracle(self):
        with workspace() as (root, _):
            runner = self.runner(root)
            runner.generation = [2, 1]
            results = [
                (b"a\nb\n", b""),
                (b"boot:1\n", b""),
                (b"boot:1\nboot:2\n", b""),
                (b"2\n", b""),
                (b"boot:1\n", b""),
                (b"1\n", b""),
            ]
            with (
                mock.patch.object(runner, "call", side_effect=results) as call,
                mock.patch.object(chaos.time, "sleep"),
            ):
                runner.health()
            self.assertEqual(
                call.call_args_list,
                [
                    mock.call("ps", "-q"),
                    mock.call("logs", runner.names[0]),
                    mock.call("logs", runner.names[0]),
                    mock.call("exec", runner.names[0], "cat", "/boot-count", out=b"2\n", err=b""),
                    mock.call("logs", runner.names[1]),
                    mock.call("exec", runner.names[1], "cat", "/boot-count", out=b"1\n", err=b""),
                ],
            )

    def test_health_missing_or_prefix_only_marker_is_a_bounded_failure(self):
        for log in [b"boot:1\n", b"boot:20\n"]:
            with self.subTest(log=log), workspace() as (root, _):
                runner = self.runner(root)
                runner.generation = [2, 1]
                with (
                    mock.patch.object(runner, "call", side_effect=[(b"a\nb\n", b""), (log, b"")]),
                    mock.patch.object(chaos.time, "monotonic", side_effect=[0, 6]),
                    self.assertRaisesRegex(AssertionError, "application readiness timed out"),
                ):
                    runner.health()

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
            manifest_path = runner.root / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["scenarios"] = ["streams"]
            chaos.save(manifest_path, manifest)
            self.assertEqual(audit_cli.audit(runner.root)["focused_scenarios"], ["streams"])
            manifest["scenarios"] = ["streams", "manager"]
            chaos.save(manifest_path, manifest)
            with self.assertRaises(AssertionError):
                audit_cli.audit(runner.root)
            manifest["scenarios"] = ["streams"]
            chaos.save(manifest_path, manifest)
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
