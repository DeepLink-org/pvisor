#!/usr/bin/env python3
"""Benchmark: B-FS-ENG/B-FS-DIAG, ordinary CLI guest-writeback off/on verification.

Preparation copies the audited AF private-Host worker and its unchanged workload
and correctness helpers into a NEW batch. It reuses prepared input generations,
never historical timing. The current CLI build receipt supplies the binaries and
retained product source. No command here builds a product or changes a listener.

  guest_writeback_cli.py --prepare NEW_BATCH --setup OWNED_BUDGET_SETUP
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-pre/config.json --check
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-pre/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-profiles/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-timing/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-timing/config.json --analyze

Execute each campaign's in.json through actual semspec bench inside the setup's
delegated slice, with CPU affinity 0,1 and
RLIMIT_NOFILE=65536. Only --run launches processes/VMs. Ready includes the fresh
private Host and VM, excludes exit. Worker clocks retain the original workload's
fsync/close semantics; they are not the microcurve accepted/applied boundaries.
"""

import argparse
import hashlib
import importlib.util
import json
import math
import os
import random
import shutil
import statistics
import subprocess
import sys
import threading
import traceback
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE = Path(__file__).resolve().parent
DESIGN = "ordinary-cli-guest-writeback-anchor"
ARMS = ("off", "on")
FACTORS = dict(off=0, on=1)
HOST_ENV = "PVISOR_VM_FS_HOST_WRITEBACK"
WRITEBACK_ENV = "PVISOR_VM_FS_WRITEBACK"
FLUSH_ENV = "PVISOR_VM_FS_READONLY_FLUSH_ELISION"
FIXED = {
    "PVISOR_VM_GUEST_CACHE": "1",
    HOST_ENV: "0",
    "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY": "256",
    "PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE": "0",
    "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE": "0",
    "PVISOR_VM_FS_LOWER_DATA_CACHE": "0",
    "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE": "0",
    "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION": "0",
    FLUSH_ENV: "0",
    "PVISOR_FS_PROFILE": "0",
    "PVISOR_STARTUP_TIMING": "0",
    "PVISOR_STARTUP_PROFILE": "0",
    "RUST_LOG": "off",
}
OPERATIONS = ("metadata", "read", "write", "git", "rg", "cargo", "npm", "hot_open", "stream_write")
HISTOGRAM = {
    "overlay-core": 3,
    "preimage-log": 2,
    "preimage-log-read": 2,
    "virtio-fs-overlay": 2,
    "virtio-fs-protocol": 2,
    "virtio-fs-dispatch": 2,
}


def require(value, message):
    if not value:
        raise ValueError(message)


def load(path):
    return json.loads(Path(path).read_text())


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def record(path):
    return dict(path=str(Path(path).resolve()), sha256=sha(path))


def environment(condition, diagnostic=False):
    require(condition in ARMS, "unknown guest-writeback condition")
    return {
        **FIXED,
        WRITEBACK_ENV: str(FACTORS[condition]),
        "PVISOR_FS_PROFILE": "1" if diagnostic else "0",
        "PVISOR_STARTUP_TIMING": "1" if diagnostic else "0",
    }


def validate_guest_writeback_state(measurements, condition):
    require(condition in FACTORS, "unknown actual writeback condition")
    actual = {}
    for label in ("guest_writeback_enabled", "guest_batched_writeback_enabled"):
        value = measurements.get(label, {}).get("units")
        require(
            type(value) is int and value == FACTORS[condition],
            "explicit actual guest writeback differs: " + label,
        )
        actual[label] = value
    host = measurements.get("host_writeback_enabled", {}).get("units")
    require(host is None or (type(host) is int and host == 0), "host buffer must remain off")
    return dict(requested=FACTORS[condition], actual=actual)


def firmware_mapping_entry(line, expected, identity, *, mountinfo=None, mapped_sha256=None):
    parts = line.split(None, 5)
    if len(parts) != 6 or parts[5] != str(expected):
        return None
    major, minor = (int(value, 16) for value in parts[3].split(":"))
    mapped_device = os.makedev(major, minor)
    require(int(parts[4]) == identity.st_ino, "loaded firmware mapping inode differs")
    backing_mount = None
    if mapped_device != identity.st_dev:
        # Btrfs stat reports the subvolume's anonymous device, whereas maps uses
        # the superblock device. Match that device to the actual process mount,
        # and bind the file bytes through /proc/PID/root instead of equating them.
        mounts = []
        for entry in (mountinfo or "").splitlines():
            fields = entry.split()
            if len(fields) < 10 or "-" not in fields:
                continue
            separator = fields.index("-")
            path = Path(fields[4].replace("\\040", " ").replace("\\134", "\\"))
            if expected == path or path in expected.parents:
                mounts.append((len(path.parts), fields, separator, entry))
        require(bool(mounts), "mapping device mismatch without process mount proof")
        _, fields, separator, backing_mount = max(mounts, key=lambda item: item[0])
        mount_major, mount_minor = map(int, fields[2].split(":"))
        require(
            fields[separator + 1] == "btrfs"
            and os.makedev(mount_major, mount_minor) == mapped_device
            and isinstance(mapped_sha256, str)
            and len(mapped_sha256) == 64,
            "mapping device mismatch is not a byte-bound Btrfs subvolume",
        )
    return dict(
        path=str(expected),
        device=identity.st_dev,
        inode=identity.st_ino,
        mapping_device=mapped_device,
        maps_line=line,
        btrfs_mountinfo=backing_mount,
        mapped_sha256=mapped_sha256,
    )


