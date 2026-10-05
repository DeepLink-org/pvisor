#!/usr/bin/env python3
"""Historical retired-snapshot harness; use an archived binary.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role historical;
the snapshot command is retired, so this harness produces no new conclusions.

Real stage VM restore gate: eager RAM, guest continuation, FD and private forks.

The static snapshot_guest.rs reads /stage-file-count and creates the fixture
inside the writable stage. Timing includes CLI startup; heartbeat must differ
from the sealed value to prove execution rather than host materialization.
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--files", type=int, default=2048)
    parser.add_argument(
        "--diagnostic-profile",
        action="store_true",
        help="instrumented diagnosis; not performance acceptance",
    )
    parser.add_argument(
        "--create-only",
        action="store_true",
        help="validate guest startup and request, without saving/restoring",
    )
    args = parser.parse_args()
    legacy_help = subprocess.run([str(args.binary.resolve()), "snapshot", "--help"], capture_output=True, timeout=10)
    if legacy_help.returncode != 0:
        parser.error("this historical harness requires an archived binary exposing the retired snapshot command")
    if args.samples < 1 or args.warmups < 0 or not 0 <= args.files <= 8192:
        parser.error("invalid samples, warmups or files")
    out = args.output.resolve()
    out.mkdir(mode=0o700)
    binary = out / "pvisor"
    shutil.copy2(args.binary.resolve(), binary)
    shutil.copy2(args.firmware.resolve(), out / args.firmware.name)
    processes = []
    rows = []
    env = dict(os.environ)
    env.pop("PVISOR_FS_PROFILE", None)
    if args.diagnostic_profile:
        env["PVISOR_FS_PROFILE"] = "1"

    def command(store, *arguments):
        started = time.perf_counter()
        result = subprocess.run(
            [str(binary), "snapshot", "--store", str(store), *map(str, arguments)],
            env=env,
            capture_output=True,
            text=True,
            timeout=180,
        )
        if result.returncode:
            raise RuntimeError((arguments, result.stdout, result.stderr))
        return result.stdout.strip(), (time.perf_counter() - started) * 1000

    def start(store, name, *arguments):
        with (out / f"{store.parent.name}-{name}.log").open("w") as log:
            started = time.perf_counter()
            process = subprocess.Popen(
                [
                    str(binary),
                    "snapshot",
                    "--store",
                    str(store),
                    *map(str, arguments),
                    "--name",
                    name,
                ],
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        processes.append(process)
        return process, started

    def wait(path, process, old=None):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"VM exited {process.returncode}: {path}")
            try:
                value = path.read_text()
            except FileNotFoundError:
                value = ""
            if value and value != old:
                return value
            time.sleep(0.005)
        raise TimeoutError(str(path))

    def request(root, process, tag, old=None):
        started = time.perf_counter()
        temporary = root / "request.tmp"
        temporary.write_text(tag)
        temporary.replace(root / "request")
        value = wait(root / "ack", process, old)
        if value.split()[0] != tag:
            raise AssertionError(value)
        return value, (time.perf_counter() - started) * 1000

    def stop(process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)

    try:
        for trial in range(args.warmups + args.samples):
            folder = out / f"trial-{trial}"
            folder.mkdir()
            source = folder / "input"
            source.mkdir()
            # Native PID 1 needs a devtmpfs mountpoint before it starts. An
            # absent /dev can leave stdio unavailable during runtime startup.
            (source / "dev").mkdir()
            shutil.copy2(args.guest.resolve(), source / "init.krun")
            (source / "stage-file-count").write_text(str(args.files))
            store = folder / "store"
            process, started = start(
                store,
                "source",
                "run",
                "--rootfs",
                source,
                "--native-init",
                "--ram-storage",
                "raw",
                "--memory",
                "256",
                "--cpus",
                "2",
            )
            root = store / "runs/source/rootfs/upper"
            wait(root / "ready", process)
            ready_ms = (time.perf_counter() - started) * 1000
            before, check_ms = request(root, process, "before")
            if args.create_only:
                stop(process)
                row = {
                    "trial": trial,
                    "warmup": trial < args.warmups,
                    "ready_ms": ready_ms,
                    "request_ms": check_ms,
                    "correctness": "passed",
                    "ack": before,
                }
                rows.append(row)
                (folder / "result.json").write_text(json.dumps(row, indent=2))
                print(json.dumps(row), flush=True)
                continue
            identity, save_ms = command(store, "save", "source")
            if process.wait(timeout=15) != 0:
                raise RuntimeError("source did not exit successfully after save")
            sealed = store / "objects" / identity / "rootfs/upper"
            manifest = json.loads((sealed.parent.parent / "manifest.json").read_text())
            if manifest["version"] != 4 or not manifest["stage_bases"]:
                raise AssertionError("expected raw stage snapshot")
            if args.files and len(list((sealed / "stage-files").iterdir())) != args.files:
                raise AssertionError("guest did not create the complete stage fixture")
            heartbeat = (sealed / "heartbeat").read_text()
            ack = (sealed / "ack").read_text()
            shutil.rmtree(source)
            shutil.rmtree(store / "runs/source")
            branches = []
            live = []
            for index in range(2):
                name = f"fork{index}"
                restored, started = start(store, name, "restore", identity, "--eager-ram")
                branch = store / "runs" / name / "rootfs/upper"
                wait(branch / "heartbeat", restored, heartbeat)
                heartbeat_ms = (time.perf_counter() - started) * 1000
                after, request_ms = request(branch, restored, name, ack)
                if int(after.split()[1]) <= int(before.split()[1]):
                    raise AssertionError("guest counter did not continue")
                branches.append(
                    {
                        "name": name,
                        "restore_heartbeat_ms": heartbeat_ms,
                        "restore_checked_ms": (time.perf_counter() - started) * 1000,
                        "request_ms": request_ms,
                        "guest_check_ms": float(after.split()[2]),
                        "ack": after,
                    }
                )
                live.append(restored)
            first = store / "runs/fork0/rootfs/upper"
            second = store / "runs/fork1/rootfs/upper"
            (first / "open-file").write_text("private")
            if (second / "open-file").read_bytes() != b"abcdef" or (
                sealed / "open-file"
            ).read_bytes() != b"abcdef":
                raise AssertionError("fork write leaked")
            if args.files:
                (first / "stage-files/0000").write_bytes(b"changed")
                if (second / "stage-files/0000").read_bytes() != bytes([0x5A]) * 16:
                    raise AssertionError("stage file write leaked")
                if (sealed / "stage-files/0000").read_bytes() != bytes([0x5A]) * 16:
                    raise AssertionError("sealed stage was modified")
            for restored in live:
                stop(restored)
            row = {
                "trial": trial,
                "warmup": trial < args.warmups,
                "ready_ms": ready_ms,
                "save_ms": save_ms,
                "snapshot_id": identity,
                "correctness": "passed",
                "restores": branches,
            }
            rows.append(row)
            (folder / "result.json").write_text(json.dumps(row, indent=2))
            print(json.dumps(row), flush=True)
    finally:
        for process in processes:
            stop(process)
        report = {
            "schema": "pvisor-stage-vm-gate/v1",
            "diagnostic_profile": args.diagnostic_profile,
            "create_only": args.create_only,
            "platform": platform.platform(),
            "files": args.files,
            "ram": "raw eager; 256MiB, 2 vCPUs",
            "samples": args.samples,
            "warmups": args.warmups,
            "rows": rows,
            "sha256": {
                str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in (
                    binary,
                    args.guest.resolve(),
                    args.firmware.resolve(),
                    Path(__file__).resolve(),
                )
            },
        }
        (out / "report.json").write_text(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
