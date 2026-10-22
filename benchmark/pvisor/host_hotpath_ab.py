#!/usr/bin/env python3
"""Frozen real-FSKit hot-path comparison.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: measure creation-metadata reuse, combined lookup and prepared write open.
Conclusion sought: same-host paired median changes, 95% CIs and explicit unknowns.
Design: only three production sources differ; default TTL, 3 warmups/30 random
pairs per independent immutable-nojournal/mutable-compact condition, fresh mounts,
host-hot 4 KiB files, full lower/upper/journal/detach checks. No slow-value pruning;
any failure/source change/build-test interference invalidates the cohort. Python
phase clocks via a fixed semspec row; profiles run separately; no VM/Linux claim.
"""

import argparse
import difflib
import json
import os
import platform
import random
import shutil
import stat
import subprocess
import sys
import time
import traceback
from pathlib import Path

from host_copy_up_ab import Worker, competing_work, file_digest, identity
from immutable_lower_cache import paired_ci, run, sha, source_files, write_json
from publication import distribution

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
ARMS = ("baseline", "candidate")
CONDITIONS = ("immutable-nojournal", "mutable-compact")
CHANGED = (
    "crates/pvisor-overlay-core/src/core.rs",
    "crates/pvisor-overlay-core/src/service.rs",
    "crates/pvisor-overlayfs/src/fs.rs",
)
HARNESS = (
    "host_hotpath_ab.py",
    "host_hotpath_driver.rs",
    "host_copy_up_ab.py",
    "immutable_lower_cache.py",
    "publication.py",
)
PAYLOAD = bytes(range(256)) * 16


def inventory(root):
    return {
        str(p.relative_to(root)): dict(identity(p), sha256=file_digest(p))
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def prepare(args):
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    harness = batch / "harness"
    harness.mkdir()
    for name in HARNESS:
        shutil.copy2(HERE / name, harness / name)
    candidate = batch / "candidate/source"
    for name in source_files():
        dst = candidate / name
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, dst)
    baseline = batch / "baseline/source"
    shutil.copytree(candidate, baseline)
    patches = []
    for name in CHANGED:
        before = args.before.resolve() / name
        (baseline / name).write_bytes(before.read_bytes())
        patches.extend(
            difflib.unified_diff(
                before.read_text().splitlines(True),
                (candidate / name).read_text().splitlines(True),
                fromfile="baseline/" + name,
                tofile="candidate/" + name,
            )
        )
    (batch / "subject.diff").write_text("".join(patches))
    receipt = dict(
        benchmark="B-FS-ENG",
        role="engineering A/B",
        arms={},
        rustc=subprocess.check_output(["rustc", "-vV"]).decode(),
        head=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip(),
        status=subprocess.check_output(["git", "status", "--short"], cwd=ROOT).decode(),
        harness={name: sha(harness / name) for name in HARNESS},
    )
    inventories = {}
    for arm in ARMS:
        directory = batch / arm
        source = directory / "source"
        inventories[arm] = {
            str(p.relative_to(source)): file_digest(p)
            for p in sorted(source.rglob("*"))
            if p.is_file()
        }
        write_json(directory / "source-manifest.json", inventories[arm])
        driver = directory / "driver"
        driver.mkdir()
        shutil.copy2(harness / "host_hotpath_driver.rs", driver / "main.rs")
        (driver / "Cargo.toml").write_text(f"""[package]
name="host-hotpath-ab-driver"
version="0.1.0"
edition="2021"
[workspace]
[[bin]]
name="host-hotpath-ab-driver"
path="main.rs"
[dependencies]
anyhow="1"
pvisor-overlayfs={{path={json.dumps(str(source / "crates/pvisor-overlayfs"))}}}
pvisor-overlay-core={{path={json.dumps(str(source / "crates/pvisor-overlay-core"))}}}
[patch.crates-io]
fuser={{path={json.dumps(str(source / "vendor/fuser"))}}}
""")
        shutil.copy2(source / "Cargo.lock", driver / "Cargo.lock")
        command = [
            "cargo",
            "build",
            "--offline",
            "--release",
            "--manifest-path",
            str(driver / "Cargo.toml"),
            "--target-dir",
            str(directory / "target"),
            "-j",
            "4",
        ]
        run(command, ROOT, directory / "build.log", timeout=600)
        shutil.copy2(directory / "target/release/host-hotpath-ab-driver", directory / "driver-bin")
        receipt["arms"][arm] = dict(
            binary_sha256=sha(directory / "driver-bin"),
            command=command,
            source_manifest_sha256=sha(directory / "source-manifest.json"),
            driver_sha256=sha(driver / "main.rs"),
            lock_sha256=sha(driver / "Cargo.lock"),
        )
    changed = [
        name
        for name in inventories["candidate"]
        if inventories["candidate"][name] != inventories["baseline"][name]
    ]
    assert set(changed) == set(CHANGED), changed
    receipt["changed_sources"] = changed
    write_json(batch / "build-receipt.json", receipt)
    print(batch, flush=True)


def verify(batch):
    receipt = json.loads((batch / "build-receipt.json").read_text())
    for arm, item in receipt["arms"].items():
        base = batch / arm
        assert sha(base / "driver-bin") == item["binary_sha256"]
        assert sha(base / "source-manifest.json") == item["source_manifest_sha256"]
        for name, digest in json.loads((base / "source-manifest.json").read_text()).items():
            assert file_digest(base / "source" / name) == digest, name
        assert sha(base / "driver/main.rs") == item["driver_sha256"]
        assert sha(base / "driver/Cargo.lock") == item["lock_sha256"]
    for name, digest in receipt["harness"].items():
        assert sha(batch / "harness" / name) == digest, name
    return receipt