def source_audit(config, destination):
    """One retained-source check before collection and one after, never per VM."""
    receipt = load(config["build_receipt"]["path"])
    root = Path(receipt["source_root"])
    hashes = receipt["source_hashes"]
    require(
        hashes and all(sha(root / name) == expected for name, expected in hashes.items()),
        "retained current CLI build source changed",
    )
    result = dict(
        state="verified",
        checked_at=datetime.now(timezone.utc).isoformat(),
        build_receipt=config["build_receipt"],
        files=len(hashes),
        source_root=str(root),
        source_hashes_sha256=hashlib.sha256(
            json.dumps(hashes, sort_keys=True).encode()
        ).hexdigest(),
        scope="retained current build input, no filesystem-input inventory gate",
    )
    save(destination, result)
    return result


def build_cli(args):
    """Build the actual GNU frontends from a newly retained current source set."""
    output = args.build.resolve()
    require(
        ".data" in output.parts and not output.exists(), "fresh private build directory required"
    )
    output.relative_to(REPO)
    output.mkdir(parents=True)
    source = output / "source"
    source.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(REPO / name, source / name)
    for name in ("crates", "vendor/fuser", ".cargo"):
        if (REPO / name).exists():
            shutil.copytree(
                REPO / name,
                source / name,
                ignore=shutil.ignore_patterns("target", ".data", "__pycache__"),
            )
    hashes = {
        str(path.relative_to(source)): sha(path) for path in source.rglob("*") if path.is_file()
    }
    baseline = load(args.source_baseline)
    source_manifest = dict(
        schema=1,
        source_root=str(source),
        files=hashes,
        head=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
        baseline=record(args.source_baseline),
        differences_from_sdk={
            name: dict(previous=baseline["source_hashes"].get(name), current=hashes.get(name))
            for name in sorted(hashes.keys() | baseline["source_hashes"].keys())
            if hashes.get(name) != baseline["source_hashes"].get(name)
        },
        scope="new current CLI build inputs; SDK protocol supplies source identity only, never its executable or timing",
    )
    save(output / "build-source-manifest.json", source_manifest)
    require(
        all(sha(REPO / name) == expected for name, expected in hashes.items()),
        "current source changed while freezing",
    )
    command = [
        "cargo",
        "build",
        "--locked",
        "--offline",
        "--profile",
        "performance",
        "-p",
        "pvisor-cli",
        "--bin",
        "pvisor",
        "--bin",
        "pvisor-cache",
        "--message-format=json-render-diagnostics",
    ]
    env = os.environ.copy()
    env.pop("CARGO_BUILD_TARGET", None)
    env.update(
        CARGO_NET_OFFLINE="true", CARGO_BUILD_JOBS="4", CARGO_TARGET_DIR=str(REPO / "target")
    )
    save(
        output / "build-start.json",
        dict(
            command=command,
            started_at=datetime.now(timezone.utc).isoformat(),
            source_manifest=record(output / "build-source-manifest.json"),
            environment={
                key: env.get(key)
                for key in (
                    "CARGO_NET_OFFLINE",
                    "CARGO_BUILD_JOBS",
                    "CARGO_TARGET_DIR",
                    "RUSTFLAGS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "PVISOR_KRUNFW_PATH",
                    "PVISOR_KRUNFW_KERNEL_BUNDLE",
                )
            },
        ),
    )
    with (
        (output / "cargo-messages.jsonl").open("x") as messages,
        (output / "build.log").open("x") as log,
    ):
        completed = subprocess.run(command, cwd=REPO, env=env, stdout=messages, stderr=log)
    require(completed.returncode == 0, "CLI build failed; complete source/logs retained")
    messages = [
        json.loads(line)
        for line in (output / "cargo-messages.jsonl").read_text().splitlines()
        if line.startswith("{")
    ]
    (output / "bin").mkdir()
    artifacts, compiler_artifacts = {}, {}
    for name in ("pvisor", "pvisor-cache"):
        selected = [
            row
            for row in messages
            if row.get("reason") == "compiler-artifact"
            and row.get("target", {}).get("name") == name
            and "bin" in row["target"]["kind"]
            and "pvisor-cli" in row.get("package_id", "")
            and row.get("executable")
        ]
        require(
            len(selected) == 1, "exactly one actual frontend compiler artifact required: " + name
        )
        binary = Path(selected[0]["executable"])
        require(
            binary == REPO / "target/performance" / name and binary.read_bytes()[:4] == b"\x7fELF",
            "GNU performance artifact path/ELF differs: " + name,
        )
        shutil.copy2(binary, output / "bin" / name)
        artifacts[name], compiler_artifacts[name] = record(output / "bin" / name), selected[0]
    scripts = [
        row
        for row in messages
        if row.get("reason") == "build-script-executed" and "pvisor-vm" in row.get("package_id", "")
    ]
    guests = {
        Path(value)
        for row in scripts
        for key, value in row.get("env", [])
        if key == "PVISOR_GUEST_BINARY"
    }
    require(len(guests) == 1, "actual VM build-script embedded guest association required")
    guest = guests.pop()
    require(guest.read_bytes()[:4] == b"\x7fELF", "embedded guest is not ELF")
    shutil.copy2(guest, output / "bin/pvisor-guest")
    artifacts["embedded-guest"] = record(output / "bin/pvisor-guest")
    require(
        all(
            sha(REPO / name) == expected and sha(source / name) == expected
            for name, expected in hashes.items()
        ),
        "source changed during CLI build; retain attempt",
    )
    receipt = dict(
        schema=1,
        state="built",
        head=source_manifest["head"],
        source_root=str(source),
        source_hashes=hashes,
        build_profile="performance",
        commands=[command],
        artifacts=artifacts,
        compiler_artifacts=compiler_artifacts,
        vm_build_scripts=scripts,
        source_manifest=record(output / "build-source-manifest.json"),
        build_started=record(output / "build-start.json"),
        compiler=subprocess.check_output(["rustc", "-vV"], text=True),
        cargo=subprocess.check_output(["cargo", "-V"], text=True).strip(),
        finished_at=datetime.now(timezone.utc).isoformat(),
        logs={name: sha(output / name) for name in ("cargo-messages.jsonl", "build.log")},
        firmware_scope="GNU runtime-selected firmware; measurement must explicitly freeze/select its final firmware, not assume compile binding",
        VM_runs=0,
        performance_measurements=0,
    )
    save(output / "build-receipt.json", receipt)
    print(
        json.dumps(
            dict(
                state="built", source_files=len(hashes), receipt=str(output / "build-receipt.json")
            )
        )
    )


