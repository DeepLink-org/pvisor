"""Exercise complete sharing publication with synthetic, non-public evidence."""

import copy
import json
import shutil
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from memory_savings import digest
from memory_sharing import ARMS, matrix
from publish_memory_sharing import load_cohort, publish
from test_memory_scale import valid


def cohort(root):
    root.mkdir()
    for name in (
        "probe",
        "source-manifest.json",
        "memory_sharing.py",
        "memory_savings.py",
        "memory_scale.py",
        "linux_cold_runtime.py",
    ):
        (root / name).write_text("synthetic fixture\n")
    receipt = dict(
        example_sha256=digest(root / "probe"),
        source_manifest_sha256=digest(root / "source-manifest.json"),
    )
    (root / "build-receipt.json").write_text(json.dumps(receipt))
    (root / "input-manifest.json").write_text("{}\n")
    value = dict(
        schema="pvisor-memory-sharing-cohort/v1",
        benchmark_id="B-VM-MEMORY",
        role="user-facing",
        complete=True,
        selected=matrix(),
        arguments=dict(
            preflight=False,
            samples=30,
            warmups=3,
            scan_seconds=30,
            rootfs="/rootfs",
            firmware="/firmware",
        ),
        host={},
        budget=dict(cpu_cores=4, memory_max=2147483648, swap_max=0),
        input_verification=dict(rootfs=True, firmware=True),
        host_verification={k: True for k in ("kernel", "cpu_model", "ksm", "host_cp_sha256")},
        binary_sha256=digest(root / "probe"),
        harnesses={
            name: digest(root / name)
            for name in (
                "memory_sharing.py",
                "memory_savings.py",
                "memory_scale.py",
                "linux_cold_runtime.py",
            )
        },
        attempts=[],
    )
    for round_id in range(-3, 30):
        for n, pattern, arm in matrix():
            trial = root / str(len(value["attempts"]))
            (trial / "w").mkdir(parents=True)
            condition = dict(
                vms=n,
                pattern=pattern,
                **ARMS[arm],
                cpus=2,
                seed=20261006,
                settle_ms=500,
                ksm_wait_seconds=30,
            )
            raw, _ = valid(mode=condition["mode"], n=n, scanner="1")
            raw["conditions"].update(condition)
            raw["profile"]["cpus"] = 2
            raw["source"]["binary_sha256"] = value["binary_sha256"]
            raw["ksm_scan_window"]["seconds"] = 30
            if condition["mode"] == "ksm":
                raw["dynamic_ksm_scan_window"]["seconds"] = 30
            if condition["independent_inodes"]:
                raw["checks"] += [
                    dict(
                        name="independent_ram_inode",
                        passed=True,
                        evidence=dict(instance=i, device=1, inode=i, bytes=256 * 1024**2),
                    )
                    for i in range(1, n + 1)
                ]
            raw_path = trial / "w/raw.json"
            raw_path.write_text(json.dumps(raw))
            manifest = trial / "retired-runtime-manifest.json"
            manifest.write_text("[]\n")
            monitor = [
                dict(
                    group=raw["before"]["cgroup"],
                    memory_peak=200,
                    budget={
                        "cpu.max": "400000 100000",
                        "memory.max": "2147483648",
                        "memory.swap.max": "0",
                        "memory.swap.current": "0",
                        "pids.max": "128",
                    },
                )
            ]
            (trial / "monitor.json").write_text(json.dumps(monitor))
            (trial / "guard.json").write_text('[{"jobs":[]}]')
            value["attempts"].append(
                dict(
                    condition=condition,
                    arm=arm,
                    round=round_id,
                    warmup=round_id < 0,
                    status="successful",
                    returncode=0,
                    unit_quiescent=True,
                    raw_sha256=digest(raw_path),
                    retired_manifest_sha256=digest(manifest),
                )
            )
    path = root / "report.json"
    path.write_text(json.dumps(value))
    return path, value


class PublishMemorySharingTests(unittest.TestCase):
    def test_complete_sharing_cohort_exports_aggregates(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            path, _ = cohort(tmp_path / "fixture")
            rows = publish(path, tmp_path / "derived")
            assert rows and all(row["n"] == 30 for row in rows)
            metrics = {row["metric"] for row in rows}
            assert {
                "ready_per_vm_scan_ms",
                "cow25_per_vm_checked_write_ms",
                "cow100_per_vm_checked_write_ms",
            } <= metrics
            assert not any("group_scan" in metric for metric in metrics)
            assert len(load_cohort(path)[1]) == 33 * 36
            assert (tmp_path / "derived/memory-sharing-comparisons.csv").is_file()

    def test_incomplete_or_changed_sharing_evidence_cannot_publish(self):
        for change in [
            lambda r: r["attempts"].pop(0),
            lambda r: r["attempts"].append(copy.deepcopy(r["attempts"][0])),
            lambda r: r["attempts"][0]["condition"].update(cpus=1),
            lambda r: r["host_verification"].update(ksm=False),
            lambda r: r["budget"].update(cpu_cores=2),
            lambda r: r["harnesses"].clear(),
        ]:
            with self.subTest(change=change):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    path, value = cohort(tmp_path / "fixture")
                    change(value)
                    path.write_text(json.dumps(value))
                    with self.assertRaises(ValueError):
                        load_cohort(path)

    def test_static_four_vm_export_is_distinct_from_formal_statistics(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            original, value = cohort(tmp_path / "fixture")
            root = tmp_path / "static"
            root.mkdir()
            for name in (
                "probe",
                "source-manifest.json",
                "build-receipt.json",
                "input-manifest.json",
                *value["harnesses"],
            ):
                shutil.copy2(original.parent / name, root / name)
            selected = []
            for index, row in enumerate(value["attempts"][:36]):
                if row["condition"]["vms"] != 4:
                    continue
                trial = root / str(len(selected))
                shutil.copytree(original.parent / str(index), trial)
                row.update(round=0, warmup=False)
                row["condition"]["ksm_wait_seconds"] = 2
                raw_path = trial / "w/raw.json"
                raw = json.loads(raw_path.read_text())
                raw["conditions"]["ksm_wait_seconds"] = 2
                raw["ksm_scan_window"]["seconds"] = 2
                if "dynamic_ksm_scan_window" in raw:
                    raw["dynamic_ksm_scan_window"]["seconds"] = 2
                raw_path.write_text(json.dumps(raw))
                row["raw_sha256"] = digest(raw_path)
                selected.append(row)
            value["attempts"] = selected
            value["selected"] = [c for c in matrix() if c[0] == 4]
            value["arguments"].update(static=True, samples=1, warmups=0, scan_seconds=2)
            path = root / "report.json"
            path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):
                load_cohort(path)
            rows = publish(path, tmp_path / "derived", static=True)
            assert rows and all(
                row["n"] == 1 and "value" in row and "p50" not in row for row in rows
            )
            assert len(load_cohort(path, static=True)[1]) == 12
