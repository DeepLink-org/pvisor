#!/usr/bin/env python3
"""B-FS-ENG (benchmark/README.md#b-fs-eng), engineering A/B.

Measure the real private host adapter without FSKit transport. Both frozen
subjects receive an identical test-only probe and ReplySender export; product
sources stay untouched. Same input/correctness/order/bootstrap rules as the mount
cohort; phase timers exclude setup, process startup and final verification.
With --before, restore the four pre-cleanup host/Core sources and add the same
directory/reclaim probe. Three warmups and thirty randomized pairs per condition;
no slow-value exclusion, paired bootstrap CIs, profiles in a separate cohort.
"""

import argparse
import json
import os
import platform
import random
import shutil
import stat
import subprocess
import sys
import traceback
from pathlib import Path

from host_copy_up_ab import competing_work, file_digest
from host_hotpath_ab import ARMS, CHANGED, CONDITIONS, PAYLOAD, fixture, inventory, summary, verify
from immutable_lower_cache import ROOT, sha, source_files, write_json

HERE = Path(__file__).resolve().parent
HARNESS = (
    "host_hotpath_phase_ab.py",
    "host_hotpath_probe.rs",
    "host_cleanup_probe.rs",
    "host_hotpath_ab.py",
    "host_copy_up_ab.py",
    "immutable_lower_cache.py",
    "publication.py",
)


def prepare(args):
    if args.before:
        assert not args.build, "choose --before or --build"
    else:
        verify(args.build.resolve())
    changed_sources = CHANGED + (("crates/pvisor-overlay-core/src/preimage_log.rs",) if args.before else ())
    probe = (HERE / "host_hotpath_probe.rs").read_bytes()
    if args.before:
        probe = probe.replace(b"drop(overlay);", b"super::cleanup_benchmark::extra(&mut overlay);\n        drop(overlay);")
        probe += b"\n" + (HERE / "host_cleanup_probe.rs").read_bytes()
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    for name in HARNESS:
        shutil.copy2(HERE / name, batch / "harness" / name)
    receipt = dict(
        benchmark="B-FS-ENG",
        role="engineering A/B",
        arms={},
        parent_build=str(args.build.resolve()) if args.build else None,
        before=str(args.before.resolve()) if args.before else None,
        scenario="cleanup" if args.before else "request-reuse",
        common_instrumentation="test-only fs.rs probe; frozen fuser ReplySender re-export",
        rustc=subprocess.check_output(["rustc", "-vV"]).decode(),
        harness={name: sha(batch / "harness" / name) for name in HARNESS},
    )
    inventories = {}
    for arm in ARMS:
        base = batch / arm
        source = base / "source"
        if args.before:
            for name in source_files():
                destination = source / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                origin = args.before / name if arm == "baseline" and name in changed_sources else ROOT / name
                shutil.copy2(origin, destination)
        else:
            shutil.copytree(args.build.resolve() / arm / "source", source)
        fs = source / "crates/pvisor-overlayfs/src/fs.rs"
        fs.write_bytes(fs.read_bytes() + b"\n" + probe)
        lib = source / "vendor/fuser/src/lib.rs"
        lib.write_text(lib.read_text() + "\npub use reply::ReplySender;\n")
        command = [
            "cargo",
            "nextest",
            "list",
            "--offline",
            "--release",
            "--lib",
            "-p",
            "pvisor-overlayfs",
            "--manifest-path",
            str(source / "Cargo.toml"),
            "--target-dir",
            str(base / "target"),
            "--message-format",
            "json",
        ]
        with (base / "build.log").open("w") as log, (base / "nextest.json").open("w") as result:
            subprocess.run(command, stdout=result, stderr=log, check=True, timeout=600)
        suites = json.loads((base / "nextest.json").read_text())["rust-suites"]
        assert len(suites) == 1
        binary = Path(next(iter(suites.values()))["binary-path"])
        shutil.copy2(binary, base / "driver-bin")
        driver = base / "driver"
        driver.mkdir()
        (driver / "main.rs").write_bytes(probe)
        shutil.copy2(source / "Cargo.lock", driver / "Cargo.lock")
        inventories[arm] = {
            str(p.relative_to(source)): file_digest(p)
            for p in sorted(source.rglob("*"))
            if p.is_file()
        }
        write_json(base / "source-manifest.json", inventories[arm])
        receipt["arms"][arm] = dict(
            binary_sha256=sha(base / "driver-bin"),
            command=command,
            source_manifest_sha256=sha(base / "source-manifest.json"),
            driver_sha256=sha(driver / "main.rs"),
            lock_sha256=sha(driver / "Cargo.lock"),
        )
        print("built", arm, file=sys.stderr, flush=True)
    changed = [
        name
        for name in inventories["candidate"]
        if inventories["candidate"][name] != inventories["baseline"][name]
    ]
    assert set(changed) == set(changed_sources), changed
    assert receipt["arms"]["baseline"]["driver_sha256"] == receipt["arms"]["candidate"]["driver_sha256"]
    receipt["common_probe_sha256"] = receipt["arms"]["baseline"]["driver_sha256"]
    receipt["changed_sources"] = changed
    write_json(batch / "build-receipt.json", receipt)


