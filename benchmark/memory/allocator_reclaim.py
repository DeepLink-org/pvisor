#!/usr/bin/env python3
"""Benchmark: B-ALLOCATOR-RECLAIM-ENG, role engineering A/B.
Motivation: quantify whole-VMM host memory released by PID-1 controlled malloc_trim.
Conclusion sought: per-VM resident/physical-footprint drops versus identical-SO inspect.
Design: Apple Silicon/HVF, fresh 512MiB/2-vCPU VMs, four allocation threads/one
arena, 64/256MiB retained free heap, randomized five paired rounds, fixed 5s
baseline/15s observation, 250ms samples, final 2s medians, full refill verification.
No offload, cold pager, dedup, global cache changes or user benchmark publication.
"""

import argparse
import ctypes
import hashlib
import json
import os
import platform
import queue
import random
import shutil
import socket
import statistics
import struct
import subprocess
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Usage(ctypes.Structure):
    # SDK sys/resource.h rusage_info_v0, checked by ctypes.sizeof below.
    _fields_ = [("uuid", ctypes.c_ubyte * 16)] + [
        (key, ctypes.c_uint64)
        for key in (
            "user_time",
            "system_time",
            "pkg_idle_wkups",
            "interrupt_wkups",
            "pageins",
            "wired_size",
            "resident_size",
            "phys_footprint",
            "proc_start_abstime",
            "proc_exit_abstime",
        )
    ]


assert ctypes.sizeof(Usage) == 96 and Usage.phys_footprint.offset == 72


def usage(pid):
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    library.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    value = Usage()
    if library.proc_pid_rusage(pid, 0, ctypes.byref(value)):
        raise OSError(ctypes.get_errno(), "proc_pid_rusage failed")
    return {key: getattr(value, key) for key, _ in Usage._fields_ if key != "uuid"}


def guard(owned=None):
    output = subprocess.check_output(["ps", "-axo", "pid=,comm="], text=True)
    conflicts = []
    for line in output.splitlines():
        pid, command = line.strip().split(maxsplit=1)
        if int(pid) != owned and Path(command).name in {
            "pvisor",
            "vmm",
            "firecracker",
            "qemu-system-aarch64",
            "qemu-system-x86_64",
            "cargo",
            "rustc",
            "zig",
            "cargo-nextest",
        }:
            conflicts.append(dict(pid=int(pid), name=Path(command).name))
    if conflicts:
        raise RuntimeError(f"foreign VM/build interference: {conflicts}")


def exact(stream, length):
    result = bytearray()
    while len(result) < length:
        part = stream.recv(length - len(result))
        if not part:
            raise EOFError("incomplete guest frame")
        result.extend(part)
    return bytes(result)


def receive(stream):
    length = struct.unpack("!I", exact(stream, 4))[0]
    if length > 1048576:
        raise ValueError("oversized guest frame")
    value = json.loads(exact(stream, length))
    if value.get("kind") in ("Failure", "Error"):
        raise RuntimeError(value)
    return value


def send(stream, value):
    data = json.dumps(value).encode()
    stream.sendall(struct.pack("!I", len(data)) + data)


def connect(endpoint):
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(30)
    stream.connect(str(endpoint))
    return stream


def request(endpoint, value):
    with connect(endpoint) as stream:
        send(stream, value)
        return receive(stream)


def guest_file(endpoint, path):
    with connect(endpoint) as stream:
        send(stream, dict(operation="File", request=dict(action="Read", path=path)))
        assert receive(stream)["kind"] == "Ready"
        data = bytearray()
        while True:
            frame = receive(stream)
            if frame["kind"] == "Exit":
                assert frame["code"] == 0
                return data.decode()
            assert frame["kind"] == "Output"
            data.extend(frame["bytes"])


def counters(text):
    result = {}
    for line in text.splitlines():
        key, _, value = line.partition(":")
        parts = value.split()
        if parts and parts[0].isdigit():
            result[key] = int(parts[0]) * (1024 if len(parts) > 1 and parts[1] == "kB" else 1)
    return result


def phase_summary(rows):
    end = rows[-1]["time"]
    tail = [row for row in rows if row["time"] >= end - 2]
    if len(tail) < 6 or end - rows[0]["time"] < 4:
        raise ValueError("insufficient stable phase observations")
    return {
        key: statistics.median(row[key] for row in tail)
        for key in ("resident_size", "phys_footprint", "wired_size")
    }


