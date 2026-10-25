#!/usr/bin/env python3
"""Prepare a private rootless Docker image/store for B-MEMORY-SCALE; no timings."""

import argparse
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

from docker_support import Engine, create_slice, verify_inputs
from prepare_pool import tree_manifest
from scanner_compare import digest, save


def internal(root, unit):
    engine = Engine(root, unit, root / "seed")
    try:
        engine.start()
        with (root / "rootfs.tar").open("rb") as source:
            image = engine.call(
                ["import", "-", "pvisor-memory:frozen"], stdin=source, text=True, timeout=120
            ).stdout.strip()
        save(root / "image-inspect.json", engine.inspect(image))
        engine.call(["image", "save", "--output", str(root / "image.tar"), image], timeout=120)
        save(root / "image.json", dict(image_id=image))
    finally:
        if not engine.stop():
            raise RuntimeError("Docker input-preparation cleanup failed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--internal", type=Path)
    parser.add_argument("--slice")
    args = parser.parse_args()
    if args.internal:
        internal(args.internal, args.slice)
        return
    if not args.inputs or not args.output:
        parser.error("--inputs and --output required")
    inputs, root = args.inputs.resolve(), args.output.resolve()
    pool = json.loads((inputs / "receipt.json").read_text())
    from library_compare import verify_inputs as verify_pool

    verify_pool(inputs, pool)
    root.mkdir(mode=0o700)
    shutil.copy2(Path(__file__).with_name("docker_init.rs"), root / "docker_init.rs")
    command = [
        "rustc",
        "--edition=2021",
        "--target",
        "x86_64-unknown-linux-musl",
        "-C",
        "opt-level=2",
        "-o",
        str(root / "memory-init"),
        str(root / "docker_init.rs"),
    ]
    build = subprocess.run(command, capture_output=True, text=True, timeout=60)
    save(
        root / "init-build.json",
        dict(
            command=command,
            returncode=build.returncode,
            stdout=build.stdout,
            stderr=build.stderr,
            compiler=subprocess.check_output(["rustc", "-Vv"], text=True),
        ),
    )
    build.check_returncode()

    def owner(item):
        item.uid = item.gid = 0
        item.uname = item.gname = "root"
        return item

    with tarfile.open(root / "rootfs.tar", "w", dereference=False) as archive:
        archive.add(inputs / "python-root", arcname=".", filter=owner)
        archive.add(inputs / "worker", arcname="worker", filter=owner)
        archive.add(root / "memory-init", arcname="memory-init", filter=owner)
    unit, group = create_slice(root)
    child = unit.removesuffix(".slice") + "-prepare.service"
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit=" + child,
        "--slice=" + unit,
        "-p",
        "Delegate=yes",
        "-p",
        "CPUAffinity=0 1 2 3",
        "-p",
        "RuntimeMaxSec=210",
        "-p",
        "TimeoutStopSec=15",
        "-p",
        "KillMode=control-group",
        sys.executable,
        str(Path(__file__).resolve()),
        "--internal",
        str(root),
        "--slice",
        unit,
    ]
    save(root / "preparation-command.json", dict(command=command, group=str(group)))
    try:
        result = subprocess.run(command, capture_output=True, timeout=230)
        (root / "prepare.stdout").write_bytes(result.stdout)
        (root / "prepare.stderr").write_bytes(result.stderr)
        result.check_returncode()
    finally:
        subprocess.run(["systemctl", "--user", "stop", unit], check=True, timeout=20)
    save(root / "seed-manifest.json", tree_manifest(root / "seed"))
    source = root / "preparation-source"
    source.mkdir()
    for name in ("prepare_docker.py", "docker_support.py", "docker_init.rs"):
        shutil.copy2(Path(__file__).with_name(name), source / name)
    runtime = [
        "/usr/bin/" + name
        for name in (
            "docker",
            "dockerd",
            "dockerd-rootless.sh",
            "rootlesskit",
            "pasta",
            "containerd",
            "containerd-shim-runc-v2",
            "runc",
        )
    ]
    receipt = dict(
        schema="pvisor-memory-docker-inputs/v1",
        image_id=json.loads((root / "image.json").read_text())["image_id"],
        pool_inputs=str(inputs),
        pool_receipt_sha256=digest(inputs / "receipt.json"),
        hashes={
            name: digest(root / name)
            for name in (
                "rootfs.tar",
                "image.tar",
                "memory-init",
                "docker_init.rs",
                "seed-manifest.json",
                "image-inspect.json",
            )
        },
        runtime_sha256={name: digest(Path(name)) for name in runtime},
        preparation_sha256={path.name: digest(path) for path in source.iterdir()},
        source_bytes={
            name: pool["hashes"][name]
            for name in (
                "worker",
                "library_worker.py",
                "libraries/libpayload.so",
                "python-root-manifest.json",
            )
        },
        scope="same frozen Python userspace, library and static oracle; image file owners normalized to container root; affinity launcher execs oracle; rootless dedicated daemon included in product slice; no network",
    )
    save(root / "receipt.json", receipt)
    verify_inputs(root, receipt)
    print(root)


if __name__ == "__main__":
    main()
