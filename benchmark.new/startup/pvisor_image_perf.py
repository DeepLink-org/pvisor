#!/usr/bin/env python3
"""B-LAZY-STARTUP single-trial adapter; sampling belongs to the caller.

prepare freezes existing artifacts and an explicit supported store, then starts
one persistent private user/mount/PID namespace. trial times only Popen through
the unique workload marker and normal process exit, excluding this interpreter,
JSON RPC, preparation and audit. prepared is a warm, materialized-rootfs control,
never a cold OCI download. lazy uses a real counted cache-service connection.
All failures retain evidence. This adapter neither builds nor publishes results.
"""

import argparse
import fcntl
import hashlib
import json
import os
import re
import secrets
import selectors
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import traceback
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SOURCE = (
    "docker.io/library/ubuntu@sha256:"
    "88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794"
)
CANARY = b"unchanged image startup input\n"
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,95}\Z")


def save(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def helpers(output):
    sys.path.insert(0, str(output / "frozen/harness"))
    import lazy_image_v2
    import lazy_startup

    return lazy_image_v2, lazy_startup


def cpu_list(value):
    values = [int(part) for part in value.split(",")]
    if not values or len(set(values)) != len(values) or min(values) < 0:
        raise ValueError("CPUs must be distinct nonnegative comma-separated integers")
    return values


def namespaces():
    return {key: os.readlink("/proc/self/ns/" + key) for key in ("user", "mnt", "pid")}


def process_identity(pid):
    try:
        path = Path("/proc") / str(pid)
        return dict(
            pid=pid, starttime=int((path / "stat").read_text().rpartition(")")[2].split()[19])
        )
    except (OSError, ValueError, IndexError):
        return None


def existing_pvisor():
    """Observe stable service identities and namespace visibility without signals."""
    result = []
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            identity = process_identity(int(path.name))
            if identity is None:
                continue
            command = (path / "cmdline").read_bytes()
            argv = command.split(b"\0")
            executable = Path(os.fsdecode(argv[0])).name
            if not (
                executable == "pvisor"
                and b"--pvisor-internal-host" in argv
                or executable == "pvisor-daemon"
                and argv[1:2] == [b"serve"]
            ):
                continue
            record = identity | dict(command_sha256=hashlib.sha256(command).hexdigest())
            for name in ("user", "mnt"):
                try:
                    record[name + "_namespace"] = os.readlink(path / "ns" / name)
                except OSError as error:
                    record[name + "_namespace"] = None
                    record[name + "_namespace_error"] = repr(error)
            if process_identity(int(path.name)) != identity:
                record["user_namespace"] = None
                record["user_namespace_error"] = "process identity changed during observation"
            result.append(record)
        except OSError:
            continue
    return result


def service_preservation(before, after, reference_user_namespace):
    """Require survival only for services proven to share the caller's userns.

    Other user namespaces own their natural service lifetimes. Unreadable
    namespace identities remain explicitly unverified, never proof of isolation
    or survival. Mount namespace changes do not change this userns-based scope.
    """

    def identity(record):
        return tuple(record[key] for key in ("pid", "starttime", "command_sha256"))

    same = [record for record in before if record.get("user_namespace") == reference_user_namespace]
    external = [
        record
        for record in before
        if record.get("user_namespace") is not None
        and record["user_namespace"] != reference_user_namespace
    ]
    unknown = [record for record in before if record.get("user_namespace") is None]
    observed_after = {identity(record) for record in after}
    missing = [record for record in same if identity(record) not in observed_after]
    return dict(
        existing_pvisor_preserved=not missing,
        existing_pvisor_preservation_scope="only preexisting services proven to share the caller user namespace; "
        "PID/starttime/command identities must survive; other namespaces have independent natural lifetimes; "
        "unreadable namespace identities are unverified",
        existing_pvisor_scope_reference_user_namespace=reference_user_namespace,
        existing_pvisor_scope_complete=not unknown,
        existing_pvisor_same_user_namespace_before=same,
        existing_pvisor_missing_same_user_namespace=missing,
        existing_pvisor_other_namespace_observations_before=external,
        existing_pvisor_other_namespace_observations_after=[
            record
            for record in after
            if record.get("user_namespace") is not None
            and record["user_namespace"] != reference_user_namespace
        ],
        existing_pvisor_namespace_unverified_before=unknown,
        existing_pvisor_namespace_unverified_after=[
            record for record in after if record.get("user_namespace") is None
        ],
    )


def send(stream, value):
    body = json.dumps(value).encode()
    stream.sendall(struct.pack("!I", len(body)) + body)


def receive(stream):
    def exact(size):
        value = bytearray()
        while len(value) < size:
            block = stream.recv(size - len(value))
            if not block:
                raise EOFError("incomplete control message")
            value.extend(block)
        return bytes(value)

    size = struct.unpack("!I", exact(4))[0]
    if not 0 < size <= 8 * 1024 * 1024:
        raise ValueError("invalid control message length")
    return json.loads(exact(size))


def rpc(output, request):
    descriptor = json.loads((output / "worker.json").read_text())
    protocol = json.loads((output / "protocol.json").read_text())
    with socket.create_connection(("127.0.0.1", descriptor["port"]), timeout=5) as connection:
        connection.settimeout(protocol["timeout_seconds"] + 30)
        send(connection, request | dict(token=descriptor["token"]))
        return receive(connection)


def prepare(args):
    output = args.output.resolve()
    if output.exists() or output.is_relative_to("/tmp"):
        raise ValueError("output must be a new host-visible directory outside /tmp")
    binary_dir = args.binary_dir.resolve(strict=True)
    source_store = args.prepared_store.resolve(strict=True)
    cpus, service_cpus = cpu_list(args.cpus), cpu_list(args.service_cpus)
    available = os.sched_getaffinity(0)
    if not set(cpus + service_cpus) <= available:
        raise ValueError("requested CPUs are outside current allowed affinity")
    if set(cpus) & set(service_cpus):
        raise ValueError("workload and service CPUs must be disjoint")
    if args.timeout <= 0 or args.memory_mib <= 0 or args.lifetime <= 0:
        raise ValueError("timeout, memory and lifetime must be positive")
    output.mkdir(parents=True, mode=0o700)
    output.chmod(0o700)
    for name in ("frozen/bin", "frozen/firmware", "frozen/harness", "trials", "client-caches"):
        (output / name).mkdir(parents=True)
    # Freeze the adapter and the proven namespace/framed-proxy helpers it uses.
    shutil.copy2(__file__, output / "frozen/adapter.py")
    harness_sources = []
    for directory, children, names in os.walk(ROOT / "benchmark/pvisor"):
        children[:] = [name for name in children if name not in (".data", "__pycache__")]
        harness_sources.extend(Path(directory) / name for name in names if name.endswith(".py"))
    for source in harness_sources:
        relative = source.relative_to(ROOT / "benchmark/pvisor")
        if ".data" in relative.parts or "__pycache__" in relative.parts:
            continue
        destination = output / "frozen/harness" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
    legacy, startup = helpers(output)
    source = args.source or (
        startup.NUMPY_SOURCE if args.workload == "numpy-script" else DEFAULT_SOURCE
    )
    digest = source.rsplit("@", 1)[-1]
    digest_hex = startup.digest_hex(digest)
    manifest_path = source_store / "metadata/manifests-v1" / (digest_hex + ".json")
    if sha(manifest_path) != digest_hex:
        raise ValueError("pinned platform manifest does not match the store")
    manifest = json.loads(manifest_path.read_bytes())
    for item in [manifest["config"], *manifest["layers"]]:
        startup.verify_oci_blob(
            source_store / "blobs/sha256" / startup.digest_hex(item["digest"]),
            item["digest"],
            item["size"],
        )
    config = json.loads(
        (
            source_store / "blobs/sha256" / startup.digest_hex(manifest["config"]["digest"])
        ).read_bytes()
    )
    if (config.get("os"), config.get("architecture")) != ("linux", "amd64"):
        raise ValueError("source must be the pinned linux/amd64 manifest")
    artifacts = {}
    runtime_libraries = {}
    for name in ("pvisor", "pvisor-daemon"):
        source_binary = binary_dir / name
        destination = output / "frozen/bin" / name
        original = sha(source_binary)
        shutil.copy2(source_binary, destination)
        if sha(destination) != original or sha(source_binary) != original:
            raise ValueError("artifact changed while freezing: " + name)
        artifacts[name] = dict(source=str(source_binary), frozen=str(destination), sha256=original)
        linked = subprocess.run(
            ["ldd", str(destination)], capture_output=True, text=True, timeout=10
        )
        (output / ("ldd-" + name + ".txt")).write_text(linked.stdout + linked.stderr)
        for library in re.findall(r"(/[^\s()]+)", linked.stdout):
            path = Path(library)
            if path.is_file():
                runtime_libraries[str(path.resolve())] = sha(path)
    firmware = args.firmware
    if firmware is None:
        firmware = binary_dir / "libkrunfw.so.5"
        if not firmware.is_file():
            choices = sorted(binary_dir.glob("libkrunfw.so.5.*"))
            if len(choices) != 1:
                raise ValueError("select an explicit --firmware file for the dynamic CLI")
            firmware = choices[0]
    firmware = firmware.resolve(strict=True)
    destination = output / "frozen/firmware/libkrunfw.so.5"
    shutil.copy2(firmware, destination)
    artifacts["firmware"] = dict(
        source=str(firmware), frozen=str(destination), sha256=sha(destination)
    )
    shutil.copytree(source_store, output / "service-store", symlinks=True)
    rootfs = output / "service-store/rootfs-v3/sha256" / digest_hex
    rootfs_manifest = legacy.prepared_store_manifest(rootfs)
    if rootfs_manifest != legacy.prepared_store_manifest(
        source_store / "rootfs-v3/sha256" / digest_hex
    ):
        raise ValueError("copied rootfs differs from the retained source")
    save(output / "rootfs-manifest.json", rootfs_manifest)
    shutil.copy2(manifest_path, output / "source-manifest.json")
    workload, marker = startup.workload(args.workload)
    if args.workload == "ubuntu-shell":
        workload = [
            "/bin/sh",
            "-ec",
            '. /etc/os-release; test "$ID" = ubuntu; '
            'test "$VERSION_ID" = 26.04; printf "LAZY_READY ubuntu %s\\n" "$VERSION_ID"',
        ]
    protocol = dict(
        schema=1,
        benchmark="B-LAZY-STARTUP",
        source=source,
        digest=digest,
        prepared_store=str(source_store),
        rootfs=str(rootfs),
        workload=args.workload,
        workload_argv=workload,
        marker=marker.decode(),
        cpus=cpus,
        service_cpus=service_cpus,
        memory_mib=args.memory_mib,
        timeout_seconds=args.timeout,
        lifetime_seconds=args.lifetime,
        artifacts=artifacts,
        rootfs_manifest_sha256=sha(output / "rootfs-manifest.json"),
        runtime_libraries=runtime_libraries,
        source_manifest_sha256=sha(output / "source-manifest.json"),
        adapter_sha256=sha(output / "frozen/adapter.py"),
        helper_sha256={
            str(p.relative_to(output / "frozen/harness")): sha(p)
            for p in (output / "frozen/harness").rglob("*.py")
        },
        binary_source_relationship="preexisting artifacts; build-time source correspondence unverified",
        clock="perf_counter_ns directly around worker Popen; excludes RPC, observer startup and audit",
        prepared_scope="materialized-rootfs warm VM only; no cold OCI preparation",
        lazy_transport="authenticated unencrypted loopback TCP; response frames excluding TCP/IP",
        traffic_fields=dict(
            content_bytes="Read Data payload",
            metadata_bytes="Metadata Data payload",
            response_bytes="all completed JSON frames, length prefixes and Data payloads; excludes TCP/IP",
            failed_transfers="partial failed send bytes are not claimed; errors invalidate the trial",
        ),
        host_page_cache="retained; no global drop_caches",
        cache_token=secrets.token_hex(24),
    )
    protocol["existing_pvisor_preservation_scope"] = (
        "hard preservation only for stable preexisting services with a verified caller user namespace; "
        "other private user namespaces are observational; unreadable namespace identities are unverified"
    )
    save(output / "protocol.json", protocol)
    (output / "protocol.json").chmod(0o600)
    # The preparation receipt hashes immutable identity, not mutable service bookkeeping.
    save(
        output / "prepared-input.json",
        {
            key: protocol[key]
            for key in (
                "source",
                "digest",
                "rootfs_manifest_sha256",
                "source_manifest_sha256",
                "artifacts",
                "workload_argv",
                "cpus",
                "service_cpus",
                "memory_mib",
                "timeout_seconds",
            )
        },
    )
    protocol["prepared_sha"] = sha(output / "prepared-input.json")
    save(output / "protocol.json", protocol)
    (output / "protocol.json").chmod(0o600)
    save(
        output / "containment-parent.json",
        dict(namespaces=namespaces(), existing_pvisor=existing_pvisor()),
    )
    command = [
        "unshare",
        "--user",
        "--map-root-user",
        "--mount",
        "--pid",
        "--fork",
        "--kill-child=KILL",
        "--mount-proc",
        "--propagation",
        "private",
        sys.executable,
        str(output / "frozen/adapter.py"),
        "_worker",
        "--output",
        str(output),
    ]
    save(output / "namespace-command.json", command)
    with (
        (output / "worker.stdout").open("wb") as stdout,
        (output / "worker.stderr").open("wb") as stderr,
    ):
        process = subprocess.Popen(
            command,
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
            env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"),
        )
    save(output / "supervisor.json", process_identity(process.pid))
    deadline = time.monotonic() + 100
    while time.monotonic() < deadline:
        if (output / "worker.json").exists():
            return dict(
                valid=True,
                output=str(output),
                worker=rpc(output, dict(op="status")),
                prepared_sha=sha(output / "prepared-input.json"),
            )
        if process.poll() is not None:
            raise RuntimeError("namespace worker exited; see " + str(output / "worker.stderr"))
        time.sleep(0.05)
    os.killpg(process.pid, signal.SIGKILL)
    process.wait(timeout=10)
    raise TimeoutError("owned worker startup exceeded 100 seconds")


class TimestampRows(list):
    def append(self, row):
        super().append(row | dict(response_finished_ns=time.perf_counter_ns()))


def clean_env(output, protocol):
    env = {key: value for key, value in os.environ.items() if not key.startswith("PVISOR_")}
    env.update(
        PVISOR_CACHE_TOKEN=protocol["cache_token"],
        PVISOR_STARTUP_TIMING="0",
        PVISOR_FS_PROFILE="0",
        PVISOR_LAZY_IMAGE_V2="1",
        PVISOR_LAZY_INDEX_PAGES="1",
        PVISOR_LAZY_BRIDGE_V2="1",
        PYTHONDONTWRITEBYTECODE="1",
        TMPDIR="/tmp",
        XDG_RUNTIME_DIR="/tmp",
        XDG_CONFIG_HOME=str(output / "service-config"),
        XDG_CACHE_HOME=str(output / "service-client-cache"),
    )
    return env


def observe(command, cwd, env, folder, marker, timeout):
    logs = dict(stdout=bytearray(), stderr=bytearray())
    ready = None
    clock_start = time.perf_counter_ns()
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    try:
        while selector.get_map():
            if time.perf_counter_ns() - clock_start > timeout * 1_000_000_000:
                raise TimeoutError("trial exceeded the fixed timeout")
            for key, _ in selector.select(0.05):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                logs[key.data].extend(data)
                if ready is None and marker in logs["stdout"]:
                    ready = time.perf_counter_ns()
        code = process.wait(timeout=5)
        finished = time.perf_counter_ns()
        return dict(
            exit_code=code,
            ready_ns=None if ready is None else ready - clock_start,
            completion_ns=finished - clock_start,
            clock_start_ns=clock_start,
            ready_observed_ns=ready,
            exit_observed_ns=finished,
        )
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)
        selector.close()
        process.stdout.close()
        process.stderr.close()
        for name, value in logs.items():
            (folder / (name + ".log")).write_bytes(value)