def trial(binary, lower, stage, condition, profiles, cleanup=False):
    stage.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("PVISOR_")}
    env.update(
        HOTPATH_LOWER=str(lower),
        HOTPATH_STAGE=str(stage),
        HOTPATH_CONDITION=condition,
        PVISOR_FS_PROFILE="1" if profiles else "0",
    )
    command = [str(binary), "--ignored", "--exact", "fs::hotpath_benchmark::probe", "--nocapture"]
    write_json(stage / "launch.json", dict(command=command, condition=condition, profile=profiles))
    with (stage / "stdout.log").open("w") as out, (stage / "stderr.log").open("w") as err:
        subprocess.run(command, env=env, stdout=out, stderr=err, check=True, timeout=120)
    values = {}
    for line in (stage / "stdout.log").read_text().splitlines():
        if line.startswith("HOTPATH "):
            _, metric, ns = line.split()
            assert metric not in values
            values[metric] = int(ns) / 1e6
    assert len(values) == (14 if cleanup else 7), values
    upper = stage / "upper"
    for i in range(128):
        assert (upper / "append" / f"f{i:04}").read_bytes() == PAYLOAD + b"CHANGED!"
        assert (upper / "truncate" / f"f{i:04}").read_bytes() == b"CHANGED!"
        assert (upper / "created-mkdir" / f"f{i:04}").is_dir()
        assert stat.S_ISFIFO((upper / "created-fifo" / f"f{i:04}").lstat().st_mode)
        assert os.readlink(upper / "created-symlink" / f"f{i:04}") == "unchanged-target"
    assert not list((stage / "work").iterdir())
    return values


def measure(args):
    batch, out = args.build.resolve(), args.measure.resolve()
    out.mkdir(parents=True, exist_ok=False)
    report = dict(
        benchmark="B-FS-DIAG" if args.profiles else "B-FS-ENG",
        rows=[],
        failures=[],
        scope="private host adapter phases; no kernel/FSKit transport or scheduling",
        host=platform.uname()._asdict(),
        build=verify(batch),
        samples=args.samples,
        warmups=args.warmups,
        seed=20261020,
        profiling=args.profiles,
        affinity="macOS scheduler, unpinned",
        memory_budget="not enforced",
        exclusion_rule="no slow-value exclusion; correctness/source/build-test failures invalidate cohort",
    )
    try:
        lower = out / "lower"
        fixture(lower)
        if report["build"].get("scenario") == "cleanup":
            for count in (128, 2048):
                directory = lower / f"directory-{count}"
                directory.mkdir()
                for index in range(count):
                    (directory / f"f{index:04}").write_bytes(PAYLOAD)
        before = inventory(lower)
        write_json(out / "input-before.json", before)
        rng = random.Random(20261020)
        plan = []
        for _ in range(args.samples + args.warmups):
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
                values = trial(batch / arm / "driver-bin", lower, stage, condition, args.profiles,
                               cleanup=report["build"].get("scenario") == "cleanup")
                assert inventory(lower) == before, "lower mutation"
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
    p.add_argument("--build", type=Path)
    p.add_argument("--before", type=Path, help="saved pre-cleanup sources; enables directory/reclaim probe")
    p.add_argument("--measure", type=Path)
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--profiles", action="store_true")
    args = p.parse_args()
    if not args.build and not (args.prepare and args.before):
        p.error("--build is required unless preparing with --before")
    prepare(args) if args.prepare else measure(args)


if __name__ == "__main__":
    main()
