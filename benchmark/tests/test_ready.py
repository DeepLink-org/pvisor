"""Ready timing stops on receipt, while correctness still requires normal exit."""

import json
import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from tests.fixtures import workspace

# The shared observer and its evidence auditor are tested separately.

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "startup"))
import audit as AUDIT  # noqa: E402
import timing as READY  # noqa: E402


class ReadyContracts(unittest.TestCase):
    def capture(self, root, code, timeout=2):
        return READY.capture(
            [sys.executable, "-c", code], root, os.environ.copy(), root / "output.log", timeout
        )

    def test_ready_excludes_exit_delay_and_keeps_output(self):
        with workspace() as (root, resources):
            result = self.capture(
                root, "import time; print('REFERENCE_READY', flush=True); time.sleep(.25)"
            )
            self.assertGreater(result["end_ns"] - result["ready_ns"], 200_000_000)
            self.assertEqual(result["ready_duration_ns"], result["ready_ns"] - result["start_ns"])
            self.assertEqual(result["exit_duration_ns"], result["end_ns"] - result["start_ns"])
            self.assertEqual((root / "output.log").read_text(), "REFERENCE_READY\n")
            self.assertEqual(json.loads((root / "output.timing.json").read_text()), result)

    def test_missing_duplicate_partial_and_failed_exit_reject_data(self):
        for code in (
            "print('not ready')",
            "print('REFERENCE_READY\\nREFERENCE_READY')",
            "print('REFERENCE_READY suffix')",
            "print('REFERENCE_READY', end='')",
            "print('REFERENCE_READY'); raise SystemExit(7)",
        ):
            with self.subTest(code=code), workspace() as (root, resources):
                with self.assertRaises(ValueError):
                    self.capture(root, code)
                self.assertTrue((root / "output.log").read_bytes())
                self.assertIn("error", json.loads((root / "output.timing.json").read_text()))

    def test_timeout_after_ready_kills_owned_child(self):
        with workspace() as (root, resources):
            code = (
                "import subprocess, time; "
                "child=subprocess.Popen(['sleep','10']); "
                f"open({str(root / 'child.pid')!r},'w').write(str(child.pid)); "
                "print('REFERENCE_READY',flush=True); time.sleep(10)"
            )
            with self.assertRaises(TimeoutError):
                self.capture(root, code, timeout=0.2)
            pid = int((root / "child.pid").read_text())
            path = Path(f"/proc/{pid}/stat")
            if path.exists():
                self.assertEqual(path.read_text().split()[2], "Z")
            self.assertIn("error", json.loads((root / "output.timing.json").read_text()))

    def test_statistics_use_formal_samples_and_sample_sd(self):
        result = READY.summarize([1_000_000, 3_000_000, 5_000_000])
        self.assertEqual(result, dict(zip(READY.READY_FIELDS, [3, 3, 3, 2, 1, 5])))
        for invalid in ([1], [True, 2], [0, 2], [1.1, 2]):
            with self.assertRaises(ValueError):
                READY.summarize(invalid)

    def test_paired_summary_rejects_unpaired_and_reversed_durations(self):
        valid = [
            {"ready_ns": 1_000_000, "exit_ns": 3_000_000},
            {"ready_ns": 3_000_000, "exit_ns": 5_000_000},
        ]
        result = READY.summarize_pairs(valid)
        self.assertEqual(result["N"], 2)
        self.assertEqual(result["Ready median ms"], 2)
        self.assertEqual(result["Exit median ms"], 4)
        for bad in (
            [{"ready_ns": 1}],
            [{"ready_ns": 2, "exit_ns": 1}],
            [{"ready_ns": True, "exit_ns": 3}],
            [valid[0]],
        ):
            with self.assertRaises(ValueError):
                READY.summarize_pairs(bad)

    def test_row_returns_once_after_three_warmups_and_sixty_samples(self):
        with workspace() as (root, resources):
            (root / "trials/00000").mkdir(parents=True)
            (root / "trials/00000/timing.json").write_text("{}")
            (root / "manifest.json").write_text(
                json.dumps(
                    dict(
                        status="prepared", metric_scope=READY.COMMON.READY_EXIT_SCOPE, artifacts={}
                    )
                )
            )
            with patch.object(
                READY,
                "run_trial",
                side_effect=lambda batch, phase, index: {
                    "ready_ns": (index + 1) * 1_000_000,
                    "exit_ns": (index + 1) * 2_000_000,
                },
            ) as trial:
                result = READY.measure(root)
            self.assertEqual(result["N"], 60)
            self.assertEqual(result["Ready median ms"], 30.5)
            self.assertEqual(result["Exit median ms"], 61)
            self.assertEqual(len(result), 11)
            self.assertEqual(
                [(call.args[1], call.args[2]) for call in trial.call_args_list],
                [("warmup", i) for i in range(3)] + [("sample", i) for i in range(60)],
            )
            self.assertEqual(
                len(json.loads((root / "timing.samples.json").read_text())["samples"]), 60
            )

    def test_failed_sample_cannot_write_summary(self):
        with workspace() as (root, resources):
            (root / "trials/00000").mkdir(parents=True)
            (root / "trials/00000/timing.json").write_text("{}")
            (root / "manifest.json").write_text(
                json.dumps(
                    dict(
                        status="prepared", metric_scope=READY.COMMON.READY_EXIT_SCOPE, artifacts={}
                    )
                )
            )
            with patch.object(
                READY, "run_trial", side_effect=[1, 1, 1, 1, ValueError("incorrect output")]
            ):
                with self.assertRaisesRegex(ValueError, "incorrect output"):
                    READY.measure(root)
            self.assertFalse((root / "timing.samples.json").exists())

    def test_audit_recomputes_all_samples_and_rejects_tampering(self):
        import shlex
        from copy import deepcopy

        with workspace() as (root, resources):
            subject = ["/frozen/pvisor", "run"]
            protocol = dict(
                subject=subject,
                scope=READY.COMMON.READY_EXIT_SCOPE,
                measurement_engine="python-perf_counter_ns",
                ready="exact REFERENCE_READY line received",
                guest_cpus=2,
                guest_memory_mib=128,
            )
            READY.COMMON.save(root / "protocol.json", protocol)
            phases = (
                [("preflight", 0)]
                + [("warmup", i) for i in range(3)]
                + [("sample", i) for i in range(60)]
            )
            samples = []
            for i, (phase, index) in enumerate(phases):
                trial = root / f"trials/{i:05d}"
                duration = (index + 1) * 1_000_000
                start = (i + 1) * 1_000_000_000
                READY.COMMON.save(
                    trial / "timing.json",
                    dict(
                        command=subject,
                        phase=phase,
                        index=index,
                        start_ns=start,
                        ready_ns=start + duration,
                        end_ns=start + duration + 1_000_000,
                        ready_duration_ns=duration,
                        exit_duration_ns=duration + 1_000_000,
                        exit_code=0,
                    ),
                )
                (trial / "stdout").write_text(
                    'REFERENCE_READY\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\n'
                )
                (trial / "stderr").touch()
                READY.COMMON.save(
                    trial / "stage/run-bundle.json",
                    dict(
                        run=dict(
                            state="completed",
                            exit_code=0,
                            executor=dict(isolation="virtual_machine"),
                        ),
                        safety=dict(filesystem_changes_staged=True),
                    ),
                )
                if phase == "sample":
                    samples.append({"ready_ns": duration, "exit_ns": duration + 1_000_000})
            result = READY.summarize_pairs(samples)
            READY.COMMON.save(
                root / "timing.samples.json",
                dict(
                    engine="python-perf_counter_ns",
                    unit="nanosecond",
                    samples=samples,
                    result=result,
                ),
            )
            command = shlex.join(["python3", str(ROOT / "startup/pvisor_vm_timing.py"), str(root)])
            evidence = dict(
                commands=[dict(kind="script-row", command=command, raw=result, value=result)],
                cells=[],
                rows=[
                    dict(
                        row="pVisor VM",
                        command=command,
                        expression=command,
                        columns=list(READY.FIELDS),
                    )
                ],
            )
            manifest = dict(
                metric_scope=READY.COMMON.READY_EXIT_SCOPE,
                entry="startup/pvisor_vm_timing.json",
                conditions=["pvisor-vm"],
                benchmark_id="B-STARTUP",
                measurement_engine="python-perf_counter_ns",
                runs=60,
                warmup=3,
                rows={"pVisor VM": "pvisor-vm"},
            )
            template = {"pVisor VM": "$ " + command}
            resolved = {"pVisor VM": result}
            resources.enter_context(patch("audit.audit_startup"))
            AUDIT.audit_timing(root, manifest, evidence, resolved, template)
            changed = deepcopy(resolved)
            changed["pVisor VM"]["Ready mean ms"] += 0.01
            with self.assertRaisesRegex(ValueError, "resolved row"):
                AUDIT.audit_timing(root, manifest, evidence, changed, template)
            (root / "trials/00001/timing.json").unlink()
            with self.assertRaisesRegex(ValueError, "missing Ready"):
                AUDIT.audit_timing(root, manifest, evidence, resolved, template)


if __name__ == "__main__":
    unittest.main()