def prepare(args):
    repo = REPO
    template = args.template.resolve()
    original = load(template / "campaign-pre/config.json")
    build_path = args.build_receipt.resolve()
    build = load(build_path)
    require(
        build.get("state") == "built"
        and build.get("build_profile") == "performance"
        and isinstance(build.get("source_hashes"), dict)
        and build["source_hashes"]
        and Path(build["source_root"]).is_dir(),
        "current build/source receipt is incomplete",
    )
    for name in ("pvisor", "pvisor-cache", "embedded-guest"):
        require(
            name in build["artifacts"]
            and sha(build["artifacts"][name]["path"]) == build["artifacts"][name]["sha256"],
            f"current CLI build artifact differs: {name}",
        )
    require(
        args.firmware is not None and args.firmware_source is not None,
        "final runtime-selected firmware and matching SOURCE are required",
    )
    firmware = args.firmware.resolve()
    firmware_source = load(args.firmware_source)
    require(
        sha(firmware) == firmware_source.get("firmware_sha256")
        and "patches/0048-fuse-pipeline-native-writeback-pressure.patch"
        in firmware_source.get("inputs", {}),
        "selected firmware differs from final writeback SOURCE",
    )
    setup = load(args.setup)
    require(
        setup.get("state") == "ready"
        and setup.get("cgroup")
        and setup.get("slice_unit")
        and setup.get("memory_bytes") == 16 * 1024**3
        and setup.get("swap_bytes") == 0
        and setup.get("cpu_ids") == [0, 1]
        and setup.get("cpu_quota") == 2,
        "new owned two-core/16-GiB budget is not ready",
    )
    batch = args.prepare.resolve()
    require(not batch.exists(), "batch already exists; retained evidence is never overwritten")
    # Every original path used by the worker must be visible under one physical
    # bind. Repository root also covers the new benchmark.new output directory.
    for path in (batch, template, Path(build["source_root"]), args.setup.resolve()):
        path.relative_to(repo)
    batch.mkdir(parents=True)
    shutil.copy2(__file__, batch / "entry.py")
    shutil.copy2(template / "entry.py", batch / "af_entry.py")
    shutil.copy2(template / "workspace_sources.py", batch / "workspace_sources.py")
    shutil.copy2(template / "reference-arguments.json", batch / "reference-arguments.json")
    for path in sorted((template / "harness").rglob("*.py")):
        target = batch / "harness" / path.relative_to(template / "harness")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
    artifacts = {
        name: build["artifacts"][name] for name in ("pvisor", "pvisor-cache", "embedded-guest")
    }
    firmware_dir = batch / "firmware"
    firmware_dir.mkdir()
    shutil.copy2(firmware, firmware_dir / "libkrunfw.so.5.6.2")
    (firmware_dir / "libkrunfw.so.5").symlink_to("libkrunfw.so.5.6.2")
    shutil.copy2(args.firmware_source, firmware_dir / "SOURCE.json")
    for name, expected in firmware_source["inputs"].items():
        if name.startswith("patches/"):
            path = REPO / "fw" / name
            require(
                ".." not in Path(name).parts and sha(path) == expected,
                "selected firmware patch changed: " + name,
            )
            target = firmware_dir / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
    artifacts["firmware"] = record(firmware_dir / "libkrunfw.so.5.6.2")
    references = dict(
        current_build=record(build_path),
        original_preparation=record(original["input_preparation"]),
        prepared_input_association=record(original["reuse_artifacts"]),
        inherited_worker=record(template / "entry.py"),
        inherited_config_controls=record(template / "campaign-pre/config.json"),
        inherited_anchor=record(BASE / "curves_cli.py"),
        selected_firmware=record(firmware),
        selected_firmware_source=record(args.firmware_source),
        scope="current binaries/source; earlier public input generations and validators only; no earlier samples or new import cost",
    )
    save(batch / "origins.json", references)
    common = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="engineering A/B",
        design=DESIGN,
        repo=str(repo),
        raw_root=str(repo),
        conditions=list(ARMS),
        factors=FACTORS,
        contrasts=[["off", "on"]],
        resident_rounds=3,
        seed=20261017,
        memory_mib=16384,
        cpu_affinity="0,1",
        dax=False,
        environment=FIXED,
        setup=str(args.setup.resolve()),
        setup_sha256=sha(args.setup),
        slice_unit=setup["slice_unit"],
        budget_binding="bound-new-owned-budget",
        quick_source_report=str(batch / "reference-arguments.json"),
        firmware=str(firmware_dir),
        artifacts=artifacts,
        firmware_source=record(firmware_dir / "SOURCE.json"),
        build_receipt=record(build_path),
        product_source_root=build["source_root"],
        product_source_files=len(build["source_hashes"]),
        input_preparation=original["input_preparation"],
        inputs=original["inputs"],
        origins=record(batch / "origins.json"),
        profile_inventory={arm: HISTOGRAM for arm in ARMS},
        treatment_scope="guest writeback0/1 only; same final firmware and current host; hostWB0/DAX0 and DATA/FLUSH/NE/SQ/Core absence0",
    )
    for phase in ("pre", "profiles", "timing"):
        directory = batch / f"campaign-{phase}"
        directory.mkdir()
        config = {
            **common,
            "phase": phase,
            "samples": 5 if phase == "timing" else 1,
            "benchmark_id": "B-FS-DIAG" if phase == "profiles" else "B-FS-ENG",
            "role": "diagnostic" if phase == "profiles" else "engineering A/B",
            "output": str(batch / phase),
        }
        save(directory / "config.json", config)
        save(
            directory / "in.json",
            {
                "guest-writeback-cli": "$ "
                + sys.executable
                + " "
                + str(batch / "entry.py")
                + " --config "
                + str(directory / "config.json")
                + " --run"
            },
        )
    source_audit(common, batch / "source-before.json")
    files = [path for path in batch.rglob("*") if path.is_file()]
    save(
        batch / "harness-manifest.json",
        dict(
            schema=1,
            files={str(path.relative_to(batch)): sha(path) for path in sorted(files)},
            origins=references,
            frozen_at=datetime.now(timezone.utc).isoformat(),
        ),
    )
    print(
        json.dumps(
            dict(
                state="prepared",
                batch=str(batch),
                frozen_files=len(files),
                launch="entry.py --config campaign-{pre,profiles,timing}/config.json --run",
                analysis="entry.py --config campaign-timing/config.json --analyze",
            ),
            indent=2,
        )
    )


