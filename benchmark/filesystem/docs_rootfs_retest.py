#!/usr/bin/env python3
"""Benchmark: B-FS-ENG (REGISTRY.md#b-fs-eng), engineering A/B.

Reuse B-FS-TOOLS' directory-rootfs payload, private namespace, staged-output
oracle, Ready and three-round task clocks. Compare base/PR defaults in randomized
revision order at each case/pair/writeback setting. N3 is exploratory only.
"""

import argparse
import contextlib
import json
import random
import shutil
import sys
from pathlib import Path

REPO = next(
    p
    for p in Path(__file__).resolve().parents
    if (p / "benchmark/filesystem/user_workloads.py").is_file()
)
sys.path.insert(0, str(REPO / "benchmark/filesystem"))
import user_workloads as user  # noqa: E402


def prepare(args):
    root = args.prepare.resolve()
    assert ".data" in root.parts
    root.mkdir(parents=True, exist_ok=False)
    batches = {}
    for name in ("main", "candidate"):
        batch = root / name
        settings = argparse.Namespace(
            prepare=batch,
            payload_batch=args.payload,
            rootfs=args.rootfs,
            artifacts=args.builds / name,
            samples=args.samples,
            cases=args.cases,
            backends="vm,vm-writeback",
            seed=20261103,
            timeout=90,
            docker_setup=None,
            stock_kernel_receipt=None,
        )
        with contextlib.redirect_stdout(sys.stderr):
            user.prepare(settings)
        protocol = json.loads((batch / "protocol.json").read_text())
        protocol.update(benchmark_id="B-FS-ENG", role="engineering A/B")
        user.save(batch / "protocol.json", protocol)
        batches[name] = str(batch)
    user.save(
        root / "plan.json",
        {
            "benchmark_id": "B-FS-ENG",
            "role": "engineering A/B",
            "batches": batches,
            "seed": 20261103,
            "samples": args.samples,
            "scope": "published M01-M12/S01-S03; resource saturation not retested",
        },
    )
    shutil.copy2(__file__, root / "docs_rootfs_retest.py")
    import shlex

    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "-p",
        "CPUQuota=200%",
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
        str(root / "docs_rootfs_retest.py"),
        "--measure",
        str(root),
    ]
    user.save(root / "in.json", {"Directory rootfs regression": "$ " + shlex.join(command)})
    print(root)


def measure(root):
    plan = json.loads((root / "plan.json").read_text())
    protocols = {
        name: json.loads((Path(batch) / "protocol.json").read_text())
        for name, batch in plan["batches"].items()
    }
    for name, protocol in protocols.items():
        user.verify_inputs(Path(plan["batches"][name]), protocol)
    state = user.resource_state(protocol)
    assert state["cpu.max"] == "200000 100000"
    assert state["memory.max"] == str(2 << 30) and state["memory.swap.max"] == "0"
    assert state["affinity"] == [0, 1]
    rng = random.Random(plan["seed"])
    orders = list(protocol["orders"])
    records = []
    failed = set()
    for order in orders:
        for backend in order["backends"]:
            for name in rng.sample(list(protocols), len(protocols)):
                cell = (name, backend, order["case_id"])
                if cell in failed:
                    continue
                row = user.one_trial(
                    Path(plan["batches"][name]),
                    protocols[name],
                    order["case_id"],
                    order["pair"],
                    backend,
                )
                row["revision"] = name
                records.append(row)
                if not row["valid"]:
                    failed.add(cell)
                user.save(root / "records.partial.json", records)
                print(
                    cell, order["pair"], row["valid"], row.get("error"), file=sys.stderr, flush=True
                )
    for name, protocol in protocols.items():
        user.verify_inputs(Path(plan["batches"][name]), protocol)
    user.save(root / "records.json", records)
    user.save(
        root / "status.json",
        {
            "valid": not failed,
            "failed_cells": sorted(failed),
            "trials": len(records),
            "excluded": 0,
            "scope": plan["scope"],
        },
    )
    if failed:
        raise ValueError("failed cells retained; no complete-cohort performance conclusion")
    print(json.dumps({"N environments per cell": plan["samples"], "Trials": len(records)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare", type=Path)
    mode.add_argument("--measure", type=Path)
    parser.add_argument("--payload", type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--builds", type=Path)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument(
        "--cases", default=",".join([f"M{i:02}" for i in range(1, 13)] + ["S01", "S02", "S03"])
    )
    args = parser.parse_args()
    if args.prepare:
        prepare(args)
    else:
        measure(args.measure.resolve())
