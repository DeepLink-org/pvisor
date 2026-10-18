#!/usr/bin/env python3
"""Inspect prerequisites for B-LAZY-STARTUP's TurboOCI comparison, without changes.

This preparation helper does not run a benchmark or any discovered executable.
It does not install software, load modules, start/connect to services, change
configuration, create block devices, or clean caches. Exit 0 means the inspected
prerequisites passed, not that a real TurboOCI/container/VM launch was verified.
Exit 2 means prerequisites are blocked. JSON is written only to standard output.
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import stat
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def inspect_tool(names, directories):
    """Find a readable executable file; hash its bytes without executing it."""
    candidates = [directory / name for directory in directories for name in names]
    candidates.extend(Path(path) for name in names if (path := shutil.which(name)))
    errors = []
    for candidate in dict.fromkeys(candidates):
        try:
            mode = candidate.stat().st_mode
            if not stat.S_ISREG(mode) or not os.access(candidate, os.X_OK):
                continue
            digest = hashlib.sha256()
            with candidate.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
            return {"ready": True, "path": str(candidate.resolve()), "sha256": digest.hexdigest()}
        except FileNotFoundError:
            continue
        except OSError as error:
            errors.append({"path": str(candidate), "error": str(error)})
    result = {"ready": False, "path": None, "sha256": None, "names": list(names)}
    if errors:
        result["errors"] = errors
    return result


def inspect_binary(path):
    """Inspect the exact requested pVisor artifact, without a PATH fallback."""
    result = {"ready": False, "path": str(path.absolute()), "sha256": None}
    try:
        if not stat.S_ISREG(path.stat().st_mode):
            result["error"] = "not a regular file"
        elif not os.access(path, os.X_OK):
            result["error"] = "file is not executable by this user"
        else:
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
            result.update(ready=True, path=str(path.resolve()), sha256=digest.hexdigest())
    except OSError as error:
        result["error"] = str(error)
    return result


def device_access(path):
    """Open and close an existing character device; issue no ioctl/read/write."""
    result = {"path": str(path), "character_device": False, "read_write_open": False}
    try:
        result["character_device"] = stat.S_ISCHR(path.stat().st_mode)
        if not result["character_device"]:
            result["error"] = "not a character device"
            return result
        descriptor = os.open(path, os.O_RDWR | os.O_CLOEXEC)
        os.close(descriptor)
        result["read_write_open"] = True
    except OSError as error:
        result["error"] = str(error)
    return result


def configfs_access():
    """Check existing target configfs access; do not create or modify objects."""
    path = Path("/sys/kernel/config/target/core")
    result = {"path": str(path), "configfs_mounted": False, "read_write_search_access": False}
    try:
        # The fixed target path must actually lie on a mounted configfs instance;
        # an empty /sys/kernel/config directory is not an available TCMU backend.
        for line in Path("/proc/self/mountinfo").read_text().splitlines():
            before, separator, after = line.partition(" - ")
            if not separator or after.split()[0] != "configfs":
                continue
            mountpoint = Path(before.split()[4].replace("\\040", " ").replace("\\134", "\\"))
            if path.is_relative_to(mountpoint):
                result["configfs_mounted"] = True
                result["mountpoint"] = str(mountpoint)
        result["read_write_search_access"] = path.is_dir() and os.access(
            path, os.R_OK | os.W_OK | os.X_OK, effective_ids=True
        )
    except OSError as error:
        result["error"] = str(error)
    return result


def inspect(args):
    system, architecture = platform.system(), platform.machine()
    checks = {}
    blocking = []

    def check(name, passed, detail):
        checks[name] = {"status": "passed" if passed else "blocked", "detail": detail}
        if not passed:
            blocking.append({"check": name, "reason": detail})

    check(
        "platform",
        system == "Linux" and architecture.lower() in ("x86_64", "amd64"),
        "requires Linux amd64; observed " + system + "/" + architecture,
    )
    linux = system == "Linux"
    kvm = device_access(Path("/dev/kvm")) if linux else {"read_write_open": False}
    check("kvm", kvm["read_write_open"], kvm)

    overlaybd_dir = args.overlaybd_dir.expanduser().absolute() if args.overlaybd_dir else None
    tool_dirs = []
    if overlaybd_dir:
        tool_dirs.extend(
            (
                overlaybd_dir,
                overlaybd_dir / "bin",
                overlaybd_dir / "snapshotter",
                overlaybd_dir / "opt/overlaybd/bin",
                overlaybd_dir / "opt/overlaybd/snapshotter",
                overlaybd_dir / "opt/overlaybd-snapshotter/bin",
            )
        )
    tool_dirs.extend(
        (
            Path("/opt/overlaybd/bin"),
            Path("/opt/overlaybd/snapshotter"),
            Path("/opt/overlaybd-snapshotter/bin"),
        )
    )
    tools = {
        "containerd": inspect_tool(("containerd",), []),
        "ctr": inspect_tool(("ctr",), []),
        "snapshotter": inspect_tool(
            ("overlaybd-snapshotter", "containerd-overlaybd-grpc"), tool_dirs
        ),
        "convertor": inspect_tool(("convertor", "overlaybd-convertor"), tool_dirs),
        "ublk": inspect_tool(("overlaybd-ublk",), tool_dirs),
        "tcmu": inspect_tool(("overlaybd-tcmu",), tool_dirs),
    }
    for name in ("containerd", "ctr", "snapshotter", "convertor"):
        check("tool_" + name, tools[name]["ready"], tools[name])

    binary_dir = args.binary_dir.expanduser().absolute()
    for name in ("pvisor", "pvisor-cache"):
        tools[name] = inspect_binary(binary_dir / name)
        check("tool_" + name, tools[name]["ready"], tools[name])

    ublk = {
        "module": "ublk_drv",
        "module_loaded": linux and Path("/sys/module/ublk_drv").is_dir(),
        "device": device_access(Path("/dev/ublk-control")) if linux else {},
        "tool": tools["ublk"],
    }
    ublk["prerequisites_ready"] = bool(
        ublk["module_loaded"] and ublk["device"].get("read_write_open") and tools["ublk"]["ready"]
    )
    tcmu = {
        "module": "target_core_user",
        "module_loaded": linux and Path("/sys/module/target_core_user").is_dir(),
        "configfs": configfs_access() if linux else {},
        "tool": tools["tcmu"],
    }
    tcmu["prerequisites_ready"] = bool(
        tcmu["module_loaded"]
        and tcmu["configfs"].get("configfs_mounted")
        and tcmu["configfs"].get("read_write_search_access")
        and tools["tcmu"]["ready"]
    )
    frontends = {"ublk": ublk, "tcmu": tcmu}
    selected = args.frontend
    if selected == "auto":
        selected = next(
            (name for name, value in frontends.items() if value["prerequisites_ready"]), None
        )
    check(
        "frontend",
        selected is not None and frontends[selected]["prerequisites_ready"],
        {"requested": args.frontend, "selected": selected, "candidates": frontends},
    )

    return {
        "benchmark_id": "B-LAZY-STARTUP",
        "status": "blocked" if blocking else "ready",
        "ready": not blocking,
        "scope": "read-only prerequisite inspection; no workload or runtime launch verified",
        "host": {
            "system": system,
            "architecture": architecture,
            "kernel": platform.release(),
            "effective_uid": os.geteuid() if hasattr(os, "geteuid") else None,
        },
        "privileges": {
            "runtime_privileges_verified": False,
            "note": "Root/capabilities for actual device creation, mounts, namespaces, cgroups and networking are unverified; this script does not invoke sudo.",
        },
        "frontend": selected,
        "tools": tools,
        "checks": checks,
        "blocking": blocking,
        "limitations": [
            "A device open does not verify KVM ioctls, ublk/TCMU device creation or a working frontend.",
            "Module status is read from sysfs; installed module files alone do not pass the check.",
            "Executable files and SHA256 do not verify versions, dynamic dependencies or TurboOCI support.",
            "No containerd socket is contacted; service health, snapshotter registration and compatible configuration are unverified.",
            "Images, cache state, resource budgets and real container/VM correctness remain unverified.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--overlaybd-dir",
        type=Path,
        help="Search this directory/bin or extracted /opt layout before /opt and PATH",
    )
    parser.add_argument(
        "--binary-dir",
        type=Path,
        default=REPO / "target/release",
        help="Exact directory containing executable pvisor and pvisor-cache (default: repo/target/release)",
    )
    parser.add_argument(
        "--frontend",
        choices=("auto", "ublk", "tcmu"),
        default="auto",
        help="Required block frontend; auto prefers ublk when its inspected prerequisites pass",
    )
    report = inspect(parser.parse_args())
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["ready"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
