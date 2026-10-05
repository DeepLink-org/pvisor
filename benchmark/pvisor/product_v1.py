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
    density,
    filesystem,
    isolation,
    network,
    oci,
    replay,
    supervision,
)
from v1.common import Context


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/release/pvisor"))
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--replay-binary", type=Path, default=Path("target/release/pvisor-replay"))
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--oci-shape", choices=["tools", "shell"], default="tools")
    parser.add_argument("--density-concurrencies", default="1,8,32,128")
    parser.add_argument("--network-backends", default="native,host,vm,podman,container")
    parser.add_argument("--network-modes", default="small,bulk,stream,deny")
    parser.add_argument("--density-backends", default="native,host,staged,safe,vm,podman,container")
    parser.add_argument("--apply-sizes", default="10,1000,100000")
    parser.add_argument(
        "--filesystem-backends", default="native,host,staged,safe,vm,podman,container"
    )
    parser.add_argument("--filesystem-modes", default="metadata,read,write,git,rg,cargo,npm")
    parser.add_argument("--vm-memory", default="1GiB")
    parser.add_argument("--crash-files", type=int, default=10000)
    parser.add_argument("--crash-states", default="prepared,target_applied,committed")
    parser.add_argument("--suites", default="filesystem")
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    ctx = Context(args)
    (oci.prepare_shell if args.oci_shape == "shell" else oci.prepare)(ctx)
    suites = {
        "filesystem": filesystem.run,
        "network": network.run,
        "apply": apply.run,
        "density": density.run,
        "agent": agent.run,
        "isolation": isolation.run,
        "replay": replay.run,
        "supervision": supervision.run,
        "baselines": baselines.run,
        "crashes": apply.crashes,
    }
    for name in args.suites.split(","):
        if name not in suites:
            parser.error(f"unknown suite: {name}")
        try:
            suites[name](ctx)
        except Exception as error:
            import traceback

            ctx.capabilities["suite/" + name] = {"state": "failed", "reason": str(error)}
            (ctx.output / (name + "-failure.txt")).write_text(traceback.format_exc())
            ctx.save()
            print(f"suite {name} failed: {error}", flush=True)
    ctx.save()
    print(f"report: {ctx.output}/report.json")
    if any(value["state"] == "failed" for value in ctx.capabilities.values()):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
