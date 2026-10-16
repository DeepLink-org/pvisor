#!/usr/bin/env python3
"""Prepare and run the explicit immutable-share DAX experiment.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: decide whether the new immutable share's DAX path helps readers.
Conclusion sought: paired median differences and 95% CIs by access pattern.
Design: one frozen optimized subject, mount toggle only, host-hot/fresh guests,
2 CPU/2 GiB complete VM group, 3 warmups/30 balanced random pairs, full checks.
This is a phase-clock experiment, not hyperfine or the writable CLI workload.
"""

import argparse
import csv
import hashlib
import json
import os
import platform
import random
import resource
import shutil
import statistics
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
METRICS = [
    f"{kind}_{phase}"
    for kind in ("metadata", "small", "read", "mmap", "parallel")
    for phase in ("first", "repeat")
]
BUILDERS = {"cargo", "rustc", "pytest", "nextest", "hyperfine", "ninja", "make", "cargo-nextest"}


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def tree(root):
    """Content/namespace/permission inventory, excluding read-induced atime."""
    result = {}
    for p in sorted(root.rglob("*")):
        s = p.lstat()
        item = {
            "mode": s.st_mode,
            "size": s.st_size if p.is_file() else None,
            "mtime_ns": s.st_mtime_ns,
            "ctime_ns": s.st_ctime_ns,
        }
        if p.is_symlink():
            item["link"] = os.readlink(p)
        elif p.is_file():
            item["sha256"] = digest(p)
        result[str(p.relative_to(root))] = item
    return result


def pattern(seed, length=1 << 20):
    out = bytearray(length)
    x = seed
    for i in range(0, length, 4):
        x ^= (x << 13) & 0xFFFFFFFF
        x ^= x >> 17
        x ^= (x << 5) & 0xFFFFFFFF
        out[i : i + 4] = x.to_bytes(4, "little")
    return out