def run_trial(output, protocol, request, proxy, counts):
    trial_id, key = request["trial_id"], request["cache_key"]
    if not NAME.fullmatch(trial_id) or not NAME.fullmatch(key):
        raise ValueError("trial-id/cache-key must be bounded plain names")
    if request["mode"] not in ("prepared", "lazy") or request["cache_state"] not in (
        "fresh",
        "reuse",
    ):
        raise ValueError("unsupported mode/cache state")
    folder = output / "trials" / trial_id
    folder.mkdir()
    workspace = folder / "workspace"
    workspace.mkdir()
    (workspace / "startup-canary").write_bytes(CANARY)
    cache = output / "client-caches" / key
    if request["mode"] == "lazy":
        if request["cache_state"] == "fresh":
            cache.mkdir()  # Refuse existing paths; do not delete another trial's cache.
        elif not cache.is_dir() or not (cache / "successful-cold.json").is_file():
            raise ValueError("reuse requires this adapter's successful cold cache")
    else:
        cache = folder / "unused-client-cache"
    env = clean_env(output, protocol)
    env.update(
        XDG_CACHE_HOME=str(cache),
        XDG_CONFIG_HOME=str(folder / "config"),
        PVISOR_RUN_HOME=str(folder / "runs"),
        PVISOR_IMAGE_STORE=str(folder / "store"),
    )
    if request["mode"] == "lazy":
        env["PVISOR_CACHE_SERVER"] = f"tcp://127.0.0.1:{proxy.server_address[1]}"
    rootfs = protocol["rootfs"] if request["mode"] == "prepared" else "image=" + protocol["source"]
    command = [
        "taskset",
        "-c",
        ",".join(map(str, protocol["cpus"])),
        "/tmp/lazy-binaries/pvisor",
        "run",
        "--vm",
        "--rootfs",
        rootfs,
        "--no-agent-defaults",
        "--overlaynet",
        "off",
        "--gateway-mode",
        "off",
        "--stdio",
        "inherit",
        "--stage",
        str(folder / "stage"),
        "--cpu",
        str(len(protocol["cpus"])),
        "--memory",
        str(protocol["memory_mib"]) + "MiB",
        "--vm-library-dir",
        str(output / "frozen/firmware"),
        "--timeout",
        str(protocol["timeout_seconds"]) + "s",
        "--",
        *protocol["workload_argv"],
    ]
    save(folder / "command.json", command)
    result = dict(
        schema=1,
        valid=False,
        mode=request["mode"],
        cache_state=request["cache_state"],
        cache_key=key,
        evidence_dir=str(folder),
        timed_argv=command,
        prepared_sha=protocol["prepared_sha"],
        actualMode=None,
        workload_argv=protocol["workload_argv"],
    )
    before = counts.snapshot()
    try:
        if sha(output / "prepared-input.json") != protocol["prepared_sha"]:
            raise ValueError("prepared input receipt changed")
        for record in protocol["artifacts"].values():
            if sha(record["frozen"]) != record["sha256"]:
                raise ValueError("frozen artifact changed")
        result.update(
            observe(
                command,
                workspace,
                env,
                folder,
                protocol["marker"].encode(),
                protocol["timeout_seconds"],
            )
        )
        after = counts.snapshot()
        requests = after["rows"][len(before["rows"]) :]
        errors = after["errors"][len(before["errors"]) :]
        save(folder / "requests.json", dict(rows=requests, errors=errors))
        stdout = (folder / "stdout.log").read_bytes()
        if (
            result["exit_code"] != 0
            or result["ready_ns"] is None
            or stdout.splitlines().count(protocol["marker"].rstrip("\n").encode()) != 1
            or stdout.count(protocol["marker"].encode()) != 1
        ):
            raise ValueError("incorrect output, missing unique marker, or unsuccessful exit")
        bundles = list((folder / "stage").rglob("run-bundle.json"))
        if len(bundles) != 1:
            raise ValueError("missing/duplicate Run Bundle")
        bundle = json.loads(bundles[0].read_text())
        if (
            bundle["run"]["state"] != "completed"
            or bundle["run"]["exit_code"] != 0
            or bundle["run"]["executor"]["isolation"] != "virtual_machine"
            or not bundle["safety"]["filesystem_changes_staged"]
        ):
            raise ValueError("Run Bundle did not establish a completed staged VM")
        if (workspace / "startup-canary").read_bytes() != CANARY:
            raise ValueError("host canary changed")
        if list(workspace.iterdir()) != [workspace / "startup-canary"]:
            raise ValueError("read-only workload changed its host workspace")
        if any(p.is_file() or p.is_symlink() for p in (folder / "stage/upper").rglob("*")):
            raise ValueError("read-only workload produced staged changes")
        if errors:
            raise ValueError("cache counting proxy reported an error")

        def totals(rows):
            return dict(
                requests=len(rows),
                content_bytes=sum(r["content_bytes"] for r in rows),
                metadata_bytes=sum(r["metadata_bytes"] for r in rows),
                response_bytes=sum(r["response_bytes"] for r in rows),
                framing_and_json_bytes=sum(
                    r["response_bytes"] - r["content_bytes"] - r["metadata_bytes"] for r in rows
                ),
                metadata_response_bytes=sum(r["response_bytes"] for r in rows if r["op"] != "read"),
            )

        result.update(
            totals(requests),
            at_ready=totals(
                [r for r in requests if r["response_finished_ns"] <= result["ready_observed_ns"]]
            ),
            bundle=str(bundles[0]),
            actualMode=bundle["run"]["executor"]["isolation"],
            canary_unchanged=True,
            stage_readonly_validated=True,
            stdout_sha256=sha(folder / "stdout.log"),
            stderr_sha256=sha(folder / "stderr.log"),
            bundle_sha256=sha(bundles[0]),
            requests_sha256=sha(folder / "requests.json"),
        )
        if request["mode"] == "lazy":
            if request["cache_state"] == "fresh" and result["content_bytes"] <= 0:
                raise ValueError("fresh lazy cache obtained no remote file content")
            if request["cache_state"] == "reuse" and result["content_bytes"] != 0:
                raise ValueError("warm lazy cache fetched remote file content")
            if request["cache_state"] == "fresh":
                save(
                    cache / "successful-cold.json",
                    dict(trial_id=trial_id, source=protocol["source"]),
                )
        elif requests:
            raise ValueError("prepared-rootfs control unexpectedly used the cache service")
        result["valid"] = True
    except Exception as error:
        result["error"] = repr(error)
        (folder / "validation-error.log").write_text(traceback.format_exc())
    save(folder / "result.json", result)
    return result


