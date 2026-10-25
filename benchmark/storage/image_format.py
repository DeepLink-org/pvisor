#!/usr/bin/env python3
"""Benchmark: B-IMAGE-FORMAT-ENG (REGISTRY.md#b-image-format-eng), engineering A/B.
Motivation: quantify framed directory metadata and small-file packing costs.
Conclusion sought: paired storage-only stage differences and complete byte inventories.
Design: identical frozen Rust runner, private source/stores, random paired order,
3 warmups/30 trials, warm host page cache, new decoded caches, exact content/count oracles.
No VM/application startup, cold-disk, S3 or WAN claim is made.
"""

import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import platform
import random
import shutil
import statistics
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RUNTIME_PATH = REPO / "tools/semspec/runtime.py"
SPEC = importlib.util.spec_from_file_location("semspec_runtime", RUNTIME_PATH)
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)
TEST = "image::cache::portable::measurement::image_format_measurement"


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def inventory(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        stat = path.stat()
        category = "data" if path.relative_to(root).parts[0] == "data" else "metadata"
        entry = result.setdefault(category, {"objects": 0, "bytes": 0, "allocated": 0})
        entry["objects"] += 1
        entry["bytes"] += stat.st_size
        entry["allocated"] += stat.st_blocks * 512
    return result


def interval(pairs, key):
    differences = [pair["candidate"][key] - pair["baseline"][key] for pair in pairs]
    generator = random.Random(435)
    resampled = sorted(
        statistics.median(generator.choices(differences, k=len(differences))) for _ in range(5000)
    )
    return {
        "baseline_median_ms": statistics.median(p["baseline"][key] for p in pairs) / 1e6,
        "candidate_median_ms": statistics.median(p["candidate"][key] for p in pairs) / 1e6,
        "paired_difference_median_ms": statistics.median(differences) / 1e6,
        "paired_difference_ci95_ms": [resampled[125] / 1e6, resampled[4874] / 1e6],
    }


def run(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    cpus = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else []
    if args.cpu is not None:
        if args.cpu not in cpus:
            raise ValueError("requested CPU is not available")
        os.sched_setaffinity(0, {args.cpu})
        cpus = [args.cpu]
    binaries = {}
    for name in ("baseline", "candidate"):
        source = getattr(args, name).resolve()
        target = output / name
        shutil.copy2(source, target)
        binaries[name] = target
    runner = REPO / "crates/pvisor/src/image/cache/portable/measurement.rs"
    shutil.copy2(args.baseline_runner, output / "baseline-runner.rs")
    shutil.copy2(runner, output / "candidate-runner.rs")
    normalized = output / "normalized-runner.rs"
    shutil.copy2(args.baseline_runner, normalized)
    subprocess.run(
        ["mise", "exec", "--", "rustfmt", "--edition", "2021", str(normalized)],
        cwd=REPO,
        check=True,
    )
    if normalized.read_bytes() != runner.read_bytes():
        raise ValueError("baseline/candidate measurement runners differ beyond formatting")
    manifest = {
        "benchmark_id": "B-IMAGE-FORMAT-ENG",
        "role": "engineering A/B",
        "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "profile": "release; cargo nextest list --locked -p pvisor --lib --cargo-profile release",
        "kernel": platform.platform(),
        "cpu_affinity": cpus,
        "files": args.files,
        "directories": args.directories,
        "payload": args.payload,
        "backend": args.backend,
        "runs": args.runs,
        "warmup": args.warmup,
        "seed": 437,
        "host_load_before": os.getloadavg(),
        "artifacts": {name: sha(path) for name, path in binaries.items()},
        "harness_sha256": sha(__file__),
        "runtime_sha256": sha(RUNTIME_PATH),
        "source_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
        ).strip(),
        "toolchain": subprocess.check_output(
            ["mise", "exec", "--", "rustc", "--version"], cwd=REPO, text=True
        ).strip(),
        "filesystem": subprocess.check_output(
            ["findmnt", "-no", "FSTYPE,OPTIONS", "--target", str(output)], text=True
        ).strip(),
        "cpu_model": next(
            (
                line.split(":", 1)[1].strip()
                for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")
            ),
            "unknown",
        ),
        "runner_sources": {
            "baseline": sha(args.baseline_runner),
            "candidate": sha(runner),
            "rustfmt_equivalent": True,
        },
        "cache_state": "new reader process; warm host page cache; no persistent client mirror",
        "status": "preparing",
    }
    RUNTIME.save(output / "manifest.json", manifest)
    shutil.copy2(__file__, output / "harness.py")
    shutil.copy2(RUNTIME_PATH, output / "runtime.py")
    (output / "candidate.patch").write_bytes(
        subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=REPO)
    )
    publisher = output / "publisher"
    source = publisher / "rootfs-v3/sha256" / ("a" * 64)
    source.mkdir(parents=True)
    paths = []
    directories = []
    for index in range(args.directories):
        directory = source / f"d-{index:05}"
        directory.mkdir()
        directories.append(directory)
    for index in range(args.files):
        directory = directories[index % len(directories)]
        path = directory / f"f-{index:07}.py"
        payload = (
            hashlib.sha256(str(index).encode()).digest() * 32
            if args.payload == "repeat"
            else hashlib.shake_256(str(index).encode()).digest(1024)
        )
        path.write_bytes(payload)
        os.utime(path, (1700000000, 1700000000))
        paths.append(path.relative_to(source).as_posix().encode())
    for path in [source, *directories]:
        os.utime(path, (1700000000, 1700000000))
    selected = random.Random(439).sample(paths, min(len(paths), 1000))
    handles = {}

    def command(name, action, label):
        trial = output / label
        trial.mkdir()
        plan = {
            "action": action,
            "backend": args.backend,
            "publisher": str(publisher),
            "source": str(source),
            "cache": str(output / f"cache-{name}"),
            "output": str(trial / "result.json"),
            "paths": [list(p) for p in selected],
            "entries": args.files + args.directories,
            "handle": handles.get(name),
        }
        RUNTIME.save(trial / "plan.json", plan)
        config = {
            "argv": [str(binaries[name]), "--exact", TEST, "--ignored", "--nocapture"],
            "cwd": str(REPO),
            "timeout": 1800,
            "env": {"PVISOR_IMAGE_MEASUREMENT": str(trial / "plan.json")},
        }
        RUNTIME.command_trial(trial / "command.json", name, action, 0, config)
        return json.loads((trial / "result.json").read_text())

    try:
        publication = {}
        for name in random.Random(431).sample(list(binaries), 2):
            publication[name] = command(name, "publish", f"publish-{name}")
            handles[name] = publication[name]["handle"]
        RUNTIME.save(output / "publication.json", publication)
        RUNTIME.save(
            output / "inventory.json",
            {name: inventory(output / f"cache-{name}") for name in binaries},
        )
        for name in binaries:
            root = output / f"cache-{name}"
            RUNTIME.save(
                output / f"objects-{name}.json",
                [
                    {
                        "key": str(path.relative_to(root)),
                        "bytes": path.stat().st_size,
                        "allocated": path.stat().st_blocks * 512,
                        "sha256": sha(path),
                    }
                    for path in sorted(root.rglob("*"))
                    if path.is_file()
                ],
            )
        manifest["status"] = "measuring"
        RUNTIME.save(output / "manifest.json", manifest)
        generator = random.Random(437)

        def pair(phase, index):
            results = {}
            for name in generator.sample(list(binaries), 2):
                results[name] = command(name, "read", f"{phase}-{index:03}-{name}")
            return results

        pairs = RUNTIME.repeat(args.warmup, args.runs, pair)
        RUNTIME.save(output / "pairs.json", pairs)
        metrics = (
            "open_ns",
            "cold_stat_ns",
            "warm_stat_ns",
            "cold_read_ns",
            "warm_read_ns",
            "scan_ns",
        )
        summary = {key: interval(pairs, key) for key in metrics}
        RUNTIME.save(output / "summary.json", summary)
        manifest.update(status="complete", host_load_after=os.getloadavg())
        RUNTIME.save(output / "manifest.json", manifest)
        print(json.dumps({"output": str(output), "summary": summary}, indent=2))
    except BaseException as error:
        manifest.update(status="failed", error=str(error), host_load_after=os.getloadavg())
        RUNTIME.save(output / "manifest.json", manifest)
        raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--baseline-runner", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--files", type=int, default=10000)
    parser.add_argument("--directories", type=int, default=1)
    parser.add_argument("--backend", choices=("pool", "filesystem"), default="pool")
    parser.add_argument("--payload", choices=("repeat", "entropy"), default="repeat")
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--cpu", type=int)
    args = parser.parse_args()
    if args.files < 1 or not 1 <= args.directories <= args.files:
        parser.error("require 1 <= directories <= files")
    if args.runs < 1 or args.warmup < 0:
        parser.error("require runs >= 1 and warmup >= 0")
    run(args)
