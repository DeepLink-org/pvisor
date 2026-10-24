"""B-STARTUP-DIAG: preserve failures, clock boundaries and independent trials."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "startup_diagnostic", Path(__file__).resolve().parents[1] / "startup/startup_profile.py"
)
PROFILE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROFILE)


def span(stage="vm.build", pid=22, start=0, end=1000, **extra):
    return PROFILE.PREFIX + json.dumps(
        {
            "schema": 1,
            "pid": pid,
            "role": "runner",
            "clock": "host_monotonic_process_relative",
            "stage": stage,
            "start_ns": start,
            "end_ns": end,
            "duration_ns": end - start,
            "epoch": "vm-runtime",
            "outcome": "ok",
            **extra,
        }
    )


def mark(pid, stage, us, run="run-1"):
    return f'pvisor-startup pid={pid} run_id="{run}" stage={stage} monotonic_us={us}'


class StartupProfileContracts(unittest.TestCase):
    def parse(self, text):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "log"
            path.write_text(text)
            return PROFILE.analyze(path)

    def test_process_epochs_and_nested_spans_are_never_summed_or_aligned(self):
        trial = self.parse(
            "\n".join(
                [
                    span(),
                    span("vmm.guest_memory", start=100, end=600),
                    span("vm.build", pid=33, end=2000, epoch="other-process"),
                ]
            )
        )
        rows = PROFILE.summarize([trial])
        self.assertEqual(len(rows), 3)
        self.assertEqual(sorted(row["median_ms"] for row in rows), [0.0005, 0.001, 0.002])
        self.assertTrue(all(row["kind"] == "span" for row in rows))

    def test_checkpoints_require_same_process_and_skip_workload_teardown(self):
        trial = self.parse(
            "\n".join(
                [
                    mark(1, "session.begin", 10),
                    mark(2, "session.begin", 500),
                    mark(1, "process.spawn_ready", 20),
                    mark(1, "attempt.teardown_begin", 900000),
                    mark(2, "process.spawn_ready", 520),
                    mark(2, "cli.run_finished", 1000000),
                ]
            )
        )
        self.assertEqual([row["duration_ns"] for row in trial["observations"]], [10000, 20000])

    def test_only_explicit_owner_binding_permits_cross_process_handoff(self):
        text = "\n".join(
            [
                mark(10, "cli.owner_spawn_begin", 10, "-"),
                mark(20, "cli.run_begin", 40, "-"),
                mark(20, "process.spawn_ready", 50),
            ]
        )
        self.assertFalse(any(row["kind"] == "handoff" for row in self.parse(text)["observations"]))
        trial = self.parse(text + "\npvisor-owner-request frontend_pid=10 owner_pid=20")
        handoff = [row for row in trial["observations"] if row["kind"] == "handoff"]
        self.assertEqual(handoff[0]["duration_ns"], 30000)

    def test_guest_clock_is_accepted_as_a_separate_epoch(self):
        trial = self.parse(
            "\n".join(
                [
                    span(),
                    span(
                        "guest.initialize",
                        pid=1,
                        end=99,
                        clock="guest_monotonic_process_relative",
                        epoch="guest-supervisor",
                    ),
                ]
            )
        )
        rows = PROFILE.summarize([trial])
        self.assertEqual({row["epoch"] for row in rows}, {"vm-runtime", "guest-supervisor"})
        self.assertEqual(len(rows), 2)

    def test_missing_and_failed_spans_are_explicit(self):
        first = self.parse(span(outcome="unknown"))
        second = self.parse(span(stage="vmm.devices"))
        rows = PROFILE.summarize([first, second])
        self.assertTrue(all(row["missing_trials"] == 1 for row in rows))
        self.assertIn("unknown", [row["outcome"] for row in rows])

    def test_malformed_and_failed_observer_records_are_rejected(self):
        for text in [
            span(duration_ns=99),
            span(schema=2),
            span(start_ns=-1),
            span() + '\npvisor-startup-observer {"exit_code":1,"ready_count":1}',
            span() + '\npvisor-startup-observer {"exit_code":0,"ready_count":2}',
        ]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.parse(text)

    def test_collector_observes_exact_ready_and_retains_raw_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "batch"
            source = f'import sys; print("READY", flush=True); print({span()!r}, file=sys.stderr)'
            logs = PROFILE.collect(output, [sys.executable, "-c", source], 2, "READY", 5)
            trials = [PROFILE.analyze(log) for log in logs]
            self.assertEqual(len(trials), 2)
            self.assertTrue(all(trial["observer"]["ready_count"] == 1 for trial in trials))
            self.assertTrue((output / "manifest.json").is_file())
            self.assertEqual((output / "trial-000/stdout.log").read_text(), "READY\n")
            with self.assertRaises(FileExistsError):
                PROFILE.collect(output, [sys.executable, "-c", source], 1, "READY", 5)

    def test_duplicate_missing_ready_nonzero_and_timeout_preserve_failure(self):
        for source, timeout in [
            ('print("READY\\nREADY")', 5),
            ('print("OTHER")', 5),
            ('print("READY"); exit(2)', 5),
            ("import time; time.sleep(10)", 0.05),
        ]:
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "batch"
                with self.assertRaises(ValueError):
                    PROFILE.collect(output, [sys.executable, "-c", source], 1, "READY", timeout)
                text = (output / "trial-000/stderr.log").read_text()
                self.assertIn(PROFILE.OBSERVER, text)
                self.assertTrue((output / "manifest.json").is_file())