def worker(output):
    protocol = json.loads((output / "protocol.json").read_text())
    legacy, startup = helpers(output)
    parent = json.loads((output / "containment-parent.json").read_text())
    own = namespaces()
    if any(own[key] == parent["namespaces"][key] for key in own):
        raise RuntimeError("namespace containment failed")
    with open("/dev/kvm", "rb+", buffering=0) as handle:
        if fcntl.ioctl(handle.fileno(), 0xAE00) != 12:
            raise RuntimeError("KVM API unavailable")
    save(
        output / "containment-child.json",
        dict(
            namespaces=own,
            kvm_api=12,
            host_listener_action="none; private tmpfs /tmp and pivot_root",
        ),
    )
    subprocess.run(
        ["mount", "-t", "tmpfs", "-o", "mode=1777,nosuid,nodev", "tmpfs", "/tmp"],
        check=True,
        timeout=10,
    )
    original = legacy.chroot_bind_plan
    legacy.chroot_bind_plan = lambda: startup.docker_bind_plan(original())
    try:
        chroot = legacy.enter_private_chroot(output, output)
    finally:
        legacy.chroot_bind_plan = original
    os.sched_setaffinity(0, protocol["service_cpus"])
    env = clean_env(output, protocol)
    reservation = socket.socket()
    reservation.bind(("127.0.0.1", 0))
    upstream_port = reservation.getsockname()[1]
    reservation.close()
    service_log = (output / "cache-service.log").open("wb")
    service = subprocess.Popen(
        [
            "/tmp/lazy-binaries/pvisor-daemon",
            "serve",
            "--cache",
            "--cache-listen",
            f"tcp://127.0.0.1:{upstream_port}",
            "--cache-image-store",
            str(output / "service-store"),
        ],
        env=env,
        stdout=service_log,
        stderr=subprocess.STDOUT,
    )
    counts = legacy.Counts()
    counts.rows = TimestampRows()
    proxy = legacy.TCPServer(("127.0.0.1", 0), legacy.CacheProxy)
    proxy.counts, proxy.upstream = counts, upstream_port
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    control = socket.socket()
    control.bind(("127.0.0.1", 0))
    control.listen(4)
    control.settimeout(1)
    token = secrets.token_hex(24)
    tainted = False
    try:
        deadline = time.monotonic() + 30
        while True:
            if service.poll() is not None:
                raise RuntimeError("cache service exited before readiness")
            try:
                with socket.create_connection(("127.0.0.1", upstream_port), timeout=0.1):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise TimeoutError("cache service readiness timed out")
                time.sleep(0.05)
        preparation_env = env | dict(
            PVISOR_CACHE_SERVER=f"tcp://127.0.0.1:{proxy.server_address[1]}"
        )
        prepared = subprocess.run(
            ["/tmp/lazy-binaries/pvisor", "cache", "prepare", protocol["source"]],
            env=preparation_env,
            capture_output=True,
            timeout=60,
        )
        (output / "cached-service-prepare.stdout").write_bytes(prepared.stdout)
        (output / "cached-service-prepare.stderr").write_bytes(prepared.stderr)
        if prepared.returncode:
            raise RuntimeError("cached-service prepare failed")
        startup.validate_prepared_record(prepared.stdout, protocol["digest"])
        save(output / "preparation-requests.json", counts.snapshot())
        if counts.errors:
            raise RuntimeError("proxy error during cached service preparation")
        save(
            output / "worker.json",
            dict(
                port=control.getsockname()[1],
                token=token,
                cache_proxy_port=proxy.server_address[1],
                cache_service_port=upstream_port,
                namespace=own,
                prepared_sha=sha(output / "prepared-input.json"),
            ),
        )
        (output / "worker.json").chmod(0o600)
        expires = time.monotonic() + protocol["lifetime_seconds"]
        while time.monotonic() < expires:
            try:
                connection, _ = control.accept()
            except socket.timeout:
                continue
            with connection:
                connection.settimeout(5)
                try:
                    request = receive(connection)
                    if not secrets.compare_digest(request.pop("token", ""), token):
                        raise ValueError("control authentication failed")
                    if request["op"] == "stop":
                        send(connection, dict(valid=True, stopped=True))
                        break
                    if request["op"] == "status":
                        send(
                            connection,
                            dict(
                                valid=not tainted and service.poll() is None,
                                tainted=tainted,
                                prepared_sha=sha(output / "prepared-input.json"),
                            ),
                        )
                        continue
                    if request["op"] != "trial" or tainted or service.poll() is not None:
                        raise ValueError(
                            "worker unavailable or invalid operation; stop tainted workers"
                        )
                    result = run_trial(output, protocol, request, proxy, counts)
                    tainted = not result["valid"]
                    send(connection, result)
                except Exception as error:
                    send(connection, dict(valid=False, error=repr(error)))
        legacy.validate_launch_tree(Path("/"))
        legacy.verify_launch_binaries(chroot["binaries"])
    finally:
        control.close()
        proxy.shutdown()
        proxy.server_close()
        service.terminate()
        try:
            service.wait(timeout=10)
        except subprocess.TimeoutExpired:
            service.kill()
            service.wait(timeout=5)
        service_log.close()
        final_rootfs = legacy.prepared_store_manifest(Path(protocol["rootfs"]))
        frozen_unchanged = all(
            sha(record["frozen"]) == record["sha256"] for record in protocol["artifacts"].values()
        )
        helpers_unchanged = all(
            sha(output / "frozen/harness" / name) == digest
            for name, digest in protocol["helper_sha256"].items()
        )
        runtime_libraries_unchanged = all(
            sha(Path(name)) == digest for name, digest in protocol["runtime_libraries"].items()
        )
        save(
            output / "worker-stop.json",
            dict(
                tainted=tainted,
                cache_service_exit=service.returncode,
                frozen_artifacts_unchanged=frozen_unchanged,
                helpers_unchanged=helpers_unchanged,
                adapter_unchanged=sha(output / "frozen/adapter.py") == protocol["adapter_sha256"],
                runtime_libraries_unchanged=runtime_libraries_unchanged,
                rootfs_unchanged=final_rootfs
                == json.loads((output / "rootfs-manifest.json").read_text()),
            ),
        )


