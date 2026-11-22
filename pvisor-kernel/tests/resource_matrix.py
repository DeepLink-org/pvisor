"""Linux x86_64/KVM acceptance for the embedded Rust native kernel."""

import argparse
import os
import platform
import shutil
import subprocess
import tempfile
from pathlib import Path

CASES = (
    (1, 16),
    (2, 16),
    (3, 16),
    (4, 16),
    (7, 16),
    (8, 16),
    (10, 16),
    (11, 17),
    (16, 24),
    (24, 36),
    (32, 48),
    (48, 72),
    (64, 96),
    (2, 256),
    (64, 256),
)
INPUT = bytes(range(256)) * 47


def resource_pair(value):
    try:
        cpus, memory = map(int, value.split(":"))
        if 1 <= cpus <= 64 and 1 <= memory <= 65536:
            return cpus, memory
    except ValueError:
        pass
    raise argparse.ArgumentTypeError("expected VCPUS:MIB (1–64 vCPUs, 1–65536 MiB)")


def check_case(binary, guest, cpus, memory, timeout):
    env = {**os.environ, "PVISOR_GUEST_KERNEL": "rust-native"}
    env.pop("RUST_LOG", None)
    with tempfile.TemporaryDirectory(prefix="pvisor-native-resources-") as directory:
        root = Path(directory)
        shutil.copy2(guest, root / "probe")
        command = [
            binary,
            "--cpus",
            str(cpus),
            "--memory",
            str(memory),
            "--task-stdio",
            "--rootfs",
            root,
            "--",
            "/probe",
            "7",
            "--echo-stdin",
            "--smp",
            str(cpus),
        ]
        try:
            result = subprocess.run(
                command, input=INPUT, capture_output=True, timeout=timeout, env=env
            )
        except subprocess.TimeoutExpired as error:
            return (
                f"timeout ({timeout}s); stdout={bytes(error.stdout or b'')[-120:]!r}; "
                f"stderr={bytes(error.stderr or b'')[-1000:]!r}"
            )

        expected_mask = (1 << cpus) - 1
        expected_stdout = (
            INPUT
            + f"RUST_NATIVE_KERNEL_SMP_OK {expected_mask:016x}\n".encode()
            + b"RUST_NATIVE_KERNEL_SHARED_ROOTFS_OK\n"
        )
        written = root / "pvisor-kernel-smoke"
        if (
            result.returncode != 7
            or result.stdout != expected_stdout
            or result.stderr != b"RUST_NATIVE_KERNEL_STDERR_OK\n"
            or not written.is_file()
            or written.read_bytes() != b"shared rootfs\nappended\n"
        ):
            return (
                f"exit={result.returncode}; stdout={result.stdout[-120:]!r}; "
                f"stderr={result.stderr[-1000:]!r}"
            )
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=Path, required=True, help="embedded vm_api_case executable"
    )
    parser.add_argument("--guest", type=Path, required=True, help="static tests/guest.rs workload")
    parser.add_argument("--case", type=resource_pair, action="append", help="VCPUS:MIB; repeatable")
    parser.add_argument("--repeat", type=int, default=1, help="fresh boots per pair")
    parser.add_argument("--timeout", type=int, default=30, help="deadline in seconds per boot")
    args = parser.parse_args()
    if args.repeat < 1 or args.timeout < 1:
        parser.error("--repeat and --timeout must be positive")
    if (platform.system(), platform.machine()) != ("Linux", "x86_64"):
        parser.error("requires Linux x86_64 with /dev/kvm")
    with Path("/dev/kvm").open("rb+"):
        pass
    binary = args.binary.resolve(strict=True)
    guest = args.guest.resolve(strict=True)
    failures = 0
    for cpus, memory in args.case or CASES:
        for trial in range(1, args.repeat + 1):
            failure = check_case(binary, guest, cpus, memory, args.timeout)
            failures += failure is not None
            print(
                f"{'FAIL' if failure else 'PASS'}: {cpus} vCPU, {memory} MiB, "
                f"run {trial}/{args.repeat}" + (f": {failure}" if failure else ""),
                flush=True,
            )
    return int(failures != 0)


if __name__ == "__main__":
    raise SystemExit(main())
