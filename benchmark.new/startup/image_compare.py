#!/usr/bin/env python3
"""Prepare, measure and audit a same-image prepared-instance comparison.

Benchmark: B-LAZY-STARTUP (benchmark/README.md#b-lazy-startup), user-facing scope.
Question: with image inputs prepared, how long do new OCI/TurboOCI containers
and a pVisor VM take to produce the same verified output and exit normally?
Design: 3 warmups and 30 balanced randomized rounds, CPU 12/13, 2 CPU/2 GiB
container tasks vs 2-vCPU/2-GiB guest RAM (not equal enclosing memory budgets).
This is a Python event-clock experiment, not hyperfine, cold pull or OCI-vm
preparation. Deployment helpers and source artifacts must already be available.
"""

import argparse
import hashlib
import itertools
import json
import os
import platform
import random
import shlex
import shutil
import subprocess
import sys
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
GROUPS = ["oci", "turbo", "pvisor"]
WORKLOAD = [
    "/bin/sh",
    "-ec",
    '. /etc/os-release; test "$ID" = ubuntu; '
    'test "$VERSION_ID" = 26.04; printf "LAZY_READY ubuntu %s\\n" "$VERSION_ID"',
]
SOURCE = "sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794"
BUILDERS = {"cargo", "rustc", "ninja", "make", "pytest", "cargo-nextest", "nextest", "hyperfine"}


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def invoke(argv, directory, timeout=600):
    directory.mkdir(parents=True, exist_ok=False)
    save(directory / "argv.json", [str(value) for value in argv])
    with (
        (directory / "stdout.txt").open("wb") as stdout,
        (directory / "stderr.txt").open("wb") as stderr,
    ):
        result = subprocess.run(
            [str(value) for value in argv], stdout=stdout, stderr=stderr, timeout=timeout
        )
    save(directory / "exit.json", {"exit_code": result.returncode})
    if result.returncode:
        raise RuntimeError(f"Adapter failed ({result.returncode}); see {directory}")
    value = json.loads((directory / "stdout.txt").read_text())
    save(directory / "result.json", value)
    return value


def parse_cpus(value):
    cpus = [int(part) for part in value.split(",")]
    if len(cpus) != 2 or len(set(cpus)) != 2 or not set(cpus) <= os.sched_getaffinity(0):
        raise ValueError("Specify two distinct CPUs available to this observer")
    return cpus


