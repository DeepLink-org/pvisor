#!/usr/bin/env python3
"""Real KVM lazy-image A/B with a local, immutable cache-protocol fixture.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: decide whether a lazy-image path change helps users who run VMs
on images that are fetched on demand.
Conclusion sought: cold and warm client-cache differences for traversal,
reads, large reads and copy-up, with confidence intervals and request counts
proving both versions fetched the same bytes.
Design: shuffled version order per round, cold then warm per version, local
cache fixture on separate cores, no injected latency; not a WAN/S3 test.
"""

import argparse
import hashlib
import json
import math
import os
import random
import shutil
import socketserver
import stat
import struct
import subprocess
import threading
import time
from collections import Counter
from contextlib import contextmanager
from pathlib import Path

from bench import percentile
from reference_baselines import digest, validate_bundle_execution

MARKER = "LAZY_RESULT "
IMAGE_DIGEST = "sha256:" + "e" * 64
TIMINGS = {
    "metadata",
    "metadata_guest_hot",
    "open_read",
    "open_read_guest_hot",
    "read_64m",
    "copy_up_32",
}
SMALL = b"pvisor-fixture-needle " + b"x" * 1000 + b"\n"
PAYLOAD = b"0123456789abcdef" * (4 * 1024 * 1024)
WORKLOAD = """import hashlib, json, time
from pathlib import Path
root = Path('/lazy-bench')
timings = {}
def measure(name, operation):
    start = time.perf_counter_ns()
    operation()
    timings[name] = (time.perf_counter_ns() - start) / 1e6
def metadata():
    files = list((root / 'tree').rglob('*.txt'))
    assert len(files) == 2048
    assert sum(p.stat().st_size for p in files) == 2048 * SMALL_SIZE
def open_read():
    for i in range(2048):
        data = (root / f'tree/d{i // 64:03d}/f{i:05d}.txt').read_bytes()
        assert data == b'pvisor-fixture-needle ' + b'x' * 1000 + b'\\n'
def bulk_read():
    with (root / 'payload.bin').open('rb') as source:
        assert hashlib.file_digest(source, 'sha256').hexdigest() == PAYLOAD_SHA256
def copy_up():
    for i in range(32):
        path = root / f'tree/d{i // 64:03d}/f{i:05d}.txt'
        path.write_bytes(b'changed in upper\\n')
        assert path.read_bytes() == b'changed in upper\\n'
# Expire the default one-second attribute TTL after interpreter startup.
time.sleep(1.2)
measure('metadata', metadata)
measure('metadata_guest_hot', metadata)
measure('open_read', open_read)
measure('open_read_guest_hot', open_read)
measure('read_64m', bulk_read)
measure('copy_up_32', copy_up)
Path('workspace-proof').write_bytes(b'changed in upper\\n')
print('LAZY_RESULT ' + json.dumps({'timings_ms': timings, 'correctness': 'passed'}), flush=True)
"""


def receive_exact(stream, length):
    data = bytearray()
    while len(data) < length:
        block = stream.recv(length - len(data))
        if not block:
            raise EOFError("incomplete cache request")
        data.extend(block)
    return data