def prepare(args):
    batch = args.prepare.resolve()
    if ".data" not in batch.parts:
        raise ValueError("raw batch must be under .data")
    batch.mkdir(parents=True, exist_ok=False)
    harness = batch / "harness"
    harness.mkdir()
    for name in ("dax_ab.py", "dax_driver.rs", "dax_workload.c"):
        shutil.copy2(HERE / name, harness / name)
    (batch / "bin").mkdir()
    data = batch / "immutable"
    data.mkdir()
    for name, seed in (("sequential", 101), ("mapped", 202), ("parallel", 303)):
        block = pattern(seed)
        with (data / name).open("wb") as f:
            for _ in range(64):
                f.write(block)
    (data / "small").mkdir()
    for i in range(128):
        (data / "small" / f"f{i:03}").write_bytes(pattern(1000 + i, 65536))
    for i in range(2048):
        d = data / "meta" / f"d{i // 64:02}"
        d.mkdir(parents=True, exist_ok=True)
        (d / f"f{i:04}").write_bytes(bytes(range(256)))
    with (batch / "guest-build.log").open("w") as log:
        subprocess.run(
            [
                "cc",
                "-O3",
                "-static",
                "-pthread",
                "-Wall",
                "-Wextra",
                "-Werror",
                str(harness / "dax_workload.c"),
                "-o",
                str(data / "workload"),
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
            check=True,
        )
    driver = batch / "driver"
    driver.mkdir()
    (driver / "src").mkdir()
    shutil.copy2(harness / "dax_driver.rs", driver / "src/main.rs")
    (driver / "Cargo.toml").write_text(
        '[workspace]\n[package]\nname="pvisor-dax-ab-subject"\nversion="0.0.0"\nedition="2024"\n'
        "[dependencies]\npvisor-vm={path=" + json.dumps(str(REPO / "crates/pvisor-vm")) + "}\n"
        'serde_json="1"\n[profile.release]\nopt-level=3\nlto="thin"\nstrip="symbols"\n'
        'codegen-units=8\npanic="abort"\n[profile.release.build-override]\nstrip="none"\n'
        '[profile.performance]\ninherits="release"\n[profile.performance.build-override]\nstrip="none"\n'
        "[patch.crates-io]\nfuser={path=" + json.dumps(str(REPO / "vendor/fuser")) + "}\n"
    )
    shutil.copy2(REPO / "Cargo.lock", driver / "Cargo.lock")
    # Preserve the exact worktree bytes used for this build, including untracked
    # implementation files. No checkout or workspace-wide build is performed.
    source = batch / "source"
    source.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(REPO / name, source / name)
    shutil.copytree(
        REPO / "crates",
        source / "crates",
        ignore=shutil.ignore_patterns(".data", "target", "__pycache__"),
    )
    if (REPO / ".cargo").is_dir():
        shutil.copytree(REPO / ".cargo", source / ".cargo")
    shutil.copytree(
        REPO / "vendor/fuser",
        source / "vendor/fuser",
        ignore=shutil.ignore_patterns(".data", "target", "__pycache__"),
    )
    source_hashes = {
        str(p.relative_to(source)): digest(p) for p in source.rglob("*") if p.is_file()
    }
    with (batch / "build.log").open("w") as log:
        subprocess.run(
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
            stdout=log,
            stderr=subprocess.STDOUT,
            check=True,
            cwd=REPO,
        )
    for relative, expected in source_hashes.items():
        if digest(REPO / relative) != expected:
            raise ValueError("build input changed: " + relative)
    subject = batch / "bin/subject"
    shutil.copy2(REPO / "target/performance/pvisor-dax-ab-subject", subject)
    firmware = batch / "bin/libkrunfw.so"
    shutil.copy2(args.firmware.resolve(), firmware)
    orders = [["off", "on"] for _ in range(15)] + [["on", "off"] for _ in range(15)]
    random.Random(20261011).shuffle(orders)
    protocol = {
        "benchmark_id": "B-FS-ENG",
        "role": "engineering A/B",
        "batch": batch.name,
        "entry": "filesystem/dax_ab.json",
        "template_sha256": digest(HERE / "dax_ab.json"),
        "question": "Does immutable-share DAX reduce verified read and mmap phase latency?",
        "rootfs": str(args.rootfs.resolve()),
        "firmware_source": str(args.firmware.resolve()),
        "subject": str(subject),
        "guest_cpus": 2,
        "guest_memory_mib": 512,
        "window_bytes": 128 << 20,
        "cpu_affinity": [0, 1],
        "cpu_quota": "200000 100000",
        "memory_max": 2 << 30,
        "swap_max": 0,
        "nofile_limit": 65536,
        "warmups_per_arm": 3,
        "pairs": 30,
        "orders": orders,
        "seed": 20261011,
        "bootstrap_samples": 5000,
        "cache": "warm host backing; new guest per trial; first data access vs same-guest repeat",
        "timer": "guest CLOCK_MONOTONIC_RAW; verification excluded; parallel includes thread create/join",
        "repeat_normalization": {"read": 4, "parallel": 4, "mmap": 16, "small": 1, "metadata": 1},
        "distribution_rule": "sorted gap >=1.5x, >=3 samples on each side, cluster medians >=1.7x => separated; no overall ranking",
        "failure_rule": "any validation/exit/input/artifact/budget/OOM/build-or-test interference error rejects entire batch; no slow-sample exclusion",
        "artifacts": {
            str(p.relative_to(batch)): digest(p)
            for p in (
                subject,
                firmware,
                data / "workload",
                *harness.iterdir(),
                driver / "Cargo.toml",
                driver / "Cargo.lock",
            )
        },
        "source_hashes": source_hashes,
        "host": {
            "platform": platform.platform(),
            "cpuinfo": Path("/proc/cpuinfo").read_text(),
            "cc": subprocess.check_output(["cc", "--version"], text=True),
            "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        },
        "input_before": tree(data),
        "rootfs_before": tree(args.rootfs.resolve()),
    }
    save(batch / "protocol.json", protocol)
    template = json.loads((HERE / "dax_ab.json").read_text())
    save(
        batch / "in.json",
        {
            name: command.replace("@HARNESS@", str(harness)).replace("@BATCH@", str(batch))
            for name, command in template.items()
        },
    )
    print(batch, flush=True)


def verify_artifacts(batch, protocol):
    for relative, expected in protocol["artifacts"].items():
        if digest(batch / relative) != expected:
            raise ValueError("artifact changed: " + relative)


def budget():
    relative = Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1]
    root = Path("/sys/fs/cgroup") / relative.lstrip("/")
    state = {
        name: (root / name).read_text().strip()
        for name in ("cpu.max", "memory.max", "memory.swap.max", "memory.events")
    }
    state["cpuset.cpus.effective"] = (
        (root / "cpuset.cpus.effective").read_text().strip()
        if (root / "cpuset.cpus.effective").exists()
        else None
    )
    state["cgroup"] = str(root)
    state["affinity"] = sorted(os.sched_getaffinity(0))
    state["nofile"] = list(resource.getrlimit(resource.RLIMIT_NOFILE))
    if not (
        state["cpu.max"] == "200000 100000"
        and state["memory.max"] == str(2 << 30)
        and state["memory.swap.max"] == "0"
        and state["affinity"] == [0, 1]
        and state["nofile"] == [65536, 65536]
    ):
        raise ValueError("budget not installed: " + json.dumps(state))
    return state


def trial(batch, protocol, group, index, mode):
    folder = batch / "trials" / group / f"{index:02}-{mode}"
    folder.mkdir(parents=True)
    stage = folder / "stage"
    stage.mkdir()
    (stage / "upper").mkdir()
    (stage / "work").mkdir()
    command = [
        protocol["subject"],
        protocol["rootfs"],
        str(batch / "bin/libkrunfw.so"),
        str(batch / "immutable"),
        str(stage),
        mode,
    ]
    started = time.monotonic_ns()
    with (folder / "stdout.log").open("wb") as out, (folder / "stderr.log").open("wb") as err:
        result = subprocess.run(
            command, stdin=subprocess.DEVNULL, stdout=out, stderr=err, timeout=60, check=False
        )
    ended = time.monotonic_ns()
    lines = [
        line.split("DAX_BENCH_RESULT ", 1)[1]
        for line in (folder / "stdout.log").read_text().splitlines()
        if "DAX_BENCH_RESULT " in line
    ]
    record = {
        "group": group,
        "index": index,
        "mode": mode,
        "command": command,
        "host_start_ns": started,
        "host_end_ns": ended,
        "returncode": result.returncode,
    }
    save(folder / "host.json", record)
    if result.returncode != 0 or len(lines) != 1:
        raise ValueError("failed guest trial: " + str(folder))
    observed = json.loads(lines[0])
    if (
        observed["mode"] != mode
        or observed["dax"] != (mode == "on")
        or observed["verified_bytes"] != 3 * (64 << 20) + 128 * (64 << 10)
        or observed["metadata_files"] != 2048
        or set(observed["metrics_ns"]) != set(METRICS)
        or not all(isinstance(v, int) and v > 0 for v in observed["metrics_ns"].values())
    ):
        raise ValueError("guest validation missing: " + str(folder))
    record.update(observed)
    save(folder / "result.json", record)
    shutil.rmtree(stage)  # the parent owns cleanup, including normal process exit
    return record


def separated(values):
    xs = sorted(values)
    candidates = [(xs[i] / xs[i - 1], i) for i in range(3, len(xs) - 2)]
    gap, split = max(candidates)
    if gap >= 1.5 and statistics.median(xs[split:]) / statistics.median(xs[:split]) >= 1.7:
        return [
            {"n": len(part), "median_ms": statistics.median(part)}
            for part in (xs[:split], xs[split:])
        ]
    return []


def analyze(batch, records):
    samples = [r for r in records if r["group"] == "samples"]
    rows = {}
    for metric in METRICS:
        off = [
            next(r for r in samples if r["index"] == i and r["mode"] == "off")["metrics_ns"][metric]
            / 1e6
            for i in range(30)
        ]
        on = [
            next(r for r in samples if r["index"] == i and r["mode"] == "on")["metrics_ns"][metric]
            / 1e6
            for i in range(30)
        ]
        a, b = statistics.median(off), statistics.median(on)
        diffs, ratios = [], []
        rng = random.Random(20261011)
        for _ in range(5000):
            indices = [rng.randrange(30) for _ in range(30)]
            x = statistics.median(off[i] for i in indices)
            y = statistics.median(on[i] for i in indices)
            diffs.append(y - x)
            ratios.append((y / x - 1) * 100)
        diffs.sort()
        ratios.sort()
        off_clusters, on_clusters = separated(off), separated(on)
        split = bool(off_clusters or on_clusters)
        rows[metric] = {
            "pairs": 30,
            "off_median_ms": None if off_clusters else a,
            "on_median_ms": None if on_clusters else b,
            "delta_ms": None if split else b - a,
            "delta_percent": None if split else (b / a - 1) * 100,
            "ci95_delta_ms": None if split else [diffs[124], diffs[4874]],
            "ci95_percent": None if split else [ratios[124], ratios[4874]],
            "off_clusters": off_clusters,
            "on_clusters": on_clusters,
            "off_sorted_ms": sorted(off),
            "on_sorted_ms": sorted(on),
            "verdict": "separated distribution"
            if split
            else "not detected"
            if diffs[124] <= 0 <= diffs[4874]
            else "faster"
            if diffs[4874] < 0
            else "slower",
        }
    save(batch / "summary.json", rows)
    with (batch / "summary.csv").open("w") as f:
        writer = csv.writer(f)
        writer.writerow(
            [
                "batch",
                "metric",
                "pairs",
                "off_ms",
                "on_ms",
                "delta_percent",
                "ci95_low_percent",
                "ci95_high_percent",
                "verdict",
            ]
        )
        for name, r in rows.items():
            ci = r["ci95_percent"] or [None, None]
            writer.writerow(
                [
                    batch.name,
                    name,
                    30,
                    r["off_median_ms"],
                    r["on_median_ms"],
                    r["delta_percent"],
                    *ci,
                    r["verdict"],
                ]
            )
    return rows


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_artifacts(batch, protocol)
    before = budget()
    save(batch / "budget-before.json", before)
    if tree(batch / "immutable") != protocol["input_before"]:
        raise ValueError("immutable inputs changed")
    records = []
    for i in range(3):
        for mode in ("off", "on"):
            records.append(trial(batch, protocol, "warmups", i, mode))
    for i, order in enumerate(protocol["orders"]):
        for mode in order:
            records.append(trial(batch, protocol, "samples", i, mode))
        save(batch / "records.partial.json", records)
        print(f"completed pair {i + 1}/30", file=sys.stderr, flush=True)
    after = budget()
    save(batch / "budget-after.json", after)

    def parse_events(value):
        return dict(line.split() for line in value.splitlines())

    prior_events, final_events = (
        parse_events(before["memory.events"]),
        parse_events(after["memory.events"]),
    )
    if any(prior_events[key] != final_events[key] for key in ("oom", "oom_kill", "oom_group_kill")):
        raise ValueError("OOM events occurred")
    if (
        tree(batch / "immutable") != protocol["input_before"]
        or tree(Path(protocol["rootfs"])) != protocol["rootfs_before"]
    ):
        raise ValueError("host input changed")
    verify_artifacts(batch, protocol)
    save(batch / "records.json", records)
    rows = analyze(batch, records)
    flat = {"Pairs": 30, "Measured VMs": 60, "Failed": 0, "Excluded": 0}
    for name, row in rows.items():
        flat[name + " off ms"] = row["off_median_ms"]
        flat[name + " on ms"] = row["on_median_ms"]
        flat[name + " delta %"] = row["delta_percent"]
        flat[name + " CI low %"], flat[name + " CI high %"] = row["ci95_percent"] or [None, None]
    print(json.dumps(flat), flush=True)


def measure(batch):
    if (batch / "records.json").exists():
        raise ValueError("refusing to reuse a measured batch")
    stops = threading.Event()
    interference = []
    ticks = []

    def monitor():
        while not stops.is_set():
            found = []
            for process in Path("/proc").glob("[0-9]*"):
                try:
                    name = (process / "comm").read_text().strip()
                    if name in BUILDERS:
                        found.append(
                            {
                                "pid": int(process.name),
                                "comm": name,
                                "cmdline": (process / "cmdline")
                                .read_bytes()
                                .replace(b"\0", b" ")
                                .decode(errors="replace"),
                            }
                        )
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    continue
            tick = {
                "monotonic_ns": time.monotonic_ns(),
                "builders": found,
                "loadavg": Path("/proc/loadavg").read_text().strip(),
            }
            ticks.append(tick)
            interference.extend(found)
            stops.wait(0.5)

    # The controller and monitor stay off the VM's two CPUs and outside its
    # resource cgroup. The whole campaign and its children share one budget.
    os.sched_setaffinity(0, {2, 3})
    worker = threading.Thread(target=monitor, daemon=True)
    worker.start()
    unit = "pvisor-dax-ab-" + uuid.uuid4().hex[:12]
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
        str(batch / "harness/dax_ab.py"),
        "--campaign",
        str(batch),
    ]
    save(batch / "campaign-command.json", command)
    try:
        with (
            (batch / "campaign.stdout.log").open("wb") as out,
            (batch / "campaign.stderr.log").open("wb") as err,
        ):
            result = subprocess.run(
                command, stdout=out, stderr=err, stdin=subprocess.DEVNULL, timeout=600
            )
    except BaseException:
        subprocess.run(
            ["systemctl", "--user", "stop", unit],
            check=False,
            timeout=20,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        raise
    finally:
        stops.set()
        worker.join()
        save(batch / "interference.json", {"ticks": ticks, "interference": interference})
    if result.returncode != 0 or interference:
        save(
            batch / "status.json",
            {"valid": False, "returncode": result.returncode, "interference": interference},
        )
        raise ValueError("campaign invalid; inspect " + str(batch / "campaign.stderr.log"))
    rows = json.loads((batch / "campaign.stdout.log").read_text())
    save(
        batch / "status.json",
        {
            "valid": True,
            "failures": 0,
            "excluded": 0,
            "pairs": 30,
            "monitor_ticks": len(ticks),
            "interference": 0,
        },
    )
    print(json.dumps(rows), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--prepare", type=Path)
    modes.add_argument("--measure", type=Path)
    modes.add_argument("--campaign", type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--firmware", type=Path)
    args = parser.parse_args()
    if args.prepare:
        if not args.rootfs or not args.firmware:
            parser.error("--prepare needs --rootfs and --firmware")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    else:
        campaign(args.campaign.resolve())


if __name__ == "__main__":
    main()
