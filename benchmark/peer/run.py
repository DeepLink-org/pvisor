#!/usr/bin/env python3
"""Benchmark: B-PEER-NET-ENG (benchmark/README.md#b-peer-net-eng), engineering A/B.

Motivation: prevent excessive same-host direct-packet forwarding overhead.
Conclusion sought: verified goodput, RTT and process CPU paired differences + 95% CI.
Design: frozen binaries; four allowed CPUs; 3 warmup + 30 seeded paired rounds;
fresh members, identical frames, exact integrity/counts, no sample exclusions.
An optional standalone release cohort reports candidate absolute levels only.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import statistics
import subprocess
import time
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def summarize(samples):
    rng = random.Random(7021)
    result = {}
    for metric in ("goodput_mbps", "rtt_median_us", "rtt_p95_us", "cpu_ns_per_frame"):
        a = [sample["baseline"][metric] for sample in samples]
        b = [sample["candidate"][metric] for sample in samples]
        differences = [right - left for left, right in zip(a, b)]
        bootstrap = sorted(
            statistics.median(rng.choices(differences, k=len(differences))) for _ in range(5000)
        )
        interval = [bootstrap[125], bootstrap[4874]]
        result[metric] = {
            "baseline_median": statistics.median(a),
            "candidate_median": statistics.median(b),
            "paired_median_difference": statistics.median(differences),
            "paired_difference_ci95": interval,
            "difference_detected": interval[0] > 0 or interval[1] < 0,
            "baseline_range": [min(a), max(a)],
            "candidate_range": [min(b), max(b)],
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, help="omit for a standalone candidate batch")
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpus", help="comma-separated logical CPUs")
    parser.add_argument("--rounds", type=int, default=30)
    parser.add_argument(
        "--build-profile", choices=("performance", "release"), default="performance"
    )
    args = parser.parse_args()
    if args.rounds < 30:
        parser.error("at least 30 measurement rounds are required")
    allowed = sorted(os.sched_getaffinity(0))
    cpus = [int(cpu) for cpu in args.cpus.split(",")] if args.cpus else allowed[:4]
    if len(cpus) != 4 or len(set(cpus)) != 4 or not set(cpus).issubset(allowed):
        parser.error("choose exactly four allowed logical CPUs")
    os.sched_setaffinity(0, cpus)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parents[2]
    sources = [repo / "Cargo.toml", repo / "Cargo.lock", Path(__file__).resolve()]
    sources += sorted((repo / "crates/pvisor-overlaynet").rglob("*.rs"))
    sources += sorted((repo / "crates/pvisor-core").rglob("*.rs"))
    sources += [
        repo / "crates" / crate / "Cargo.toml" for crate in ("pvisor-overlaynet", "pvisor-core")
    ]
    hashes = {}
    for path in sources:
        relative = path.relative_to(repo)
        destination = output / "source" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
        hashes[str(relative)] = digest(destination)
    binaries = {}
    for label in ("baseline", "candidate"):
        if getattr(args, label) is None:
            continue
        destination = output / label
        shutil.copy2(getattr(args, label), destination)
        binaries[label] = {"path": str(destination), "sha256": digest(destination)}
    rng = random.Random(20261024)
    schedule = []
    for index in range(args.rounds + 3):
        order = list(binaries)
        rng.shuffle(order)
        schedule.append({"index": index, "warmup": index < 3, "order": order})
    save(
        output / "manifest.json",
        {
            "benchmark": "B-PEER-NET-ENG",
            "timestamp": time.time(),
            "kernel": platform.uname()._asdict(),
            "cpus": cpus,
            "cpuinfo": Path("/proc/cpuinfo").read_text(),
            "cgroup": Path("/proc/self/cgroup").read_text(),
            "limits": Path("/proc/self/limits").read_text(),
            "budget": "four logical CPUs affinity; no quota or memory cap",
            "build_profile": args.build_profile,
            "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
            "cargo": subprocess.check_output(["cargo", "-V"], text=True),
            "binaries": binaries,
            "source_sha256": hashes,
            "schedule": schedule,
            "exclusion_rule": "none; every failure invalidates batch; retain slow samples",
        },
    )
    samples = []
    for entry in schedule:
        pair = {}
        for label in entry["order"]:
            prefix = output / f"{entry['index']:02d}-{label}"
            with (
                prefix.with_suffix(".stdout").open("w") as stdout,
                prefix.with_suffix(".stderr").open("w") as stderr,
            ):
                try:
                    completed = subprocess.run(
                        [binaries[label]["path"]],
                        stdout=stdout,
                        stderr=stderr,
                        timeout=60,
                        check=False,
                    )
                    if completed.returncode:
                        raise RuntimeError(f"child exit {completed.returncode}")
                except (subprocess.TimeoutExpired, RuntimeError) as error:
                    save(
                        output / "failure.json",
                        {"entry": entry, "label": label, "error": str(error)},
                    )
                    raise
            try:
                data = json.loads(prefix.with_suffix(".stdout").read_text())
                assert data["benchmark"] == "B-PEER-NET-ENG"
                assert data["drops"] == 0
                assert data["frames"] == 16384 and data["frame_bytes"] == 1514
                assert data["pings"] == 2000 and data["window"] == 8
                assert data["a_sent"] == data["b_received"] == 18512
                assert data["b_sent"] == data["a_received"] == 4176
                assert len(data["rtt_us"]) == 2000
                for metric in ("goodput_mbps", "rtt_median_us", "rtt_p95_us", "cpu_ns_per_frame"):
                    assert 0 < data[metric] < float("inf")
            except (ValueError, KeyError, AssertionError) as error:
                save(
                    output / "failure.json", {"entry": entry, "label": label, "error": repr(error)}
                )
                raise
            pair[label] = data
        if not entry["warmup"]:
            samples.append(pair)
        if entry["index"] % 5 == 0:
            print(f"completed round {entry['index'] + 1}/{len(schedule)}", flush=True)
    save(
        output / "summary.json",
        {
            "benchmark": "B-PEER-NET-ENG",
            "rounds": len(samples),
            "failures": 0,
            "excluded": 0,
            "metrics": summarize(samples)
            if args.baseline
            else {
                metric: {
                    "candidate_median": statistics.median(
                        pair["candidate"][metric] for pair in samples
                    ),
                    "candidate_range": [
                        min(pair["candidate"][metric] for pair in samples),
                        max(pair["candidate"][metric] for pair in samples),
                    ],
                }
                for metric in ("goodput_mbps", "rtt_median_us", "rtt_p95_us", "cpu_ns_per_frame")
            },
        },
    )
    print((output / "summary.json").read_text())


if __name__ == "__main__":
    main()