class ImageFixture:
    def __init__(self, source, fixture):
        self.source, self.fixture = source, fixture
        self.counts, self.lock = Counter(), threading.Lock()

    def local(self, relative):
        path = Path(os.fsdecode(relative))
        if path.is_absolute() or ".." in path.parts:
            raise PermissionError("invalid image path")
        if relative == b"bench/lazy_workload.py":
            return self.fixture / "lazy_workload.py"
        if path.parts and path.parts[0] == "lazy-bench":
            return self.fixture.joinpath(*path.parts[1:])
        return self.source / path

    def metadata(self, relative):
        path = self.local(relative)
        attr = path.lstat()
        kind = (
            "directory"
            if stat.S_ISDIR(attr.st_mode)
            else "symlink"
            if stat.S_ISLNK(attr.st_mode)
            else "file"
        )
        return dict(
            status="metadata",
            kind=kind,
            size=attr.st_size,
            mode=attr.st_mode,
            uid=0,
            gid=0,
            inode=attr.st_ino,
            nlink=attr.st_nlink,
            mtime=attr.st_mtime_ns // 10**9,
            mtime_nsec=attr.st_mtime_ns % 10**9,
            target=list(os.fsencode(os.readlink(path))) if kind == "symlink" else None,
        )

    def request(self, request):
        op = request["op"]
        with self.lock:
            self.counts[op] += 1
        body = b""
        try:
            if op == "ping":
                response = dict(status="ready")
            elif op == "prepare":
                response = dict(
                    status="prepared",
                    digest=IMAGE_DIGEST,
                    architecture="amd64",
                    env={"PATH": "/usr/local/bin:/usr/bin:/bin"},
                    entrypoint=[],
                    cmd=[],
                    metadata_generation="lazy-filesystem-ab-v1",
                )
            else:
                if request["digest"] != IMAGE_DIGEST:
                    raise PermissionError("different image")
                relative = bytes(request["path"])
                if op == "stat":
                    response = self.metadata(relative)
                elif op == "list":
                    names = [os.fsencode(p.name) for p in self.local(relative).iterdir()]
                    if relative == b"":
                        names.append(b"lazy-bench")
                    elif relative == b"bench":
                        names.append(b"lazy_workload.py")
                    names = sorted(set(names))
                    offset = request["offset"]
                    page = names[offset : offset + 256]
                    response = dict(
                        status="entries",
                        names=[list(n) for n in page],
                        metadata=[
                            self.metadata(relative + b"/" + n if relative else n) for n in page
                        ],
                        next_offset=offset + 256 if offset + 256 < len(names) else None,
                    )
                elif op == "read":
                    if not 0 <= request["length"] <= 1024 * 1024:
                        raise ValueError("invalid read size")
                    with self.local(relative).open("rb") as source:
                        source.seek(request["offset"])
                        body = source.read(request["length"])
                    with self.lock:
                        self.counts["read_bytes"] += len(body)
                        if relative.startswith(b"lazy-bench/"):
                            self.counts["fixture_reads"] += 1
                            self.counts["fixture_read_bytes"] += len(body)
                    response = dict(
                        status="data",
                        length=len(body),
                        sha256="sha256:" + hashlib.sha256(body).hexdigest(),
                    )
                else:
                    raise ValueError("unsupported cache operation")
        except FileNotFoundError:
            response = dict(status="error", code="not_found", message="missing image path")
        except Exception as error:
            response = dict(status="error", code="request_failed", message=str(error))
        return response, body

    def snapshot(self):
        with self.lock:
            return self.counts.copy()