def fixture(lower):
    paths = []
    for i in range(512):
        path = Path("stat") / ("a/b/c/d/e/f" if i % 2 else "shallow") / f"f{i:04}"
        paths.append(path)
    for kind in ("append", "truncate"):
        paths.extend(Path(kind) / f"f{i:04}" for i in range(128))
    for path in paths:
        target = lower / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(PAYLOAD)
    return paths[:512]


def timed(operation, paths):
    start = time.perf_counter_ns()
    for path in paths:
        operation(path)
    return (time.perf_counter_ns() - start) / 1e6


def trial(binary, lower, stage, condition, stat_paths):
    worker = Worker(binary, lower, stage, condition)
    result = {}
    try:
        mount = worker.mount
        paths = [mount / path for path in stat_paths]
        result["first_stat"] = timed(os.stat, paths)
        result["repeat_stat"] = timed(os.stat, paths)
        for kind in ("append", "truncate"):
            paths = [mount / kind / f"f{i:04}" for i in range(128)]
            for path in paths:
                assert path.stat().st_size == len(PAYLOAD)
            flags = os.O_WRONLY | (os.O_APPEND if kind == "append" else os.O_TRUNC)

            def write(path):
                fd = os.open(path, flags)
                try:
                    assert os.write(fd, b"CHANGED!") == 8
                finally:
                    os.close(fd)

            result[kind + "_open_write_close"] = timed(write, paths)
        for kind, operation in (
            ("mkdir", os.mkdir),
            ("fifo", os.mkfifo),
            ("symlink", lambda p: os.symlink("unchanged-target", p)),
        ):
            parent = mount / ("created-" + kind)
            parent.mkdir()
            result[kind] = timed(operation, [parent / f"f{i:04}" for i in range(128)])
        worker.close()
        upper = stage / "upper"
        for i in range(128):
            assert (upper / "append" / f"f{i:04}").read_bytes() == PAYLOAD + b"CHANGED!"
            assert (upper / "truncate" / f"f{i:04}").read_bytes() == b"CHANGED!"
            assert (upper / "created-mkdir" / f"f{i:04}").is_dir()
            assert stat.S_ISFIFO((upper / "created-fifo" / f"f{i:04}").lstat().st_mode)
            assert os.readlink(upper / "created-symlink" / f"f{i:04}") == "unchanged-target"
        assert not list((stage / "work").iterdir()), "unused copy-up work"
        return result
    finally:
        worker.cleanup()


def summary(rows):
    result = []
    for condition in CONDITIONS:
        selected = [r for r in rows if r["condition"] == condition and not r["warmup"]]
        for metric in selected[0]["milliseconds"]:
            values = {
                arm: [r["milliseconds"][metric] for r in selected if r["arm"] == arm]
                for arm in ARMS
            }
            item = dict(
                condition=condition,
                metric=metric,
                baseline=distribution(values["baseline"]),
                candidate=distribution(values["candidate"]),
            )
            item.update(paired_ci(values["baseline"], values["candidate"]))
            result.append(item)
    return result


def measure(args):
    batch = args.build.resolve()
    out = args.measure.resolve()
    out.mkdir(parents=True, exist_ok=False)
    report = dict(
        benchmark="B-FS-DIAG" if args.profiles else "B-FS-ENG",
        rows=[],
        failures=[],
        host=platform.uname()._asdict(),
        build=verify(batch),
        samples=args.samples,
        warmups=args.warmups,
        seed=20261020,
        profiling=args.profiles,
        cpu_affinity="not enforced on macOS",
        memory_budget="not enforced",
        exclusion_rule="none; any correctness/source/build-test interference fails cohort",
    )
    lower = out / "lower"
    rng = random.Random(20261020)
    try:
        stat_paths = fixture(lower)
        before = inventory(lower)
        write_json(out / "input-before.json", before)
        plan = []
        for i in range(args.warmups + args.samples):
            conditions = list(CONDITIONS)
            rng.shuffle(conditions)
            cells = []
            for condition in conditions:
                arms = list(ARMS)
                rng.shuffle(arms)
                cells.extend((condition, arm) for arm in arms)
            plan.append(cells)
        write_json(out / "order.json", plan)
        for i, cells in enumerate(plan):
            for condition, arm in cells:
                assert not competing_work(), "concurrent build/test before trial"
                stage = out / f"round-{i:02}-{condition}-{arm}"
                values = trial(
                    batch / arm / "driver-bin",
                    lower,
                    stage,
                    condition + ("-profile" if args.profiles else ""),
                    stat_paths,
                )
                assert inventory(lower) == before, "lower contents/identity changed"
                assert not competing_work(), "concurrent build/test after trial"
                report["rows"].append(
                    dict(
                        round=i,
                        warmup=i < args.warmups,
                        condition=condition,
                        arm=arm,
                        milliseconds=values,
                        correctness="passed",
                        stage=str(stage),
                    )
                )
                write_json(out / "report.json", report)
                print(
                    f"pair {i + 1}/{len(plan)} {condition} {arm} passed",
                    file=sys.stderr,
                    flush=True,
                )
        verify(batch)
        if not args.profiles:
            report["summary"] = summary(report["rows"])
        report["status"] = "passed"
    except BaseException:
        report["status"] = "failed"
        report["failures"].append(traceback.format_exc())
        raise
    finally:
        write_json(out / "report.json", report)
    print(json.dumps({"status": report["status"], "trials": len(report["rows"])}))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--prepare", type=Path)
    p.add_argument("--before", type=Path)
    p.add_argument("--build", type=Path)
    p.add_argument("--measure", type=Path)
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--profiles", action="store_true")
    args = p.parse_args()
    if args.prepare:
        prepare(args)
    else:
        measure(args)


if __name__ == "__main__":
    main()
