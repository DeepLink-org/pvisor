#!/usr/bin/env python3
"""Prepare an official complete Ubuntu VM image; pVisor never consumes this image.

Only regular files under --output are modified. No mounts, loop devices or host
package/service changes are used. Distribution kernel, initramfs, partitions,
modules and system services are retained. App provisioning is a separate QEMU
boot with user networking; measured Firecracker boots have private TAPs with working
user-mode DNS/NAT.
"""

import argparse
import json
import shutil
import subprocess
import time
from pathlib import Path

from reference_baselines import digest
from reference_fixture import prepare_fixture

BASE = "https://cloud-images.ubuntu.com/releases/resolute/release-20260927/"
FILES = {
    "ubuntu-26.04-server-cloudimg-amd64.img": "",
    "ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic": "unpacked/",
    "ubuntu-26.04-server-cloudimg-amd64-initrd-generic": "unpacked/",
}
PINNED = {
    "ubuntu-26.04-server-cloudimg-amd64.img": "8800651811af9a85465ad1d552add729947bb16488dddb4a9b5305a3d97332b2",
    "ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic": "7efd88a7facf80874d781ccb2c2421f0aaaa575e3b94760430619e826f490117",
    "ubuntu-26.04-server-cloudimg-amd64-initrd-generic": "c9f23b4ac34a20df6a1d344b44bf2597faa48799d814cbc206b3974c6ead5215",
}
NETWORK = """version: 2
ethernets:
  benchmark:
    match:
      macaddress: "06:00:ac:10:00:02"
    set-name: eth0
    addresses: [10.77.0.2/24]
    routes:
      - to: default
        via: 10.77.0.1
    nameservers:
      addresses: [10.77.0.3]
"""
SERVICE = """[Unit]
Description=Full Ubuntu benchmark
DefaultDependencies=no
After=multi-user.target cloud-final.service
Conflicts=shutdown.target
Before=shutdown.target
[Service]
Type=oneshot
ExecStart=/bin/sh /root/pvisor-benchmark.sh
StandardOutput=journal+console
StandardError=journal+console
SyslogIdentifier=reference-bench
[Install]
WantedBy=multi-user.target
"""


def run(argv, **kwargs):
    return subprocess.run(list(map(str, argv)), check=True, **kwargs)


def debugfs(root, command):
    result = run(["debugfs", "-w", "-R", command, root], capture_output=True, text=True)
    # debugfs exits zero even when a command fails.
    errors = (
        "File not found",
        "File exists",
        "Could not",
        "Command not found",
        "Usage:",
        "No space",
    )
    if any(e in result.stderr for e in errors):
        raise RuntimeError(result.stderr)
    return result


def partition(disk):
    table = json.loads(subprocess.check_output(["sfdisk", "--json", str(disk)], text=True))
    root = next(
        p for p in table["partitiontable"]["partitions"] if p.get("name") == "cloudimg-rootfs"
    )
    return table, root


def extract_root(disk, destination):
    _, root = partition(disk)
    run(
        [
            "dd",
            f"if={disk}",
            f"of={destination}",
            "iflag=skip_bytes,count_bytes",
            f"skip={root['start'] * 512}",
            f"count={root['size'] * 512}",
            "status=none",
        ]
    )


def merge_root(root, disk):
    _, part = partition(disk)
    run(
        [
            "dd",
            f"if={root}",
            f"of={disk}",
            "bs=1M",
            "oflag=seek_bytes",
            f"seek={part['start'] * 512}",
            "conv=notrunc",
            "status=none",
        ]
    )