@contextmanager
def serve_image(path, fixture):
    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            size = struct.unpack("!I", receive_exact(self.request, 4))[0]
            if not 0 < size <= 1024 * 1024:
                raise ValueError("invalid frame size")
            envelope = json.loads(receive_exact(self.request, size))
            if envelope["version"] != 1:
                raise ValueError("invalid protocol version")
            response, body = fixture.request(envelope["request"])
            frame = json.dumps(response, separators=(",", ":")).encode()
            self.request.sendall(struct.pack("!I", len(frame)) + frame + body)

    class Server(socketserver.ThreadingUnixStreamServer):
        daemon_threads = True

    server = Server(str(path), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield
    finally:
        server.shutdown()
        thread.join()
        server.server_close()
        path.unlink(missing_ok=True)


def validate_trial(output, bundle, upper, fixture):
    values = [
        json.loads(line[len(MARKER) :]) for line in output.splitlines() if line.startswith(MARKER)
    ]
    if len(values) != 1 or values[0]["correctness"] != "passed":
        raise ValueError("missing successful guest result")
    timings = values[0]["timings_ms"]
    if set(timings) != TIMINGS or any(not math.isfinite(t) or t < 0 for t in timings.values()):
        raise ValueError("invalid guest timing matrix")
    validate_bundle_execution(bundle, "pvisor-vm")
    if (upper / "workspace-proof").read_bytes() != b"changed in upper\n":
        raise ValueError("workspace write was not staged")
    if any(p.read_bytes() != SMALL for p in (fixture / "tree").rglob("*.txt")):
        raise ValueError("immutable source was modified")
    return timings


@contextmanager
def observe_mounts(store):
    mounts, stop = set(), threading.Event()

    def monitor():
        while not stop.is_set():
            for line in Path("/proc/self/mountinfo").read_text().splitlines():
                if str(store) in line and " - fuse" in line:
                    mounts.add(line)
            stop.wait(0.05)

    thread = threading.Thread(target=monitor)
    thread.start()
    try:
        yield mounts
    finally:
        stop.set()
        thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("assets", "baseline", "candidate", "firmware", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--cpu-affinity", default="0,1")
    parser.add_argument("--server-affinity", default="2,3")
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("invalid sample counts")
    for name in ("assets", "baseline", "candidate", "firmware", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    fixture_path = args.output / "fixture"
    fixture_path.mkdir()
    for i in range(2048):
        directory = fixture_path / "tree" / f"d{i // 64:03d}"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"f{i:05d}.txt").write_bytes(SMALL)
    (fixture_path / "payload.bin").write_bytes(PAYLOAD)
    guest = (
        "PAYLOAD_SHA256 = "
        + repr(hashlib.sha256(PAYLOAD).hexdigest())
        + "\nSMALL_SIZE = "
        + str(len(SMALL))
        + "\n"
        + WORKLOAD
    )
    (fixture_path / "lazy_workload.py").write_text(guest)
    binaries = {}
    for variant in ("baseline", "candidate"):
        target = args.output / variant / "pvisor"
        target.parent.mkdir()
        shutil.copy2(getattr(args, variant), target)
        binaries[variant] = target
    if digest(binaries["baseline"]) == digest(binaries["candidate"]):
        parser.error("identical binaries")
    # Python server and all its threads inherit this mask. Each VM is explicitly
    # pinned to its own budget; neither CPU set is exclusive to this campaign.
    os.sched_setaffinity(0, {int(i) for i in args.server_affinity.split(",")})
    image = ImageFixture(args.assets / "rootfs", fixture_path)
    report = dict(
        schema="pvisor-lazy-filesystem-ab/v1",
        arguments={k: str(v) for k, v in vars(args).items()},
        binary_sha256={k: digest(v) for k, v in binaries.items()},
        firmware_sha256=digest(args.firmware / "libkrunfw.so.5"),
        harness_sha256=digest(Path(__file__)),
        guest_sha256=digest(fixture_path / "lazy_workload.py"),
        host_kernel=os.uname().release,
        load_before=os.getloadavg(),
        rows=[],
        protocol=dict(
            samples=args.samples,
            warmups=args.warmups,
            vm_vcpus=2,
            memory_mib=4096,
            cache="fresh client disk cache for cold; same cache for following warm VM; warm host page cache; fresh guest, projection and upper each launch",
            server="local Python Unix-socket cache v1 fixture, no artificial latency, verifies content hashes; not a WAN or S3 performance test",
            order="variant order shuffled each round with seed 20261005; cold then warm per variant",
            worker="operation plus content/attribute checks; startup excluded",
            completion="CLI launch to exit, includes guest's fixed 1.2-second TTL wait",
            correctness="guest checks including 32 rootfs copy-ups, Run Bundle VM isolation, staged workspace write, untouched immutable source",
        ),
    )

    def save():
        report["load_after"] = os.getloadavg()
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    # Linux sun_path is short; the enclosing private directory is temporary.
    import tempfile

    with tempfile.TemporaryDirectory(prefix="pvisor-lazy-ab-", dir="/tmp") as sockets:
        socket_path = Path(sockets) / "cache.sock"
        with serve_image(socket_path, image):
            rng = random.Random(20261005)
            save()
            try:
                for trial in range(-args.warmups - 1, args.samples):
                    variants = list(binaries)
                    rng.shuffle(variants)
                    for variant in variants:
                        cache = args.output / "cache" / f"{variant}-{trial}"
                        for state in ("cold", "warm"):
                            root = args.output / "trials" / f"{variant}-{state}-{trial}"
                            workspace = root / "workspace"
                            workspace.mkdir(parents=True)
                            stage = root / "stage"
                            env = os.environ | dict(
                                XDG_CACHE_HOME=str(cache),
                                XDG_CONFIG_HOME=str(root / "config"),
                                PVISOR_RUN_HOME=str(root / "runs"),
                                PVISOR_IMAGE_STORE=str(root / "store"),
                                PVISOR_CACHE_SERVER="unix://" + str(socket_path),
                                PVISOR_FS_PROFILE="0",
                                PVISOR_STARTUP_TIMING="0",
                            )
                            for key in (
                                "PVISOR_CACHE_BACKEND",
                                "PVISOR_CACHE_LOCATION",
                                "PVISOR_VM_FS_WORKERS",
                            ):
                                env.pop(key, None)
                            command = [
                                "taskset",
                                "-c",
                                args.cpu_affinity,
                                str(binaries[variant]),
                                "run",
                                "--vm",
                                "--rootfs",
                                "image=lazy-benchmark:fixture",
                                "--image-store",
                                str(root / "store"),
                                "--no-agent-defaults",
                                "--overlaynet",
                                "off",
                                "--stdio",
                                "inherit",
                                "--stage",
                                str(stage),
                                "--cpu",
                                "2",
                                "--memory",
                                "4096MiB",
                                "--vm-library-dir",
                                str(args.firmware),
                                "--timeout",
                                "120s",
                                "--",
                                "/usr/bin/python3",
                                "/bench/lazy_workload.py",
                            ]
                            before = image.snapshot()
                            start = time.perf_counter_ns()
                            with observe_mounts(root / "store") as mounts:
                                process = subprocess.run(
                                    command,
                                    cwd=workspace,
                                    env=env,
                                    capture_output=True,
                                    text=True,
                                    timeout=140,
                                )
                                completion = (time.perf_counter_ns() - start) / 1e6
                            requests = image.snapshot() - before
                            (root / "stdout.log").write_text(process.stdout)
                            (root / "stderr.log").write_text(process.stderr)
                            (root / "command.json").write_text(
                                json.dumps(dict(argv=command, exit=process.returncode), indent=2)
                                + "\n"
                            )
                            if process.returncode:
                                raise RuntimeError(
                                    f"{variant}/{state}/{trial}: {process.returncode}; {process.stderr[-2000:]}"
                                )
                            if bool(mounts) != (variant == "baseline"):
                                raise ValueError(
                                    f"unexpected host FUSE path for {variant}: {sorted(mounts)}"
                                )
                            bundle = json.loads((stage / "run-bundle.json").read_text())
                            timings = validate_trial(
                                process.stdout, bundle, stage / "upper", fixture_path
                            )
                            if (workspace / "workspace-proof").exists():
                                raise ValueError("workspace write reached the lower")
                            if trial >= 0:
                                report["rows"].append(
                                    dict(
                                        variant=variant,
                                        cache=state,
                                        trial=trial,
                                        completion_ms=completion,
                                        timings_ms=timings,
                                        requests=dict(requests),
                                        host_fuse_mounts=sorted(mounts),
                                        correctness="passed",
                                    )
                                )
                            save()
                            shutil.rmtree(stage / "upper", ignore_errors=True)
                        shutil.rmtree(cache, ignore_errors=True)
                    print(f"lazy A/B: round {trial + 1}/{args.samples}", flush=True)
                summary = {}
                for variant in binaries:
                    for state in ("cold", "warm"):
                        selected = [
                            r
                            for r in report["rows"]
                            if r["variant"] == variant and r["cache"] == state
                        ]
                        if len(selected) != args.samples:
                            raise ValueError("incomplete sample matrix")
                        keys = selected[0]["timings_ms"]
                        timings = {key: [r["timings_ms"][key] for r in selected] for key in keys}
                        timings["completion_ms"] = [r["completion_ms"] for r in selected]
                        summary[f"{variant}/{state}"] = dict(
                            n=len(selected),
                            timings_ms={
                                key: {f"p{q}": percentile(values, q) for q in (50, 95, 99)}
                                for key, values in timings.items()
                            },
                        )
                report["summary"] = summary
            except BaseException as error:
                report["failure"] = str(error) or type(error).__name__
                raise
            finally:
                save()


if __name__ == "__main__":
    main()
