"""Check host visibility of a writable guest mapping before munmap and exit."""

import argparse
import os
import resource
import select
import shutil
import signal
import subprocess
import tempfile
from pathlib import Path


def limit_host_writes():
    signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
    resource.setrlimit(resource.RLIMIT_FSIZE, (4096, 4096))


def check(binary: Path, guest: Path, mode: str):
    fail = mode == "msync-error"
    original = bytes(8192)
    expected = original if fail else original[:4097] + b"x" + original[4098:]
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        shutil.copy2(guest, root / "probe")
        mapped = root / "mapped-sync"
        mapped.write_bytes(original)
        with subprocess.Popen(
            [binary, "--task-stdio", "--rootfs", root, "--", "/probe", "7", f"--mapped-{mode}"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "PVISOR_GUEST_KERNEL": "rust-native"},
            preexec_fn=limit_host_writes if fail else None,
        ) as vm:
            try:
                ready, _, _ = select.select([vm.stdout], [], [], 30)
                marker = b"MAPPED_SYNC_OK\n"
                if not ready or os.read(vm.stdout.fileno(), len(marker)) != marker:
                    raise RuntimeError(f"{mode}: guest did not acknowledge sync")
                if mapped.read_bytes() != expected:
                    raise RuntimeError(f"{mode}: host bytes differ while guest mapping is live")
                stdout, stderr = vm.communicate(input=b"\x01", timeout=30)
                if (
                    vm.returncode != (125 if fail else 7)
                    or stdout != b"RUST_NATIVE_KERNEL_SHARED_ROOTFS_OK\n"
                    or (fail and b"guest writeback failed:" not in stderr)
                ):
                    raise RuntimeError(f"{mode}: exit {vm.returncode}, stderr {stderr[-1000:]!r}")
            finally:
                if vm.poll() is None:
                    vm.kill()
                    vm.wait()
        if mapped.read_bytes() != expected:
            raise RuntimeError(f"{mode}: host bytes differ after exit")
    print(f"PASS: mapped {mode} while live and at exit")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    args = parser.parse_args()
    for mode in ("syncfs", "msync", "msync-error"):
        check(args.binary.resolve(strict=True), args.guest, mode)
