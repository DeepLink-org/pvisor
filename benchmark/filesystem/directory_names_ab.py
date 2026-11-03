#!/usr/bin/env python3
"""B-FS-ENG: paired SDK directory-name inventory cache experiment."""

import argparse
import functools
import hashlib
import json
import os
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def firmware_name():
    return "libkrunfw.5.dylib" if sys.platform == "darwin" else "libkrunfw.so.5"


def host_cpus():
    # macOS has no hard per-process affinity API equivalent to taskset.
    return sorted(os.sched_getaffinity(0))[:2] if sys.platform == "linux" else []


def host_budget(command, unit, cpus):
    if sys.platform == "darwin":
        return command
    return [
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
        ",".join(map(str, cpus)),
    ] + command


def stop_trial(unit):
    if sys.platform == "linux":
        subprocess.run(
            ["systemctl", "--user", "stop", unit],
            check=False,
            timeout=20,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


@functools.cache
def copyup_expected_block():
    block = bytearray(1 << 20)
    state = 79213
    mask = (1 << 64) - 1
    for index in range(len(block)):
        state ^= (state << 13) & mask
        state ^= state >> 7
        state ^= (state << 17) & mask
        block[index] = state & 255
    return bytes(block)


def verify_copyup_fixture(scratch):
    fixtures = [p for p in scratch.iterdir() if (p / "stable-lower/copy-1").is_file()]
    if len(fixtures) != 1:
        raise ValueError("missing or ambiguous copy-up fixture")
    expected = copyup_expected_block()
    for round_ in (1, 2, 3):
        for directory in ("stable-lower", "stable-upper"):
            path = fixtures[0] / directory / f"copy-{round_}"
            if path.stat().st_size != 32 << 20:
                raise ValueError(f"incorrect copied file size: {path}")
            with path.open("rb") as source:
                for _ in range(32):
                    if source.read(len(expected)) != expected:
                        raise ValueError(f"copy-up byte mismatch: {path}")
                if source.read(1):
                    raise ValueError(f"file grew during validation: {path}")


def prepare(args):
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    artifacts = batch / "artifacts"
    artifacts.mkdir()
    inputs = {}
    for name, source in [
        ("driver", args.driver),
        ("guest", args.guest),
        (firmware_name(), args.firmware),
        ("libkrunfw.SOURCE", args.firmware_source),
    ]:
        destination = artifacts / name
        shutil.copy2(source.resolve(), destination)
        inputs[str(destination.relative_to(batch))] = digest(destination)
    receipt = json.loads((artifacts / "libkrunfw.SOURCE").read_text())
    if receipt["firmware_sha256"] != inputs[f"artifacts/{firmware_name()}"]:
        raise ValueError("firmware does not match its SOURCE receipt")
    repo = Path(__file__).resolve().parents[2]
    (batch / "source").mkdir()
    for root in ["crates/pvisor-vm", "crates/pvisor-overlay-core", "crates/pvisor-guest"]:
        for source in (repo / root).rglob("*"):
            if source.is_file():
                destination = batch / "source" / source.relative_to(repo)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
                inputs[str(destination.relative_to(batch))] = digest(destination)
    for name in ["Cargo.toml", "Cargo.lock"]:
        shutil.copy2(repo / name, batch / "source" / name)
        inputs[f"source/{name}"] = digest(batch / "source" / name)
    for path, expected in receipt["inputs"].items():
        if path.startswith("patches/") or path in (
            "Makefile",
            "config-libkrunfw_aarch64" if sys.platform == "darwin" else "config-libkrunfw_x86_64",
        ):
            if digest(repo / "fw" / path) != expected:
                raise ValueError(f"firmware source/config differs: {path}")
    harness = batch / "harness.py"
    shutil.copy2(__file__, harness)
    inputs["harness.py"] = digest(harness)
    (batch / "rootfs").mkdir()
    (batch / "work").mkdir()
    save(
        batch / "manifest.json",
        {
            "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
            "inputs": inputs,
            "samples": args.samples,
            "experiment": args.experiment,
            "mode": {
                "prepared-copy-up": "copyup-concurrent",
                "scoped-names": "scoped-reimport",
            }.get(args.experiment, "directory-pages"),
            "metric": {"prepared-copy-up": "copyup-total", "scoped-names": "names-reimport"}.get(
                args.experiment, "directory-pages"
            ),
            "seed": 52871,
            "cpus": host_cpus(),
            "host_platform": sys.platform,
            "host_budget": "unrestricted" if sys.platform == "darwin" else "cgroup-2G-2cpu-swap0",
            "primary": "three guest rounds summed per fresh VM",
            "controls_pairs": 5,
            "no_interference_filter": True,
        },
    )
    (batch / "source.diff").write_bytes(subprocess.check_output(["git", "diff"]))
    save(
        batch / "in.json",
        {
            "SDK directory names": "$ "
            + shlex.join([sys.executable, str(harness), "--measure", str(batch)])
        },
    )
    print(batch)


def trial(batch, manifest, pair, mode, enabled, diagnostic=False):
    label = f"{mode}-{pair}-{enabled}-{'profile' if diagnostic else 'time'}"
    unit = f"pvisor-fs-names-{os.getpid()}-{label}"
    # VmRuntime exits its isolated process on guest shutdown, bypassing Rust
    # destructors. The outer harness owns fixture validation and cleanup.
    scratch = batch / "work" / label
    scratch.mkdir()
    prepared = manifest["experiment"] == "prepared-copy-up"
    scoped = manifest["experiment"] == "scoped-names"
    names_enabled = 1 if prepared or scoped else enabled
    command = [
        "/usr/bin/env",
        f"TMPDIR={scratch}",
        f"PVISOR_VM_FS_DIRECTORY_NAMES_CACHE={names_enabled}",
        f"PVISOR_VM_FS_PREPARED_COPY_UP={enabled if prepared else 0}",
        f"PVISOR_VM_FS_SCOPED_NAMES={enabled if scoped else 0}",
        "PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE=0",
        "PVISOR_VM_FS_LOGICAL_DATA_CACHE=0",
        "PVISOR_VM_FS_REUSE_LAZY_NODES=0",
        "PVISOR_VM_FS_STREAM_LAZY_READS=0",
        "PVISOR_VM_FS_DIRECT_LAZY_COPY=0",
        f"PVISOR_FS_PROFILE={int(diagnostic)}",
        "PVISOR_VM_GUEST_CACHE=1",
        "PVISOR_VM_FS_WRITEBACK=0",
        "PVISOR_VM_FS_HOST_WRITEBACK=0",
        "PVISOR_VM_FS_LOWER_DATA_CACHE=0",
        "PVISOR_VM_FS_READONLY_FLUSH_ELISION=0",
        "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE=0",
        "PVISOR_VM_CACHE_EXPERIMENT=off",
        str(batch / "artifacts/driver"),
        str(batch / "rootfs"),
        str(batch / "artifacts" / firmware_name()),
        str(batch / "artifacts/guest"),
        mode,
    ]
    command = host_budget(command, unit, manifest["cpus"])
    save(batch / f"{label}.command.json", command)
    started = time.perf_counter_ns()
    try:
        with (batch / f"{label}.log").open("wb") as log:
            result = subprocess.run(
                command,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                timeout=90,
                check=False,
            )
    except BaseException:
        stop_trial(unit)
        raise
    elapsed = (time.perf_counter_ns() - started) / 1e6
    text = (batch / f"{label}.log").read_text()
    if result.returncode != 0 or text.count(f"GUEST_METADATA_PASS {mode}") != 1:
        raise ValueError(f"trial failed: {label}")
    phases = [
        json.loads(line.removeprefix("SDK_PHASE "))
        for line in text.splitlines()
        if line.startswith("SDK_PHASE ")
    ]
    expected = {
        "directory-pages": ["directory-pages"],
        "copyup-concurrent": ["copyup-open", "copyup-metadata", "copyup-total"],
        "scoped-reimport": ["names-reimport", "names-edit-cycle"],
        "bench": ["stat256", "scan300", "open10000"],
    }[mode]
    if sorted((p["phase"], p["round"]) for p in phases) != sorted(
        (name, round_) for name in expected for round_ in (1, 2, 3)
    ):
        raise ValueError(f"incomplete or duplicate phase inventory: {label}")
    if mode == "copyup-concurrent":
        verify_copyup_fixture(scratch)
    record = {
        "pair": pair,
        "mode": mode,
        "enabled": enabled,
        "diagnostic": diagnostic,
        "process_ms": elapsed,
        "phases": phases,
        "totals_ms": {
            name: sum(p["ms"] for p in phases if p["phase"] == name) for name in expected
        },
    }
    if diagnostic:
        registrations = set()
        finals = {}
        for line in text.splitlines():
            if line.startswith("pvisor-fs-profile-start "):
                item = json.loads(line.partition(" ")[2])
                registrations.add((item["pid"], item["component"], item["instance"]))
            if line.startswith("pvisor-fs-profile "):
                item = json.loads(line.partition(" ")[2])
                if item["final_record"]:
                    key = (item["pid"], item["component"], item["instance"])
                    if key in finals:
                        raise ValueError("duplicate final profile")
                    finals[key] = item
        if not registrations or registrations != set(finals):
            raise ValueError("profile instance inventory incomplete")
        overlays = [
            f["measurements"] for f in finals.values() if f["component"] == "virtio-fs-overlay"
        ]
        enabled_values = sorted(m["directory_names_cache_enabled"]["units"] for m in overlays)
        if enabled_values != ([0, 1, 1] if names_enabled else [0, 0, 0]):
            raise ValueError(f"wrong actual cache qualification: {enabled_values}")
        hits = sum(m.get("directory_names_cache_hits", {}).get("units", 0) for m in overlays)
        if not prepared and not scoped and bool(hits) != bool(enabled):
            raise ValueError("cache did not exercise the intended path")
        if scoped:
            active = sorted(m["directory_names_scoped_enabled"]["units"] for m in overlays)
            retained = sum(
                m.get("directory_names_scoped_retained", {}).get("units", 0) for m in overlays
            )
            if active != ([0, 1, 1] if enabled else [0, 0, 0]) or bool(retained) != bool(enabled):
                raise ValueError("scoped invalidation path was not exercised")
        if prepared:
            actual = [m["prepared_copy_up_enabled"]["units"] for m in overlays]
            if actual != [enabled] * 3:
                raise ValueError(f"wrong actual preparation mode: {actual}")
            preparations = sum(
                f["measurements"].get("copy_up_prepare", {}).get("calls", 0)
                for f in finals.values()
                if f["component"] == "overlay-core"
            )
            if bool(preparations) != bool(enabled):
                raise ValueError("copy preparation did not exercise the intended path")
        record["profiles"] = list(finals.values())
    shutil.rmtree(scratch)  # only the successful, verified trial's owned directory
    return record


def measure(batch):
    manifest = json.loads((batch / "manifest.json").read_text())
    for name, expected in manifest["inputs"].items():
        if digest(batch / name) != expected:
            raise ValueError(f"frozen input changed: {name}")
    if (batch / "records.json").exists():
        raise ValueError("refusing to overwrite a previous attempt")
    records = []
    rng = random.Random(manifest["seed"])
    try:
        for enabled in (0, 1):
            records.append(trial(batch, manifest, "diagnostic", manifest["mode"], enabled, True))
            save(batch / "records.json", records)
        for mode, count in [
            (manifest["mode"], manifest["samples"]),
            ("bench", manifest["controls_pairs"]),
        ]:
            for pair in range(count):
                conditions = [0, 1]
                rng.shuffle(conditions)
                for enabled in conditions:
                    records.append(trial(batch, manifest, pair, mode, enabled))
                    save(batch / "records.json", records)
    except BaseException as error:
        save(batch / "status.json", {"valid": False, "error": str(error)})
        raise
    save(batch / "status.json", {"valid": True, "failures": 0, "excluded": 0})
    off = [
        r["totals_ms"][manifest["metric"]]
        for r in records
        if r["mode"] == manifest["mode"] and not r["diagnostic"] and not r["enabled"]
    ]
    on = [
        r["totals_ms"][manifest["metric"]]
        for r in records
        if r["mode"] == manifest["mode"] and not r["diagnostic"] and r["enabled"]
    ]
    print(
        json.dumps(
            {"N pairs": len(off), "Off ms": statistics.median(off), "On ms": statistics.median(on)}
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare", type=Path)
    mode.add_argument("--measure", type=Path)
    for name in ["driver", "guest", "firmware", "firmware-source"]:
        parser.add_argument(f"--{name}", type=Path)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument(
        "--experiment",
        choices=["directory-names", "prepared-copy-up", "scoped-names"],
        default="directory-names",
    )
    args = parser.parse_args()
    if args.prepare:
        if not all([args.driver, args.guest, args.firmware, args.firmware_source]):
            parser.error("prepare requires driver, guest, firmware and firmware-source")
        if args.samples < 1:
            parser.error("samples must be positive")
        prepare(args)
    else:
        measure(args.measure.resolve())


if __name__ == "__main__":
    main()