def stop(output):
    result = rpc(output, dict(op="stop"))
    supervisor = json.loads((output / "supervisor.json").read_text())
    deadline = time.monotonic() + 20
    while process_identity(supervisor["pid"]) == supervisor and time.monotonic() < deadline:
        # An exited orphan can remain a zombie; its namespaces are already gone.
        try:
            state = (
                (Path("/proc") / str(supervisor["pid"]) / "stat")
                .read_text()
                .rpartition(")")[2]
                .split()[0]
            )
            if state == "Z":
                break
        except OSError:
            break
        time.sleep(0.05)
    own = json.loads((output / "containment-child.json").read_text())["namespaces"]["user"]
    survivors = []
    for path in Path("/proc").iterdir():
        if path.name.isdigit():
            try:
                if os.readlink(path / "ns/user") == own:
                    survivors.append(int(path.name))
            except OSError:
                pass
    parent = json.loads((output / "containment-parent.json").read_text())
    before = parent["existing_pvisor"]
    after = existing_pvisor()
    result.update(
        owned_namespace_survivors=survivors,
        **service_preservation(before, after, parent["namespaces"]["user"]),
    )
    receipt = (
        json.loads((output / "worker-stop.json").read_text())
        if (output / "worker-stop.json").exists()
        else {}
    )
    result.update(receipt)
    result["valid"] = bool(
        result["valid"]
        and not survivors
        and result["existing_pvisor_preserved"]
        and all(
            receipt.get(key)
            for key in (
                "rootfs_unchanged",
                "frozen_artifacts_unchanged",
                "helpers_unchanged",
                "adapter_unchanged",
                "runtime_libraries_unchanged",
            )
        )
    )
    save(output / "teardown.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    preparation = commands.add_parser("prepare")
    preparation.add_argument("--output", type=Path, required=True)
    preparation.add_argument("--prepared-store", type=Path, required=True)
    preparation.add_argument("--binary-dir", type=Path, default=ROOT / "target/release")
    preparation.add_argument("--firmware", type=Path)
    preparation.add_argument("--source")
    preparation.add_argument(
        "--workload", choices=("ubuntu-shell", "numpy-script"), default="ubuntu-shell"
    )
    preparation.add_argument("--cpus", default="0,1")
    preparation.add_argument("--service-cpus", default="2,3")
    preparation.add_argument("--memory-mib", type=int, default=2048)
    preparation.add_argument("--timeout", type=int, default=90)
    preparation.add_argument("--lifetime", type=int, default=7200)
    trial = commands.add_parser("trial")
    trial.add_argument("--output", type=Path, required=True)
    trial.add_argument("--mode", choices=("prepared", "lazy"), required=True)
    trial.add_argument("--trial-id", required=True)
    trial.add_argument("--cache-key", default="prepared")
    trial.add_argument("--cache-state", choices=("fresh", "reuse"), default="fresh")
    for name in ("stop", "status", "_worker"):
        command = commands.add_parser(name)
        command.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.action == "_worker":
        worker(args.output.resolve())
        return
    try:
        if args.action == "prepare":
            result = prepare(args)
        elif args.action == "stop":
            result = stop(args.output.resolve())
        elif args.action == "status":
            result = rpc(args.output.resolve(), dict(op="status"))
        else:
            result = rpc(
                args.output.resolve(),
                dict(
                    op="trial",
                    mode=args.mode,
                    trial_id=args.trial_id,
                    cache_key=args.cache_key,
                    cache_state=args.cache_state,
                ),
            )
    except Exception as error:
        result = dict(valid=False, error=repr(error))
    print(json.dumps(result, sort_keys=True), flush=True)
    if not result.get("valid"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
