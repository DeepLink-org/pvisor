#!/usr/bin/env python3
"""Same staged executor, two genuinely lazy immutable image lower providers.

Benchmark: B-LAZY-STARTUP (benchmark/README.md#b-lazy-startup), role user-facing.
Motivation: compare lazy image supply without a VM/container boundary difference.
Conclusion sought: staged file loading latency and transfer volume for pVisor
host FUSE versus TurboOCI/OverlayBD under the same executor and workload.
Design: same rootless-process staged CLI and host SHA256 probe, four fixed
image files, 3 warmups + 30 balanced randomized cold/warm paired rounds.
The image's programs are not executed. No global page-cache eviction or prune.
"""

import argparse
import fcntl
import hashlib
import itertools
import json
import os
import platform
import random
import secrets
import shlex
import shutil
import socket
import socketserver
import subprocess
import sys
import threading
import time
import traceback
from pathlib import Path
from types import SimpleNamespace

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SOURCE = "sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794"
FILES = [
    "usr/lib/os-release",
    "usr/bin/dash",
    "usr/lib/x86_64-linux-gnu/libc.so.6",
    "usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
]
MARKER = b"STAGE_READY files 4 bytes 2575738\n"
EXECUTABLE = "/opt/pvisor-stage-bench/pvisor"
MOUNTER = "/opt/pvisor-stage-bench/lazy-stage-mount"


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(args):
    lab = args.lab.resolve(strict=True)
    batch = args.prepare.resolve()
    if batch.parent != lab or ".data" not in batch.parts or len(batch.name) > 36:
        raise ValueError(
            "Use a NEW, short-named evidence directory immediately under the owned lab"
        )
    batch.mkdir(mode=0o755)
    for name in ("harness", "tools", "trials", "pairs"):
        (batch / name).mkdir()
    sources = [
        "stage_lazy_compare.py",
        "stage_lazy_analysis.py",
        "stage_lazy_audit.py",
        "lazy_stage_mount.rs",
        "image_compare.py",
        "image_compare_analysis.py",
        "overlaybd_image_perf.py",
        "overlaybd_lab.py",
        "overlaybd_smoke.py",
    ]
    for name in sources:
        shutil.copy2(HERE / name, batch / "harness" / name)
    root = HERE.parents[1]
    # Freeze only the existing protocol observer, not the old campaign entry point.
    for name in ("lazy_image_v2.py", "lazy_startup.py", "lazy_common.py"):
        source = root / "benchmark/pvisor" / name
        if source.is_file():
            shutil.copy2(source, batch / "harness" / name)
    # lazy_image_v2 imports a few protocol/report helpers; retain exact bytes.
    for directory, children, names in os.walk(root / "benchmark/pvisor"):
        children[:] = [name for name in children if name not in (".data", "__pycache__")]
        for name in names:
            if not name.endswith(".py"):
                continue
            source = Path(directory) / name
            destination = batch / "harness" / source.relative_to(root / "benchmark/pvisor")
            if not destination.exists():
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination)
    for name in ("pvisor", "pvisor-daemon"):
        shutil.copy2(args.binary_dir.resolve() / name, batch / "tools" / name)
    rlib = args.runtime_rlib.resolve(strict=True)
    command = [
        "taskset",
        "-c",
        args.cpus,
        "rustc",
        "--edition=2024",
        "-C",
        "opt-level=3",
        "-C",
        "panic=abort",
        str(batch / "harness/lazy_stage_mount.rs"),
        "--extern",
        "pvisor=" + str(rlib),
        "-L",
        "dependency=" + str(rlib.parent),
        "-o",
        str(batch / "tools/lazy-stage-mount"),
    ]
    build = subprocess.run(command, capture_output=True, timeout=120)
    (batch / "helper-build.stdout").write_bytes(build.stdout)
    (batch / "helper-build.stderr").write_bytes(build.stderr)
    if build.returncode:
        raise RuntimeError("benchmark owner helper failed to link; no product source was changed")
    save(
        batch / "helper-build.json",
        dict(
            argv=command,
            rlib_sha256=sha(rlib),
            compiler=subprocess.check_output(["rustc", "--version"], text=True).strip(),
        ),
    )
    store = args.prepared_store.resolve(strict=True)
    subprocess.run(
        ["cp", "-a", "--reflink=auto", str(store), str(batch / "service-store")], check=True
    )
    rootfs = batch / "service-store/rootfs-v3/sha256" / SOURCE[7:]
    manifest = batch / "service-store/metadata/manifests-v1" / (SOURCE[7:] + ".json")
    if sha(manifest) != SOURCE[7:]:
        raise ValueError("source platform manifest mismatch")
    files = {}
    for name in FILES:
        path = rootfs / name
        if path.is_symlink() or any(
            parent.is_symlink() for parent in path.parents if parent != rootfs.parent
        ):
            raise ValueError("probe paths must contain no symlinks")
        files[name] = dict(bytes=path.stat().st_size, sha256=sha(path))
    if sum(item["bytes"] for item in files.values()) != 2575738:
        raise ValueError("source file sizes changed")
    expected = "".join(item["sha256"] + "  " + name + "\n" for name, item in files.items())
    probe = (
        "printf %s "
        + shlex.quote(expected)
        + " | sha256sum -c - >/dev/null; printf "
        + shlex.quote(MARKER.decode())
    )
    (batch / "expected.sha256").write_text(expected)
    # Verify the probe files against original OCI TAR entries, not just a local extraction.
    import tarfile

    seen = {}
    source_manifest = json.loads(manifest.read_bytes())
    for layer in source_manifest["layers"]:
        blob = batch / "service-store/blobs/sha256" / layer["digest"][7:]
        if sha(blob) != layer["digest"][7:] or blob.stat().st_size != layer["size"]:
            raise ValueError("source compressed layer mismatch")
        with tarfile.open(blob, "r:*") as archive:
            for member in archive:
                name = member.name.removeprefix("./")
                if name in FILES:
                    if not member.isfile():
                        raise ValueError("probe OCI entry must be a regular file")
                    data = archive.extractfile(member).read()
                    seen[name] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
    if files != seen:
        raise ValueError("local prepared files differ from original OCI layer contents")
    lab_manifest = json.loads((lab / "lab.json").read_text())
    source = json.loads((lab / "source-image.json").read_text())
    smoke = sorted(lab.glob("smoke-*/frozen-images.json"))[-1]
    frozen = json.loads(smoke.read_text())
    if frozen["source_digest"] != SOURCE:
        raise ValueError("OverlayBD source digest mismatch")
    for name, original in (
        ("source-manifest.json", "source-manifest.json"),
        ("derived-manifest.json", "turbo-manifest.json"),
    ):
        shutil.copy2(smoke.parent / original, batch / name)
    shutil.copy2(smoke, batch / "frozen-images.json")
    derived = json.loads((batch / "derived-manifest.json").read_text())
    config_digest = derived["config"]["digest"][7:]
    config_path = (
        lab
        / "registry/docker/registry/v2/blobs/sha256"
        / config_digest[:2]
        / config_digest
        / "data"
    )
    if sha(config_path) != config_digest:
        raise ValueError("derived OCI config mismatch")
    shutil.copy2(config_path, batch / "derived-config.json")
    config = json.loads(config_path.read_bytes())
    chain = ""
    for diff in config["rootfs"]["diff_ids"]:
        chain = (
            diff
            if not chain
            else "sha256:" + hashlib.sha256((chain + " " + diff).encode()).hexdigest()
        )
    rng = random.Random(20261018)
    orders = list(itertools.permutations(["pvisor", "turbo"])) * 15
    rng.shuffle(orders)
    protocol = dict(
        schema=1,
        benchmark="B-LAZY-STARTUP",
        scope="same-host-staged-lower-readonly",
        groups=["pvisor", "turbo"],
        runs=30,
        warmups=3,
        orders=dict(warmup=[rng.sample(["pvisor", "turbo"], 2) for _ in range(3)], sample=orders),
        lab=str(lab),
        batch=str(batch),
        container=lab_manifest["container"],
        lab_id=lab_manifest["id"],
        cpus=list(map(int, args.cpus.split(","))),
        service_cpus=list(map(int, args.service_cpus.split(","))),
        source_digest=SOURCE,
        derived_digest=frozen["derived_digest"],
        derived_reference=frozen["derived_reference"],
        chain_id=chain,
        source_files=files,
        workload_argv=["/bin/sh", "-ec", probe],
        marker=MARKER.decode(),
        timeout=90,
        platform=platform.platform(),
        seed=20261018,
        clock_ticks=os.sysconf("SC_CLK_TCK"),
        clock="Popen before common Docker exec; exact verified stdout and normal wrapper exit (includes lower open/rpull/mount, staged CLI and unmount)",
        cache="fresh private client cache and remount, then same persistent client cache and new mount/stage; host page cache retained",
        budget="same taskset 12,13; services 14,15; no enclosing cgroup memory cap claimed",
        transport="pVisor: private Unix proxy to loopback TCP cache; TurboOCI: lab loopback HTTP registry; no WAN or latency injection",
        source_build_relationship="existing runtime rlib/CLI; exact source-build correspondence unverified; hashes frozen",
        preflight_write_limitation="root-level write preflight failed DAC: source root UID1000, Docker caller UID0; this protocol intentionally measures read-only loading; source permissions unchanged",
    )
    protocol["artifacts"] = {
        str(path): sha(path)
        for directory in ("harness", "tools")
        for path in (batch / directory).rglob("*")
        if path.is_file()
    }
    for name in (
        "source-manifest.json",
        "derived-manifest.json",
        "derived-config.json",
        "frozen-images.json",
        "expected.sha256",
        "helper-build.json",
    ):
        protocol["artifacts"][str(batch / name)] = sha(batch / name)
    save(batch / "protocol.json", protocol)
    save(
        batch / "in.json",
        {
            "same-stage-cold-warm-paired": "$ "
            + shlex.join(
                [
                    sys.executable,
                    str(batch / "harness/stage_lazy_compare.py"),
                    "--measure",
                    str(batch),
                ]
            )
        },
    )
    print(batch)