def verify(config):
    require(
        config["design"] == DESIGN
        and config["conditions"] == list(ARMS)
        and config["factors"] == FACTORS
        and config["contrasts"] == [["off", "on"]],
        "anchor design/condition differs",
    )
    require(
        config["phase"] in ("pre", "profiles", "timing")
        and config["samples"] == (5 if config["phase"] == "timing" else 1)
        and config["resident_rounds"] == 3
        and config["seed"] == 20261017
        and config["environment"] == FIXED
        and config["dax"] is False
        and config["memory_mib"] == 16384
        and config["cpu_affinity"] == "0,1",
        "anchor controls differ",
    )
    require(
        config["budget_binding"] == "bound-new-owned-budget"
        and sha(config["setup"]) == config["setup_sha256"],
        "owned budget binding changed",
    )
    require(sha(config["origins"]["path"]) == config["origins"]["sha256"], "origins changed")
    origins = load(config["origins"]["path"])
    for name in (
        "current_build",
        "original_preparation",
        "prepared_input_association",
        "selected_firmware",
        "selected_firmware_source",
    ):
        ref = origins[name]
        require(sha(ref["path"]) == ref["sha256"], f"input/build association changed: {name}")
    require(
        config["build_receipt"] == origins["current_build"], "current build association differs"
    )
    build = load(config["build_receipt"]["path"])
    require(
        build["source_root"] == config["product_source_root"]
        and len(build["source_hashes"]) == config["product_source_files"],
        "source binding differs",
    )
    for name, ref in config["artifacts"].items():
        require(sha(ref["path"]) == ref["sha256"], f"artifact changed: {name}")
    require(
        all(
            config["artifacts"][name] == build["artifacts"][name]
            for name in ("pvisor", "pvisor-cache", "embedded-guest")
        ),
        "CLI receipt/artifact join differs",
    )
    require(
        sha(config["firmware_source"]["path"]) == config["firmware_source"]["sha256"]
        and load(config["firmware_source"]["path"])["firmware_sha256"]
        == config["artifacts"]["firmware"]["sha256"]
        and config["artifacts"]["firmware"]["sha256"] == origins["selected_firmware"]["sha256"]
        and Path(config["firmware"]) / "libkrunfw.so.5.6.2"
        == Path(config["artifacts"]["firmware"]["path"])
        and (Path(config["firmware"]) / "libkrunfw.so.5").resolve()
        == Path(config["artifacts"]["firmware"]["path"]),
        "final explicit runtime firmware association differs",
    )
    association = load(origins["prepared_input_association"]["path"])
    require(
        config["inputs"] == association["inputs"]
        and config["input_preparation"] == origins["original_preparation"]["path"]
        and load(config["input_preparation"])["state"] == "verified",
        "public prepared input association differs",
    )
    frozen = load(BASE / "harness-manifest.json")
    for name, expected in frozen["files"].items():
        require(sha(BASE / name) == expected, f"frozen anchor input changed: {name}")
    selected = config["inputs"]["profiles" if config["phase"] == "profiles" else "timing"]
    root = Path(selected["store"]) / "bases" / selected["base_id"]
    for name, expected in selected["small_files_sha256"].items():
        require(sha(root / name) == expected, f"prepared input identity changed: {name}")
    workspace = Path(selected["workspace_store"]) / "bases" / selected["workspace_base_id"]
    require(
        (root / "rootfs").is_dir()
        and not (root / "rootfs").is_symlink()
        and (workspace / "rootfs").is_dir()
        and not (workspace / "rootfs").is_symlink()
        and sha(workspace / "seal.json") == selected["workspace_base_id"],
        "owned roots are absent or changed",
    )
    require(
        sha(workspace / "rootfs/_fs/fixture.json")
        == sha(Path(selected["assets"]) / "rootfs/work/_fs/fixture.json")
        and load(workspace / "rootfs/_fs/fixture.json")["rounds"] == 3,
        "prepared workspace/live trial fixture differs",
    )
    return dict(
        state="verified-no-VM-launched",
        phase=config["phase"],
        build_receipt_sha256=config["build_receipt"]["sha256"],
        frozen_harness_sha256=sha(BASE / "harness-manifest.json"),
        product_source_files=config["product_source_files"],
        artifact_hashes={k: v["sha256"] for k, v in config["artifacts"].items()},
    )


