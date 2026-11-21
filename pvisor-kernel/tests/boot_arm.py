"""ARM64 boot-only TCG diagnostic; deliberately supplies no rootfs devices."""

import argparse
import json
import struct
import subprocess
import tempfile
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--qemu", required=True)
    parser.add_argument("--bundle", type=Path, required=True)
    args = parser.parse_args()
    metadata = json.loads((args.bundle / "kernel.json").read_text())
    if (
        metadata["architecture"] != "aarch64"
        or metadata["guest_addr"] != 0x80200000
        or metadata["entry_addr"] != 0x80200000
    ):
        parser.error("requires the pVisor aarch64 bundle")
    with tempfile.TemporaryDirectory(prefix="pvisor-arm-boot-") as temporary:
        shim = Path(temporary) / "entry.bin"
        # QEMU virt RAM starts at 0x40000000. Preserve its x0/FDT while branching
        # to the fixed pVisor Image address within the 1536 MiB RAM allocation.
        shim.write_bytes(
            struct.pack("<IIQQQQQQII", 0x14000010, 0, 0x200000, 80, 2, 0, 0, 0, 0x644D5241, 0)
            + struct.pack("<IIQ", 0x58000044, 0xD61F0080, 0x80200000)
        )
        result = subprocess.run(
            [
                args.qemu,
                "-machine",
                "virt,gic-version=3,virtualization=off",
                "-accel",
                "tcg",
                "-cpu",
                "cortex-a72",
                "-m",
                "1536",
                "-smp",
                "1",
                "-nodefaults",
                "-display",
                "none",
                "-serial",
                "stdio",
                "-monitor",
                "none",
                "-nic",
                "none",
                "-no-reboot",
                "-kernel",
                str(shim),
                "-device",
                f"loader,file={(args.bundle / 'kernel.bin').resolve()},addr=0x80200000,force-raw=on",
                "-append",
                "earlycon root=/dev/root rootfstype=virtiofs init=/init.krun",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=30,
            check=True,
            text=True,
        )
    print(result.stdout, end="")
    for marker in (
        "Found and initialized GICv3",
        "PSCI detected: Hvc",
        "OSTD initialized",
        "Spawn the first kernel thread",
        "virtiofs device is not found",
    ):
        if marker not in result.stdout:
            raise RuntimeError(f"ARM boot did not reach expected stage: {marker}")
    print("BOOT-ONLY diagnostic passed; missing rootfs was expected. No workload was executed.")


if __name__ == "__main__":
    main()
