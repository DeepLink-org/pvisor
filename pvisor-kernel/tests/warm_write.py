"""Check a short write and its live visibility through a read-warmed virtio-fs inode."""

import argparse
import asyncio
import os
import shutil
import signal
import sys
import tempfile
from pathlib import Path

LIMIT = 1 << 30


async def check(process, subject):
    for step in (b"warm\n", b"written\n"):
        actual = await process.stdout.readline()
        if actual != step:
            raise RuntimeError(f"expected {step!r}, got {actual!r}")
        with subject.open("r+b", buffering=0) as file:
            file.seek(LIMIT - 8)
            if step == b"warm\n":
                if file.write(b"HOST") != 4:
                    raise RuntimeError("short fixture write")
            elif file.read(13) != b"HOST.parkeep":
                raise RuntimeError("host did not see the exact accepted bytes before fsync")
        process.stdin.write(b"\n")
        await process.stdin.drain()
    if await process.wait() != 0:
        raise RuntimeError(f"guest exited {process.returncode}")
    with subject.open("rb") as file:
        file.seek(LIMIT - 8)
        if file.read(13) != b"HOST.parkeep":
            raise RuntimeError("unexpected bytes after fsync and close")


async def run(binary, guest):
    with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryFile() as errors:
        root = Path(directory)
        shutil.copy2(guest, root / "probe")
        subject = root / "subject"
        with subject.open("xb") as file:
            file.seek(LIMIT - 8)
            file.write(b"cold....keep")
        # Inherit SIG_IGN so the host FUSE worker returns EFBIG instead of exiting.
        signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
        process = await asyncio.create_subprocess_exec(
            "prlimit", f"--fsize={LIMIT}", binary, "--rootfs", root,
            "--cpus", "1", "--memory", "256", "--task-stdio", "--", "/probe", "warm", "/subject",
            env=os.environ | {"PVISOR_GUEST_KERNEL": "rust-native"},
            restore_signals=False,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=errors,
        )
        try:
            await asyncio.wait_for(check(process, subject), timeout=30)
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()
            if process.returncode != 0:
                errors.seek(0)
                shutil.copyfileobj(errors, sys.stderr.buffer)
    print("PASS: read-warmed short write, EFBIG, and host visibility before fsync")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="native-enabled vm_api_case")
    parser.add_argument("--guest", required=True, type=Path, help="static write_ack binary")
    args = parser.parse_args()
    asyncio.run(run(args.binary.resolve(strict=True), args.guest.resolve(strict=True)))
