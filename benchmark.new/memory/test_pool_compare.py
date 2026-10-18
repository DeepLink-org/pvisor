"""Evidence gates for the Firecracker/pooled-memory curve experiment."""

import copy
import errno
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "pool_compare", Path(__file__).with_name("pool_compare.py")
)
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


def fixture():
    hashes = {f"{n}:{p}": f"digest-{n}-{p}" for n in range(1, 5) for p in (0, 25, 100)}
    cfg = dict(arm="firecracker", hashes=hashes)
    counters = {
        "memory.max": str(2**31),
        "memory.swap.max": "0",
        "memory.swap.current": "0",
        "cpu.max": "400000 100000",
        "cpu.stat": "usage_usec 1\n",
        "memory.events": "oom 0\nmax 0\n",
    }
    pids = [11, 12, 13, 14]
    processes = [
        dict(
            pid=n,
            pss_bytes=4096,
            smaps_rollup="Pss: 4 kB\n",
            mapping_evidence=dict(mergeable_bytes=0),
        )
        for n in pids
    ]

    def phase(name, second):
        return dict(
            name=name,
            monotonic_ns=second * 10**9,
            counters=copy.deepcopy(counters),
            guest_pids=pids.copy(),
            processes=copy.deepcopy(processes),
        )

    phases = [phase(f"idle-{n}", n) for n in subject.BOUNDARIES]
    phases += [phase("readback", 61), phase("cow-25", 62), phase("cow-100", 63)]
    files = dict(
        run="1",
        pages_to_scan="100",
        sleep_millisecs="20",
        smart_scan="1",
        advisor_mode="[none] scan-time",
        merge_across_nodes="1",
        use_zero_pages="0",
        max_page_sharing="256",
    )
    raw = dict(
        status="passed",
        condition=cfg,
        all_ready_ns=0,
        before=phase("before", 0),
        after=phase("after", 64),
        phases=phases,
        checks=[
            dict(token=t, instance=n, percent=p, sha256=hashes[f"{n}:{p}"])
            for (t, n), p in subject.expected_checks().items()
        ],
        cleanup=[dict(pid=n, reaped=True) for n in pids],
        kernel_before=dict(files=files.copy()),
        kernel_after=dict(files=files.copy()),
    )
    return raw, cfg


