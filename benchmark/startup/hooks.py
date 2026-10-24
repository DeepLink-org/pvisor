#!/usr/bin/env python3
"""Prepare fresh inputs and validate each paired startup trial."""

import json
import re
import shutil
import subprocess


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def archive(root):
    current = root / "current"
    if current.exists():
        history = root / "trials"
        history.mkdir(exist_ok=True)
        # The validated disk is disposable; retain output, bundles and timestamps.
        if (current / "condition.json").is_file():
            condition = json.loads((current / "condition.json").read_text())
            if condition.get("validated") is True:
                (current / "rootfs.ext4").unlink(missing_ok=True)
        current.rename(history / f"{len(list(history.iterdir())):05d}")


def prepare(root, backend):
    protocol = json.loads((root / "protocol.json").read_text())
    archive(root)
    current = root / "current"
    work = current / "workspace"
    current.mkdir(parents=True)
    stable = root / "workspace"
    stable.mkdir(exist_ok=True)
    for child in stable.iterdir():
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()
    work.symlink_to(stable, target_is_directory=True)
    (current / "stderr").touch()
    if protocol["assets"]:
        subprocess.run(
            ["cp", "-a", "--reflink=auto", protocol["assets"] + "/rootfs/work/.", str(work)],
            check=True,
        )
    (work / "startup-canary").write_bytes(b"unchanged startup input\n")
    (current / "runs").mkdir()
    save(current / "condition.json", {"backend": backend, "validated": False})
    if backend in ("fc-system", "qemu", "qemu-microvm"):
        subprocess.run(
            [
                "cp",
                "--reflink=auto",
                protocol["assets"] + "/agent-env.ext4",
                str(current / "rootfs.ext4"),
            ],
            check=True,
        )
    if backend == "fc-system":
        kernel = protocol["kernel"]["paths"]
        boot = {"kernel_image_path": kernel["kernel"], "boot_args": protocol["boot_args"]}
        if "initrd" in kernel:
            boot["initrd_path"] = kernel["initrd"]
        (current / "firecracker.log").touch()
        save(
            current / "firecracker.json",
            {
                "boot-source": boot,
                "drives": [
                    {
                        "drive_id": "rootfs",
                        "path_on_host": str(current / "rootfs.ext4"),
                        "is_root_device": True,
                        "is_read_only": False,
                    }
                ],
                "machine-config": {"vcpu_count": 2, "mem_size_mib": 128},
                "logger": {
                    "log_path": str(current / "firecracker.log"),
                    "level": "Warning",
                    "show_level": True,
                    "show_log_origin": True,
                },
            },
        )


def validate_output(stdout, stderr, guest=False):
    if re.search(r"kernel panic", stdout + "\n" + stderr, re.I):
        raise ValueError("guest kernel panic")
    lines = stdout.splitlines()
    ready = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_READY")]
    result = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_RESULT")]
    exits = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_EXIT")]
    if len(ready) != 1 or lines[ready[0]] != "REFERENCE_READY" or len(result) != 1:
        raise ValueError("missing or duplicate startup markers")
    if json.loads(lines[result[0]].removeprefix("REFERENCE_RESULT ")) != {
        "mode": "ready",
        "correctness": "passed",
    }:
        raise ValueError("incorrect startup result")
    if not ready[0] < result[0]:
        raise ValueError("startup markers out of order")
    if guest:
        if len(exits) != 1 or lines[exits[0]] != "REFERENCE_EXIT 0" or not result[0] < exits[0]:
            raise ValueError("guest did not report a unique ordered successful exit")
    elif exits:
        raise ValueError("unexpected guest exit marker in process output")


def validate(root, backend):
    protocol = json.loads((root / "protocol.json").read_text())
    current = root / "current"
    condition = json.loads((current / "condition.json").read_text())
    if condition != {"backend": backend, "validated": False}:
        raise ValueError("wrong trial condition or duplicate validation")
    stdout, stderr = ((current / name).read_text(errors="replace") for name in ("stdout", "stderr"))
    if backend == "fc-system":
        stderr += (current / "firecracker.log").read_text(errors="replace")
    validate_output(stdout, stderr, backend in ("fc-system", "qemu", "qemu-microvm"))
    if (current / "workspace/startup-canary").read_bytes() != b"unchanged startup input\n":
        raise ValueError("host workspace was modified")
    if backend.startswith("pvisor-"):
        bundles = (
            [current / "stage/run-bundle.json"]
            if backend in ("pvisor-staged", "pvisor-vm")
            else list((current / "runs").glob("run-*/run-bundle.json"))
        )
        bundles = [path for path in bundles if path.is_file()]
        if len(bundles) != 1:
            raise ValueError("expected exactly one Run Bundle")
        bundle = json.loads(bundles[0].read_text())
        run = bundle["run"]
        expected = (
            "virtual_machine"
            if backend == "pvisor-vm"
            else protocol["staged_isolation"]
            if backend == "pvisor-staged"
            else protocol["host_isolation"]
        )
        if (
            run["state"] != "completed"
            or run["exit_code"] != 0
            or run["executor"]["isolation"] != expected
        ):
            raise ValueError("wrong completed Run or observed isolation")
        staged = backend in ("pvisor-staged", "pvisor-vm")
        if bundle["safety"]["filesystem_changes_staged"] != staged:
            raise ValueError("wrong staging observation")
        if staged and any(path.is_file() for path in (current / "stage/upper").rglob("*")):
            raise ValueError("read-only startup payload produced staged file changes")
    condition["validated"] = True
    save(current / "condition.json", condition)


def cleanup_container(root):
    protocol = json.loads((root / "protocol.json").read_text())
    if "docker" not in protocol["backends"]:
        return
    docker = protocol["docker_command"]
    name = protocol["container_name"]
    listed = subprocess.check_output(
        [*docker, "ps", "--all", "--filter", "name=^" + name + "$", "--format", "{{.ID}}"],
        text=True,
        timeout=10,
    ).strip()
    if not listed:
        return
    result = subprocess.run(
        [*docker, "inspect", "--format", '{{index .Config.Labels "pvisor.startup.batch"}}', name],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if result.returncode != 0 or result.stdout.strip() != protocol["batch"]:
        raise ValueError("refuse to remove a container without this benchmark label")
    subprocess.run([*docker, "rm", "--force", name], check=True, capture_output=True, timeout=10)
