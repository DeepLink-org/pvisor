import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import vcpu_idle as v


def snapshot(enabled=True, cpus=1):
    return dict(
        hypervisor="Kvm",
        enabled=enabled,
        session=int(enabled),
        topology_generation=1,
        sequence=1,
        sampled_at_ns=100,
        all_waiting=False,
        idle_epoch=0,
        all_waiting_since_ns=None,
        completed_all_waiting_ns=0,
        rejection="Unknown" if enabled else "Disabled",
        vcpus=[
            dict(
                id=i,
                online=True,
                state="Unknown",
                sequence=1,
                since_ns=0,
                transitions=0,
                wait_entries=0,
                wait_exits=0,
                completed_wait_ns=0,
            )
            for i in range(cpus)
        ],
    )


class VcpuIdleTests(unittest.TestCase):
    def test_schedule_complete_reproducible_adjacent_pairs(self):
        plan = v.schedule(3, 17)
        self.assertEqual(plan, v.schedule(3, 17))
        self.assertNotEqual(plan, v.schedule(3, 18))
        self.assertEqual(len(plan), 42)
        for a, b in zip(plan[::2], plan[1::2]):
            self.assertEqual(
                (a["round"], a["workload"], a["cpus"]), (b["round"], b["workload"], b["cpus"])
            )
            self.assertEqual({a["observer"], b["observer"]}, {True, False})
        for r in range(3):
            self.assertEqual(
                {(x["workload"], x["cpus"]) for x in plan if x["round"] == r}, set(v.CASES)
            )

    def test_unknown_is_valid_not_idle(self):
        v.validate_snapshot(snapshot(), 1, True)
        v.validate_snapshot(snapshot(False), 1, False)
        s = snapshot()
        s["all_waiting"] = True
        with self.assertRaises(ValueError):
            v.validate_snapshot(s, 1, True)

    def test_hvf_wait_and_smp_negative(self):
        s = snapshot(cpus=2)
        s.update(
            hypervisor="Hvf",
            all_waiting=True,
            all_waiting_since_ns=10,
            rejection="WakeDeadlineUnavailable",
        )
        for cpu in s["vcpus"]:
            cpu["state"] = "WaitingForEvent"
        v.validate_snapshot(s, 2, True)
        s["vcpus"][1]["state"] = "Executing"
        with self.assertRaises(ValueError):
            v.validate_snapshot(s, 2, True)
        s.update(all_waiting=False, all_waiting_since_ns=None, rejection="NotAllWaiting")
        v.validate_snapshot(s, 2, True)

    def test_bad_topology_and_pause_rejected(self):
        for key, value in [
            ("state", "ManualPaused"),
            ("state", "Stopped"),
            ("state", "HostDescheduled"),
            ("id", 7),
            ("since_ns", 101),
        ]:
            s = snapshot()
            s["vcpus"][0][key] = value
            with self.assertRaises(ValueError):
                v.validate_snapshot(s, 1, True)

    def fixture(self, d, enabled=True):
        cell = dict(round=0, workload="busy", cpus=1, observer=enabled)
        v.save(d / "initial.json", snapshot(enabled))
        v.save(
            d / "guest.json",
            dict(
                mode="busy",
                cpus=1,
                start_method="fork",
                elapsed_ns=10**9,
                workers=[
                    dict(
                        cpu=0,
                        affinity=[0],
                        iterations=1,
                        digest=v.DIGEST,
                        elapsed_ns=10**9,
                        started_ns=1,
                        finished_ns=10**9 + 1,
                        busy=True,
                        cpu_ns=100,
                    )
                ],
            ),
        )
        v.save(
            d / "observer.json",
            dict(observer=enabled, samples=int(enabled), limit=6102, complete=True),
        )
        (d / "samples.jsonl").write_text(
            json.dumps(dict(snapshot=snapshot(), sample_call_and_encode_ns=9)) + "\n"
            if enabled
            else ""
        )
        (d / "stdout.log").write_text("vcpu-guest-ok\n")
        return cell

    def test_output_and_off_validation(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for enabled in (False, True):
                cell = self.fixture(d, enabled)
                self.assertEqual(v.validate_trial(d, cell, 1, 10)["samples"], int(enabled))
            guest = json.loads((d / "guest.json").read_text())
            guest["workers"][0]["digest"] = "corrupt"
            v.save(d / "guest.json", guest)
            with self.assertRaises(ValueError):
                v.validate_trial(d, cell, 1, 10)

    def test_incomplete_session_regression_and_logs(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            cell = self.fixture(d)
            for mutate in ("session", "regression", "stdout", "incomplete", "capacity"):
                self.fixture(d)
                if mutate in ("session", "regression"):
                    s = snapshot()
                    s["session" if mutate == "session" else "sequence"] = 0
                    (d / "samples.jsonl").write_text(
                        json.dumps(dict(snapshot=s, sample_call_and_encode_ns=1)) + "\n"
                    )
                elif mutate == "stdout":
                    (d / "stdout.log").write_text("")
                else:
                    ob = json.loads((d / "observer.json").read_text())
                    ob["complete" if mutate == "incomplete" else "limit"] = False
                    v.save(d / "observer.json", ob)
                with self.assertRaises(ValueError):
                    v.validate_trial(d, cell, 1, 10)

    def test_timeout_and_nonzero_have_logs_and_cpu(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            result = v.execute(
                [sys.executable, "-c", 'import time; print("before", flush=True); time.sleep(5)'],
                d,
                0.1,
                None,
            )
            self.assertTrue(result["timeout"])
            self.assertLess(result["returncode"], 0)
            self.assertIn("before", (d / "stdout.log").read_text())
            self.assertGreaterEqual(result["cpu_s"], 0)
            result = v.execute([sys.executable, "-c", "import sys; sys.exit(3)"], d, 2, None)
            self.assertEqual(result["returncode"], 3)
            self.assertFalse(result["timeout"])

    def test_summary_excludes_whole_failed_pair(self):
        trials = []
        for r in range(3):
            for on in (False, True):
                trials.append(
                    dict(
                        cell=dict(round=r, workload="sleep", cpus=1, observer=on),
                        status="failed" if r == 2 and on else "valid",
                        execution=dict(wall_s=1 + int(on), cpu_s=0.1 + int(on)),
                    )
                )
        row = v.paired_summary(trials, 17)[0]
        self.assertEqual(row["valid_pairs"], 2)
        self.assertEqual(row["invalid_pairs"], 1)
        self.assertEqual(row["wall_s"]["ci95"], [1, 1])

    def test_input_inventory_symlinks_and_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "a").write_text("abc")
            (d / "b").symlink_to("a")
            before = v.inventory(d)
            self.assertEqual(before["b"], {"link": "a"})
            (d / "a").write_text("changed")
            self.assertNotEqual(before, v.inventory(d))

    def test_receipt_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            v.save(
                d / "build-receipt.json",
                dict(schema=1, complete=False, benchmark="B-VCPU-IDLE-ENG"),
            )
            with self.assertRaises(ValueError):
                v.verify_build(d / "build-receipt.json")

    def test_complete_receipt_detects_binary_and_frozen_source_tampering(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "source").mkdir()
            (d / "source/input.rs").write_text("source")
            (d / "vm_vcpu_observe").write_text("binary")
            sources = {"input.rs": v.digest(d / "source/input.rs")}
            v.save(d / "sources.json", sources)
            sdk = {"embedded_kernel": None}
            v.save(
                d / "build-receipt.json",
                dict(
                    schema=1,
                    complete=True,
                    benchmark="B-VCPU-IDLE-ENG",
                    sources_sha256=v.digest(d / "sources.json"),
                    binary_sha256=v.digest(d / "vm_vcpu_observe"),
                    sdk=sdk,
                ),
            )
            with (
                patch.object(v, "source_inventory", return_value=sources),
                patch.object(v.subprocess, "check_output", return_value=json.dumps(sdk).encode()),
            ):
                self.assertEqual(
                    v.verify_build(d / "build-receipt.json"), (d / "vm_vcpu_observe").resolve()
                )
                (d / "vm_vcpu_observe").write_text("tampered")
                with self.assertRaises(ValueError):
                    v.verify_build(d / "build-receipt.json")
                (d / "vm_vcpu_observe").write_text("binary")
                (d / "source/input.rs").write_text("tampered")
                with self.assertRaises(ValueError):
                    v.verify_build(d / "build-receipt.json")

    def test_rejection_and_single_cpu_kvm_wait_fail_closed(self):
        s = snapshot()
        s["rejection"] = "NotAllWaiting"
        with self.assertRaises(ValueError):
            v.validate_snapshot(s, 1, True)
        s = snapshot(cpus=2)
        s["vcpus"][0]["state"] = "WaitingForEvent"
        with self.assertRaises(ValueError):
            v.validate_snapshot(s, 2, True)

    def test_guest_script_compiles_without_booting_vm(self):
        source = (v.REPO / "crates/pvisor/examples/vm_vcpu_observe.rs").read_text()
        guest = source.split('const GUEST: &str = r#"', 1)[1].split('"#;', 1)[0]
        compile(guest, "vcpu-work.py", "exec")

    @unittest.skipUnless(sys.platform == "linux", "guest fork/affinity contract requires Linux")
    def test_guest_uses_explicit_fork_when_default_is_forkserver(self):
        source = (v.REPO / "crates/pvisor/examples/vm_vcpu_observe.rs").read_text()
        guest = source.split('const GUEST: &str = r#"', 1)[1].split('"#;', 1)[0]
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            guest = guest.replace("root = pathlib.Path('/')", f"root = pathlib.Path({str(d)!r})")
            # Translate guest CPU IDs to permitted host CPUs without changing the
            # payload's process/context/handshake code or writing the host root.
            prefix = """import multiprocessing as mp, os
mp.set_start_method('forkserver', force=True)
allowed = sorted(os.sched_getaffinity(0))
set_affinity, get_affinity = os.sched_setaffinity, os.sched_getaffinity
os.sched_setaffinity = lambda pid, cpus: set_affinity(pid, {allowed[c] for c in cpus})
os.sched_getaffinity = lambda pid: {allowed.index(c) for c in get_affinity(pid)}
"""
            cpus = min(2, len(os.sched_getaffinity(0)))
            (d / "guest.py").write_text(
                prefix + guest + "\nassert mp.get_start_method() == 'forkserver'\n"
            )
            for name in ("vcpu-go", "vcpu-release"):
                (d / name).write_text("ready")
            result = subprocess.run(
                [sys.executable, str(d / "guest.py"), "sleep", str(cpus), "1"],
                capture_output=True,
                text=True,
                timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("vcpu-guest-ok", result.stdout)
            done = json.loads((d / "vcpu-done").read_text())
            self.assertEqual(done["start_method"], "fork")
            self.assertEqual(len(done["workers"]), cpus)
            for cpu, row in enumerate(done["workers"]):
                self.assertEqual(row["affinity"], [cpu])
                self.assertEqual(row["digest"], v.DIGEST)

    def test_guest_wrong_start_method_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            cell = self.fixture(d)
            guest = json.loads((d / "guest.json").read_text())
            guest["start_method"] = "forkserver"
            v.save(d / "guest.json", guest)
            with self.assertRaises(ValueError):
                v.validate_trial(d, cell, 1, 10)

    def test_complete_failure_report_on_preflight_error(self):
        import argparse

        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            a = argparse.Namespace(output=d / "new", build_receipt=d / "missing", pairs=1, seed=1)
            self.assertEqual(v.run(a), 1)
            report = json.loads((a.output / "report.json").read_text())
            self.assertFalse(report["complete"])
            self.assertIn("error", report)
            self.assertEqual(report["offload_benefit"], "unmeasured")


if __name__ == "__main__":
    unittest.main()