def measure(batch, preflight_only=False):
    import lazy_image_v2 as legacy
    import overlaybd_image_perf as obd
    from image_compare import interference, process_snapshot
    from overlaybd_lab import host_inventory
    from overlaybd_smoke import CTR, SOCKET, Evidence, audit_inventory

    protocol = json.loads((batch / "protocol.json").read_text())
    lab = Path(protocol["lab"])
    inside = "/lab/" + batch.name
    if (batch / "records.json").exists() or (batch / "measurement-started.json").exists():
        raise ValueError("batch already measured; use a new prepared batch")
    lock = (lab / "stage-comparison.lock").open("a+")
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for name, digest in protocol["artifacts"].items():
        if sha(name) != digest:
            raise ValueError("frozen input changed: " + name)
    save(
        batch / "measurement-started.json", dict(pid=os.getpid(), monotonic_ns=time.monotonic_ns())
    )
    os.sched_setaffinity(0, protocol["service_cpus"])
    docker = ["docker", "exec", protocol["container"]]
    ctr = docker + [CTR, "--address", SOCKET, "--namespace", "pvisor-obd-smoke"]
    manifest = json.loads((lab / "lab.json").read_text())
    if manifest["id"] != protocol["lab_id"]:
        raise ValueError("lab changed")
    evidence = Evidence(batch, 90)
    verification = batch / "verification"
    verification.mkdir()
    checked = SimpleNamespace(lab=lab, frozen_images=batch / "frozen-images.json")
    actual_manifest, _, checked_docker, checked_ctr, _, provenance = obd.verify_lab(
        checked, verification, Evidence(verification, 90)
    )
    if actual_manifest != manifest or provenance["source_digest"] != SOURCE:
        raise ValueError("private lab provenance changed")
    docker, ctr = checked_docker, checked_ctr
    save(
        batch / "initial-service-affinity.json",
        obd.pin_services(evidence, docker, protocol["service_cpus"]),
    )
    # These are this benchmark's container-local binaries; never alter the host installation.
    evidence.command(
        docker
        + [
            "sh",
            "-ec",
            "mkdir -p /opt/pvisor-stage-bench; "
            'cp "$1/tools/pvisor" /opt/pvisor-stage-bench/pvisor; '
            'cp "$1/tools/lazy-stage-mount" /opt/pvisor-stage-bench/lazy-stage-mount; '
            "chmod 755 /opt/pvisor-stage-bench /opt/pvisor-stage-bench/*",
            "sh",
            inside,
        ]
    )
    baseline = host_inventory()
    save(batch / "host-before.json", baseline)
    if baseline["errors"] or baseline["targets"]:
        raise ValueError("require readable inventory and no active targets before measuring")
    token = secrets.token_hex(24)
    service_env = {key: value for key, value in os.environ.items() if not key.startswith("PVISOR_")}
    service_env.update(
        PVISOR_CACHE_TOKEN=token,
        PVISOR_STARTUP_TIMING="0",
        XDG_CACHE_HOME=str(batch / "server-cache"),
        XDG_CONFIG_HOME=str(batch / "server-config"),
    )
    reservation = socket.socket()
    reservation.bind(("127.0.0.1", 0))
    port = reservation.getsockname()[1]
    reservation.close()
    service_log = (batch / "cache-service.log").open("wb")
    service = subprocess.Popen(
        [
            str(batch / "tools/pvisor-daemon"),
            "serve",
            "--cache",
            "--cache-listen",
            f"tcp://127.0.0.1:{port}",
            "--cache-image-store",
            str(batch / "service-store"),
        ],
        env=service_env,
        stdout=service_log,
        stderr=subprocess.STDOUT,
    )

    class UnixRequest:
        def __init__(self, stream):
            self.stream = stream

        def __getattr__(self, name):
            return getattr(self.stream, name)

        def setsockopt(self, *args):
            if args[0] != socket.IPPROTO_TCP:
                return self.stream.setsockopt(*args)

    class Proxy(legacy.CacheProxy):
        def setup(self):
            self.request = UnixRequest(self.request)

    class Server(socketserver.ThreadingUnixStreamServer):
        daemon_threads = True

    descriptor = os.open(batch, os.O_RDONLY | os.O_DIRECTORY)
    proxy = None
    rows = []
    try:
        deadline = time.monotonic() + 30
        while True:
            if service.poll() is not None:
                raise RuntimeError("cache service exited")
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                    break
            except OSError:
                if time.monotonic() > deadline:
                    raise TimeoutError("cache service startup")
                time.sleep(0.05)
        proxy = Server("/proc/self/fd/" + str(descriptor) + "/proxy.sock", Proxy)
        proxy.upstream, proxy.counts = port, legacy.Counts()
        os.chmod(batch / "proxy.sock", 0o666)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        prepared = subprocess.run(
            [str(batch / "tools/pvisor"), "cache", "prepare", "docker.io/library/ubuntu@" + SOURCE],
            capture_output=True,
            timeout=60,
            env=service_env | dict(PVISOR_CACHE_SERVER=f"tcp://127.0.0.1:{port}"),
        )
        (batch / "cache-prepare.stdout").write_bytes(prepared.stdout)
        (batch / "cache-prepare.stderr").write_bytes(prepared.stderr)
        if prepared.returncode:
            raise RuntimeError("cached source preparation failed")
        opened = json.loads(prepared.stdout)
        if opened["digest"] != SOURCE:
            raise ValueError("cache source identity mismatch")
        save(batch / "cache-prepared.json", opened)
        handle = opened["image_handle"]
        obd.MARKER = MARKER

        def trial(group, phase, number, cache, cache_key):
            name = f"{phase}-{number:02d}-{group}-{cache}"
            folder = batch / "trials" / name
            folder.mkdir()
            target = inside + "/trials/" + name
            cache_path = inside + "/clients/" + cache_key
            if group == "pvisor":
                evidence.command(
                    docker
                    + [
                        "sh",
                        "-ec",
                        'if test "$1" = cold; then test ! -e "$2"; else test -d "$2"; fi',
                        "sh",
                        cache,
                        cache_path,
                    ]
                )
            env_args = [
                "-e",
                "PVISOR_CACHE_TOKEN=" + token,
                "-e",
                "PVISOR_STARTUP_TIMING=0",
                "-e",
                "PVISOR_FS_PROFILE=0",
                "-e",
                "PVISOR_LAZY_IMAGE_V2=1",
                "-e",
                "PVISOR_LAZY_INDEX_PAGES=1",
                "-e",
                "XDG_CACHE_HOME=" + cache_path,
                "-e",
                "XDG_CONFIG_HOME=" + target + "/config",
                "-e",
                "PVISOR_RUN_HOME=" + target + "/runs",
                "-e",
                "PVISOR_IMAGE_STORE=" + cache_path + "/store",
            ]
            # Identical staged command and source-byte probe. The program is the host /bin/sh.
            common = [
                EXECUTABLE,
                "run",
                "--executor",
                "host",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--gateway-mode",
                "off",
                "--stdio",
                "inherit",
                "--timeout",
                "30s",
                "--stage",
                target + "/stage",
                "--",
                *protocol["workload_argv"],
            ]
            if group == "pvisor":
                invocation = [
                    MOUNTER,
                    "unix://" + inside + "/proxy.sock",
                    handle,
                    cache_path + "/store",
                    target + "/mount.receipt",
                    "--",
                    *common,
                ]
            else:
                key = "stage-" + name
                # ctr emits a trusted mount command composed exclusively of this owned
                # snapshotter's mount options and our bounded paths; retain exact text.
                script = """set -eu
target=$1
key=$2
parent=$3
ref=$4
shift 4
ctr=/opt/overlaybd/snapshotter/ctr
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke rpull --plain-http "$ref" >&2
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke images mount --snapshotter overlaybd "$ref" "$target/lower" > "$target/image-mount.log"
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke snapshots --snapshotter overlaybd mounts "$target/lower" "$target/lower" > "$target/mount-command.sh"
read -r mountcmd opt kind source_path rest < "$target/mount-command.sh"
test "$mountcmd" = mount && test "$opt" = -t && test "$kind" = bind
case "$source_path" in /lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/*/block/mountpoint) ;; *) exit 1 ;; esac
sn=${source_path%/block/mountpoint}
sn=${sn##*/}
case "$sn" in ''|*[!0-9]*) exit 1 ;; esac
info=/sys/kernel/config/target/core/user_999999999/dev_$sn/info
grep -F "overlaybd//lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/$sn/block/config.v1.json" "$info" > "$target/target-info.txt"
cat "${source_path%/mountpoint}/backstore_mark" > "$target/device.txt"
set +e
"$@"
code=$?
set -e
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke images unmount --snapshotter overlaybd --rm "$target/lower" >&2
if test -f "$info"; then
  umount "$source_path"
  /opt/overlaybd/snapshotter/overlaybd-attacher detach --id "$sn" >&2
fi
exit "$code"
"""
                invocation = [
                    "sh",
                    "-ec",
                    script,
                    "sh",
                    target,
                    key,
                    protocol["chain_id"],
                    protocol["derived_reference"],
                    MOUNTER,
                    "--plain",
                    target + "/lower",
                    "-",
                    target + "/mount.receipt",
                    "--",
                    *common,
                ]
                (folder / "lower").mkdir()
            command = [
                "docker",
                "exec",
                *env_args,
                protocol["container"],
                "/lab/tools/taskset",
                "-c",
                ",".join(map(str, protocol["cpus"])),
                *invocation,
            ]
            before = proxy.counts.snapshot()
            logpath = lab / "log/registry.log"
            stat = logpath.stat()
            cursor = ((stat.st_dev, stat.st_ino), stat.st_size)
            timing = obd.capture(command, folder / "timed", protocol["timeout"], include_pull=True)
            timing.update(
                includes=protocol["clock"],
                excludes="observer interpreter startup, source preparation/conversion, reset, validation and post-exit audit",
            )
            save(folder / "timed/timing.json", timing)
            # Evidence made by the root-owned sandbox remains readable to the workspace owner.
            evidence.command(
                docker + ["chown", "-R", str(os.getuid()) + ":" + str(os.getgid()), target]
            )
            after = proxy.counts.snapshot()
            requests = after["rows"][len(before["rows"]) :]
            errors = after["errors"][len(before["errors"]) :]
            save(folder / "requests.json", dict(rows=requests, errors=errors))
            traffic = obd.registry_observation(
                lab, cursor, folder, blob_roles=obd.image_blob_roles(batch)
            )
            if errors or traffic["error_body_bytes"] or traffic["unclassified_body_bytes"]:
                raise ValueError("transfer observer failed or encountered unclassified content")
            bundle_file = folder / "stage/run-bundle.json"
            bundle = json.loads(bundle_file.read_text())
            if (
                bundle["run"]["state"] != "completed"
                or bundle["run"]["exit_code"] != 0
                or bundle["run"]["executor"]["isolation"] != "rootless_process"
                or not bundle["safety"]["filesystem_changes_staged"]
                or bundle["filesystem"]["changed_files"] != 0
                or bundle["filesystem"]["changes"]
            ):
                raise ValueError("not a completed, genuinely staged, read-only host task")
            observed = bundle["run_observation"]["filesystem"]["paths"]
            for filename, expected in protocol["source_files"].items():
                if (
                    observed.get(filename, {}).get("read", {}).get("bytes_read")
                    != expected["bytes"]
                ):
                    raise ValueError("stage did not observe the exact source bytes: " + filename)
            receipt = dict(
                line.split("=", 1) for line in (folder / "mount.receipt").read_text().splitlines()
            )
            lower = receipt["rootfs"]
            mounts = (folder / "mount.receipt.mountinfo").read_text().splitlines()
            actual = [line for line in mounts if line.split()[4] == lower]
            if len(actual) != 1 or " ro," not in actual[0] + ",":
                raise ValueError("missing unique read-only lower mount")
            expected_type = " - fuse pvisor-image " if group == "pvisor" else " - ext4 "
            if expected_type not in actual[0]:
                raise ValueError("unexpected actual lower filesystem")
            if group == "pvisor":
                if (
                    receipt["digest"] != SOURCE
                    or (folder / "mount.receipt.unmounted").read_text()
                    != "mountpoint_removed=true\n"
                ):
                    raise ValueError("FUSE owner did not release its image")
                content = sum(item["content_bytes"] for item in requests)
                metadata = sum(item["metadata_bytes"] for item in requests)
                response = sum(item["response_bytes"] for item in requests)
                if traffic["response_body_bytes"]:
                    raise ValueError("pVisor unexpectedly used registry")
            else:
                if requests:
                    raise ValueError("OverlayBD unexpectedly used pVisor cache")
                content, metadata, response = (
                    traffic[key]
                    for key in ("content_body_bytes", "metadata_body_bytes", "response_body_bytes")
                )
            if (cache == "cold" and content <= 0) or (cache == "warm" and content != 0):
                raise ValueError("client cache content gate failed")
            row = dict(
                phase=phase,
                round=number,
                group=group,
                cache=cache,
                valid=True,
                ready_ns=timing["ready_ns"],
                completion_ns=timing["completion_ns"],
                content_bytes=content,
                metadata_bytes=metadata,
                response_bytes=response,
                stage_completion_ns=int((folder / "mount.receipt.stage").read_text().split("=")[1]),
                evidence_dir=str(folder),
            )
            save(
                folder / "result.json",
                row
                | dict(
                    timing=timing,
                    lower_mount=actual[0],
                    bundle_sha256=sha(bundle_file),
                    requests_sha256=sha(folder / "requests.json"),
                    registry_sha256=sha(folder / "registry-responses.json"),
                    mount_receipt=receipt,
                ),
            )
            audit_inventory(baseline, folder, wait=15)
            print("completed " + name, file=sys.stderr, flush=True)
            return row

        phases = (
            [("preflight", [["pvisor", "turbo"]])]
            if preflight_only
            else [
                ("warmup", protocol["orders"]["warmup"]),
                ("sample", protocol["orders"]["sample"]),
            ]
        )
        for phase, orders in phases:
            for number, order in enumerate(orders):
                before_processes = process_snapshot()
                for group in order:
                    pair = batch / "pairs" / f"{phase}-{number:02d}-{group}"
                    pair.mkdir()
                    if group == "turbo":
                        shutil.copy2(batch / "frozen-images.json", pair / "frozen-images.json")
                        args = SimpleNamespace(lab=lab, service_cpus=protocol["service_cpus"])
                        reset = obd.reset_client(
                            args,
                            manifest,
                            docker,
                            ctr,
                            manifest["owned_cgroup_prefix"],
                            baseline,
                            pair,
                            Evidence(pair, 90),
                        )
                        save(pair / "reset-return.json", reset)
                    # Persistent Host is equally prepared after every container restart.
                    evidence.command(
                        docker
                        + [
                            "/lab/tools/taskset",
                            "-c",
                            ",".join(map(str, protocol["cpus"])),
                            EXECUTABLE,
                            "run",
                            "--executor",
                            "host",
                            "--no-agent-defaults",
                            "--overlaynet",
                            "off",
                            "--gateway-mode",
                            "off",
                            "--stdio",
                            "inherit",
                            "--filesystem",
                            "host",
                            "--",
                            "/bin/true",
                        ]
                    )
                    cache_key = f"{phase}-{number:02d}-{group}"
                    for cache in ("cold", "warm"):
                        rows.append(trial(group, phase, number, cache, cache_key))
                    audit_inventory(baseline, pair, wait=15)
                after_processes = process_snapshot()
                flags = interference(before_processes, after_processes, protocol["cpus"])
                save(
                    batch / f"background-{phase}-{number:02d}.json",
                    dict(before=before_processes, after=after_processes, interference=flags),
                )
                if flags:
                    raise ValueError("foreign benchmark/build interference invalidates this cohort")
                save(batch / "records.partial.json", rows)
        save(batch / ("preflight-records.json" if preflight_only else "records.json"), rows)
    except BaseException:
        (batch / "failure.txt").write_text(traceback.format_exc())
        save(batch / "records.partial.json", rows)
        raise
    finally:
        if proxy is not None:
            proxy.shutdown()
            proxy.server_close()
        os.close(descriptor)
        service.terminate()
        try:
            service.wait(timeout=10)
        except subprocess.TimeoutExpired:
            service.kill()
            service.wait(timeout=5)
        service_log.close()
        save(
            batch / "teardown.json",
            dict(
                cache_service_exit=service.returncode,
                frozen_artifacts_unchanged=all(
                    sha(name) == digest for name, digest in protocol["artifacts"].items()
                ),
            ),
        )
    if not preflight_only:
        from stage_lazy_audit import audit

        audit(batch)
        from stage_lazy_analysis import analyze

        row = analyze(batch)["row"]
        save(batch / "measured-row.json", row)
        return row
    return dict(valid=True, trials=len(rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--prepare", type=Path)
    action.add_argument("--measure", type=Path)
    action.add_argument("--preflight", type=Path)
    action.add_argument("--audit", type=Path)
    parser.add_argument("--lab", type=Path)
    parser.add_argument("--prepared-store", type=Path)
    parser.add_argument("--binary-dir", type=Path, default=HERE.parents[1] / "target/release")
    parser.add_argument("--runtime-rlib", type=Path)
    parser.add_argument("--cpus", default="12,13")
    parser.add_argument("--service-cpus", default="14,15")
    args = parser.parse_args()
    if args.prepare:
        prepare(args)
    elif args.audit:
        from stage_lazy_audit import audit

        audit(args.audit.resolve())
        from stage_lazy_analysis import analyze, render

        result = analyze(args.audit.resolve())
        if (args.audit / "in.render.json").exists():
            exported = json.loads((args.audit / "in.render.json").read_text())
            measured = json.loads((args.audit / "measured-row.json").read_text())
            if measured != result["row"] or exported != {"same-stage-cold-warm-paired": measured}:
                raise ValueError("semspec export differs from independently audited samples")
        (args.audit / "report.md").write_text(render(result))
        print(json.dumps(result["row"]))
    else:
        print(json.dumps(measure((args.measure or args.preflight).resolve(), bool(args.preflight))))


if __name__ == "__main__":
    main()
