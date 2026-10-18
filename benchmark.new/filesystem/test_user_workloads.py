"""Observer contracts without launching a workload or touching a Host listener."""

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("user_workloads", HERE / "user_workloads.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class ObserverContractTests(unittest.TestCase):
    def observe(
        self,
        *,
        duplicate_ready=False,
        launch=True,
        host_valid=True,
        backend="native",
        lower_mutated=False,
    ):
        temporary = tempfile.TemporaryDirectory(prefix="user-workloads-observer-")
        self.addCleanup(temporary.cleanup)
        batch = Path(temporary.name)
        fixture = batch / "fixture"
        fixture.mkdir()
        folder = batch / ("trials/M01-001-" + backend)
        rows = [
            dict(
                id="M01",
                round=n,
                correctness=True,
                work_ns=10,
                final_sync_ns=2,
                completed_ns=12,
                metadata_ops=2048,
            )
            for n in (1, 2, 3)
        ]
        lines = []
        if launch:
            lines.append('USER_WORKLOADS_LAUNCH {"monotonic_ns":105}\n')
        lines.append('WORKLOADS_V2_READY {"actual_guest_memtotal_kib":123}\n')
        if duplicate_ready:
            lines.append("WORKLOADS_V2_READY {}\n")
        lines.extend("WORKLOADS_V2_RESULT " + json.dumps(row) + "\n" for row in rows)
        lines.append('WORKLOADS_V2_DONE {"correctness":true}\n')
        subject = mock.Mock(stdout=iter(lines))
        subject.wait.return_value = 0
        subject.poll.return_value = 0

        def command(*args):
            (folder / "work").mkdir()
            if backend in ("staged", "vm", "vm-writeback"):
                (folder / "work/input").mkdir()
                (folder / "work/output").mkdir()
                if lower_mutated:
                    (folder / "work/output/unexpected-write").write_bytes(b"changed lower")
            return ["frozen-subject"], {}

        oracle = subprocess.CompletedProcess(
            ["frozen-oracle"],
            0 if host_valid else 1,
            stdout='WORKLOADS_V2_VERIFIED {"verified":true}\n' if host_valid else "",
            stderr="" if host_valid else "retained bytes differ",
        )
        state = {"cpu.stat": "usage_usec 10\n"}
        ticks = iter(range(100, 1000, 10))
        with (
            mock.patch.object(RUNNER.BACKENDS, "build_command", side_effect=command),
            mock.patch.object(
                RUNNER.BACKENDS, "output_root", return_value=(folder / "work", False)
            ),
            mock.patch.object(RUNNER.SHARED, "cgroup_state", return_value=state),
            mock.patch.object(RUNNER.subprocess, "Popen", return_value=subject),
            mock.patch.object(RUNNER.subprocess, "run", return_value=oracle) as verification,
            mock.patch.object(RUNNER.threading, "Timer"),
            mock.patch.object(RUNNER.time, "monotonic_ns", side_effect=lambda: next(ticks)),
        ):
            record = RUNNER.one_trial(
                batch, {"fixture": str(fixture), "trial_timeout_seconds": 300}, "M01", 1, backend
            )
        return record, folder, verification

    def test_ready_uses_preexec_marker_and_excludes_exit(self):
        record, folder, verification = self.observe()
        self.assertTrue(record["valid"])
        self.assertTrue(record["host_verified"])
        self.assertEqual(record["wrapper_prepare_ns"], 5)
        self.assertEqual(record["ready_ns"], 15)
        self.assertEqual(record["exit_after_done_ns"], 10)
        self.assertEqual(len(record["rows"]), 3)
        verification.assert_called_once()
        self.assertFalse((folder / "work").exists())
        self.assertTrue((folder / "result.json").exists())

    def test_missing_launch_marker_is_retained_as_invalid(self):
        record, folder, verification = self.observe(launch=False)
        self.assertFalse(record["valid"])
        self.assertIsNone(record["ready_ns"])
        self.assertTrue((folder / "work").exists())
        verification.assert_not_called()

    def test_duplicate_ready_is_not_silently_replaced(self):
        record, folder, verification = self.observe(duplicate_ready=True)
        self.assertFalse(record["valid"])
        self.assertIn("duplicate Ready", record["error"])
        self.assertTrue((folder / "work").exists())
        verification.assert_not_called()

    def test_guest_success_without_independent_host_bytes_fails(self):
        record, folder, verification = self.observe(host_valid=False)
        self.assertFalse(record["valid"])
        self.assertFalse(record["host_verified"])
        self.assertIn("independent final host verification failed", record["error"])
        self.assertTrue((folder / "work").exists())
        verification.assert_called_once()

    def test_staged_write_into_original_workspace_is_invalid(self):
        for backend in ("staged", "vm", "vm-writeback"):
            with self.subTest(backend=backend):
                record, folder, verification = self.observe(backend=backend, lower_mutated=True)
                self.assertFalse(record["valid"])
                self.assertIn("modified the original workspace", record["error"])
                self.assertTrue((folder / "work/output/unexpected-write").exists())
                verification.assert_not_called()


class CampaignContractTests(unittest.TestCase):
    def test_first_failure_skips_only_remaining_attempts_of_that_cell(self):
        with tempfile.TemporaryDirectory(prefix="user-workloads-campaign-") as directory:
            batch = Path(directory)
            protocol = dict(
                skip_remaining_on_case_failure=True,
                orders=[
                    dict(case_id=case, pair=pair, backends=["native", "vm"])
                    for pair in (1, 2, 3)
                    for case in ("M01", "M02")
                ],
            )
            (batch / "protocol.json").write_text(json.dumps(protocol))
            state = {
                "cpu.max": "200000 100000",
                "memory.max": str(2 << 30),
                "memory.swap.max": "0",
                "affinity": [0, 1],
            }

            def trial(_batch, _protocol, case, pair, backend):
                valid = (case, backend) != ("M01", "vm")
                return dict(
                    case_id=case,
                    pair=pair,
                    backend=backend,
                    valid=valid,
                    rows=[{}, {}, {}] if valid else [],
                    error=None if valid else "retained first timeout",
                )

            with (
                mock.patch.object(RUNNER, "verify_inputs") as verification,
                mock.patch.object(RUNNER.SHARED, "cgroup_state", return_value=state),
                mock.patch.object(RUNNER, "one_trial", side_effect=trial) as attempts,
                mock.patch("builtins.print"),
            ):
                RUNNER.campaign(batch)
            observed = [
                (call.args[2], call.args[3], call.args[4]) for call in attempts.call_args_list
            ]
            self.assertEqual(len(observed), 10)
            self.assertEqual(
                [item for item in observed if item[0::2] == ("M01", "vm")], [("M01", 1, "vm")]
            )
            self.assertEqual(
                [item[1] for item in observed if item[0::2] == ("M02", "vm")], [1, 2, 3]
            )
            records = json.loads((batch / "records.json").read_text())
            skipped = json.loads((batch / "skipped.json").read_text())
            status = json.loads((batch / "status.json").read_text())
            self.assertEqual(len(records), 10)
            self.assertEqual(sum(not row["valid"] for row in records), 1)
            self.assertEqual(
                [(row["case_id"], row["backend"], row["pair"]) for row in skipped],
                [("M01", "vm", 2), ("M01", "vm", 3)],
            )
            self.assertTrue(all("not executed" in row["reason"] for row in skipped))
            self.assertEqual(status["trials"], 10)
            self.assertEqual(status["failed"], 1)
            self.assertEqual(status["skipped"], 2)
            self.assertEqual(status["excluded"], 0)
            self.assertEqual(status["measurements"], 27)
            self.assertFalse(status["complete"])
            self.assertEqual(verification.call_count, 2)


if __name__ == "__main__":
    unittest.main()