def fetch(url, target):
    if not target.exists():
        temporary = target.with_suffix(target.suffix + ".partial")
        run(["curl", "-fL", "--retry", "3", "--output", temporary, url])
        temporary.replace(target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--extract-vmlinux",
        type=Path,
        help="Optional Linux scripts/extract-vmlinux; default unpacks Ubuntu zstd payload",
    )
    parser.add_argument(
        "--fixture",
        type=Path,
        help="Workspace inputs only; default creates the offline reference fixture",
    )
    parser.add_argument("--toolchain", type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    downloads = out / "downloads"
    downloads.mkdir(exist_ok=True)
    if args.fixture is None:
        args.fixture = out / "fixture"
        if not args.fixture.exists():
            prepare_fixture(args.fixture)
    identities = {}
    for subdir in ("", "unpacked/"):
        fetch(
            BASE + subdir + "SHA256SUMS",
            downloads / ("unpacked-SHA256SUMS" if subdir else "SHA256SUMS"),
        )
    for name, subdir in FILES.items():
        target = downloads / name
        alias = {
            "ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic": "ubuntu-vmlinuz",
            "ubuntu-26.04-server-cloudimg-amd64-initrd-generic": "ubuntu-initrd",
        }.get(name)
        if alias and (downloads / alias).exists() and not target.exists():
            shutil.copy2(downloads / alias, target)
        fetch(BASE + subdir + name, target)
        manifest = downloads / ("unpacked-SHA256SUMS" if subdir else "SHA256SUMS")
        expected = next(
            s.split()[0]
            for s in manifest.read_text().splitlines()
            if s.split()[-1].lstrip("*") == name
        )
        actual = digest(target)
        if expected != actual or actual != PINNED[name]:
            raise ValueError(f"SHA256 mismatch: {name}")
        identities[name] = {
            "url": BASE + subdir + name,
            "sha256": actual,
            "bytes": target.stat().st_size,
        }
    kernel = out / "ubuntu-vmlinux"
    source = downloads / "ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic"
    if args.extract_vmlinux:
        with kernel.open("wb") as stream:
            run(["bash", args.extract_vmlinux.resolve(), source], stdout=stream)
    else:
        data = source.read_bytes()
        offset = data.find(bytes.fromhex("28b52ffd"))
        if offset < 0:
            raise RuntimeError(
                "Expected Ubuntu zstd kernel; use --extract-vmlinux for another compression"
            )
        unpacked = subprocess.run(
            ["zstd", "-dq", "--stdout"], input=data[offset:], capture_output=True
        )
        if not unpacked.stdout.startswith(b"\x7fELF"):
            raise RuntimeError("Ubuntu kernel extraction did not produce ELF")
        kernel.write_bytes(unpacked.stdout)
    header = kernel.read_bytes()[:20]
    if header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
        raise RuntimeError("Expected unmodified Ubuntu x86_64 ELF kernel")
    original = out / "ubuntu-original.raw"
    if not original.exists():
        run(
            [
                "qemu-img",
                "convert",
                "-f",
                "qcow2",
                "-O",
                "raw",
                downloads / "ubuntu-26.04-server-cloudimg-amd64.img",
                original,
            ]
        )
    root = out / "root-clean.ext4"
    extract_root(original, root)
    injection = out / "injection-final"
    injection.mkdir(exist_ok=True)
    content = {
        "/etc/systemd/system/reference-benchmark.service": SERVICE,
        "/var/lib/cloud/seed/nocloud/meta-data": "instance-id: pvisor-ubuntu-reference\nlocal-hostname: pvisor-ubuntu-reference\n",
        "/var/lib/cloud/seed/nocloud/user-data": "#cloud-config\nssh_pwauth: false\n",
        "/var/lib/cloud/seed/nocloud/network-config": NETWORK,
        "/etc/netplan/50-cloud-init.yaml": "network:\n"
        + "".join("  " + s + "\n" for s in NETWORK.splitlines()),
        "/root/pvisor-benchmark.sh": Path(__file__).with_name("ubuntu_guest.sh").read_text(),
    }
    for path in ("/var/lib/cloud", "/var/lib/cloud/seed", "/var/lib/cloud/seed/nocloud"):
        debugfs(root, f"mkdir {path}")
    for index, (dest, text) in enumerate(content.items()):
        src = injection / str(index)
        src.write_text(text)
        debugfs(root, f"write {src} {dest}")
        if dest.endswith(".yaml"):
            debugfs(root, f"set_inode_field {dest} mode 0100600")
        got = subprocess.check_output(["debugfs", "-R", f"cat {dest}", str(root)], text=True)
        if got != text:
            raise RuntimeError(f"Injection verification failed: {dest}")
    debugfs(
        root,
        "symlink /etc/systemd/system/multi-user.target.wants/reference-benchmark.service ../reference-benchmark.service",
    )
    stock = out / "ubuntu-stock.raw"
    run(["cp", "--reflink=auto", original, stock])
    merge_root(root, stock)
    table, part = partition(stock)
    (out / "partition-layout.json").write_text(json.dumps(table, indent=2) + "\n")
    # Payload contains toolchain, clients and task inputs, never a replacement OS.
    payload = out / "payload"
    if payload.exists():
        shutil.rmtree(payload)
    payload.mkdir()
    toolchain = args.toolchain or Path(
        subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    )
    for source, target in (
        (toolchain, payload / "opt/toolchain"),
        (args.fixture.resolve(), payload / "work"),
        (Path(__file__).parent, payload / "bench/harness"),
    ):
        target.parent.mkdir(parents=True, exist_ok=True)
        run(["cp", "--reflink=auto", "-a", source, target])
    shutil.copy2(
        Path(__file__).with_name("reference_workload.py"), payload / "bench/reference_workload.py"
    )
    for name in ("@anthropic-ai/claude-code", "@openai/codex"):
        target = payload / "usr/local/lib/node_modules" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        run(["cp", "--reflink=auto", "-a", Path("/usr/local/lib/node_modules") / name, target])
    (payload / "usr/local/bin").mkdir(parents=True, exist_ok=True)
    for name in ("claude", "codex"):
        (payload / "usr/local/bin" / name).symlink_to(
            "../lib/node_modules/"
            + (
                "@anthropic-ai/claude-code/bin/claude.exe"
                if name == "claude"
                else "@openai/codex/bin/codex.js"
            )
        )
    payload_disk = out / "payload.ext4"
    with payload_disk.open("wb") as stream:
        stream.truncate(5 * 1024**3)
    run(["mkfs.ext4", "-q", "-F", "-d", payload, payload_disk])
    # Extend only the image's last root partition; boot/EFI partitions are retained.
    provisioned = out / "ubuntu-agent.raw"
    run(["cp", "--reflink=auto", stock, provisioned])
    with provisioned.open("r+b") as stream:
        stream.truncate(12 * 1024**3)
    run(["sfdisk", "--relocate", "gpt-bak-std", provisioned])
    run(
        ["sfdisk", "--no-reread", "--no-tell-kernel", "--wipe", "never", "-N", "1", provisioned],
        input=", +\n",
        text=True,
    )
    manifest = {
        "schema": "pvisor-ubuntu-reference-assets/v1",
        "release": "Ubuntu 26.04 LTS (20260927)",
        "image_policy": "Firecracker/QEMU consume Ubuntu; pVisor uses --rootfs host and its own embedded kernel",
        "assets": identities,
        "kernel_elf_sha256": digest(kernel),
        "guest_workload_sha256": digest(payload / "bench/reference_workload.py"),
        "initrd": str(downloads / "ubuntu-26.04-server-cloudimg-amd64-initrd-generic"),
        "root_partuuid": part["uuid"].lower(),
        "preserved": "Full GPT, /boot, EFI, stock kernel/initrd/modules/services",
        "sha_verification": "Official HTTPS SHA256SUMS; GPG signature not verified",
        "provisioning": "QEMU KVM, user networking, native Ubuntu apt tools plus installed Rust/Claude/Codex; outside timings",
    }
    (out / "assets.json").write_text(json.dumps(manifest, indent=2) + "\n")
    command = [
        "taskset",
        "--cpu-list",
        "0,1",
        "qemu-system-x86_64",
        "-machine",
        "q35",
        "-accel",
        "kvm",
        "-cpu",
        "host,-rdseed,-rdrand",
        "-smp",
        "1",
        "-m",
        "4096",
        "-nodefaults",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-no-reboot",
        "-kernel",
        str(downloads / "ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic"),
        "-initrd",
        manifest["initrd"],
        "-append",
        "console=ttyS0 root=PARTUUID="
        + part["uuid"].lower()
        + " rw reboot=k panic=1 quiet pvbench.provision=1",
        "-drive",
        f"file={provisioned},format=raw,if=none,id=root",
        "-device",
        "virtio-blk-pci,drive=root",
        "-drive",
        f"file={payload_disk},format=raw,if=none,id=payload,readonly=on",
        "-device",
        "virtio-blk-pci,drive=payload",
        "-netdev",
        "user,id=net,net=10.77.0.0/24,host=10.77.0.1,dns=10.77.0.3",
        "-device",
        "virtio-net-pci,netdev=net,mac=06:00:ac:10:00:02",
    ]
    (out / "provision-command.json").write_text(json.dumps(command, indent=2) + "\n")
    start = time.monotonic()
    with (out / "provision.log").open("wb") as log:
        run(command, stdout=log, stderr=subprocess.STDOUT, timeout=1800)
    if "REFERENCE_PROVISIONED" not in (out / "provision.log").read_text():
        raise RuntimeError("Ubuntu tool provisioning did not complete; see provision.log")
    manifest["provisioning_shape"] = (
        "Untimed QEMU: 1 vCPU host,-rdseed,-rdrand; measured Firecracker: 2 vCPU defaults"
    )
    manifest["provision_seconds"] = time.monotonic() - start
    manifest['prepared_sha256']={name:digest(out/name) for name in ('ubuntu-stock.raw','ubuntu-agent.raw','payload.ext4')}
    manifest['preparation_harness_sha256']={name:digest(Path(__file__).with_name(name)) for name in ('prepare_ubuntu_reference.py','ubuntu_guest.sh','reference_workload.py')}
    (out / "assets.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
