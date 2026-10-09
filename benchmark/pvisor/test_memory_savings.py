import copy
import hashlib
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from memory_savings import matrix, validate, validate_observation


def report():
    memory = dict(
        cgroup="/owned",
        current=100,
        peak=110,
        stat=dict(anon=60, file=30, kernel=10),
        cpu=dict(usage_usec=10),
        events=dict(oom=0, oom_kill=0, max=0),
    )
    names = ["before", "active", "parked", "idle-60", "restored", "after"]
    return dict(
        schema="pvisor-memory-savings/v1",
        correctness="passed",
        mode="default",
        pattern="random",
        wait=60,
        cleanup=True,
        exit_code=0,
        cpus=2,
        memory_mib=256,
        payload_bytes=64 * 1024**2,
        expected_digest="a" * 64,
        phases=[
            dict(name=name, elapsed_ms=index * 60000, memory=copy.deepcopy(memory))
            for index, name in enumerate(names)
        ],
        tasks=[
            dict(token=name, digest="a" * 64, bytes=64 * 1024**2)
            for name in ("ready", "restored", "exit")
        ],
    )


class MemorySavingsTests(unittest.TestCase):
    def test_success_requires_complete_accounting_and_integrity(self):
        value = report()
        condition = dict(mode="default", pattern="random", wait=60)
        validate(value, condition)
        changes = [
            lambda r: r["tasks"][1].update(digest="b" * 64),
            lambda r: r["phases"][3]["memory"]["events"].update(oom_kill=1),
            lambda r: r["phases"][3]["memory"].update(cgroup="/escaped"),
            lambda r: r["phases"].pop(3),
            lambda r: r.update(cleanup=False),
            lambda r: r["phases"][3].update(elapsed_ms=120001),
        ]
        for mutation in changes:
            corrupted = copy.deepcopy(value)
            mutation(corrupted)
            with self.assertRaises(ValueError):
                validate(corrupted, condition)

    def test_release_requires_empty_working_set_proof(self):
        value = report()
        value["mode"] = "release"
        condition = dict(mode="release", pattern="random", wait=60)
        with self.assertRaisesRegex(ValueError, "release proof"):
            validate(value, condition)
        value["tasks"].append(dict(token="free", bytes=0, digest=hashlib.sha256(b"").hexdigest()))
        validate(value, condition)

    def test_matrix_keeps_hot_read_control_and_offload_compression_separate(self):
        cells = matrix()
        assert len(cells) == 16 and len(set(cells)) == 16
        assert ("default", "mixed") in cells and ("cold", "mixed") in cells
        assert ("raw", "random") in cells and ("compressed", "random") in cells

    def test_observer_requires_installed_budget_and_visible_quiet_host(self):
        rows = [
            dict(
                budget={
                    "cpu.max": "400000 100000",
                    "memory.max": "2147483648",
                    "memory.swap.max": "0",
                    "memory.swap.current": "0",
                    "pids.max": "128",
                }
            )
        ]
        guards = [dict(jobs=[])]
        validate_observation(rows, guards)
        for bad_rows, bad_guards in [
            ([], guards),
            (rows, []),
            (rows, [dict(jobs=[dict(pid=123)])]),
            ([dict(error="permission denied")], guards),
        ]:
            with self.assertRaises(ValueError):
                validate_observation(bad_rows, bad_guards)
        rows[0]["budget"]["memory.swap.current"] = "4096"
        with self.assertRaisesRegex(ValueError, "budget"):
            validate_observation(rows, guards)

    def test_publication_rejects_preflight_and_incomplete_cohorts(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            import json

            from publish_memory_savings import load_cohort

            value = dict(
                schema="pvisor-memory-savings-cohort/v1",
                role="user-facing",
                complete=True,
                arguments=dict(samples=1, warmups=0, wait=5),
                selected=matrix(),
                input_verification={"rootfs": True, "firmware": True},
            )
            path = tmp_path / "report.json"
            for changes in (
                {},
                {"complete": False},
                {"arguments": dict(samples=30, warmups=3, wait=60), "selected": matrix()[:-1]},
            ):
                path.write_text(json.dumps(value | changes))
                with self.assertRaisesRegex(ValueError, "complete"):
                    load_cohort(path)

    def test_static_observation_records_builds_but_rejects_foreign_vms(self):
        rows = [
            dict(
                budget={
                    "cpu.max": "400000 100000",
                    "memory.max": "2147483648",
                    "memory.swap.max": "0",
                    "memory.swap.current": "0",
                    "pids.max": "128",
                }
            )
        ]
        builds = [dict(jobs=[dict(pid=123, kind="build/test")])]
        validate_observation(rows, builds, allow_builds=True)
        with self.assertRaises(ValueError):
            validate_observation(rows, builds)
        for jobs in ([dict(pid=123, kind="VM")], [dict(pid=123)]):
            with self.assertRaises(ValueError):
                validate_observation(rows, [dict(jobs=jobs)], allow_builds=True)

    def test_512_mib_reading_requires_complete_resident_evidence(self):
        raw = report()
        raw["memory_mib"] = 512
        condition = dict(mode="default", pattern="random", wait=60, memory_mib=512)
        with self.assertRaisesRegex(ValueError, "resident"):
            validate(raw, condition)
        for phase in raw["phases"]:
            phase["memory"].update(
                pss_bytes=1024,
                processes=[dict(pid=123, pss_bytes=1024, smaps_rollup="Pss: 1 kB\n")],
            )
        validate(raw, condition)
        raw["phases"][1]["memory"]["pss_bytes"] = 0
        with self.assertRaisesRegex(ValueError, "resident"):
            validate(raw, condition)
