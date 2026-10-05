#!/usr/bin/env python3
"""Compare strict/checkpoint stage durability using one pinned binary.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: quantify what per-mutation persistence costs users, so the
default durability policy is chosen on evidence.
Conclusion sought: per-workload and whole-task difference between strict and
checkpoint, including the completion seal cost, with confidence intervals.
Design: one binary, fresh stage per job, modes shuffled per round, >=30
samples; completion includes sealing; correctness gates unchanged.
"""
import argparse
import csv
import json
import os
import random
import shutil
from pathlib import Path
from types import SimpleNamespace

from bench import percentile
from reference_baselines import digest, run_trial

WORKLOADS = ("metadata", "read", "write", "git", "rg", "cargo", "npm")


def validate_completion(stage, durability):
    journal = stage / "preimages"
    if (journal / "durability-v1").read_bytes() != f"pvisor.stage.{durability}/1\n".encode():
        raise ValueError("wrong stage durability policy")
    if (journal / "sealed-v1").read_bytes() != b"pvisor.stage.sealed/1\n":
        raise ValueError("job exited without durable stage completion")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--backend", choices=("pvisor-staged", "pvisor-vm"), default="pvisor-staged")
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--cpu-affinity", default="0,1")
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    assets = args.assets.resolve()
    firmware = args.output / "firmware"
    shutil.copytree(args.firmware, firmware, symlinks=True)
    source = args.output / "binary"
    shutil.copy2(args.binary, source)
    os.environ["PVISOR_FS_PROFILE"] = "0"
    metadata = {"assets": json.loads((assets / "assets.json").read_text())}
    cells = ("native", "strict", "checkpoint")
    configurations = {}
    for cell in cells:
        root = args.output / cell
        (root / "bin").mkdir(parents=True)
        shutil.copy2(source, root / "bin/pvisor")
        configurations[cell] = SimpleNamespace(
            assets=assets, output=root, firmware=firmware, cpu_affinity=args.cpu_affinity,
            memory_mib=16384, staged_isolation="rootless_process", docker_root_pid=None,
            stage_durability=None if cell == "native" else cell,
        )
    rows = []
    rng = random.Random(20261005)
    for index in range(args.warmups + args.samples):
        order = list(cells)
        rng.shuffle(order)
        for cell in order:
            row = run_trial(configurations[cell], metadata,
                            "native" if cell == "native" else args.backend, "filesystem", index)
            if cell != "native":
                validate_completion(Path(row["logs"]) / "stage", cell)
            row["durability"] = cell
            if index >= args.warmups:
                rows.append(row)
                with (args.output / "samples.jsonl").open("a") as file:
                    file.write(json.dumps(row) + "\n")
            print(json.dumps({"round": index, "durability": cell,
                              "completion_ms": row["completion_ms"]}), flush=True)
    summary = {}
    for cell in cells:
        selected = [row for row in rows if row["durability"] == cell]
        assert len(selected) == args.samples
        values = {mode: [row["result"]["filesystem"][mode]["worker_ms"] for row in selected]
                  for mode in WORKLOADS}
        values["completion_ms"] = [row["completion_ms"] for row in selected]
        summary[cell] = {mode: {f"p{q}": percentile(timings, q) for q in (50, 95, 99)}
                         for mode, timings in values.items()}
    report = {
        "schema": "pvisor-stage-durability/v1", "binary_sha256": digest(source),
        "workload_sha256": digest(assets / "rootfs/bench/reference_workload.py"),
        "arguments": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "protocol": "same binary; fresh stage/workspace; shuffled modes; warm page cache; completion includes sealing",
        "summary": summary, "samples": rows,
    }
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    with (args.output / "summary.tsv").open("w") as file:
        writer = csv.writer(file, delimiter="\t")
        writer.writerow(("durability", "operation", "p50_ms", "p95_ms", "p99_ms"))
        for cell, operations in summary.items():
            for mode, metrics in operations.items():
                writer.writerow((cell, mode, *(metrics[f"p{q}"] for q in (50, 95, 99))))


if __name__ == "__main__":
    main()
