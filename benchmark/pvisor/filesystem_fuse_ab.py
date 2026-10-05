#!/usr/bin/env python3
"""Matched host native / direct / passthrough FUSE / staged comparison.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role diagnostic.
Motivation: separate FUSE transport cost from staging semantics cost.
Conclusion sought: how much of the staged-to-native gap is transport and how
much is OverlayCore, persistence and content fingerprints.
Design: one batch with native, direct host, benchmark-only passthrough FUSE
and staged; identical fuser version, TTL and mount options. The passthrough
driver is a lower bound, never a product mode.
"""
import argparse
import csv
import json
import os
import random
import shutil
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from zoneinfo import ZoneInfo

from filesystem_stage_ab import WORKLOADS, summarize
from reference_baselines import digest, run_trial


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("assets", "binary", "fuse-driver", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--cpu-affinity", default="0,1")
    args = parser.parse_args()
    for name in ("assets", "binary", "fuse_driver", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    shutil.copy2(args.fuse_driver, args.output / "bin/fuse-passthrough")
    configuration = SimpleNamespace(
        assets=args.assets, output=args.output, firmware=None,
        cpu_affinity=args.cpu_affinity, staged_isolation="rootless_process",
        host_isolation="rootless_process", docker_root_pid=None,
        fuse_driver=args.output / "bin/fuse-passthrough", fuse_ttl_seconds=1,
    )
    os.environ["PVISOR_FS_PROFILE"] = "0"
    os.environ.pop("PVISOR_VM_FS_WORKERS", None)
    backends = ["native", "pvisor-host", "pvisor-fuse", "pvisor-staged"]
    metadata = {
        "schema": "pvisor-filesystem-fuse-ab/v1",
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "binary_sha256": {name: digest(args.output / "bin" / name)
                          for name in ("pvisor", "fuse-passthrough")},
        "workload_sha256": digest(args.assets / "rootfs/bench/reference_workload.py"),
        "fs_workload_sha256": digest(args.assets / "rootfs/bench/harness/v1/workload.py"),
        "harness_sha256": {name: digest(Path(__file__).parent / name) for name in
                           ("filesystem_fuse_ab.py", "filesystem_fuse_passthrough.rs",
                            "reference_baselines.py", "filesystem_stage_ab.py")},
        "host_kernel": os.uname().release,
        "protocol": {
            "samples": args.samples, "warmups": args.warmups,
            "order": "four cells randomly interleaved each round; seed 20261005",
            "cache": "warm host caches; fresh copied workspace and mount/upper per job",
            "cpu_affinity": args.cpu_affinity, "host_isolation": "rootless_process",
            "fuse": "same frozen vendored fuser 0.15.1, abi-7-31, default init flags; one synchronous request loop",
            "ttl_seconds": 1, "writeback_cache": False, "keep_cache": False,
            "mount_options": "RW, NoAtime, DefaultPermissions; no kernel backing-FD passthrough",
            "native_control": "filesystem_fuse_passthrough.rs: libc/native files only; no OverlayCore, copy-up, access policy or preimage journal",
            "timing": "seven unchanged workers and checks; wall includes FUSE mount, child pvisor Run and unmount for passthrough; staged CLI mounts internally",
            "correctness": "Run Bundle/isolation, full tools, 256 backing or upper writes; passthrough mountinfo plus actual LOOKUP/READ/WRITE request and byte counts",
            "counters": "eight relaxed integer counters in passthrough control; no per-request clocks or logging",
        }, "load_before": os.getloadavg(),
    }
    rows, preflights = [], {}

    def save():
        report = metadata | {"rows": rows, "preflights": preflights,
                             "load_after": os.getloadavg()}
        temporary = args.output / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n")
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
    with (args.output / "summary.tsv").open("w") as stream:
        writer = csv.writer(stream, delimiter="\t")
        writer.writerow(("backend", "operation", "n", "p50_ms", "p95_ms", "p99_ms"))
        for backend, summary in metadata["summary"].items():
            for op in (*WORKLOADS, "completion_ms"):
                values = summary["timings_ms"][op]
                writer.writerow((backend, op, summary["n"], values["p50"], values["p95"], values["p99"]))
    print("report", args.output / "report.json", flush=True)


if __name__ == "__main__":
    main()
