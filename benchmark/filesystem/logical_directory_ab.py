#!/usr/bin/env python3
"""B-FS-ENG: fixed V2 logical metadata through the actual Docker-style CLI."""

import argparse
import importlib.util
import json
import os
import random
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "directory_names_ab", Path(__file__).with_name("directory_names_ab.py")
)
BASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BASE)


def environment(batch, enabled, diagnostic=False, experiment="logical-directory"):
    logical = enabled if experiment == "logical-directory" else 1
    return [
        "/usr/bin/env",
        "-i",
        "PATH=/usr/bin:/bin",
        f"HOME={os.environ['HOME']}",
        f"TMPDIR={batch / 'work'}",
        f"XDG_CACHE_HOME={batch / 'cache'}",
        f"XDG_DATA_HOME={batch / 'data'}",
        f"PVISOR_DOCKER_HOME={batch / 'store'}",
        f"PVISOR_IMAGE_STORE={batch / 'images'}",
        f"PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE={logical}",
        f"PVISOR_VM_FS_REUSE_LAZY_NODES={enabled if experiment == 'lazy-nodes' else 0}",
        f"PVISOR_VM_FS_STREAM_LAZY_READS={enabled if experiment == 'read-sink' else 0}",
        f"PVISOR_VM_FS_LOGICAL_DATA_CACHE={enabled if experiment == 'logical-data' else 0}",
        f"PVISOR_VM_FS_DIRECT_LAZY_COPY={enabled if experiment == 'direct-copy' else 0}",
        f"PVISOR_FS_PROFILE={int(diagnostic)}",
        "PVISOR_VM_FS_DIRECTORY_NAMES_CACHE=1",
        "PVISOR_VM_FS_PREPARED_COPY_UP=1",
        "PVISOR_VM_GUEST_CACHE=1",
        "PVISOR_VM_CACHE_EXPERIMENT=off",
        "PVISOR_VM_FS_WRITEBACK=0",
        "PVISOR_VM_FS_HOST_WRITEBACK=0",
        "PVISOR_VM_FS_LOWER_DATA_CACHE=0",
        "PVISOR_VM_FS_READONLY_FLUSH_ELISION=0",
        "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE=0",
    ]


def prepare(args):
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False, mode=0o700)
    for name in ["bin", "work", "context", "cache", "data"]:
        (batch / name).mkdir(mode=0o700)
    for source, name in [
        (args.cli, "bin/pvisor"),
        (args.guest, "context/probe"),
        (args.firmware, "bin/libkrunfw.so.5"),
        (args.firmware_source, "bin/libkrunfw.SOURCE"),
    ]:
        shutil.copy2(source, batch / name)
    receipt = json.loads((batch / "bin/libkrunfw.SOURCE").read_text())
    if BASE.digest(batch / "bin/libkrunfw.so.5") != receipt["firmware_sha256"]:
        raise ValueError("firmware receipt mismatch")
    for directory in range(32):
        path = batch / "context/fixture" / f"d{directory:02}"
        if args.experiment == "lazy-nodes":
            path = path / "a/b/c/d/e/f/g"
        path.mkdir(parents=True)
        for index in range(64):
            (path / f"f{index:03}").write_bytes((directory * 64 + index).to_bytes(8, "little"))
    if args.experiment == "read-sink":
        for index in range(8):
            block = bytearray(BASE.copyup_expected_block())
            block[:8] = index.to_bytes(8, "little")
            with (batch / "context/fixture" / f"stream-{index}").open("wb") as file:
                for _ in range(4):
                    file.write(block)
    if args.experiment == "logical-data":
        for index in range(256):
            block = bytearray(BASE.copyup_expected_block()[:65536])
            block[:8] = index.to_bytes(8, "little")
            (batch / "context/fixture" / f"data-{index}").write_bytes(block)
    if args.experiment == "direct-copy":
        for index in range(1, 4):
            block = bytearray(BASE.copyup_expected_block())
            block[:8] = index.to_bytes(8, "little")
            with (batch / "context/fixture" / f"copy-{index}").open("wb") as file:
                for _ in range(8):
                    file.write(block)
    mode = "lazy-deep" if args.experiment == "lazy-nodes" else "logical-root"
    if args.experiment == "read-sink":
        mode = "read-stream"
    if args.experiment == "logical-data":
        mode = "logical-data"
    if args.experiment == "direct-copy":
        mode = "direct-copy"
    (batch / "context/Dockerfile").write_text(
        "FROM scratch\nCOPY fixture /fixture\nCOPY probe /probe\n" + f'CMD ["/probe", "{mode}"]\n'
    )
    command = environment(batch, 0, experiment=args.experiment) + [
        str(batch / "bin/pvisor"),
        "build",
        "-q",
        "-t",
        "logical-fixture:local",
        str(batch / "context"),
    ]
    BASE.save(batch / "build.command.json", command)
    with (batch / "build.log").open("wb") as log:
        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120)
    if args.experiment == "logical-data":
        review = environment(batch, 1, experiment=args.experiment) + [
            str(batch / "bin/pvisor"),
            "run",
            "--rm",
            "--network",
            "none",
            "logical-fixture:local",
            "/probe",
            "logical-data-review",
        ]
        BASE.save(batch / "review.command.json", review)
        with (batch / "review.log").open("wb") as log:
            subprocess.run(review, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=90)
    repo = Path(__file__).resolve().parents[2]
    inputs = {}
    for root in [
        "crates/pvisor-vm",
        "crates/pvisor-overlay-core",
        "crates/pvisor-guest",
        "crates/pvisor",
        "crates/pvisor-cli",
    ]:
        for source in (repo / root).rglob("*"):
            if source.is_file():
                target = batch / "source" / source.relative_to(repo)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
                inputs[str(target.relative_to(batch))] = BASE.digest(target)
    for name in ["logical_directory_ab.py", "directory_names_ab.py"]:
        shutil.copy2(Path(__file__).with_name(name), batch / name)
    for root in ["bin", "context"]:
        for path in (batch / root).rglob("*"):
            if path.is_file():
                inputs[str(path.relative_to(batch))] = BASE.digest(path)
    for name in ["logical_directory_ab.py", "directory_names_ab.py"]:
        inputs[name] = BASE.digest(batch / name)
    BASE.save(
        batch / "manifest.json",
        {
            "inputs": inputs,
            "samples": args.samples,
            "controls_pairs": 0,
            "cpus": sorted(os.sched_getaffinity(0))[:2],
            "seed": 98171,
            "experiment": args.experiment,
            "mode": mode,
            "metric": {
                "read-sink": "stream-reopen",
                "logical-data": "data-reopen",
                "direct-copy": "copy-completed",
            }.get(args.experiment, "logical-stat"),
            "no_interference_filter": True,
        },
    )
    (batch / "source.diff").write_bytes(subprocess.check_output(["git", "diff"]))
    BASE.save(
        batch / "in.json",
        {
            "Docker logical directory": "$ "
            + shlex.join(
                [sys.executable, str(batch / "logical_directory_ab.py"), "--measure", str(batch)]
            ),
        },
    )
    print(batch)


