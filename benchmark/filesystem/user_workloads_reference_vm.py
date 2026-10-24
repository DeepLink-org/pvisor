#!/usr/bin/env python3
"""B-FS-TOOLS prepared stock-kernel Firecracker and QEMU launch adapters.

This helper prepares private block-backed environments for the same frozen
static workload and tools as the ordinary CLI conditions. Preparation, extraction
and host validation are outside Ready and payload phase clocks. No measurement
runs on import. All paths a cleanup operation can remove belong to its trial.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

BACKENDS = ("fc-system", "qemu", "qemu-microvm")
HARNESS = ("user_workloads_reference_vm.py",)
ROOT_BYTES = 8 << 30
WORK_BYTES = 8 << 30

# The retained stock initrd loads its original virtio_mmio module, mounts vda
# and enters /bench/init. This small PID1 supplies no distribution services.
INIT_SOURCE = r"""
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
static int failed = 0;
static void check(int rc, const char *what) {
    if (rc < 0) { perror(what); failed = 1; }
}
int main(void) {
    setbuf(stdout, NULL); setbuf(stderr, NULL);
    if (getpid() != 1) return 2;
    check(mount("proc", "/proc", "proc", 0, NULL), "proc");
    check(mount("sysfs", "/sys", "sysfs", 0, NULL), "sysfs");
    check(mount("tmpfs", "/tmp", "tmpfs", 0, "mode=1777,size=256m"), "tmpfs");
    check(mount("tmpfs", "/run", "tmpfs", 0, "mode=0755,size=16m"), "run");
    check(mount(NULL, "/", NULL, MS_REMOUNT | MS_RDONLY, NULL), "readonly root");
    char cmdline[8192], task[4] = "", *state;
    FILE *cmd = fopen("/proc/cmdline", "r");
    if (!cmd || !fgets(cmdline, sizeof(cmdline), cmd)) failed = 1;
    if (cmd) fclose(cmd);
    if (!failed) {
        for (char *p = strtok_r(cmdline, " \n", &state); p; p = strtok_r(NULL, " \n", &state))
            if (!strncmp(p, "fsbench.case=", 13) && strlen(p + 13) == 3)
                memcpy(task, p + 13, 4);
        if (!strchr("MRS", task[0]) || task[1] < '0' || task[1] > '9' ||
                task[2] < '0' || task[2] > '9') failed = 1;
    }
    for (int i = 0; i < 500 && access("/dev/vdb", F_OK); i++) usleep(10000);
    check(mount("/dev/vdb", "/bench-work", "ext4", 0, NULL), "work disk");
    check(mount("/fs-v2-fixture", "/bench-work/input", NULL, MS_BIND, NULL), "input bind");
    check(mount(NULL, "/bench-work/input", NULL, MS_BIND | MS_REMOUNT | MS_RDONLY, NULL), "readonly input");
    check(chdir("/bench-work"), "work cwd");
    int rc = 1;
    if (!failed) {
        setenv("HOME", "/tmp", 1); setenv("PATH", "/usr/bin:/bin", 1);
        setenv("LC_ALL", "C", 1); setenv("GIT_CONFIG_NOSYSTEM", "1", 1);
        setenv("GIT_CONFIG_GLOBAL", "/dev/null", 1);
        setenv("PYTHONDONTWRITEBYTECODE", "1", 1);
        setenv("PYTHONPYCACHEPREFIX", "/__pvisor_reference_no_pyc__", 1);
        pid_t child = fork();
        if (child == 0) {
            char *args[] = {"/bench-artifacts/payload", task, "output", "input", "3", "512", NULL};
            execv(args[0], args); perror("exec frozen payload"); _exit(127);
        }
        if (child < 0) { perror("fork"); failed = 1; }
        else {
            int status;
            while (waitpid(child, &status, 0) < 0) if (errno != EINTR) { failed = 1; break; }
            if (!failed) rc = WIFEXITED(status) ? WEXITSTATUS(status) : 128;
        }
    }
    check(chdir("/"), "leave work cwd");
    sync();
    check(umount("/bench-work/input"), "unmount input");
    check(umount("/bench-work"), "unmount work");
    printf("USER_WORKLOADS_VM_EXIT {\"returncode\":%d,\"teardown_ok\":%s}\n", rc,
           failed ? "false" : "true");
    sync();
    reboot(RB_AUTOBOOT);
    perror("reboot");
    for (;;) pause();
}
"""


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def _run(command, *, log=None):
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    if log is not None:
        Path(log).write_text(result.stdout + result.stderr)
    if result.returncode:
        raise RuntimeError(
            "reference VM preparation failed: " + repr(command) + "\n" + result.stderr[-3000:]
        )
    return result


def _kernel_receipt(receipt):
    receipt = Path(receipt).resolve(strict=True)
    record = json.loads(receipt.read_text())
    if (
        record.get("schema") != "pvisor-firecracker-kernel/v1"
        or record.get("variant") != "fc-system"
    ):
        raise ValueError("an explicit fc-system stock kernel receipt is required")
    provenance = record.get("provenance", {})
    if (
        provenance.get("origin") != "official-distro-stock"
        or provenance.get("pvisor_firmware") is not False
    ):
        raise ValueError("reference kernels must have independent official stock provenance")
    for key in ("distribution", "package", "package_version", "source_url"):
        if not isinstance(provenance.get(key), str) or not provenance[key].strip():
            raise ValueError("missing stock package provenance: " + key)
    names = {"kernel", "vmlinuz", "config", "provenance", "extractor", "initrd"}
    if set(record.get("artifacts", {})) != names:
        raise ValueError("the stock VM protocol requires a complete kernel and module initrd")
    paths = {}
    for name, artifact in record["artifacts"].items():
        path = receipt.parent / artifact["file"]
        if path.parent.resolve() != receipt.parent or path.is_symlink() or not path.is_file():
            raise ValueError("kernel artifacts must be retained regular files")
        if sha(path) != artifact["sha256"]:
            raise ValueError("stock kernel artifact changed: " + name)
        paths[name] = path
    if json.loads(paths["provenance"].read_text()) != provenance:
        raise ValueError("stock provenance bytes differ from receipt")
    with paths["kernel"].open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            raise ValueError("Firecracker requires the extracted stock ELF")
    return record, paths


def prepare_assets(batch, stock_receipt_path):
    """Freeze VMM/kernel inputs and make common root/work ext4 templates.

    ``batch/toolroot`` must already contain the common tools, fixture and static
    payload wrapper. Only new ``reference-vm`` paths are created. The returned
    mapping is JSON serializable; merge its three backend keys into
    ``protocol.reference_configuration``. All retained assets are below
    ``batch/reference-assets/vm`` for the common cohort artifact inventory.
    """
    batch = Path(batch).resolve(strict=True)
    root = batch / "reference-assets/vm"
    root.parent.mkdir(exist_ok=True)
    root.mkdir(mode=0o700)
    toolroot = batch / "toolroot"
    if not all(
        (toolroot / name).is_file()
        for name in ("bench-artifacts/payload", "bench-artifacts/workload", "usr/bin/cc")
    ):
        raise ValueError("common prepared toolroot and frozen payload are required")
    record, original = _kernel_receipt(stock_receipt_path)
    kernel = root / "stock-kernel"
    kernel.mkdir()
    for name, path in original.items():
        shutil.copy2(path, kernel / record["artifacts"][name]["file"])
    shutil.copy2(stock_receipt_path, kernel / "kernel-receipt.json")
    _kernel_receipt(kernel / "kernel-receipt.json")
    binary = root / "bin"
    binary.mkdir()
    runtimes = {}
    for name in ("firecracker", "qemu-system-x86_64"):
        source = Path(shutil.which(name) or "")
        if not source.is_file():
            raise ValueError("reference VMM unavailable: " + name)
        frozen = binary / name
        shutil.copy2(source, frozen)
        runtimes[name] = dict(
            source=str(source),
            executable=str(frozen),
            sha256=sha(frozen),
            version=_run([str(frozen), "--version"]).stdout.strip(),
        )
    # A relocated QEMU executable searches data relative to its new location.
    # Retain the actual PC BIOS, microvm qboot, and boot option ROM bytes and
    # supply both -bios and -L rather than depending on installation layout.
    qemu_data = root / "qemu-data"
    qemu_data.mkdir()
    qemu_firmware = {}
    firmware_sources = [Path("/usr/share/seabios/bios-256k.bin")]
    firmware_sources.extend(
        sorted(
            path
            for path in Path("/usr/share/qemu").iterdir()
            if path.is_file() and path.suffix in (".bin", ".rom")
        )
    )
    for path in firmware_sources:
        if not path.is_file():
            raise ValueError("QEMU boot firmware unavailable: " + str(path))
        target = qemu_data / path.name
        shutil.copy2(path, target)
        qemu_firmware[path.name] = dict(source=str(path), path=str(target), sha256=sha(target))
    if not {"bios-256k.bin", "qboot.rom", "linuxboot_dma.bin"} <= set(qemu_firmware):
        raise ValueError("QEMU requires retained PC/microvm firmware and Linux boot ROM")
    source = root / "init.c"
    source.write_text(INIT_SOURCE)
    init = root / "init"
    command = ["gcc", "-static", "-Os", "-Wall", "-Werror", "-o", str(init), str(source)]
    _run(command, log=root / "init-build.log")
    # Add the tiny PID1 only to a new reflink tree, never the shared input.
    image_tree = root / "image-tree"
    image_tree.mkdir()
    _run(["cp", "-a", "--reflink=auto", str(toolroot) + "/.", str(image_tree)])
    (image_tree / "bench-work").mkdir(exist_ok=True)
    (image_tree / "bench").mkdir(exist_ok=True)
    destination = image_tree / "bench/init"
    if destination.exists() or destination.is_symlink():
        destination.unlink()
    shutil.copy2(init, destination)
    root_disk = root / "root.ext4"
    with root_disk.open("xb") as stream:
        stream.truncate(ROOT_BYTES)
    mkroot = [
        "mke2fs",
        "-q",
        "-F",
        "-t",
        "ext4",
        "-b",
        "4096",
        "-m",
        "0",
        "-E",
        "lazy_itable_init=0,lazy_journal_init=0",
        "-d",
        str(image_tree),
        str(root_disk),
    ]
    _run(mkroot, log=root / "root-prepare.log")
    shutil.rmtree(image_tree)
    seed = root / "work-seed"
    seed.mkdir()
    (seed / "input").mkdir()
    (seed / "output").mkdir()
    work_disk = root / "work.ext4"
    with work_disk.open("xb") as stream:
        stream.truncate(WORK_BYTES)
    mkwork = [
        "mke2fs",
        "-q",
        "-F",
        "-t",
        "ext4",
        "-b",
        "4096",
        "-m",
        "0",
        "-E",
        "lazy_itable_init=0,lazy_journal_init=0",
        "-d",
        str(seed),
        str(work_disk),
    ]
    _run(mkwork, log=root / "work-prepare.log")
    shutil.rmtree(seed)
    configurations = {}
    for backend in BACKENDS:
        configurations[backend] = dict(
            executor="Firecracker"
            if backend == "fc-system"
            else "QEMU microvm"
            if backend == "qemu-microvm"
            else "QEMU q35",
            guest_vcpus=2,
            guest_memory_mib=512,
            kernel=record["provenance"],
            kernel_receipt=str(kernel / "kernel-receipt.json"),
            storage="private root and workspace ext4/virtio-blk; root remounted readonly; input readonly bind",
            transport="virtio-mmio" if backend != "qemu" else "virtio-pci",
            runtime=runtimes["firecracker" if backend == "fc-system" else "qemu-system-x86_64"],
            userspace="same frozen prepared tools and static payload; minimal PID1, no distro services",
            durability="payload file/directory synchronization retained; normal work unmount/reboot-to-exit outside completion clock",
            device_cache="Firecracker Writeback with flush enabled"
            if backend == "fc-system"
            else "QEMU cache=writeback with flush enabled",
            network="no network device",
        )
        if backend != "fc-system":
            configurations[backend]["boot_firmware"] = qemu_firmware[
                "qboot.rom" if backend == "qemu-microvm" else "bios-256k.bin"
            ]
            configurations[backend]["boot_option_roms"] = qemu_firmware
    artifacts = {
        str(path.relative_to(batch)): sha(path) for path in root.rglob("*") if path.is_file()
    }
    result = dict(
        schema=1,
        root_disk=str(root_disk),
        work_disk=str(work_disk),
        root_disk_bytes=ROOT_BYTES,
        work_disk_bytes=WORK_BYTES,
        kernel_receipt=str(kernel / "kernel-receipt.json"),
        kernel_paths={name: str(kernel / record["artifacts"][name]["file"]) for name in original},
        runtimes=runtimes,
        qemu_data_dir=str(qemu_data),
        qemu_firmware=qemu_firmware,
        init_build_command=command,
        image_commands=[mkroot, mkwork],
        artifacts=artifacts,
    )
    save(root / "receipt.json", result)
    result["artifacts"][str((root / "receipt.json").relative_to(batch))] = sha(
        root / "receipt.json"
    )
    for configuration in configurations.values():
        configuration["assets"] = result
    return configurations


def verify_assets(batch, protocol):
    """Recheck frozen reference bytes at campaign boundaries, outside timing."""
    assets = next(
        value["assets"]
        for key, value in protocol["reference_configuration"].items()
        if key in BACKENDS
    )
    _kernel_receipt(assets["kernel_receipt"])
    for relative, digest in assets["artifacts"].items():
        if sha(Path(batch) / relative) != digest:
            raise ValueError("reference artifact changed: " + relative)


def _boot_args(case, backend):
    if not re.fullmatch(r"[MRS][0-9]{2}", case):
        raise ValueError("a registered workload case ID is required")
    # Keep the RTC for stock kernels: removing it causes driver timeout waits.
    reboot = "t" if backend == "qemu-microvm" else "k"
    pci = " pci=off" if backend == "qemu-microvm" else ""
    return f"console=ttyS0 reboot={reboot} panic=1 root=/dev/vda rw init=/bench/init quiet{pci} fsbench.case={case}"


def build_command(batch, protocol, folder, case, backend):
    """Prepare private disks/config and return a pre-exec-marked VMM command."""
    if backend not in BACKENDS:
        raise ValueError("unknown stock reference VM")
    if protocol.get("rounds", 3) != 3 or protocol.get("ram_mib", 512) != 512:
        raise ValueError("the shallow protocol requires three rounds and 512 MiB")
    folder = Path(folder).resolve(strict=True)
    assets = protocol["reference_configuration"][backend]["assets"]
    for key in ("root", "work"):
        disk = folder / (key + ".ext4")
        if disk.exists():
            raise FileExistsError(disk)
        _run(["cp", "--reflink=auto", "--sparse=always", assets[key + "_disk"], str(disk)])
    paths = assets["kernel_paths"]
    boot = _boot_args(case, backend)
    if backend == "fc-system":
        config = {
            "boot-source": {
                "kernel_image_path": paths["kernel"],
                "initrd_path": paths["initrd"],
                "boot_args": boot,
            },
            "machine-config": {"vcpu_count": 2, "mem_size_mib": 512},
            "drives": [
                {
                    "drive_id": name,
                    "path_on_host": str(folder / (name + ".ext4")),
                    "is_root_device": name == "root",
                    "is_read_only": False,
                    "cache_type": "Writeback",
                }
                for name in ("root", "work")
            ],
        }
        (folder / "firecracker.log").touch(exist_ok=False)
        config["logger"] = dict(
            log_path=str(folder / "firecracker.log"),
            level="Warning",
            show_level=True,
            show_log_origin=True,
        )
        config_path = folder / "firecracker.json"
        save(config_path, config)
        argv = [
            assets["runtimes"]["firecracker"]["executable"],
            "--no-api",
            "--config-file",
            str(config_path),
        ]
    else:
        machine = (
            "microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=on"
            if backend == "qemu-microvm"
            else "q35"
        )
        argv = [
            assets["runtimes"]["qemu-system-x86_64"]["executable"],
            "-machine",
            machine,
            "-accel",
            "kvm",
            "-cpu",
            "host",
            "-smp",
            "2",
            "-m",
            "512",
            "-nodefaults",
            "-L",
            assets["qemu_data_dir"],
            "-bios",
            assets["qemu_firmware"]["qboot.rom" if backend == "qemu-microvm" else "bios-256k.bin"][
                "path"
            ],
            "-display",
            "none",
            "-serial",
            "stdio",
            "-no-reboot",
            "-kernel",
            paths["vmlinuz"],
            "-initrd",
            paths["initrd"],
            "-append",
            boot,
        ]
        for name in ("root", "work"):
            argv += [
                "-drive",
                f"file={folder / (name + '.ext4')},format=raw,if=none,id={name},cache=writeback",
                "-device",
                ("virtio-blk-device" if backend == "qemu-microvm" else "virtio-blk-pci")
                + ",drive="
                + name,
            ]
    launch = folder / "reference-launch.json"
    save(
        launch,
        dict(
            schema=1,
            backend=backend,
            case_id=case,
            argv=argv,
            storage="two private ext4 images; readonly root/input after PID1 setup",
            guest_vcpus=2,
            guest_memory_mib=512,
        ),
    )
    environment = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("PVISOR_", "WORKLOADS_V2_"))
        and k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
    }
    environment.update(PATH="/usr/bin:/bin", LC_ALL="C")
    return [sys.executable, str(Path(__file__).resolve()), "--launch", str(launch)], environment


def finalize_trial(batch, protocol, folder, case, backend):
    """After normal VMM exit, export clean workspace output for the host oracle.

    This read-only check refuses guest failures/panics and an unclean filesystem.
    It does not turn controlled VMM termination into successful completion.
    Returned output is an ordinary physical directory, not an OverlayFS upper.
    """
    if backend not in BACKENDS:
        raise ValueError("unknown stock reference VM")
    folder = Path(folder).resolve(strict=True)
    text = (folder / "stdout.log").read_text()
    if re.search(r"kernel panic|not syncing|BUG:|Oops:", text, re.IGNORECASE):
        raise ValueError("reference guest kernel failed")
    exits = [
        json.loads(line.split(" ", 1)[1])
        for line in text.splitlines()
        if line.startswith("USER_WORKLOADS_VM_EXIT ")
    ]
    if exits != [dict(returncode=0, teardown_ok=True)]:
        raise ValueError("reference payload or clean guest teardown failed")
    disk = folder / "work.ext4"
    _run(["e2fsck", "-f", "-n", str(disk)], log=folder / "work-fsck.log")
    view = folder / "reference-output"
    view.mkdir()
    exported = _run(
        ["debugfs", "-R", "rdump /output " + str(view), str(disk)],
        log=folder / "output-extraction.log",
    )
    if "File not found" in exported.stderr or not (view / "output").is_dir():
        raise ValueError("reference output extraction failed")
    save(
        folder / "reference-finalization.json",
        dict(
            schema=1,
            backend=backend,
            case_id=case,
            guest_exit=exits[0],
            clean_work_filesystem=True,
            output=str(view / "output"),
        ),
    )
    return view / "output"


def cleanup_trial(batch, protocol, folder, case, backend):
    """Check private image ownership; the coordinator reclaims valid trials.

    Failed image bytes remain available for diagnosis. The common coordinator
    deletes these two files only after its independent host oracle succeeds.
    VMM descendants share its process group and are reaped by its timeout path.
    """
    folder = Path(folder).resolve(strict=True)
    for name in ("root.ext4", "work.ext4"):
        path = folder / name
        if path.is_symlink():
            raise ValueError("refusing to clean an aliased private VM image")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--launch", type=Path, required=True)
    args = parser.parse_args()
    description = json.loads(args.launch.read_text())
    print("USER_WORKLOADS_LAUNCH " + json.dumps(dict(monotonic_ns=time.monotonic_ns())), flush=True)
    os.execvpe(description["argv"][0], description["argv"], os.environ)


if __name__ == "__main__":
    main()
