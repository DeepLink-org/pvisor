#!/usr/bin/env python3
"""Historical retired-snapshot harness; use an archived binary.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role historical;
the snapshot command is retired, so this harness produces no new conclusions.

KVM gate: fault cold RAM after delete/gc, then save and restore a COW VM.

This is a correctness check, not a latency distribution. Uses the same static
snapshot_guest.rs binary as vm_snapshot.py and requires usable KVM and FUSE.
"""

import argparse
import hashlib
import json
import os
import shutil
import signal
import subprocess
import time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--guest", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
legacy_help = subprocess.run([str(args.binary.resolve()), "snapshot", "--help"], capture_output=True, timeout=10)
if legacy_help.returncode != 0:
    parser.error("this historical harness requires an archived binary exposing the retired snapshot command")
out = args.output.resolve()
out.mkdir(mode=0o700)
binary = out / "pvisor"
shutil.copy2(args.binary.resolve(), binary)
processes = []
rows = []


def command(store, *arguments):
    result = subprocess.run(
        [str(binary), "snapshot", "--store", str(store), *map(str, arguments)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, (arguments, result.stdout, result.stderr)
    return result.stdout.strip()


def start(store, name, *arguments):
    with (out / f"{store.parent.name}-{name}.log").open("w") as log:
        process = subprocess.Popen(
            [str(binary), "snapshot", "--store", str(store), *map(str, arguments), "--name", name],
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    processes.append(process)
    return process


def wait(path, process, old=None):
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        assert process.poll() is None, f"VM exited {process.returncode}: {path}"
        if path.exists():
            value = path.read_text()
            if value and value != old:
                return value
        time.sleep(0.005)
    raise TimeoutError(str(path))


def request(root, process, name):
    old = (root / "ack").read_text() if (root / "ack").exists() else None
    (root / "request.tmp").write_text(name)
    (root / "request.tmp").replace(root / "request")
    value = wait(root / "ack", process, old)
    assert value.split()[0] == name, value
    return value


def wait_unmounted(path):
    deadline = time.monotonic() + 10
    while str(path) in Path("/proc/self/mountinfo").read_text():
        assert time.monotonic() < deadline, "snapshot RAM mount survived VM termination"
        time.sleep(0.01)


try:
    for storage in ("raw", "compressed"):
        base = out / storage
        base.mkdir()
        store = base / "store"
        source = base / "input"
        source.mkdir()
        (source / "dev").mkdir()
        shutil.copy2(args.guest.resolve(), source / "init.krun")
        process = start(
            store,
            "source",
            "run",
            "--rootfs",
            source,
            "--native-init",
            "--ram-storage",
            storage,
            "--memory",
            "256",
            "--cpus",
            "2",
        )
        root = store / "runs/source/rootfs"
        wait(root / "ready", process)
        before = request(root, process, "before")
        identity = command(store, "save", "source")
        assert process.wait(timeout=15) == 0
        shutil.rmtree(source)
        shutil.rmtree(store / "runs/source")
        restores = []
        for generation in (1, 2):
            # Compare against the sealed heartbeat, not a value read while the
            # source was still running: materializing a newer saved value alone
            # must never count as proof that the restored guest has executed.
            heartbeat = (store / "objects" / identity / "rootfs/heartbeat").read_text()
            name = f"generation{generation}"
            started = time.perf_counter()
            process = start(store, name, "restore", identity)
            root = store / "runs" / name / "rootfs"
            heartbeat = wait(root / "heartbeat", process, heartbeat)
            restore_ms = (time.perf_counter() - started) * 1000
            # Delete and collect BEFORE asking the guest to scan its full heap.
            command(store, "delete", identity)
            command(store, "gc")
            ack = request(root, process, name)
            assert int(ack.split()[1]) > int(before.split()[1])
            restores.append(dict(name=name, restore_heartbeat_ms=restore_ms, ack=ack))
            if generation == 1:
                # Capture guest COW writes, with the parent snapshot already gone.
                identity = command(store, "save", name)
                assert process.wait(timeout=15) == 0
                wait_unmounted(store / "runs" / name)
                shutil.rmtree(store / "runs" / name)
        rows.append(dict(storage=storage, correctness="passed", restores=restores))
        print(json.dumps(rows[-1]), flush=True)
finally:
    for process in processes:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()

# Exit watchers run outside each runner's process group; they must still clean
# every mount when the harness terminates a whole VM group.
wait_unmounted(out)
for storage in ("raw", "compressed"):
    command(out / storage / "store", "gc")
(out / "result.json").write_text(
    json.dumps(
        dict(
            schema="pvisor-kvm-lazy-snapshot-correctness/v1",
            cpus=2,
            memory_mib=256,
            binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
            guest_sha256=hashlib.sha256(args.guest.read_bytes()).hexdigest(),
            rows=rows,
        ),
        indent=2,
    )
    + "\n"
)
