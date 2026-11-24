"""Strict named-stdio workload check on KVM or HVF with an embedded Rust guest kernel."""

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="vm_api_case executable")
    parser.add_argument("--guest", type=Path, required=True, help="static tests/guest.rs workload")
    parser.add_argument("--cpus", type=int, default=2)
    args = parser.parse_args()
    data = bytes(range(256)) * 47
    for payload in (b"", data):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy2(args.guest, root / "probe")
            result = subprocess.run(
                [
                    args.binary.resolve(strict=True),
                    "--cpus",
                    str(args.cpus),
                    "--task-stdio",
                    "--rootfs",
                    root,
                    "--",
                    "/probe",
                    "7",
                    "--echo-stdin",
                ],
                input=payload,
                capture_output=True,
                timeout=30,
                env={**os.environ, "PVISOR_GUEST_KERNEL": "rust-native"},
            )
            if (
                result.returncode != 7
                or result.stdout != payload + b"RUST_NATIVE_KERNEL_SHARED_ROOTFS_OK\n"
                or result.stderr != b"RUST_NATIVE_KERNEL_STDERR_OK\n"
                or (root / "pvisor-kernel-smoke").read_bytes() != b"shared rootfs\nappended\n"
            ):
                raise RuntimeError(
                    f"input={len(payload)} exit={result.returncode} "
                    f"stdout={result.stdout[-100:]!r} stderr={result.stderr[-500:]!r}"
                )
            print(f"PASS: {len(payload)} input bytes, EOF, independent output streams, exit 7")


if __name__ == "__main__":
    main()