def profile_inventory(lines):
    starts, finals = set(), {}
    for line in lines:
        if line.startswith("pvisor-fs-profile-start "):
            p = json.loads(line.partition(" ")[2])
            starts.add((p["pid"], p["component"], p["instance"]))
        if line.startswith("pvisor-fs-profile "):
            p = json.loads(line.partition(" ")[2])
            if p["final_record"]:
                key = (p["pid"], p["component"], p["instance"])
                if key in finals:
                    raise ValueError("duplicate final profile")
                finals[key] = p
    if not starts or starts != set(finals):
        raise ValueError("CLI profile inventory incomplete")
    return list(finals.values())


def trial(batch, manifest, pair, enabled, diagnostic=False):
    label = f"{pair}-{enabled}-{'profile' if diagnostic else 'time'}"
    unit = f"pvisor-logical-{os.getpid()}-{label}"
    before_logs = set((batch / "store").rglob("*.log"))
    command = (
        [
            "systemd-run",
            "--user",
            "--wait",
            "--pipe",
            "--collect",
            "--quiet",
            f"--unit={unit}",
            "-p",
            "MemoryMax=2G",
            "-p",
            "MemorySwapMax=0",
            "-p",
            "CPUQuota=200%",
            "taskset",
            "-c",
            ",".join(map(str, manifest["cpus"])),
        ]
        + environment(batch, enabled, diagnostic, manifest["experiment"])
        + [
            str(batch / "bin/pvisor"),
            "run",
        ]
        + (["--name", f"logical-profile-{enabled}"] if diagnostic else ["--rm"])
        + [
            "--network",
            "none",
            "--cpus",
            "2",
            "--memory",
            "512m",
            "logical-fixture:local",
        ]
    )
    BASE.save(batch / f"{label}.command.json", command)
    started = time.perf_counter_ns()
    try:
        with (batch / f"{label}.log").open("wb") as log:
            result = subprocess.run(
                command, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, timeout=90
            )
    except BaseException:
        subprocess.run(["systemctl", "--user", "stop", unit], check=False, timeout=20)
        raise
    elapsed = (time.perf_counter_ns() - started) / 1e6
    text = (batch / f"{label}.log").read_text()
    if result.returncode or text.count(f"GUEST_METADATA_PASS {manifest['mode']}") != 1:
        raise ValueError(f"CLI trial failed: {label}")
    phases = [
        json.loads(line.removeprefix("SDK_PHASE "))
        for line in text.splitlines()
        if line.startswith("SDK_PHASE ")
    ]
    names = {
        "read-sink": ("stream-first", "stream-reopen"),
        "logical-data": ("data-first", "data-reopen"),
        "direct-copy": ("copy-open", "copy-completed"),
    }.get(manifest["experiment"], ("logical-stat", "logical-read"))
    if sorted((p["phase"], p["round"]) for p in phases) != sorted(
        (name, round_) for name in names for round_ in (1, 2, 3)
    ):
        raise ValueError("phase inventory incomplete")
    record = {
        "pair": pair,
        "enabled": enabled,
        "diagnostic": diagnostic,
        "mode": manifest["mode"],
        "phases": phases,
        "process_ms": elapsed,
        "totals_ms": {name: sum(p["ms"] for p in phases if p["phase"] == name) for name in names},
    }
    if diagnostic:
        lines = []
        for log in (batch / "store").rglob("*"):
            if not log.is_file() or log.suffix != ".log" or log in before_logs:
                continue
            saved = batch / "diagnostics" / label / log.relative_to(batch / "store")
            saved.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(log, saved)
            lines.extend(log.read_text(errors="replace").splitlines())
        # Use one authoritative stream, not a dictionary which silently hides
        # duplicate finals copied between diagnostic logs and CLI output.
        if not any(line.startswith("pvisor-fs-profile-start ") for line in lines):
            lines = text.splitlines()
        profiles = profile_inventory(lines)
        overlays = [p for p in profiles if p["component"] == "virtio-fs-overlay"]
        if len(overlays) != 1 or overlays[0]["measurements"]["logical_directory_cache_enabled"][
            "units"
        ] != (enabled if manifest["experiment"] == "logical-directory" else 1):
            raise ValueError("logical cache qualification differs from request")
        if manifest["experiment"] == "logical-data":
            measurements = overlays[0]["measurements"]
            if measurements["logical_data_cache_enabled"]["units"] != enabled:
                raise ValueError("logical data qualification differs from request")
            if bool(measurements["lower_data_cache_open_flags"]["units"]) != bool(enabled):
                raise ValueError("logical data receipt path was not exercised")
        if manifest["experiment"] == "direct-copy":
            copied = sum(
                p["measurements"].get("copy_up_direct_bytes", {}).get("units", 0)
                for p in profiles
                if p["component"] == "overlay-core"
            )
            if copied != ((24 << 20) if enabled else 0):
                raise ValueError("direct-copy byte accounting differs from workload")
        record["profiles"] = profiles
        subprocess.run(
            environment(batch, enabled, experiment=manifest["experiment"])
            + [
                str(batch / "bin/pvisor"),
                "rm",
                f"logical-profile-{enabled}",
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            timeout=30,
        )
    return record


def measure(batch):
    manifest = json.loads((batch / "manifest.json").read_text())
    for name, expected in manifest["inputs"].items():
        if BASE.digest(batch / name) != expected:
            raise ValueError(f"frozen input changed: {name}")
    if (batch / "records.json").exists():
        raise ValueError("refusing to reuse a batch")
    records = []
    rng = random.Random(manifest["seed"])
    try:
        for enabled in (0, 1):
            records.append(trial(batch, manifest, "diagnostic", enabled, True))
            BASE.save(batch / "records.json", records)
        for pair in range(manifest["samples"]):
            conditions = [0, 1]
            rng.shuffle(conditions)
            for enabled in conditions:
                records.append(trial(batch, manifest, pair, enabled))
                BASE.save(batch / "records.json", records)
    except BaseException as error:
        BASE.save(batch / "status.json", {"valid": False, "error": str(error)})
        raise
    BASE.save(batch / "status.json", {"valid": True, "failures": 0, "excluded": 0})
    print(json.dumps({"N pairs": manifest["samples"]}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare", type=Path)
    mode.add_argument("--measure", type=Path)
    for name in ["cli", "guest", "firmware", "firmware-source"]:
        parser.add_argument(f"--{name}", type=Path)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument(
        "--experiment",
        choices=["logical-directory", "lazy-nodes", "read-sink", "logical-data", "direct-copy"],
        default="logical-directory",
    )
    args = parser.parse_args()
    if args.prepare:
        if not all([args.cli, args.guest, args.firmware, args.firmware_source]) or args.samples < 1:
            parser.error("prepare requires artifacts and positive samples")
        prepare(args)
    else:
        measure(args.measure.resolve())


if __name__ == "__main__":
    main()