def observe(process, seconds, output):
    rows, start, next_guard = [], time.monotonic(), 0
    while time.monotonic() - start < seconds:
        if process.poll() is not None:
            raise RuntimeError("VMM exited during memory observation")
        if time.monotonic() >= next_guard:
            guard(process.pid)
            next_guard = time.monotonic() + 1
        row = dict(time=time.monotonic(), **usage(process.pid))
        if rows and row["proc_start_abstime"] != rows[0]["proc_start_abstime"]:
            raise RuntimeError("VMM PID identity changed")
        rows.append(row)
        output.write(json.dumps(row) + "\n")
        output.flush()
        time.sleep(0.25)
    return rows


def trial(args, directory, mib, trim):
    guard()
    directory.mkdir()
    root = directory / "root"
    shutil.copytree(args.root, root, symlinks=True)
    # AF_UNIX paths stay within the platform's small sun_path limit.
    endpoint = directory / "c.sock"
    command = [
        str(args.vmm),
        str(root),
        str(args.firmware),
        str(args.payload),
        str(args.so),
        str(endpoint),
    ]
    save(directory / "command.json", command)
    stream = None
    with (directory / "vmm.stdout").open("wb") as out, (directory / "vmm.stderr").open("wb") as err:
        process = subprocess.Popen(command, stdout=out, stderr=err)
        try:
            deadline = time.monotonic() + 30
            while True:
                try:
                    assert request(endpoint, dict(operation="Ping"))["kind"] == "Ready"
                    break
                except (OSError, EOFError):
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(
                            f"new PID-1 control adapter did not become ready; runner={process.poll()}"
                        )
                    time.sleep(0.1)
            stream = connect(endpoint)
            send(
                stream,
                dict(
                    operation="Start",
                    launch=dict(argv=["/payload", str(mib)], cwd="/", user=None, tty=None),
                ),
            )
            started = receive(stream)
            assert started["kind"] == "Started"
            pid, generation = started["pid"], started["generation"]
            events = queue.Queue()

            def drain():
                pending = bytearray()
                try:
                    with (directory / "guest.frames.jsonl").open("w") as frames:
                        while True:
                            value = receive(stream)
                            frames.write(json.dumps(value) + "\n")
                            frames.flush()
                            if value["kind"] == "Exit":
                                events.put(value)
                                return
                            assert value["kind"] == "Output" and value["stream"] == 1, value
                            pending.extend(value["bytes"])
                            while b"\n" in pending:
                                line, _, pending = pending.partition(b"\n")
                                events.put(json.loads(line))
                except Exception as error:
                    events.put(error)

            reader = threading.Thread(target=drain, daemon=True)
            reader.start()

            def phase(name):
                event = events.get(timeout=30)
                if isinstance(event, Exception):
                    raise event
                assert event.get("phase") == name and event["pid"] == pid, event

            def input_byte(value):
                send(stream, dict(input="Data", bytes=list(value.encode())))

            def memory():
                return dict(
                    guest=counters(guest_file(endpoint, "/proc/meminfo")),
                    process=counters(guest_file(endpoint, f"/proc/{pid}/status")),
                )

            def reclaim(enabled):
                value = request(
                    endpoint,
                    dict(
                        operation="ReclaimAllocator", pid=pid, generation=generation, trim=enabled
                    ),
                )
                assert value["kind"] == "Value", value
                return value["value"]

            phase("allocated")
            inspected = reclaim(False)
            allocated = memory()
            input_byte("F")
            phase("freed")
            freed = memory()
            if freed["process"]["RssAnon"] < mib * 1024**2 * 0.8:
                raise RuntimeError("payload did not retain the freed heap before trim")
            with (directory / "before.jsonl").open("w") as output:
                before = phase_summary(observe(process, 5, output))
            reply = reclaim(trim)
            with (directory / "after.jsonl").open("w") as output:
                after = phase_summary(observe(process, 15, output))
            settled = memory()
            input_byte("R")
            phase("refilled-verified")
            refill_host = usage(process.pid)
            live_reply = reclaim(trim)
            with (directory / "live-reuse.jsonl").open("w") as output:
                observe(process, 3, output)
            input_byte("Q")
            phase("verified-exit")
            assert events.get(timeout=10) == dict(kind="Exit", code=0)
            reader.join(timeout=10)
            assert not reader.is_alive()
            assert request(endpoint, dict(operation="Stop", signal=15))["kind"] == "Ready"
            assert process.wait(timeout=15) == 143
            result = dict(
                mib=mib,
                trim=trim,
                vmm_pid=process.pid,
                started=started,
                inspection=inspected,
                allocated=allocated,
                freed=freed,
                reply=reply,
                settled=settled,
                refill_host=refill_host,
                live_reply=live_reply,
                before=before,
                after=after,
                drop={key: before[key] - after[key] for key in before},
                payload_exit=0,
                vm_exit=143,
            )
            save(directory / "result.json", result)
            return result
        finally:
            if stream:
                stream.close()
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)