def process_snapshot(owned_roots=()):
    """Keep benchmark/build process metadata only, including their descendants."""
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            stat = (path / "stat").read_text().rpartition(")")[2].split()
            cmd = (path / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            comm = (path / "comm").read_text().strip()
            status = (path / "status").read_text()
            allowed = next(
                line.split(":", 1)[1].strip()
                for line in status.splitlines()
                if line.startswith("Cpus_allowed_list:")
            )
            processes[int(path.name)] = {
                "ppid": int(stat[1]),
                "ticks": int(stat[11]) + int(stat[12]),
                "starttime": int(stat[19]),
                "last_cpu": int(stat[36]),
                "comm": comm,
                "command": cmd,
                "allowed_cpus": allowed,
            }
        except (OSError, ValueError, IndexError, StopIteration):
            continue
    descendants = {os.getpid(), *owned_roots}
    for _ in range(32):
        expanded = descendants | {
            pid for pid, row in processes.items() if row["ppid"] in descendants
        }
        if expanded == descendants:
            break
        descendants = expanded
    tagged = {
        pid
        for pid, row in processes.items()
        if row["comm"] in BUILDERS
        or "benchmark.new/" in row["command"]
        or "benchmark/pvisor/" in row["command"]
    }
    for _ in range(32):
        expanded = tagged | {pid for pid, row in processes.items() if row["ppid"] in tagged}
        if expanded == tagged:
            break
        tagged = expanded
    return {str(pid): row for pid, row in processes.items() if pid in tagged - descendants}


def interference(before, after, cpus):
    """Flag active foreign builds or sustained benchmark work on sampled CPUs.

    Incidental idle coordinator scheduling is retained in logs, not flagged as a
    busy worker. At least 0.2 CPU seconds in the round is required for non-builds.
    """
    threshold = max(1, int(os.sysconf("SC_CLK_TCK") * 0.2))
    flagged = []
    for pid, row in after.items():
        old = before.get(pid)
        used = (
            row["ticks"] - old["ticks"]
            if old and old["starttime"] == row["starttime"]
            else row["ticks"]
        )
        if row["comm"] in BUILDERS and used > 0:
            flagged.append({"pid": pid, "used_ticks": used, **row})
        elif row["last_cpu"] in cpus and used >= threshold:
            flagged.append({"pid": pid, "used_ticks": used, **row})
    return flagged


def prepare(args):
    batch = args.prepare.resolve()
    if ".data" not in batch.parts:
        raise ValueError("Evidence must be retained under a .data directory")
    cpus = parse_cpus(args.cpus)
    service_cpus = parse_cpus(args.service_cpus)
    if set(cpus) & set(service_cpus):
        raise ValueError("Application and service CPUs must be distinct")
    batch.mkdir(parents=True, exist_ok=False)
    harness = batch / "harness"
    harness.mkdir()
    for name in ("image_compare.py", "image_compare_analysis.py", "pvisor_image_perf.py"):
        shutil.copy2(HERE / name, harness / name)
    overlay_source = args.overlay_adapter.resolve()
    for name in ("overlaybd_image_perf.py", "overlaybd_lab.py", "overlaybd_smoke.py"):
        shutil.copy2(overlay_source.parent / name, harness / name)
    rng = random.Random(20261017)
    permutations = list(itertools.permutations(GROUPS)) * 5
    rng.shuffle(permutations)
    warmups = [rng.sample(GROUPS, len(GROUPS)) for _ in range(3)]
    protocol = {
        "schema": 1,
        "benchmark": "B-LAZY-STARTUP",
        "scope": "warm-prepared",
        "groups": GROUPS,
        "runs": 30,
        "warmups": 3,
        "orders": {"warmup": warmups, "sample": permutations},
        "seed": 20261017,
        "source_digest": SOURCE,
        "workload_argv": WORKLOAD,
        "cpus": cpus,
        "service_cpus": service_cpus,
        "lab": str(args.lab.resolve()),
        "pvisor_output": str(batch / "pvisor"),
        "preparation": {},
        "clock": "adapter perf_counter_ns immediately before timed Popen; exact marker and normal exit",
        "excludes": "observer interpreter, JSON RPC, pull/format conversion/rootfs preparation, audit and cleanup",
        "includes": "Docker exec client on OCI/TurboOCI, local pVisor CLI on VM",
        "cache": "prepared client image inputs, retained host page cache, new task/VM and writable state",
        "budget": "2 CPU quota/2 GiB container tasks vs 2 vCPU/2 GiB guest RAM; helpers separately pinned",
        "background_policy": "foreign workloads retained and recorded; active builds or >=0.2 CPU seconds of another benchmark whose last CPU is sampled invalidate the entire cohort; no sample exclusions",
        "host": {
            "kernel": platform.release(),
            "machine": platform.machine(),
            "python": sys.version,
            "cpu_count": os.cpu_count(),
            "allowed_cpus": sorted(os.sched_getaffinity(0)),
        },
        "status": "preparing",
    }
    save(batch / "protocol.json", protocol)
    try:
        start = time.monotonic_ns()
        # prepare needs the source adapter's repository location to freeze helpers;
        # all subsequent RPC/trial calls use the byte-identical frozen copy.
        invoke(
            [
                sys.executable,
                HERE / "pvisor_image_perf.py",
                "prepare",
                "--output",
                batch / "pvisor",
                "--binary-dir",
                args.binary_dir.resolve(),
                "--prepared-store",
                args.prepared_store.resolve(),
                "--cpus",
                args.cpus,
                "--service-cpus",
                args.service_cpus,
                "--memory-mib",
                "2048",
                "--lifetime",
                "2400",
            ],
            batch / "requests/prepare-pvisor",
        )
        protocol["preparation"]["pvisor_cached_input_ns"] = time.monotonic_ns() - start
        for group in ("oci", "turbo"):
            start = time.monotonic_ns()
            value = invoke(
                [
                    sys.executable,
                    harness / "overlaybd_image_perf.py",
                    "--action",
                    "prepare",
                    "--lab",
                    args.lab.resolve(),
                    "--condition",
                    group,
                    "--cpus",
                    args.cpus,
                    "--output",
                    batch / "prepare" / group,
                ],
                batch / "requests" / ("prepare-" + group),
            )
            if not value.get("valid") or value["workload_argv"] != WORKLOAD:
                raise ValueError("Container preflight or workload equality failed")
            protocol["preparation"][group + "_prepared_validation_ns"] = time.monotonic_ns() - start
        artifacts = {}
        for directory in (harness, batch / "pvisor/frozen"):
            for path in directory.rglob("*"):
                if path.is_file() and "__pycache__" not in path.parts:
                    artifacts[str(path)] = digest(path)
        for group in ("oci", "turbo"):
            path = batch / "prepare" / group / "result.json"
            artifacts[str(path)] = digest(path)
        protocol["artifacts"] = artifacts
        supervisor = json.loads((batch / "pvisor/supervisor.json").read_text())
        protocol["owned_process_roots"] = [supervisor["pid"]]
        protocol["background_before"] = process_snapshot(protocol["owned_process_roots"])
        protocol["status"] = "prepared"
        save(batch / "protocol.json", protocol)
        command = "$ " + shlex.join(
            [sys.executable, str(harness / "image_compare.py"), "--measure", str(batch)]
        )
        save(batch / "in.json", {"warm-prepared-paired": command})
        print(batch)
    except BaseException:
        protocol["status"] = "prepare-failed"
        save(batch / "protocol.json", protocol)
        (batch / "failure.txt").write_text(traceback.format_exc())
        if (batch / "pvisor/worker.json").exists():
            invoke(
                [
                    sys.executable,
                    harness / "pvisor_image_perf.py",
                    "stop",
                    "--output",
                    batch / "pvisor",
                ],
                batch / "requests/prepare-failed-stop",
            )
        raise


def check_artifacts(protocol):
    for filename, expected in protocol["artifacts"].items():
        if digest(filename) != expected:
            raise ValueError("Frozen artifact changed: " + filename)


def measure(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    if protocol["status"] != "prepared" or (batch / "records.json").exists():
        raise ValueError("Expected a new prepared cohort; refusing to reuse measured evidence")
    check_artifacts(protocol)
    records, background, flags = [], [], []
    cpus = ",".join(map(str, protocol["cpus"]))
    try:
        for phase in ("warmup", "sample"):
            for index, order in enumerate(protocol["orders"][phase]):
                before = process_snapshot(protocol["owned_process_roots"])
                for group in order:
                    identity = f"{phase}-{index:02d}-{group}"
                    if group == "pvisor":
                        argv = [
                            sys.executable,
                            batch / "harness/pvisor_image_perf.py",
                            "trial",
                            "--output",
                            batch / "pvisor",
                            "--mode",
                            "prepared",
                            "--trial-id",
                            identity,
                        ]
                    else:
                        argv = [
                            sys.executable,
                            batch / "harness/overlaybd_image_perf.py",
                            "--action",
                            "trial",
                            "--lab",
                            protocol["lab"],
                            "--condition",
                            group,
                            "--cpus",
                            cpus,
                            "--prepare-receipt",
                            batch / "prepare" / group / "result.json",
                            "--output",
                            batch / "trials" / identity,
                        ]
                    directory = batch / "requests" / identity
                    result = invoke(argv, directory)
                    if (
                        not result.get("valid")
                        or result.get("workload_argv") != WORKLOAD
                        or type(result.get("ready_ns")) is not int
                        or not 0 < result["ready_ns"] <= result["completion_ns"]
                    ):
                        raise ValueError("Invalid measured adapter result: " + identity)
                    records.append(
                        {
                            "phase": phase,
                            "round": index,
                            "condition": group,
                            "result": result,
                            "result_path": str(directory / "result.json"),
                            "result_sha256": digest(directory / "result.json"),
                        }
                    )
                    save(batch / "records.partial.json", records)
                after = process_snapshot(protocol["owned_process_roots"])
                detected = interference(before, after, protocol["cpus"])
                background.append(
                    {
                        "phase": phase,
                        "round": index,
                        "before": before,
                        "after": after,
                        "flagged": detected,
                    }
                )
                flags.extend(detected)
                save(batch / "background.json", background)
                print(
                    f"{phase} round {index + 1}/{len(protocol['orders'][phase])} checked",
                    file=sys.stderr,
                    flush=True,
                )
        save(batch / "records.json", records)
        if flags:
            raise RuntimeError(
                "Foreign build/benchmark interference detected; entire cohort is marked invalid"
            )
        check_artifacts(protocol)
    except BaseException:
        (batch / "failure.txt").write_text(traceback.format_exc())
        raise
    finally:
        invoke(
            [
                sys.executable,
                batch / "harness/pvisor_image_perf.py",
                "stop",
                "--output",
                batch / "pvisor",
            ],
            batch / "requests/stop-pvisor",
        )
    from image_compare_analysis import analyze

    analysis = analyze(batch)
    flat = {"Pairs": protocol["runs"], "Failures": 0, "Excluded": 0}
    # Detailed report and uncertainty are produced by the dedicated audit.
    for group in protocol["groups"]:
        flat[group + " Ready median ms"] = analysis["summary"][group]["ready"]["median_ms"]
        flat[group + " Completion median ms"] = analysis["summary"][group]["completion"][
            "median_ms"
        ]
    save(batch / "measured-row.json", flat)
    print(json.dumps(flat))


def audit(batch):
    from image_compare_analysis import analyze, render

    analysis = analyze(batch)
    if (batch / "in.render.json").exists():
        rendered = json.loads((batch / "in.render.json").read_text())
        measured = json.loads((batch / "measured-row.json").read_text())
        if rendered != {"warm-prepared-paired": measured}:
            raise ValueError("semspec rendered cells do not match the audited measured row")
    (batch / "report.md").write_text(render(batch, analysis))
    print(batch / "report.md")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--prepare", type=Path)
    action.add_argument("--measure", type=Path)
    action.add_argument("--audit", type=Path)
    parser.add_argument("--lab", type=Path)
    parser.add_argument("--prepared-store", type=Path)
    parser.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--overlay-adapter", type=Path, default=HERE / "overlaybd_image_perf.py")
    parser.add_argument("--cpus", default="12,13")
    parser.add_argument("--service-cpus", default="14,15")
    args = parser.parse_args()
    if args.prepare:
        if args.lab is None or args.prepared_store is None:
            parser.error("--prepare requires --lab and --prepared-store")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    else:
        audit(args.audit.resolve())


if __name__ == "__main__":
    main()