class CurveEvidenceTests(unittest.TestCase):
    def test_complete_cohort_has_58_distinct_full_oracle_checks(self):
        raw, cfg = fixture()
        self.assertEqual(len(raw["checks"]), 58)
        subject.validate_worker(raw, cfg)

    def test_missing_payload_checks_and_wrong_digest_are_rejected(self):
        for failure in ("missing", "duplicate", "checksum", "state"):
            with self.subTest(failure=failure):
                raw, cfg = fixture()
                if failure == "missing":
                    raw["checks"].pop()
                if failure == "duplicate":
                    raw["checks"][-1] = raw["checks"][0]
                if failure == "checksum":
                    raw["checks"][0]["sha256"] = "forged"
                if failure == "state":
                    raw["checks"][0]["percent"] = 100
                with self.assertRaises(ValueError):
                    subject.validate_worker(raw, cfg)

    def test_resource_curve_and_native_ownership_gaps_are_rejected(self):
        for failure in ("phase", "duration", "process", "reaping", "pss", "oom", "swap"):
            with self.subTest(failure=failure):
                raw, cfg = fixture()
                if failure == "phase":
                    raw["phases"].pop()
                if failure == "duration":
                    raw["all_ready_ns"] = 1
                if failure == "process":
                    raw["phases"][0]["processes"].pop()
                if failure == "reaping":
                    raw["cleanup"].pop()
                if failure == "pss":
                    raw["phases"][0]["processes"][0]["pss_bytes"] = 1
                if failure == "oom":
                    raw["phases"][0]["counters"]["memory.events"] = "oom 1\n"
                if failure == "swap":
                    raw["phases"][0]["counters"]["memory.swap.current"] = "4096"
                with self.assertRaises(ValueError):
                    subject.validate_worker(raw, cfg)

    def test_stock_firecracker_must_not_silently_use_advice(self):
        raw, cfg = fixture()
        raw["phases"][0]["processes"][0]["mapping_evidence"]["mergeable_bytes"] = 4096
        with self.assertRaises(ValueError):
            subject.validate_worker(raw, cfg)

    def test_removed_arm_cannot_be_validated_or_enter_the_matrix(self):
        self.assertEqual(len(subject.ARMS), 7)
        raw, cfg = fixture()
        cfg["arm"] = "firecracker-ksm-experimental"
        self.assertNotIn(cfg["arm"], subject.ARMS)
        with self.assertRaisesRegex(ValueError, "unsupported arm"):
            subject.validate_worker(raw, cfg)

    def test_pvisor_ksm_requires_advice_on_every_vm(self):
        raw, cfg = fixture()
        cfg["arm"] = "pvisor-ksm"
        with self.assertRaisesRegex(ValueError, "advice missing"):
            subject.validate_worker(raw, cfg)
        for phase in raw["phases"]:
            for process in phase["processes"]:
                process["mapping_evidence"]["mergeable_bytes"] = 256 * 1024**2
        subject.validate_worker(raw, cfg)
        raw["phases"][-1]["processes"][0]["mapping_evidence"]["mergeable_bytes"] = 0
        with self.assertRaisesRegex(ValueError, "advice missing"):
            subject.validate_worker(raw, cfg)

    def test_unknown_schema_cannot_skip_evidence_gates(self):
        raw, cfg = fixture()
        cfg["schema"] = "future-version"
        with self.assertRaisesRegex(ValueError, "schema"):
            subject.validate_worker(raw, cfg)

    def test_new_profile_requires_observed_affinity_and_cgroup_membership(self):
        raw, cfg = fixture()
        cfg["schema"] = "pvisor-pool-curve-condition/v2"
        for phase in [raw["before"], *raw["phases"], raw["after"]]:
            phase["cgroup"] = "/sys/fs/cgroup/owned-unit"
            phase["ksm"] = raw["kernel_before"]
            for process in phase["processes"]:
                process["cpu_affinity"] = [0, 1, 2, 3]
                process["cgroup"] = "0::/owned-unit\n"
        subject.validate_worker(raw, cfg)
        raw["phases"][0]["processes"][0]["cpu_affinity"] = [4, 5]
        with self.assertRaisesRegex(ValueError, "placement"):
            subject.validate_worker(raw, cfg)

    def test_post_write_window_keeps_four_vms_and_cannot_be_shortened(self):
        raw, cfg = fixture()
        cfg["schema"] = "pvisor-pool-curve-condition/v3"
        raw["post_write_origin_ns"] = 63 * 10**9
        for boundary in (1, 2, 5, 10, 20, 30):
            phase = copy.deepcopy(raw["phases"][10])
            phase.update(name=f"post-write-{boundary}", monotonic_ns=(63 + boundary) * 10**9)
            raw["phases"].append(phase)
        raw["after"]["monotonic_ns"] = 94 * 10**9
        for phase in [raw["before"], *raw["phases"], raw["after"]]:
            phase.update(cgroup="/sys/fs/cgroup/owned-unit", ksm=raw["kernel_before"])
            for process in phase["processes"]:
                process.update(cpu_affinity=[0, 1, 2, 3], cgroup="0::/owned-unit\n")
        subject.validate_worker(raw, cfg)
        for failure in ("missing", "short", "origin", "guest"):
            with self.subTest(failure=failure):
                bad = copy.deepcopy(raw)
                if failure == "missing":
                    bad["phases"].pop()
                if failure == "short":
                    bad["phases"][-1]["monotonic_ns"] -= 1
                if failure == "origin":
                    bad["post_write_origin_ns"] += 1
                if failure == "guest":
                    bad["phases"][-1]["guest_pids"].pop()
                with self.assertRaises(ValueError):
                    subject.validate_worker(bad, cfg)


@unittest.skipUnless(
    Path("/proc/self/smaps_rollup").exists() and hasattr(os, "WNOWAIT"),
    "requires Linux procfs and waitid without reaping",
)
class PssSamplingTests(unittest.TestCase):
    def test_exited_process_with_proc_entry_does_not_invalidate_live_pss(self):
        with tempfile.TemporaryDirectory() as directory:
            group = Path(directory)
            child = subprocess.Popen(["/bin/true"])
            try:
                os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOWAIT)
                self.assertTrue((Path("/proc") / str(child.pid)).exists())
                (group / "cgroup.procs").write_text(f"{os.getpid()}\n{child.pid}\n")
                sample = subject.pss_sample(group)
                self.assertNotIn("error", sample)
                self.assertEqual(sample["exited_pids"], [child.pid])
                self.assertEqual([p["pid"] for p in sample["processes"]], [os.getpid()])
                self.assertGreater(sample["pss_bytes"], 0)
            finally:
                child.wait(timeout=5)

    def test_permission_and_io_failures_are_not_treated_as_process_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            group = Path(directory)
            (group / "cgroup.procs").write_text(f"{os.getpid()}\n")
            rollup = Path("/proc") / str(os.getpid()) / "smaps_rollup"
            original = Path.read_text
            for code in (errno.EACCES, errno.EIO):

                def read(path, *args, **kwargs):
                    if path == rollup:
                        raise OSError(code, os.strerror(code), str(path))
                    return original(path, *args, **kwargs)

                with self.subTest(errno=code), patch.object(Path, "read_text", read):
                    sample = subject.pss_sample(group)
                    self.assertIn("error", sample)
                    self.assertEqual(sample["exited_pids"], [])
                    self.assertNotIn("pss_bytes", sample)


if __name__ == "__main__":
    unittest.main()
