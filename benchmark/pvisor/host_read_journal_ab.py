#!/usr/bin/env python3
"""Read dispatch and journal fingerprint-lock engineering comparison.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: quantify read responsiveness during copy-up and unrelated journal
observations during native full-file fingerprinting, including idle regressions.
Conclusion sought: same-cohort median differences with paired bootstrap 95% CI.
Design: two newly frozen release binaries differing only in read dispatch and
journal fingerprint locks; real FSKit copy windows and separate shared-Core
native hash windows, 1 GiB allocated warm source, 3 warmups/30 shuffled pairs.
Full bytes, journal fingerprints, lower identity and detach must pass; any missed
window/error/build-test interference fails the batch. No profiles, slow-sample
exclusions, P99, cold-disk, Linux/VM or end-to-end workload claims.
"""

import argparse
import json
import os
import platform
import random
import shutil
import subprocess
import threading
import time
import traceback
from pathlib import Path

import host_copy_up_ab as common
from immutable_lower_cache import paired_ci, sha, write_json
from publication import distribution

FILES = [
    "crates/pvisor-overlay-core/src/core.rs",
    "crates/pvisor-overlay-core/src/preimage_log.rs",
    "crates/pvisor-overlayfs/src/api.rs",
    "crates/pvisor-overlayfs/src/dispatch.rs",
    "crates/pvisor-overlayfs/src/fs.rs",
]
HARNESS = (
    "host_read_journal_ab.py",
    "host_read_journal_driver.rs",
    "test_host_read_journal_ab.py",
    "host_copy_up_ab.py",
    "immutable_lower_cache.py",
    "publication.py",
)
READ_BYTES = b"read" * 16384


def conditions(mode):
    return ("nojournal", "legacy", "compact") if mode == "reads" else ("legacy", "compact")


def order(mode, seed, rounds):
    rng = random.Random(seed)
    result = []
    for _ in range(rounds):
        selected = list(conditions(mode))
        rng.shuffle(selected)
        cells = []
        for condition in selected:
            arms = list(common.ARMS)
            rng.shuffle(arms)
            cells.extend((condition, arm) for arm in arms)
        result.append(cells)
    return result


def read_probes(mount, group, held, work=None, size=None):
    barrier = threading.Barrier(4, timeout=60)
    rows, failures = {}, []

    def probe(kind):
        try:
            barrier.wait()
            if work is not None:
                assert common.copying_paths(work, size), "missed read copy window"
            start = time.perf_counter()
            if kind == "open":
                fd = os.open(mount / group / "f00", os.O_RDONLY)
                rows["open_ms"] = (time.perf_counter() - start) * 1000
                try:
                    assert os.read(fd, 8) == b"metadata"
                finally:
                    os.close(fd)
                rows["open_complete_ms"] = (time.perf_counter() - start) * 1000
            elif kind == "read":
                data = os.pread(held, len(READ_BYTES), 0)
                rows["read_ms"] = (time.perf_counter() - start) * 1000
                assert data == READ_BYTES
            else:
                names = list((mount / group).iterdir())
                rows["opendir_ms"] = (time.perf_counter() - start) * 1000
                assert sorted(p.name for p in names) == [f"f{i:02}" for i in range(64)]
            if work is not None:
                rows[kind + "_during_copy"] = bool(common.copying_paths(work, size))
        except BaseException:
            failures.append(traceback.format_exc())

    threads = [
        threading.Thread(target=probe, args=(kind,), daemon=True)
        for kind in ("open", "read", "opendir")
    ]
    for thread in threads:
        thread.start()
    barrier.wait()
    for thread in threads:
        thread.join(60)
        assert not thread.is_alive(), "read probe timeout"
    assert not failures, failures
    return rows


