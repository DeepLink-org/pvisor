#!/usr/bin/env python3
"""Measure Linux/KVM first-command readiness with prepared rootfs and warm caches."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import random
import shutil
import subprocess
from pathlib import Path

from vm_ready import WORKLOAD, distribution, trial


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "rootfs", "firmware", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--warmups", type=int, default=5)
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("this matrix requires Linux x86_64 / KVM")
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    for name in ("binary", "rootfs", "firmware", "output"):
        setattr(args, name, getattr(args, name).resolve())
    firmware_file = args.firmware / "libkrunfw.so.5"
    if not args.binary.is_file() or not args.rootfs.is_dir() or not firmware_file.is_file():
        parser.error("binary, prepared rootfs and libkrunfw.so.5 must exist")
    args.output.mkdir(parents=True, exist_ok=False, mode=0o700)
    (args.output / "trials").mkdir()
    original_binary = args.binary
    args.binary = args.output / "pvisor"
    shutil.copy2(original_binary, args.binary)
    args.firmware = args.output / "firmware"
    args.firmware.mkdir()
    shutil.copy2(firmware_file, args.firmware / firmware_file.name)
    repo = Path(__file__).resolve().parents[2]
    inputs = [args.binary, args.firmware / firmware_file.name, Path(__file__),
              Path(__file__).with_name("vm_ready.py"), Path(__file__).with_name("startup.py")]
    guest_inputs = [args.rootfs / "bin/sh", args.rootfs / "etc/os-release"]
    inputs.extend(p for p in guest_inputs if p.is_file())
    hashes = {str(p): sha256(p) for p in inputs}
    cases = [("direct", "direct", None, None, None), ("host", "host", None, None, None)]
    cases.extend((f"vm-2cpu-{memory}", "vm", 2, memory, args.firmware)
                 for memory in (128, 256, 2048))
    metadata = dict(
        schema="pvisor-vm-readiness/v1",
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        platform=platform.platform(),
        cpu=next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                 if line.startswith("model name")),
        logical_cpus=os.cpu_count(),
        host_os_release=Path("/etc/os-release").read_text(),
        guest_os_release=(args.rootfs / "etc/os-release").read_text(),
        rootfs=str(args.rootfs), source_binary=str(original_binary),
        source_commit_at_measurement=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
        source_status_at_measurement=subprocess.check_output(["git", "status", "--porcelain"], cwd=repo, text=True),
        protocol=dict(samples=args.samples, warmups=args.warmups, workload=WORKLOAD,
                      seed=20261003, host_cache="warm; no eviction", new_vm_per_trial=True,
                      startup_logging=False, shapes=[[2, 128], [2, 256], [2, 2048]],
                      metric="CLI launch to first guest command output; completion recorded separately"),
        load_before=os.getloadavg(), sha256=hashes,
    )
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rng, rows = random.Random(20261003), []
    with (args.output / "samples.jsonl").open("w") as log:
        for i in range(-args.warmups, args.samples):
            order = cases.copy()
            rng.shuffle(order)
            for case in order:
                row = trial(args, case, i)
                if i >= 0:
                    rows.append(row)
                    log.write(json.dumps(row) + "\n")
                    log.flush()
            if i < 0 or (i + 1) % 10 == 0 or i + 1 == args.samples:
                print(f"round {i + 1}/{args.samples}", flush=True)
    if any(sha256(Path(p)) != digest for p, digest in hashes.items()):
        raise RuntimeError("a measured input changed; results are not published")
    summary = {}
    for name, *_ in cases:
        group = [row for row in rows if row["case"] == name]
        summary[name] = {key: distribution([r[key] for r in group])
                         for key in ("ready_ms", "completion_ms")}
    result = metadata | dict(summary=summary, samples=rows, load_after=os.getloadavg())
    (args.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
