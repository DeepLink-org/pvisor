#!/usr/bin/env python3
"""B-FS-ENG: host-owned write-cache curves, with separate B-FS-DIAG counters.

Question: at equal explicit file+directory fsync boundaries, where do owned
asynchronous writes and merging improve completed throughput, and at what cost?
Design: frozen public-API VM mechanism subject, identical static native/guest
payload, randomized paired fresh environments with three dependent rounds.
No interference/quiet-period gates; preserve failures and every valid slow row.
This runner is deliberately separate from ordinary CLI seven-tool measurements.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
MIB = 1 << 20


def save(path, value):
    path = Path(path)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(value, indent=2) + "\n")
    temp.replace(path)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def cells(suite):
    result = []

    def add(id_, family, x, block=4096, workers=1, size=4 * MIB, sync=0, workload="stream"):
        result.append(
            dict(
                id=id_,
                family=family,
                x=x,
                workload=workload,
                block_bytes=block,
                workers=workers,
                bytes_per_worker=size // workers,
                sync_every_ops=sync,
                x_label={
                    "block": "Write block (KiB)",
                    "workers": "Concurrent files / workers",
                    "barrier": "Writes per fsync (0 = final only)",
                    "readset": "Warm upper read set (MiB)",
                    "pressure": "Bytes per round (MiB)",
                    "smallfiles": "File size (KiB)",
                    "sync": "Writes per fsync",
                }[family],
                x_scale="log2" if family in ("block", "readset", "pressure") else "linear",
            )
        )

    if suite in ("shape", "confirm"):
        for kib in [1, 64] if suite == "confirm" else [1, 4, 16, 64, 256, 1024]:
            add(f"block-{kib}k", "block", kib, block=kib << 10)
        if suite == "shape":
            for workers in (1, 2, 4):
                add(f"workers-{workers}", "workers", workers, workers=workers)
            for interval in (1, 16, 256, 0):
                add(f"barrier-{interval}", "barrier", interval, size=MIB, sync=interval)
            for mib in (1, 8, 32):
                add(f"read-{mib}m", "readset", mib, block=65536, size=mib * MIB, workload="read")
        add("tiny-1k", "smallfiles", 1, block=1024, size=128 << 10, workload="smallfiles")
        add("sync-each", "sync", 1, block=4096, size=256 << 10, sync=1)
    elif suite == "parameters":
        for mib in (8, 64, 128):
            add(f"pressure-{mib}m", "pressure", mib, block=65536, size=mib * MIB)
        add("stream-4k", "block", 4, size=8 * MIB)
    else:
        raise ValueError("unknown suite")
    return result


def conditions(suite):
    def arm(config=None, *, enabled=True, description=""):
        env = {"PVISOR_VM_FS_HOST_WRITEBACK": "1" if enabled else "0"}
        if config is not None:
            env["PVISOR_VM_FS_HOST_WRITEBACK_CONFIG"] = json.dumps(config, separators=(",", ":"))
        return dict(env=env, description=description)

    if suite == "parameters":
        result = {"async": arm(description="default asynchronous merging")}
        for mib in (4, 16, 256):
            result[f"capacity-{mib}m"] = arm(
                {"capacity_mib": mib}, description=f"{mib} MiB payload capacity"
            )
        for kib in (64, 256):
            result[f"batch-{kib}k"] = arm({"batch_kib": kib}, description=f"{kib} KiB batch limit")
        for us in (0, 100, 1000, 50000):
            result[f"delay-{us}us"] = arm(
                {"delay_us": us}, description=f"{us} us oldest-job deadline"
            )
        return result
    result = {
        "sync": arm(enabled=False, description="original direct synchronous host path"),
        "async": arm(
            description="owned asynchronous merging, qualification independent of backend lock"
        ),
    }
    if suite == "shape":
        result.update(
            {
                "native": dict(
                    env={}, description="same payload on native filesystem; reference only"
                ),
                "async-old": arm(
                    {"qualification_lock": True},
                    description="original foreground/background qualification lock coupling",
                ),
                "unmerged": arm(
                    {"chunks": 1, "delay_us": 0},
                    description="owned asynchronous writes without merging",
                ),
                "owned-sync": arm(
                    {"synchronous": True},
                    description="owned copy plus worker dispatch, drain before WRITE reply",
                ),
            }
        )
    return result


def plan_text(plan):
    return "".join(
        "\t".join(
            str(c[k])
            for k in (
                "id",
                "workload",
                "block_bytes",
                "workers",
                "bytes_per_worker",
                "sync_every_ops",
            )
        )
        + "\n"
        for c in plan
    )


def prepare(args):
    batch = args.prepare.resolve()
    if ".data" not in batch.parts:
        raise ValueError("raw evidence must be under a .data directory")
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    for name in (
        "curves.py",
        "curves_guest.rs",
        "curves_driver.rs",
        "curves_analysis.py",
        "curves_diagnostics.py",
    ):
        shutil.copy2(HERE / name, batch / "harness" / name)
    (batch / "bin").mkdir()
    shutil.copy2(args.firmware.resolve(), batch / "bin/libkrunfw.so")
    source = batch / "source"
    source.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(REPO / name, source / name)
    for name in ("crates", "vendor/fuser", ".cargo"):
        if (REPO / name).exists():
            shutil.copytree(
                REPO / name,
                source / name,
                ignore=shutil.ignore_patterns("target", ".data", "__pycache__"),
            )
    source_hashes = {str(p.relative_to(source)): sha(p) for p in source.rglob("*") if p.is_file()}
    driver = batch / "driver"
    (driver / "src").mkdir(parents=True)
    shutil.copy2(batch / "harness/curves_driver.rs", driver / "src/main.rs")
    (driver / "Cargo.toml").write_text(
        '[workspace]\n[package]\nname="pvisor-fs-curves-subject"\nversion="0.0.0"\nedition="2024"\n'
        "[dependencies]\npvisor-vm={path="
        + json.dumps(str(REPO / "crates/pvisor-vm"))
        + '}\nserde_json="1"\n'
        '[profile.release]\nopt-level=3\nlto="thin"\nstrip="symbols"\ncodegen-units=8\npanic="abort"\n'
        '[profile.release.build-override]\nstrip="none"\n[profile.performance]\ninherits="release"\n'
        '[profile.performance.build-override]\nstrip="none"\n[patch.crates-io]\nfuser={path='
        + json.dumps(str(REPO / "vendor/fuser"))
        + "}\n"
    )
    shutil.copy2(REPO / "Cargo.lock", driver / "Cargo.lock")
    commands = [
        [
            "rustc",
            "--edition=2024",
            "-O",
            "--target",
            "x86_64-unknown-linux-musl",
            str(batch / "harness/curves_guest.rs"),
            "-o",
            str(batch / "bin/payload"),
        ],
        [
            "cargo",
            "build",
            "--offline",
            "--profile",
            "performance",
            "--manifest-path",
            str(driver / "Cargo.toml"),
            "--target-dir",
            str(REPO / "target"),
        ],
    ]
    build_reuse = None
    if args.reuse_build:
        reused = args.reuse_build.resolve()
        prior = json.loads((reused / "protocol.json").read_text())
        verify_artifacts(reused, prior)
        if prior["source_hashes"] != source_hashes:
            raise ValueError("reused subject's product source differs")
        for name in ("curves_guest.rs", "curves_driver.rs"):
            if sha(batch / "harness" / name) != sha(reused / "harness" / name):
                raise ValueError("reused workload or driver source differs")
        if sha(batch / "bin/libkrunfw.so") != sha(reused / "bin/libkrunfw.so"):
            raise ValueError("reused firmware differs")
        for name in ("payload", "subject"):
            shutil.copy2(reused / "bin" / name, batch / "bin" / name)
        build_reuse = dict(
            batch=str(reused),
            protocol_sha256=sha(reused / "protocol.json"),
            scope="exact source/workload/driver/firmware-verified build bytes only; no timing rows reused",
        )
        save(batch / "build-reuse.json", build_reuse)
    else:
        with (batch / "build.log").open("w") as log:
            for command in commands:
                log.write(json.dumps(command) + "\n")
                log.flush()
                subprocess.run(command, cwd=REPO, stdout=log, stderr=subprocess.STDOUT, check=True)
        shutil.copy2(REPO / "target/performance/pvisor-fs-curves-subject", batch / "bin/subject")
    if any(sha(REPO / name) != expected for name, expected in source_hashes.items()):
        raise ValueError("source changed during build; retain attempt, prepare a fresh batch")
    plan, arms = cells(args.suite), conditions(args.suite)
    if args.conditions:
        requested = args.conditions.split(",")
        if not set(requested) <= arms.keys() or len(set(requested)) != len(requested):
            raise ValueError("unknown or repeated condition")
        arms = {name: arms[name] for name in requested}
    rng = random.Random(args.seed)
    orders = []
    for pair in range(args.samples):
        order = list(arms)
        rng.shuffle(order)
        row_plan = plan.copy()
        rng.shuffle(row_plan)
        orders.append(dict(pair=pair, conditions=order, cell_order=[c["id"] for c in row_plan]))
        (batch / f"plan-{pair:03}.tsv").write_text(plan_text(row_plan))
    artifacts = {
        str(p.relative_to(batch)): sha(p)
        for folder in (batch / "bin", batch / "harness")
        for p in folder.iterdir()
        if p.is_file()
    }
    for p in batch.glob("plan-*.tsv"):
        artifacts[p.name] = sha(p)
    protocol = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="engineering A/B",
        suite=args.suite,
        question=__doc__.split("Question: ", 1)[1].split("Design:", 1)[0].strip(),
        rootfs=str(args.rootfs.resolve()),
        source_head=subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
        ).strip(),
        source_hashes=source_hashes,
        artifacts=artifacts,
        cells=plan,
        conditions=arms,
        build_reuse=build_reuse,
        baseline_condition="async" if args.suite == "parameters" else "sync",
        samples=args.samples,
        resident_rounds=3,
        seed=args.seed,
        orders=orders,
        independent_unit="fresh environment; three resident rounds are dependent",
        budget=dict(
            cpus=[0, 1], quota="200000 100000", memory_bytes=2 << 30, swap_bytes=0, guest_mib=512
        ),
        timing="WRITE returns; dup/checked-close applied barrier; file fsync; checked-close; directory fsync; full total excludes preparation, verification and VM exit",
        cache="fresh guest, uncontrolled warm host backing; never cold-disk evidence",
        profiling=args.diagnostic,
        interference_gate=False,
        quiet_gate=False,
        inventory_gate=False,
        failures="retain error/timeout/correctness failures and all valid slow samples; no timing publication on incomplete cells",
        host=dict(
            platform=platform.platform(),
            machine=platform.machine(),
            filesystem=subprocess.check_output(
                ["findmnt", "-T", str(batch), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS", "--json"],
                text=True,
            ),
            cpuinfo=Path("/proc/cpuinfo").read_text(),
            rustc=subprocess.check_output(["rustc", "-Vv"], text=True),
        ),
    )
    if args.diagnostic:
        protocol.update(benchmark_id="B-FS-DIAG", role="diagnostic")
    if args.suite == "parameters":
        defaults = dict(capacity_mib=64, batch_kib=1024, delay_us=10000)
        protocol["condition_factors"] = {
            name: {
                **defaults,
                **json.loads(value["env"].get("PVISOR_VM_FS_HOST_WRITEBACK_CONFIG", "{}")),
            }
            for name, value in arms.items()
        }
        protocol["parameter_curve_cells"] = ["pressure-128m", "stream-4k"]
    save(batch / "protocol.json", protocol)
    # The row runs one complete predeclared paired campaign; its event-clock
    # measurements, not wrapper wall time, form the experimental data.
    save(
        batch / "in.json",
        {
            "host-write-cache-curves": "$ "
            + sys.executable
            + " "
            + str(batch / "harness/curves.py")
            + " --measure "
            + str(batch)
        },
    )
    print(batch, flush=True)


def cgroup_state(root=None):
    if root is None:
        relative = Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1]
        root = Path("/sys/fs/cgroup") / relative.lstrip("/")
    result = {"cgroup": str(root), "affinity": sorted(os.sched_getaffinity(0))}
    for name in (
        "cpu.max",
        "cpu.stat",
        "memory.max",
        "memory.swap.max",
        "memory.current",
        "memory.peak",
        "memory.events",
        "memory.stat",
        "io.stat",
        "cpu.pressure",
        "memory.pressure",
        "io.pressure",
    ):
        if (root / name).exists():
            result[name] = (root / name).read_text().strip()
    return result


def verify_artifacts(batch, protocol):
    for name, expected in protocol["artifacts"].items():
        if sha(batch / name) != expected:
            raise ValueError("frozen artifact changed: " + name)


def one_trial(batch, protocol, pair, condition):
    folder = batch / "trials" / f"{pair:03}-{condition}"
    folder.mkdir(parents=True, exist_ok=False)
    stage = folder / "stage"
    for name in ("upper", "work"):
        (stage / name).mkdir(parents=True)
    plan = batch / f"plan-{pair:03}.tsv"
    native = condition == "native"
    workspace = stage / "native" if native else stage / "upper/curve-work"
    workspace.mkdir(parents=True)
    command = (
        [str(batch / "bin/payload"), str(plan), str(workspace), "3"]
        if native
        else [
            str(batch / "bin/subject"),
            protocol["rootfs"],
            str(batch / "bin/libkrunfw.so"),
            str(batch / "bin/payload"),
            str(plan),
            str(stage),
            "3",
        ]
    )
    env = {k: v for k, v in os.environ.items() if not k.startswith("PVISOR_")}
    env.update(
        PVISOR_VM_FS_WRITEBACK="0",
        PVISOR_VM_GUEST_CACHE="1",
        PVISOR_FS_PROFILE="1" if protocol["profiling"] else "0",
        PVISOR_STARTUP_TIMING="0",
        PVISOR_STARTUP_PROFILE="0",
        RUST_LOG="off",
        CURVES_PROFILE="1" if protocol["profiling"] and not native else "0",
    )
    env.update(protocol["conditions"][condition]["env"])
    before = cgroup_state()
    current = int(before.get("memory.current", 0))
    peak, integral, samples = current, 0, 0
    stops = threading.Event()

    def monitor():
        nonlocal peak, integral, samples
        root = Path(before["cgroup"])
        last = time.monotonic_ns()
        while not stops.wait(0.01):
            now = time.monotonic_ns()
            value = int((root / "memory.current").read_text())
            peak = max(peak, value)
            integral += value * (now - last)
            samples += 1
            last = now

    monitor_thread = threading.Thread(target=monitor, daemon=True)
    started = time.monotonic_ns()
    ready = None
    ready_record = None
    parsed = []
    done = None
    monitor_thread.start()
    result = dict(
        pair=pair,
        condition=condition,
        command=command,
        environment={
            k: v for k, v in env.items() if k.startswith(("PVISOR_", "CURVES_")) or k == "RUST_LOG"
        },
        rows=parsed,
        resource_before=before,
    )
    process = None
    try:
        with (folder / "stdout.log").open("w") as out, (folder / "stderr.log").open("w") as err:
            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                env=env,
                stdin=subprocess.DEVNULL,
            )
            timer = threading.Timer(300, process.kill)
            timer.start()
            try:
                for line in process.stdout:
                    out.write(line)
                    out.flush()
                    if line.startswith("CURVES_READY "):
                        if ready is not None:
                            raise ValueError("duplicate Ready")
                        ready = time.monotonic_ns()
                        ready_record = json.loads(line.split(" ", 1)[1])
                    elif line.startswith("CURVES_RESULT "):
                        parsed.append(json.loads(line.split(" ", 1)[1]))
                    elif line.startswith("CURVES_DONE "):
                        if done is not None:
                            raise ValueError("duplicate DONE")
                        done = json.loads(line.split(" ", 1)[1])
                returncode = process.wait()
            finally:
                timer.cancel()
        ended = time.monotonic_ns()
        result.update(
            returncode=returncode,
            ready_ms=None if ready is None else (ready - started) / 1e6,
            job_ms=(ended - started) / 1e6,
        )
        expected = {(c["id"], r) for c in protocol["cells"] for r in (1, 2, 3)}
        observed = {(r["id"], r["round"]) for r in parsed}
        by_id = {c["id"]: c for c in protocol["cells"]}
        for row in parsed:
            cell = by_id[row["id"]]
            if any(
                row.get(k) != cell[k]
                for k in (
                    "workload",
                    "block_bytes",
                    "workers",
                    "bytes_per_worker",
                    "sync_every_ops",
                )
            ):
                raise ValueError("payload factors differ from frozen plan")
            if row.get("bytes") != cell["bytes_per_worker"] * cell["workers"]:
                raise ValueError("payload byte count differs")
            if row.get("total_ns") != sum(
                row.get(k, -1)
                for k in ("accepted_ns", "applied_ns", "fsync_ns", "close_ns", "dirsync_ns")
            ):
                raise ValueError("phase boundaries do not sum to total")
        if (
            returncode != 0
            or ready is None
            or ready_record != dict(schema=1, cells=len(protocol["cells"]), rounds=3)
            or done is None
            or done.get("schema") != 1
            or done.get("verified") is not True
            or expected != observed
            or len(parsed) != len(expected)
            or done.get("rows") != len(expected)
            or not all(r.get("correctness") is True for r in parsed)
        ):
            raise ValueError("exit/Ready/result inventory/full guest verification failed")
        stderr = (folder / "stderr.log").read_text()
        if not protocol["profiling"] and "pvisor-fs-profile" in stderr:
            raise ValueError("profile output in timing trial")
        verify_started = time.monotonic_ns()
        verify = subprocess.run(
            [str(batch / "bin/payload"), "--verify", str(plan), str(workspace), "3"],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        (folder / "host-verification.log").write_text(verify.stdout + verify.stderr)
        receipts = [
            json.loads(line.split(" ", 1)[1])
            for line in verify.stdout.splitlines()
            if line.startswith("CURVES_VERIFIED ")
        ]
        if (
            verify.returncode != 0
            or len(receipts) != 1
            or receipts[0].get("verified") is not True
            or receipts[0].get("bytes") != sum(r["bytes"] for r in parsed)
        ):
            raise ValueError("independent host descriptor byte verification failed")
        result.update(
            host_verified=True,
            host_verification_ms=(time.monotonic_ns() - verify_started) / 1e6,
            valid=True,
        )
    except BaseException as error:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait()
        result.update(valid=False, error=repr(error))
    finally:
        stops.set()
        monitor_thread.join()
        result.update(
            resource=cgroup_state(),
            sampled_memory_current_peak=peak,
            sampled_memory_byte_seconds=integral / 1e9,
            memory_monitor_samples=samples,
        )
        result["resource_scope"] = (
            "whole trial, all cells, includes observer and independent host verification; kernel memory.peak spans campaign"
        )
        save(folder / "result.json", result)
    if result["valid"]:
        # Verification receipts remain; failed stages are retained for debugging.
        shutil.rmtree(stage)
    return result


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_artifacts(batch, protocol)
    state = cgroup_state()
    if (
        state.get("cpu.max") != "200000 100000"
        or state.get("memory.max") != str(2 << 30)
        or state.get("memory.swap.max") != "0"
        or state["affinity"] != [0, 1]
    ):
        raise ValueError("resource budget was not installed: " + json.dumps(state))
    save(batch / "budget-before.json", state)
    records = []
    for order in protocol["orders"]:
        for condition in order["conditions"]:
            record = one_trial(batch, protocol, order["pair"], condition)
            records.append(record)
            save(batch / "records.partial.json", records)
            print(
                json.dumps(
                    {
                        "pair": order["pair"],
                        "condition": condition,
                        "valid": record["valid"],
                        "job_ms": record.get("job_ms"),
                        "error": record.get("error"),
                    }
                ),
                file=sys.stderr,
                flush=True,
            )
    verify_artifacts(batch, protocol)
    save(batch / "records.json", records)
    save(batch / "budget-after.json", cgroup_state())
    failed = sum(not r["valid"] for r in records)
    save(
        batch / "status.json",
        dict(valid=failed == 0, failed=failed, excluded=0, trials=len(records)),
    )
    print(
        json.dumps(
            dict(
                trials=len(records),
                failed=failed,
                cells=len(protocol["cells"]),
                pairs=protocol["samples"],
            )
        ),
        flush=True,
    )
    if failed:
        raise ValueError(f"{failed} failed trials; evidence retained")


def measure(batch):
    if (batch / "records.json").exists() or (batch / "records.partial.json").exists():
        raise ValueError("refusing to reuse measured batch")
    unit = "pvisor-fs-curves-" + uuid.uuid4().hex[:12]
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit",
        unit,
        "-p",
        "CPUQuota=200%",
        "-p",
        "AllowedCPUs=0,1",
        "-p",
        "MemoryMax=2G",
        "-p",
        "MemorySwapMax=0",
        "-p",
        "LimitNOFILE=65536",
        "taskset",
        "-c",
        "0,1",
        sys.executable,
        str(batch / "harness/curves.py"),
        "--campaign",
        str(batch),
    ]
    save(batch / "campaign-command.json", command)
    with (
        (batch / "campaign.stdout.log").open("w") as out,
        (batch / "campaign.stderr.log").open("w") as err,
    ):
        proc = subprocess.run(command, stdout=out, stderr=err, timeout=7200, check=False)
    if proc.returncode:
        raise ValueError("campaign failed; inspect " + str(batch / "campaign.stderr.log"))
    print((batch / "campaign.stdout.log").read_text().strip())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--prepare", type=Path)
    modes.add_argument("--measure", type=Path)
    modes.add_argument("--campaign", type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--suite", choices=("shape", "parameters", "confirm"), default="shape")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--seed", type=int, default=20261017)
    parser.add_argument("--conditions")
    parser.add_argument("--diagnostic", action="store_true")
    parser.add_argument("--reuse-build", type=Path)
    args = parser.parse_args()
    if args.prepare:
        if not args.rootfs or not args.firmware or args.samples < 1:
            parser.error("prepare needs rootfs, firmware and positive samples")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    else:
        campaign(args.campaign.resolve())


if __name__ == "__main__":
    main()
