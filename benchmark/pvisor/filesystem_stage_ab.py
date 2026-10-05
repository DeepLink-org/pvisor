#!/usr/bin/env python3
"""Separate host staging, VM workspace staging and VM rootfs OverlayFS costs.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: locate which staging layer contributes VM tool overhead.
Conclusion sought: cost attributed to host staging, VM workspace staging and
VM rootfs overlay for the same workloads.
Design: one batch toggling one layer at a time; diagnostic VM driver only.
"""
import argparse
import json
import os
import random
import shutil
import subprocess
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from zoneinfo import ZoneInfo

from bench import percentile
from reference_baselines import digest, run_trial

WORKLOADS = ("metadata", "read", "write", "git", "rg", "cargo", "npm")


def summarize(rows, backends, samples):
    results = {}
    for backend in backends:
        selected = [row for row in rows if row["backend"] == backend]
        if len(selected) != samples:
            raise ValueError(f"incomplete matrix for {backend}: {len(selected)}/{samples}")
        values = {key: [row[key] for row in selected]
                  for key in ("completion_ms", "ready_ms")}
        values.update({mode: [row["result"]["filesystem"][mode]["worker_ms"]
                              for row in selected] for mode in WORKLOADS})
        results[backend] = {"n": len(selected), "timings_ms": {
            key: {f"p{q}": percentile(items, q) for q in (50, 95, 99)}
            for key, items in values.items()}}
    return results


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ("assets", "binary", "firmware", "sdk-driver", "output"):
        p.add_argument("--" + name, type=Path, required=True)
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--cpu-affinity", default="0,1")
    p.add_argument("--backends", default="native,pvisor-host,pvisor-staged,pvisor-vm,"
                   "sdk-vm-overlay,sdk-vm-workspace-direct,sdk-vm-passthrough")
    args = p.parse_args()
    for key in ("assets", "binary", "firmware", "sdk_driver", "output"):
        setattr(args, key, getattr(args, key).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    shutil.copy2(args.sdk_driver, args.output / "bin/sdk-vm")
    # Native passthrough may write: expose only this disposable copy, never the fixture root.
    sdk_root = args.output / "sdk-rootfs"
    subprocess.run(["cp", "--reflink=auto", "-a", str(args.assets / "rootfs"),
                    str(sdk_root)], check=True)
    configuration = SimpleNamespace(
        assets=args.assets, output=args.output, firmware=args.firmware,
        cpu_affinity=args.cpu_affinity, memory_mib=4096,
        staged_isolation="rootless_process", docker_root_pid=None,
        host_isolation="rootless_process",
        sdk_driver=args.output / "bin/sdk-vm",
        sdk_rootfs=sdk_root,
    )
    os.environ["PVISOR_FS_PROFILE"] = "0"
    os.environ.pop("PVISOR_VM_FS_WORKERS", None)
    backends = args.backends.split(",")
    metadata = {
        "schema": "pvisor-filesystem-stage-ab/v1",
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {key: str(value) if isinstance(value, Path) else value
                      for key, value in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "binary_sha256": {name: digest(args.output / "bin" / name)
                          for name in ("pvisor", "sdk-vm")},
        "firmware_sha256": digest(args.firmware / "libkrunfw.so.5"),
        "workload_sha256": digest(args.assets / "rootfs/bench/reference_workload.py"),
        "fs_workload_sha256": digest(args.assets / "rootfs/bench/harness/v1/workload.py"),
        "harness_sha256": {name: digest(Path(__file__).parent / name) for name in
                           ("filesystem_stage_ab.py", "reference_baselines.py",
                            "filesystem_stage_vm.rs")},
        "host_kernel": os.uname().release,
        "protocol": {
            "samples": args.samples, "warmups": args.warmups,
            "order": "seeded shuffled backends each round; seed 20261005",
            "cache": "host warm; fresh guest/workspace/upper per job; no eviction",
            "cpu_affinity": args.cpu_affinity, "vm_vcpus": 2, "vm_memory_mib": 4096,
            "dax": False, "profiling": False,
            "sdk": "same built-in guest supervisor and frozen VMM source; no CLI lifecycle/Run Bundle; SDK-only tmpdir /dev/shm/reference and bytecode writes disabled",
            "sdk_modes": {"overlay": "rootfs and workspace OverlayFS",
                          "workspace-direct": "rootfs OverlayFS; workspace native passthrough",
                          "passthrough": "rootfs and workspace native passthrough"},
            "ttl": "OverlayFS 1s; native passthrough defaults 5s entry/attr; cache Auto",
            "timing": "seven unchanged tools and checks; launch-to-exit includes worker startup; preparation excluded",
            "correctness": "tool results; host direct/staged both rootless_process; CLI Run Bundle; 256 write names/sizes in upper or direct workspace",
        }, "load_before": os.getloadavg(),
    }
    rows, preflights = [], {}

    def save():
        result = metadata | {"rows": rows, "preflights": preflights,
                             "load_after": os.getloadavg()}
        temporary = args.output / "report.tmp"
        temporary.write_text(json.dumps(result, indent=2) + "\n")
        temporary.replace(args.output / "report.json")

    save()
    for backend in backends:
        try:
            run_trial(configuration, metadata, backend, "filesystem", -100)
        except Exception as error:
            preflights[backend] = {"state": "failed", "reason": str(error)}
            save()
            raise
        preflights[backend] = {"state": "passed"}
        save()
        print("preflight passed", backend, flush=True)
    rng = random.Random(20261005)
    for trial in range(-args.warmups, args.samples):
        order = backends.copy()
        rng.shuffle(order)
        for backend in order:
            try:
                row = run_trial(configuration, metadata, backend, "filesystem", trial)
            except Exception as error:
                metadata["failure"] = {"backend": backend, "trial": trial, "reason": str(error)}
                save()
                raise
            if trial >= 0:
                rows.append(row)
                save()
        print("round", trial + 1, "/", args.samples, flush=True)
    metadata["summary"] = summarize(rows, backends, args.samples)
    save()
    print("report", args.output / "report.json", flush=True)


if __name__ == "__main__":
    main()