def read_trial(binary, lower, stage, condition, expected):
    worker, held_idle, held_probe = None, None, None
    try:
        worker = common.Worker(binary, lower, stage, condition)
        mount = worker.mount
        held_idle = os.open(mount / "held-idle", os.O_RDONLY)
        held_probe = os.open(mount / "held-probe", os.O_RDONLY)
        row = {"idle_" + key: value for key, value in read_probes(mount, "idle", held_idle).items()}
        assert os.stat(mount / "slow").st_size == expected["size"]
        done, written = threading.Event(), {}

        def writer():
            start = time.perf_counter()
            try:
                fd = os.open(mount / "slow", os.O_RDWR)
                written["writable_open_ms"] = (time.perf_counter() - start) * 1000
                try:
                    assert os.pwrite(fd, b"changed", 0) == 7
                finally:
                    os.close(fd)
            except BaseException:
                written["error"] = traceback.format_exc()
            finally:
                done.set()

        thread = threading.Thread(target=writer, daemon=True)
        thread.start()
        deadline = time.monotonic() + 60
        while not done.is_set() and time.monotonic() < deadline:
            progress = common.copying_paths(stage / "work", expected["size"])
            if progress:
                break
            time.sleep(0.001)
        else:
            raise RuntimeError("no partial copy observed")
        row["copy_at_probe_start"] = progress
        row.update(read_probes(mount, "probe", held_probe, stage / "work", expected["size"]))
        thread.join(60)
        assert not thread.is_alive() and "error" not in written, written
        row.update(written)
        os.close(held_idle)
        held_idle = None
        os.close(held_probe)
        held_probe = None
        assert common.file_digest(stage / "upper/slow") == expected["upper_sha256"]
        assert common.file_digest(lower / "slow") == expected["source_sha256"]
        assert common.identity(lower / "slow") == expected["source_identity"]
        assert not list((stage / "work").iterdir())
        held = os.open(mount / "one", os.O_RDONLY)
        try:
            with (mount / "one").open("r+b") as output:
                output.write(b"updated!")
            assert os.pread(held, 8, 0) == b"updated!"
        finally:
            os.close(held)
        assert (mount / "two").read_bytes() == b"updated!"
        os.rename(mount / "one", mount / "renamed")
        assert (mount / "renamed").read_bytes() == b"updated!"
        assert (lower / "one").read_bytes() == b"original"
        worker.close()
        row.update(correctness="passed", detached=True)
        write_json(stage / "result.json", row)
        shutil.rmtree(stage / "upper")
        shutil.rmtree(stage / "work")
        return row
    finally:
        for fd in (held_idle, held_probe):
            if fd is not None:
                os.close(fd)
        if worker is not None:
            worker.cleanup()


def journal_trial(binary, lower, stage, condition, expected):
    stage.mkdir()
    # Warm actual allocated bytes, then reset only this owned file's atime.
    assert common.file_digest(lower / "slow") == expected["source_sha256"]
    metadata = (lower / "slow").stat()
    os.utime(lower / "slow", ns=(time.time_ns() - 3600_000_000_000, metadata.st_mtime_ns))
    identity = common.identity(lower / "slow")
    command = [str(binary), str(lower), str(stage), "unused", "journal-" + condition]
    env = {key: value for key, value in os.environ.items() if not key.startswith("PVISOR_")}
    env["PVISOR_FS_PROFILE"] = "0"
    write_json(stage / "launch.json", dict(command=command, profile=False))
    common.run(command, common.ROOT, stage / "stdout.log", timeout=120, env=env)
    lines = (stage / "stdout.log").read_text().splitlines()
    assert len(lines) == 2 and json.loads(lines[0]) == command, "unexpected driver output"
    row = json.loads(lines[1])
    assert row["observations"] == 129
    assert common.identity(lower / "slow") == identity
    assert common.file_digest(lower / "slow") == expected["source_sha256"]
    assert not any((stage / "upper").iterdir())
    row.update(correctness="passed", detached=True)
    write_json(stage / "result.json", row)
    return row


def summarize(rows, mode, samples, warmups):
    wanted = {
        (c, a, i) for c in conditions(mode) for a in common.ARMS for i in range(-warmups, samples)
    }
    assert len(rows) == len(wanted)
    assert {(r["condition"], r["arm"], r["round"]) for r in rows} == wanted
    assert all(r["correctness"] == "passed" and r["detached"] for r in rows)
    metrics = (
        (
            "open_ms",
            "open_complete_ms",
            "read_ms",
            "opendir_ms",
            "idle_open_ms",
            "idle_open_complete_ms",
            "idle_read_ms",
            "idle_opendir_ms",
            "writable_open_ms",
        )
        if mode == "reads"
        else ("first_journal_ms", "journal_batch_ms", "idle_journal_ms", "hash_ms")
    )
    result = []
    for condition in conditions(mode):
        arms = {
            a: sorted(
                [
                    r
                    for r in rows
                    if r["condition"] == condition and r["arm"] == a and r["round"] >= 0
                ],
                key=lambda r: r["round"],
            )
            for a in common.ARMS
        }
        for metric in metrics:
            values = {a: [r[metric] for r in arms[a]] for a in common.ARMS}
            result.append(
                dict(
                    condition=condition,
                    metric=metric,
                    unit="ms",
                    samples=samples,
                    baseline=distribution(values["baseline"]),
                    candidate=distribution(values["candidate"]),
                    **paired_ci(values["baseline"], values["candidate"]),
                )
            )
    return result


