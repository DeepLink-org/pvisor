#!/usr/bin/env python3
"""Compare two pinned pVisor binaries through real FUSE and virtio-fs jobs."""

import argparse
import json
import os
import random
import shutil
import subprocess
from collections import Counter
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from zoneinfo import ZoneInfo

from bench import percentile
from reference_baselines import digest, run_trial

WORKLOADS = ("metadata", "read", "write", "git", "rg", "cargo", "npm")


def validate_binaries(hashes, provenance=None):
    if hashes["baseline"] == hashes["candidate"]:
        raise ValueError("baseline and candidate binaries have identical hashes")
    if provenance and provenance["binary_sha256"] != {
        key: hashes[key] for key in ("baseline", "candidate")
    }:
        raise ValueError("build manifest does not match the copied binaries")


def summarize(rows, cells, samples):
    counts = Counter((r["variant"], r["backend"]) for r in rows)
    if counts != Counter({cell: samples for cell in cells}):
        raise ValueError(f"incomplete sample matrix: {dict(counts)}")
    summary = {}
    for variant, backend in cells:
        selected = [r for r in rows if (r["variant"], r["backend"]) == (variant, backend)]
        timings = {"completion_ms": [r["completion_ms"] for r in selected]}
        timings.update(
            {
                mode: [r["result"]["filesystem"][mode]["worker_ms"] for r in selected]
                for mode in WORKLOADS
            }
        )
        summary[f"{variant}/{backend}"] = {
            "n": samples,
            "timings_ms": {
                key: {f"p{q}": percentile(values, q) for q in (50, 95, 99)}
                for key, values in timings.items()
            },
        }
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--provenance", type=Path, help="Build/source manifest for both binaries")
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--cpu-affinity", default="0,1")
    parser.add_argument("--memory-mib", type=int, default=16384)
    parser.add_argument(
        "--backends", default="pvisor-staged,pvisor-vm",
        help="Comma-separated measured backends: pvisor-staged, pvisor-vm",
    )
    for variant in ("baseline", "candidate"):
        parser.add_argument(
            f"--{variant}-staged-isolation",
            choices=("host_process", "rootless_process"),
            default="host_process",
            help="Required observed staged isolation; does not change runtime configuration",
        )
    parser.add_argument(
        "--profile", action="store_true", help="Diagnostic run; exclude from performance claims"
    )
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    backends = args.backends.split(",")
    if (not backends or len(backends) != len(set(backends))
            or any(backend not in ("pvisor-staged", "pvisor-vm") for backend in backends)):
        parser.error("backends must be a unique nonempty subset of pvisor-staged,pvisor-vm")
    for key in ("assets", "baseline", "candidate", "firmware", "output"):
        setattr(args, key, getattr(args, key).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    shutil.copytree(
        Path(__file__).parent,
        args.output / "harness",
        ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache"),
    )
    firmware = args.output / "firmware"
    firmware.mkdir()
    shutil.copy2(args.firmware / "libkrunfw.so.5", firmware / "libkrunfw.so.5")
    cells = [("native", "native")] + [
        (variant, backend)
        for variant in ("baseline", "candidate")
        for backend in backends
    ]
    configurations = {}
    hashes = {}
    for variant in ("native", "baseline", "candidate"):
        directory = args.output / variant
        (directory / "bin").mkdir(parents=True)
        binary = args.baseline if variant == "baseline" else args.candidate
        shutil.copy2(binary, directory / "bin/pvisor")
        hashes[variant] = digest(directory / "bin/pvisor")
        configurations[variant] = SimpleNamespace(
            assets=args.assets,
            output=directory,
            firmware=firmware,
            cpu_affinity=args.cpu_affinity,
            memory_mib=args.memory_mib,
            staged_isolation=getattr(args, f"{variant}_staged_isolation", "host_process"),
            docker_root_pid=None,
        )
    validate_binaries(hashes)
    os.environ["PVISOR_FS_PROFILE"] = "1" if args.profile else "0"
    os.environ.pop("PVISOR_VM_FS_WORKERS", None)
    metadata = {
        "schema": "pvisor-filesystem-ab/v1",
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {
            key: str(value) if isinstance(value, Path) else value
            for key, value in vars(args).items()
        },
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "assets_workload_sha256": digest(args.assets / "rootfs/bench/reference_workload.py"),
        "assets_fs_workload_sha256": digest(args.assets / "rootfs/bench/harness/v1/workload.py"),
        "binary_sha256": hashes,
        "firmware_sha256": digest(firmware / "libkrunfw.so.5"),
        "harness_sha256": {
            str(p.relative_to(args.output / "harness")): digest(p)
            for p in (args.output / "harness").rglob("*.py")
        },
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "source_status": subprocess.check_output(["git", "status", "--porcelain"], text=True),
        "host_kernel": os.uname().release,
        "cpu": next(
            line.split(":", 1)[1].strip()
            for line in Path("/proc/cpuinfo").read_text().splitlines()
            if line.startswith("model name")
        ),
        "protocol": {
            "samples": args.samples,
            "warmups": args.warmups,
            "order": f"all {len(cells)} cells shuffled each round; seed 20261005",
            "cache": "warm; no eviction; fresh workspace and stage for every job",
            "cpu_affinity": args.cpu_affinity,
            "vm_vcpus": 2,
            "vm_memory_mib": args.memory_mib,
            "filesystem_profile": args.profile,
            "staged_isolation": {
                variant: configurations[variant].staged_isolation
                for variant in ("baseline", "candidate")
            },
            "worker_timing": "operation and correctness check; all seven workloads run in order per job",
            "completion_timing": "launch to process exit; preparation excluded",
            "percentile": "linear interpolation",
            "correctness": "all workload checks, Run Bundle isolation, lower write absence and upper file count/size required",
        },
        "load_before": os.getloadavg(),
    }
    if args.provenance:
        provenance = json.loads(args.provenance.read_text())
        validate_binaries(hashes, provenance)
        metadata["build_provenance"] = provenance
    rows, capabilities = [], {}

    def save():
        temporary = args.output / "report.tmp"
        temporary.write_text(
            json.dumps(
                metadata
                | {
                    "rows": rows,
                    "capabilities": capabilities,
                    "load_after": os.getloadavg(),
                },
                indent=2,
            )
            + "\n"
        )
        temporary.replace(args.output / "report.json")

    def one(cell, trial):
        variant, backend = cell
        row = run_trial(configurations[variant], metadata, backend, "filesystem", trial)
        row["variant"] = variant
        return row

    save()
    try:
        for cell in cells:
            key = "/".join(cell)
            try:
                one(cell, -100)
            except Exception as error:
                capabilities[key] = {"state": "failed", "reason": str(error)}
                raise
            capabilities[key] = {"state": "available"}
            save()
        rng = random.Random(20261005)
        for trial in range(-args.warmups, args.samples):
            order = cells.copy()
            rng.shuffle(order)
            for cell in order:
                row = one(cell, trial)
                if trial >= 0:
                    rows.append(row)
                save()
            print(f"filesystem A/B: round {trial + 1}/{args.samples}", flush=True)
        metadata["summary"] = summarize(rows, cells, args.samples)
    except BaseException as error:
        capabilities["run"] = {"state": "failed", "reason": str(error) or type(error).__name__}
        raise
    finally:
        save()
    print(f"report: {args.output / 'report.json'}", flush=True)


if __name__ == "__main__":
    main()