def bootstrap(values):
    rng = random.Random(819)
    samples = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(5000))
    return [samples[125], samples[4874]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("vmm", "firmware", "payload", "so", "root", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--preflight", action="store_true")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("this observer requires Apple Silicon macOS")
    for name in ("vmm", "firmware", "payload", "so", "root", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir()
    sources = [
        Path(__file__),
        Path(__file__).with_name("allocator_reclaim_vm.rs"),
        REPO / "tools/allocator-reclaim/payload.rs",
        REPO / "tools/allocator-reclaim/reclaim.rs",
        REPO / "tools/allocator-reclaim/fixture_runtime.rs",
        REPO / "tools/allocator-reclaim/Cargo.toml",
        REPO / "tools/allocator-reclaim/Cargo.lock",
        REPO / "tools/allocator-reclaim/build.sh",
        REPO / "Cargo.toml",
        REPO / "Cargo.lock",
    ]
    for crate in (
        "pvisor-vm",
        "pvisor-guest",
        "pvisor-core",
        "pvisor-overlay-core",
        "pvisor-journal",
    ):
        sources.extend(sorted((REPO / "crates" / crate).rglob("*.rs")))
        sources.append(REPO / "crates" / crate / "Cargo.toml")
    sources = list(dict.fromkeys(sources))
    artifacts = [
        args.vmm,
        args.firmware,
        args.payload,
        args.so,
        *sorted(p for p in args.root.rglob("*") if p.is_file()),
    ]
    hashes = {str(path): digest(path) for path in sources + artifacts}
    frozen = args.output / "sources"
    frozen.mkdir()
    for path in sources:
        destination = frozen / path.relative_to(REPO)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
    save(
        args.output / "manifest.json",
        dict(
            benchmark="B-ALLOCATOR-RECLAIM-ENG",
            role="engineering A/B",
            host=platform.uname()._asdict(),
            hashes=hashes,
            artifact_modes={str(p): p.stat().st_mode & 0o7777 for p in artifacts},
            revision=subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
            ).strip(),
            dirty=subprocess.check_output(["git", "status", "--short"], cwd=REPO, text=True),
            preflight=args.preflight,
            guest_reclaim_env=os.environ.get("PVISOR_GUEST_RECLAIM"),
            design=dict(
                guest_mib=512,
                vcpus=2,
                baseline_seconds=5,
                after_seconds=15,
                interval_seconds=0.25,
                final_seconds=2,
            ),
        ),
    )
    results, rng = [], random.Random(5271)
    try:
        for index in range(1 if args.preflight else 5):
            conditions = [(size, trim) for size in (64, 256) for trim in (False, True)]
            rng.shuffle(conditions)
            for size, trim in conditions:
                result = trial(
                    args,
                    args.output / f"r{index}-{size}-{'trim' if trim else 'inspect'}",
                    size,
                    trim,
                )
                result["round"] = index
                results.append(result)
                save(args.output / "samples.json", results)
                print(
                    json.dumps(dict(round=index, mib=size, trim=trim, drop=result["drop"])),
                    flush=True,
                )
        for path, checksum in hashes.items():
            assert digest(Path(path)) == checksum, f"input changed: {path}"
        summary = []
        for size in (64, 256):
            for metric in ("phys_footprint", "resident_size"):
                pairs = []
                for index in range(1 if args.preflight else 5):
                    rows = [r for r in results if r["round"] == index and r["mib"] == size]
                    pairs.append(
                        next(r["drop"][metric] for r in rows if r["trim"])
                        - next(r["drop"][metric] for r in rows if not r["trim"])
                    )
                summary.append(
                    dict(
                        mib=size,
                        metric=metric,
                        paired_extra_drop_bytes=pairs,
                        median=statistics.median(pairs),
                        range=[min(pairs), max(pairs)],
                        ci95=bootstrap(pairs) if not args.preflight else None,
                    )
                )
        save(
            args.output / "summary.json",
            dict(
                complete=True, preflight=args.preflight, sample_count=len(results), summary=summary
            ),
        )
    except BaseException as error:
        save(args.output / "failure.json", dict(error=repr(error), completed=len(results)))
        raise


if __name__ == "__main__":
    main()