def experiment(args):
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    receipt = args.build_receipt.resolve()
    common.verify_build(receipt)
    assert platform.system() == "Darwin", "macOS cohort only"
    report = dict(
        benchmark="B-FS-ENG",
        role="engineering A/B",
        mode=args.mode,
        arguments={k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
        build_receipt=str(receipt),
        build_receipt_sha256=sha(receipt),
        host=dict(
            uname=platform.uname()._asdict(),
            hardware=subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"])
            .decode()
            .strip(),
            memory_bytes=int(subprocess.check_output(["sysctl", "-n", "hw.memsize"])),
            mount_table=subprocess.check_output(["mount"]).decode(),
            affinity="not enforced; same macOS scheduler, no whole-host budget",
        ),
        rows=[],
        failures=[],
        guards=[],
        exclusion="no slow-sample exclusions; any failed window/error/build-test invalidates batch",
    )
    try:
        lower = out / "lower"
        expected = common.fixture(lower, args.file_mib * 1024 * 1024)
        for name in ("held-idle", "held-probe"):
            (lower / name).write_bytes(READ_BYTES)
        report["fixture"] = expected
        plan = order(args.mode, args.seed, args.samples + args.warmups)
        report["order"] = plan
        write_json(out / "report.json", report)
        for index, cells in enumerate(plan):
            for condition, arm in cells:
                guard = dict(
                    round=index - args.warmups,
                    condition=condition,
                    arm=arm,
                    before=common.competing_work(),
                    during=[],
                )
                report["guards"].append(guard)
                assert not guard["before"], "competing build/test before trial"
                stop = threading.Event()

                def observe():
                    try:
                        while not stop.wait(0.25):
                            guard["during"].append(
                                dict(at=time.monotonic(), detected=common.competing_work())
                            )
                    except BaseException:
                        guard["error"] = traceback.format_exc()

                observer = threading.Thread(target=observe, daemon=True)
                observer.start()
                stage = out / "trials" / f"{index:02}-{condition}-{arm}"
                stage.parent.mkdir(exist_ok=True)
                try:
                    function = read_trial if args.mode == "reads" else journal_trial
                    row = function(
                        receipt.parent / arm / "driver-bin", lower, stage, condition, expected
                    )
                finally:
                    stop.set()
                    observer.join()
                    guard["after"] = common.competing_work()
                assert "error" not in guard and not guard["after"]
                assert not any(item["detected"] for item in guard["during"]), (
                    "competing build/test during trial"
                )
                row.update(condition=condition, arm=arm, round=index - args.warmups)
                report["rows"].append(row)
                write_json(out / "report.json", report)
            print(
                json.dumps(dict(round=index - args.warmups, completed=len(report["rows"]))),
                flush=True,
            )
        common.verify_build(receipt)
        report["summary"] = summarize(report["rows"], args.mode, args.samples, args.warmups)
        report["status"] = "passed"
    except BaseException:
        report["status"] = "failed"
        report["failures"].append(traceback.format_exc())
        raise
    finally:
        write_json(out / "report.json", report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--baseline-ref", default="HEAD^")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target-dir", type=Path, default=common.ROOT / "target/host-copy-up-ab")
    parser.add_argument("--build-receipt", type=Path)
    parser.add_argument("--mode", choices=("reads", "journal"), default="reads")
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--file-mib", type=int, default=1024)
    parser.add_argument("--seed", type=int, default=4207)
    args = parser.parse_args()
    if args.build:
        common.build(
            args.output.resolve(),
            args.target_dir.resolve(),
            args.baseline_ref,
            baseline_files=FILES,
            driver_name="host_read_journal_driver.rs",
            harness=HARNESS,
        )
    else:
        assert args.build_receipt and args.samples > 0 and args.warmups >= 0 and args.file_mib > 0
        experiment(args)


if __name__ == "__main__":
    main()
