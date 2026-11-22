"""Hardware acceptance for fresh guests on Linux x86_64/KVM."""

import argparse
import os
import shutil
import subprocess
import tempfile
import time
from contextlib import ExitStack
from pathlib import Path

PAGE = 4096
PAGES = 1024
MARKER = 0x8C774A135BF19A2D
PRESENT_FILE = (1 << 63) | (1 << 61)


def wait_for(processes, condition, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for process in processes:
            if process.poll() is not None:
                raise RuntimeError(f"{process.args} exited {process.returncode}: {description}")
        if result := condition():
            return result
        time.sleep(0.05)
    raise TimeoutError(description)


def pool_pages(pid):
    """Map guest payload indices to resident file PTEs and their pool slots."""
    proc = Path(f"/proc/{pid}")
    slots = {}
    with (
        (proc / "mem").open("rb", buffering=0) as memory,
        (proc / "pagemap").open("rb", buffering=0) as pagemap,
    ):
        for line in (proc / "maps").read_text().splitlines():
            if "/memfd:pvisor-shared-ram (deleted)" not in line:
                continue
            addresses, perms, offset, dev, inode, _ = line.split(maxsplit=5)
            if perms != "rw-p":
                continue
            start, end = (int(part, 16) for part in addresses.split("-"))
            for address in range(start, end, PAGE):
                pte = int.from_bytes(os.pread(pagemap.fileno(), 8, address // PAGE * 8), "little")
                if pte & PRESENT_FILE != PRESENT_FILE:
                    continue
                # A racing remap could replace a VMA after reading maps; check
                # the payload, then require the same file slot in both runners.
                data = os.pread(memory.fileno(), 16, address)
                index = int.from_bytes(data[:8], "little") ^ MARKER
                if index < PAGES and data[8:] == b"\x5a" * 8:
                    slots[index] = (dev, inode, int(offset, 16) + address - start)
    return slots


def send(process, command):
    process.stdin.write(command)
    process.stdin.flush()


def finish(*processes):
    for process in processes:
        send(process, b"x")
    for process in processes:
        result = process.wait(timeout=10)
        if result != 7:
            raise RuntimeError(f"{process.args} did not exit with authenticated status 7: {result}")


def run(binary, guest, directory, backing, firmware, stack):
    env = {**os.environ, "PVISOR_GUEST_KERNEL": "linux" if firmware else "rust-native"}
    vm_args = ["--firmware", firmware] if firmware else []

    def start(name, *args):
        log_path = directory / f"{name}.log"
        stack.callback(print_log, log_path)
        with log_path.open("wb") as log:
            process = stack.enter_context(
                subprocess.Popen(
                    [binary, *args], env=env, stdin=subprocess.PIPE, stdout=log, stderr=log
                )
            )
        stack.callback(process.kill)
        return process

    def vm(name, case, *args):
        root = directory / name
        root.mkdir()
        shutil.copy2(guest, root / "probe")
        proc = start(name, *vm_args, case, root, *args)
        wait_for([proc], (root / "ready").is_file, f"{name} guest payload ready")
        return proc, root

    if not firmware:
        subprocess.run([binary, "unsupported"], env=env, check=True, timeout=10)
        print("PASS: native guest rejects Linux idle handoff and snapshot profile", flush=True)

    ksm, root = vm("ksm", "ksm")
    send(ksm, b"r")
    wait_for([ksm], (root / "checked").is_file, "KSM guest payload intact")
    finish(ksm)
    scanner = Path("/sys/kernel/mm/ksm/run").read_text().strip()
    print(f"PASS: KSM advice and guest readback; scanner run={scanner}, no merge claim", flush=True)

    offload, root = vm("offload", "offload", backing)
    wait_for([offload], (root / "resumed").is_file, "file-backed RAM offloaded and resumed")
    send(offload, b"r")
    wait_for([offload], (root / "checked").is_file, "offloaded guest payload intact")
    finish(offload)
    print("PASS: manual offload reduced residency; guest readback after resume", flush=True)

    socket = directory / "pool.sock"
    pool = start("pool", "pool", socket)
    wait_for([pool], socket.is_socket, "physical pool ready")
    first, first_root = vm("writer", "physical", socket)
    second, second_root = vm("reader", "physical", socket)

    def common_slots():
        a, b = pool_pages(first.pid), pool_pages(second.pid)
        return sum(slot == b.get(index) for index, slot in a.items())

    shared = wait_for([first, second, pool], common_slots, "guest payload mapped to common slots")
    send(first, b"w")
    wait_for([first, second], (first_root / "checked").is_file, "writer verified private bytes")
    send(second, b"r")
    wait_for([first, second], (second_root / "checked").is_file, "reader verified original bytes")
    finish(first, second)
    print(f"PASS: {shared} common payload slots before write; peer unchanged after COW", flush=True)


def print_log(path):
    if path.is_file() and path.stat().st_size:
        print(f"{path.name}:\n{path.read_text(errors='replace')}", end="", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, help="test Linux libkrunfw instead of Rust native")
    parser.add_argument(
        "--backing-dir", type=Path, required=True, help="directory on a disk filesystem"
    )
    args = parser.parse_args()
    with (
        tempfile.TemporaryDirectory(prefix="pvisor-native-memory-") as directory,
        tempfile.TemporaryDirectory(dir=args.backing_dir) as backing,
        ExitStack() as stack,
    ):
        run(
            args.binary.resolve(strict=True),
            args.guest.resolve(strict=True),
            Path(directory),
            Path(backing) / "ram",
            args.firmware.resolve(strict=True) if args.firmware else None,
            stack,
        )


if __name__ == "__main__":
    main()
