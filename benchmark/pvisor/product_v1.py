#!/usr/bin/env python3
"""Reproduce the first product benchmark reports on a Linux host.

Runs the v1 suites; each module under v1/ names its own registry entry in
benchmark/README.md#registry. This driver adds no benchmark of its own.
"""

import argparse
from pathlib import Path

from v1 import (
    agent,
    apply,
    baselines,
    isolation,
    network,
    oci,
    replay,
    supervision,
)
from v1.common import Context

BENCHMARK_IDS = {
    "network": "B-NETWORK", "apply": "B-APPLY", "baselines": "B-APPLY", "crashes": "B-APPLY", "concurrent-conflicts": "B-APPLY",
    "agent": "B-AGENT-TASK", "isolation": "B-ISOLATION",
    "replay": "B-REPLAY", "supervision": "B-SUPERVISION",
}


def benchmark_for_suites(suites):
    selected = suites.split(",")
    if any(suite not in BENCHMARK_IDS for suite in selected):
        raise ValueError("unknown or superseded suite; use reference_baselines.py for filesystem and density.py for density")
    ids = {BENCHMARK_IDS[suite] for suite in selected}
    if len(ids) != 1 or len(set(selected)) != len(selected):
        raise ValueError("run one benchmark ID per invocation, without duplicate suites")
    return ids.pop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/release/pvisor"))
    parser.add_argument("--build-receipt", type=Path)
    parser.add_argument("--cpu-affinity", default="0,1")
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--replay-binary", type=Path, default=Path("target/release/pvisor-replay"))
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--seed", type=int, default=20261006)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--oci-shape", choices=["tools", "shell"], default="tools")
    parser.add_argument("--prepared-rootfs", type=Path)
    parser.add_argument("--input-manifest", type=Path)
    parser.add_argument("--podman-root", type=Path)
    parser.add_argument("--podman-runroot", type=Path)
    parser.add_argument("--podman-image")
    parser.add_argument("--network-backends", default="native,host,vm,podman,container")
    parser.add_argument("--network-modes", default="small,bulk,stream,deny")
    parser.add_argument("--apply-sizes", default="10,1000,100000")
    parser.add_argument("--vm-memory", default="1GiB")
    parser.add_argument("--crash-files", type=int, default=10000)
    parser.add_argument("--crash-states", default="prepared,target_applied,committed")
    parser.add_argument("--suites", required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    try:
        benchmark_id = benchmark_for_suites(args.suites)
    except ValueError as error:
        parser.error(str(error))
    ctx = Context(args)
    ctx.metadata["benchmark_id"] = benchmark_id
    selected = args.suites.split(",")
    if set(selected) & {"network", "isolation", "agent"}:
        if args.prepared_rootfs:
            oci.use_prepared(ctx)
        else:
            (oci.prepare_shell if args.oci_shape == "shell" else oci.prepare)(ctx)
    else:
        ctx.metadata["oci_preparation"] = "not required for selected host-only suites"
        ctx.save()
    suites = {
        "network": network.run,
        "apply": apply.run,
        "agent": agent.run,
        "isolation": isolation.run,
        "replay": replay.run,
        "supervision": supervision.run,
        "baselines": baselines.run,
        "crashes": apply.crashes,
        "concurrent-conflicts": apply.concurrent_conflicts,
    }
    combined_apply = "apply" in selected and "baselines" in selected
    for name in selected:
        if combined_apply and name == "baselines":
            continue
        if name not in suites:
            parser.error(f"unknown suite: {name}")
        try:
            if name == "apply":
                apply.run(ctx, include_git=combined_apply)
            else:
                suites[name](ctx)
        except Exception as error:
            import traceback

            ctx.capabilities["suite/" + name] = {"state": "failed", "reason": str(error)}
            (ctx.output / (name + "-failure.txt")).write_text(traceback.format_exc())
            ctx.save()
            print(f"suite {name} failed: {error}", flush=True)
    if args.prepared_rootfs:
        import json
        try:
            oci.verify_prepared(args.prepared_rootfs.resolve(), json.loads(args.input_manifest.read_text()))
            ctx.metadata['prepared_inputs_unchanged'] = True
        except Exception as error:
            ctx.metadata['prepared_inputs_unchanged'] = False
            ctx.capabilities['prepared-inputs'] = dict(state='failed', reason=str(error))
    ctx.save()
    print(f"report: {ctx.output}/report.json")
    if any(value["state"] == "failed" for value in ctx.capabilities.values()):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