def inherited_worker(config, condition, trial, directory):
    """Keep the audited normal CLI/Bundle/full-byte validators unchanged."""
    sys.path[:0] = [str(BASE / "harness"), str(BASE)]
    spec = importlib.util.spec_from_file_location("anchor_af_worker", BASE / "af_entry.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    old_profiles = module.validate_profiles
    module.verify_inputs = verify
    module.ARMS = ARMS
    # In the inherited worker this map means readonly FLUSH, fixed OFF here.
    module.FACTORS = dict(off=0, on=0)
    module.condition_environment = environment

    def caller(_condition, receipt):
        require(
            receipt == dict(variable=FLUSH_ENV, present=True, value="0")
            and type(receipt["present"]) is bool,
            "FLUSH must stay explicitly off",
        )
        return receipt

    module.validate_caller_environment = caller

    def profiles(lines, arm, inventory):
        evidence = old_profiles(lines, arm, inventory)
        collected = evidence["collected"]
        lookup = {(p["pid"], p["component"], p["instance"]): p for p in collected["final_profiles"]}
        for scope in ("rootfs", "pvisor-workspace"):
            owner = evidence["actual"][scope]["virtio-fs-overlay"]
            values = lookup[(owner["pid"], "virtio-fs-overlay", owner["instance"])]["measurements"]
            flag = values.get("host_writeback_enabled")
            require(
                flag is None or (type(flag["units"]) is int and flag["units"] == 0),
                f"host buffer unexpectedly enabled: {scope}",
            )
            owner["host_writeback"] = dict(
                requested=0,
                actual_enabled_units=None if flag is None else flag["units"],
                disabled_counter_absence=flag is None,
                disabled_evidence="source-bound configure early-return plus explicit actual frontend env0"
                if flag is None
                else None,
            )
            owner["guest_writeback"] = validate_guest_writeback_state(values, arm)
        return evidence

    module.validate_profiles = profiles
    # This records the exact env passed to the real frontend Popen, not a config
    # echo; inherited reference already separately records FLUSH presence/value.
    actual = []
    firmware_mappings, firmware_probe_errors = [], []
    probe_stop = threading.Event()
    probe_thread = None
    original_popen = subprocess.Popen

    def observe_firmware():
        # Diagnostic only. KernelOwner retains the loaded Library throughout
        # this GNU native runner's VM lifetime. Never scans other namespaces.
        expected = module.alias(config["artifacts"]["firmware"]["path"], config).resolve()
        identity = expected.stat()
        while not probe_stop.is_set():
            for process in Path("/proc").iterdir():
                if not process.name.isdigit():
                    continue
                try:
                    lines = (process / "maps").read_text().splitlines()
                    for line in lines:
                        if len(line.split(None, 5)) != 6 or line.split(None, 5)[5] != str(expected):
                            continue
                        mapped_file = process / "root" / expected.relative_to("/")
                        mapped_hash = sha(mapped_file)
                        require(
                            mapped_hash == config["artifacts"]["firmware"]["sha256"],
                            "mapped process firmware bytes differ",
                        )
                        mapping = firmware_mapping_entry(
                            line,
                            expected,
                            identity,
                            mountinfo=(process / "mountinfo").read_text(),
                            mapped_sha256=mapped_hash,
                        )
                        if mapping is None:
                            continue
                        firmware_mappings.append(
                            dict(
                                pid=int(process.name),
                                **mapping,
                                scope="actual native process mapping, diagnostic only; GNU KernelOwner retains the Library",
                            )
                        )
                        return
                except (OSError, ValueError) as error:
                    if len(firmware_probe_errors) < 8:
                        firmware_probe_errors.append(dict(pid=process.name, error=repr(error)))
            probe_stop.wait(0.01)

    def popen(argv, *args, **kwargs):
        nonlocal probe_thread
        frontend = False
        if (
            isinstance(argv, (list, tuple))
            and "run" in argv
            and any(str(part).endswith("/bin/pvisor") for part in argv)
        ):
            env = kwargs.get("env")
            expected = environment(condition, config["phase"] == "profiles")
            require(
                env is not None and all(env.get(k) == v for k, v in expected.items()),
                "actual frontend environment differs from treatment/fixed controls",
            )
            require(
                "PVISOR_VM_FS_HOST_WRITEBACK_CONFIG" not in env,
                "unexpected private queue tuning in ordinary CLI anchor",
            )
            actual.append(
                dict(
                    argv=list(map(str, argv)),
                    controls={k: env[k] for k in expected},
                    host_writeback_config_present=False,
                )
            )
            frontend = True
        process = original_popen(argv, *args, **kwargs)
        if frontend and config["phase"] == "profiles":
            probe_thread = threading.Thread(target=observe_firmware, daemon=True)
            probe_thread.start()
        return process

    subprocess.Popen = popen
    try:
        module.worker(config, condition, trial, directory)
    finally:
        subprocess.Popen = original_popen
        probe_stop.set()
        if probe_thread is not None:
            probe_thread.join()
    require(len(actual) == 1, "not exactly one observed ordinary CLI frontend launch")
    output = module.alias(directory, config)
    row = load(output / "row.json")
    expected_firmware = module.alias(config["firmware"], config)
    job = load(Path(row["logs"]) / "stage/execution-job.json")
    argv = load(Path(row["logs"]) / "command.json")["argv"]
    require(
        argv.count("--vm-library-dir") == 1
        and argv[argv.index("--vm-library-dir") + 1] == str(expected_firmware)
        and job["config"]["vm"]["library_dir"] == str(expected_firmware),
        "real CLI argv/persisted selected firmware differs",
    )
    if config["phase"] == "profiles":
        selection = dict(
            observed=firmware_mappings,
            errors=firmware_probe_errors,
            expected_firmware_sha256=config["artifacts"]["firmware"]["sha256"],
        )
        save(output / "firmware-selection.json", selection)
        require(
            len(firmware_mappings) == 1,
            "actual final firmware was not observed in native process maps",
        )
        row["firmware_selection"] = selection
    row["factors"] = dict(
        GUEST_WRITEBACK=FACTORS[condition], HOST_WRITEBACK=0, DATA=0, FLUSH=0, NE=0, SQ=0
    )
    row["frontend_guest_writeback_environment"] = actual[0]
    row["current_cli_build_receipt"] = config["build_receipt"]
    row["selected_firmware"] = config["artifacts"]["firmware"]
    row["worker_clock_scope"] = (
        "original seven tools plus hot_open and stream_write; fsync/close retained; exit excluded"
    )
    save(output / "frontend-environment.json", actual[0])
    save(output / "row.json", row)


def run(config):
    checked = verify(config)
    output = Path(config["output"])
    output.mkdir(parents=True, exist_ok=False)
    phase = config["phase"]
    report = dict(
        schema=1,
        state="running",
        benchmark_id=config["benchmark_id"],
        role=config["role"],
        started_at=datetime.now(timezone.utc).isoformat(),
        config=config,
        input_verification=checked,
        rows=[],
        failures=[],
        order=[],
        protocol=dict(
            design=DESIGN,
            factors=FACTORS,
            conditions=list(ARMS),
            samples_per_condition=config["samples"],
            resident_rounds=3,
            seed=20261017,
            startup="actual frontend Popen to unique REFERENCE_READY; fresh private Host+VM; excludes exit",
            task="original worker_ms; three dependent rounds; original fsync/close semantics",
            write_durability="write uses create/write/close/stat without explicit fsync; stream_write includes file fsync+close. Seven-tool worker sum is not a completed-durability clock; final host bytes checked after normal exit.",
            independent_unit="five fresh paired environment blocks, never fifteen rounds",
            statistics="32 metrics; one aggregate primary; 5000 paired bootstrap; uncorrected secondary intervals",
            primary="three_rounds.seven_tools_ms",
            warmups=0,
            performance_sample=phase == "timing",
            writeback="off/on explicit",
            dax=False,
            host_writeback=False,
            guest_writeback_factors=FACTORS,
            interference_gate=False,
            quiet_gate=False,
            full_inventory_gate=False,
            retain_all_valid_slow_samples=True,
            reused_preparation="existing public owned generations only; no new import/seal performed or charged",
            cli_scope="real ordinary CLI; prepared rootfs/workspace; declared checkpoint capability false; native binding unobserved",
        ),
    )
    save(output / "report.json", report)
    rng = random.Random(20261017)
    for pair in range(config["samples"]):
        order = list(ARMS)
        rng.shuffle(order)
        for arm in order:
            directory = BASE / "raw" / phase / f"{arm}-{pair:03d}"
            directory.mkdir(parents=True, exist_ok=False)
            argv = [
                "unshare",
                "--user",
                "--map-root-user",
                "--mount",
                "--pid",
                "--fork",
                "--kill-child",
                "--mount-proc",
                sys.executable,
                str(BASE / "entry.py"),
                "--config",
                config["_config_path"],
                "--worker",
                arm,
                "--trial",
                str(pair),
                "--directory",
                str(directory),
            ]
            save(directory / "worker-command.json", dict(argv=argv))
            report["order"].append(dict(pair=pair, condition=arm))
            save(output / "report.json", report)
            try:
                with (
                    (directory / "namespace.stdout").open("wb") as out,
                    (directory / "namespace.stderr").open("wb") as err,
                ):
                    completed = subprocess.run(argv, stdout=out, stderr=err, timeout=300)
                save(directory / "worker-exit.json", dict(exit_code=completed.returncode))
                require(completed.returncode == 0, f"failed CLI sample retained: {directory}")
                row = load(directory / "row.json")
                require(
                    row["condition"] == arm
                    and row["pair"] == pair
                    and row["factors"]["GUEST_WRITEBACK"] == FACTORS[arm],
                    "worker treatment mismatch",
                )
                report["rows"].append(row)
                save(output / "report.json", report)
            except BaseException as error:
                report["failures"].append(
                    dict(condition=arm, pair=pair, error=str(error), directory=str(directory))
                )
                report["state"] = "failed"
                save(output / "report.json", report)
                raise
    report.update(
        state="complete",
        ended_at=datetime.now(timezone.utc).isoformat(),
        final_input_verification=verify(config),
    )
    save(output / "report.json", report)
    print(
        json.dumps(
            dict(state="complete", rows=len(report["rows"]), report=str(output / "report.json"))
        )
    )


def metrics(row):
    result = dict(ready_ms=row["ready_ms"])
    for number in range(1, 4):
        for operation in OPERATIONS:
            result[f"r{number}.{operation}_ms"] = row["result"]["filesystem"][operation]["rounds"][
                number - 1
            ]["worker_ms"]
        result[f"r{number}.seven_tools_ms"] = sum(
            result[f"r{number}.{op}_ms"] for op in OPERATIONS[:7]
        )
    result["three_rounds.seven_tools_ms"] = sum(result[f"r{n}.seven_tools_ms"] for n in range(1, 4))
    require(
        len(result) == 32
        and all(type(v) in (int, float) and math.isfinite(v) and v > 0 for v in result.values()),
        "invalid worker metric",
    )
    return result


def percentile(values, q):
    values = sorted(values)
    index = (len(values) - 1) * q
    low = int(index)
    return values[low] + (values[min(low + 1, len(values) - 1)] - values[low]) * (index - low)


def analyze(config):
    verify(config)
    report_path = Path(config["output"]) / "report.json"
    report = load(report_path)
    require(
        config["phase"] == "timing"
        and report["state"] == "complete"
        and not report["failures"]
        and len(report["rows"]) == 10,
        "only complete independent five-pair timing cohort can be analyzed",
    )
    expected_order = []
    rng = random.Random(20261017)
    for pair in range(5):
        arms = list(ARMS)
        rng.shuffle(arms)
        expected_order.extend(dict(pair=pair, condition=arm) for arm in arms)
    require(report["order"] == expected_order, "paired randomized order differs")
    rows = {(r["condition"], r["pair"]): r for r in report["rows"]}
    require(set(rows) == {(a, p) for a in ARMS for p in range(5)}, "missing/duplicate environment")
    for (arm, pair), row in rows.items():
        raw = BASE / "raw" / "timing" / f"{arm}-{pair:03d}"
        require(
            load(raw / "worker-exit.json")["exit_code"] == 0
            and load(raw / "row.json") == row
            and row["normal_cli"] is True,
            "retained normal-exit row differs",
        )
    extracted = {key: metrics(row) for key, row in rows.items()}
    summary, comparisons = [], []
    for metric in next(iter(extracted.values())):
        samples = {a: [extracted[(a, p)][metric] for p in range(5)] for a in ARMS}
        for arm, values in samples.items():
            summary.append(
                dict(
                    condition=arm,
                    metric=metric,
                    n=5,
                    median_ms=statistics.median(values),
                    min_ms=min(values),
                    max_ms=max(values),
                    samples_by_pair_ms=values,
                )
            )
        reduction = 100 * (1 - statistics.median(samples["on"]) / statistics.median(samples["off"]))
        rng = random.Random(20261017)
        bootstrap = []
        for _ in range(5000):
            indexes = [rng.randrange(5) for _ in range(5)]
            bootstrap.append(
                100
                * (
                    1
                    - statistics.median(samples["on"][i] for i in indexes)
                    / statistics.median(samples["off"][i] for i in indexes)
                )
            )
        comparisons.append(
            dict(
                metric=metric,
                primary=metric == "three_rounds.seven_tools_ms",
                reduction_percent=reduction,
                ci95_low=percentile(bootstrap, 0.025),
                ci95_high=percentile(bootstrap, 0.975),
                off_samples_ms=samples["off"],
                on_samples_ms=samples["on"],
                bootstrap_replicates=5000,
                seed=20261017,
            )
        )
    result = dict(
        state="verified",
        report=record(report_path),
        n_paired_environments=5,
        dependent_rounds_per_environment=3,
        summary=summary,
        comparisons=comparisons,
        multiple_comparison_adjustment="none; 32 intervals, one preregistered aggregate primary",
        scope="same-cohort real ordinary CLI anchor only; no historical or SDK curve timing pooled",
    )
    save(BASE / "timing-analysis.json", result)
    source_audit(config, BASE / "source-after.json")
    print(
        json.dumps(
            dict(
                state="verified", metrics=32, primary=next(c for c in comparisons if c["primary"])
            ),
            indent=2,
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path)
    parser.add_argument("--build", type=Path)
    parser.add_argument(
        "--source-baseline",
        type=Path,
        default=REPO
        / "benchmark.new/filesystem/.data/guest-writeback-curves-v4-20261017/protocol.json",
    )
    parser.add_argument("--setup", type=Path)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--firmware-source", type=Path)
    parser.add_argument("--template", type=Path, default=REPO / "benchmark/.data/af17-cli")
    parser.add_argument(
        "--build-receipt",
        type=Path,
        default=REPO
        / "benchmark.new/filesystem/.data/guest-writeback-cli-artifacts-v2-20261017/build-receipt.json",
    )
    parser.add_argument("--config", type=Path)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true")
    action.add_argument("--run", action="store_true")
    action.add_argument("--analyze", action="store_true")
    action.add_argument("--worker", choices=ARMS)
    parser.add_argument("--trial", type=int)
    parser.add_argument("--directory", type=Path)
    args = parser.parse_args()
    if args.build:
        require(
            args.prepare is None
            and args.config is None
            and not any((args.check, args.run, args.analyze, args.worker)),
            "--build is an independent offline action",
        )
        return build_cli(args)
    if args.prepare:
        require(
            args.setup is not None
            and args.config is None
            and not any((args.check, args.run, args.analyze, args.worker)),
            "--prepare requires --setup only",
        )
        return prepare(args)
    require(
        args.config is not None and any((args.check, args.run, args.analyze, args.worker)),
        "select --prepare or --config with an action",
    )
    config = load(args.config)
    config["_config_path"] = str(args.config.resolve())
    try:
        if args.check:
            print(json.dumps(verify(config), indent=2))
        elif args.run:
            run(config)
        elif args.analyze:
            analyze(config)
        else:
            require(args.trial is not None and args.directory is not None, "worker identity absent")
            inherited_worker(config, args.worker, args.trial, args.directory)
    except BaseException as error:
        if args.worker and args.directory and args.directory.is_dir():
            save(
                args.directory / "failure.json",
                dict(error=str(error), traceback=traceback.format_exc()),
            )
        raise


if __name__ == "__main__":
    main()
