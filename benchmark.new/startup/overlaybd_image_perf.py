#!/usr/bin/env python3
"""Single owned-lab image startup trials; not a campaign or result publisher.

Run --action prepare once per condition/CPU pair, then pass its result.json as
--prepare-receipt to individual warm-prepared trials. Preparation and validation
are outside the clock. Docker exec client startup is inside the clock. No global
cache, image, device, or container cleanup is performed.

The cold-warm-pair scenario restarts only the owned lab, archives its private
client state, and times pull plus run twice. Registry artifacts and host page
cache remain prepared. A failed reset is preserved for manual recovery.
"""

import argparse
import hashlib
import json
import os
import re
import selectors
import shlex
import subprocess
import sys
import time
import traceback
import uuid
from datetime import datetime, timezone
from pathlib import Path

sys.dont_write_bytecode = True
from overlaybd_lab import LABEL, host_inventory  # noqa: E402 - import after module-loading setup
from overlaybd_smoke import (  # noqa: E402 - import after module-loading setup
    CTR,
    LIVE_CHECK,
    MARKER,
    SOCKET,
    Evidence,
    audit_inventory,
    recovery_text,
    registry_manifest,
    save,
    validate_live,
)

SHELL = (
    r'. /etc/os-release; test "$ID" = ubuntu; test "$VERSION_ID" = 26.04; '
    r'printf "LAZY_READY ubuntu %s\n" "$VERSION_ID"'
)
BINARIES = (
    CTR,
    "/opt/overlaybd/snapshotter/overlaybd-snapshotter",
    "/opt/overlaybd/bin/overlaybd-tcmu",
    "/usr/local/bin/containerd",
    "/usr/local/bin/containerd-shim-runc-v2",
    "/usr/local/bin/runc",
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cpu_pair(value):
    if not re.fullmatch(r"[0-9]+,[0-9]+", value):
        raise argparse.ArgumentTypeError("--cpus must name exactly two CPUs, for example 12,13")
    cpus = [int(part) for part in value.split(",")]
    if len(set(cpus)) != 2:
        raise argparse.ArgumentTypeError("--cpus must name two different CPUs")
    return sorted(cpus)


def capture(argv, directory, timeout, *, include_pull=False):
    """Popen-to-exact-Ready/normal-exit observer, retaining invalid evidence."""
    directory.mkdir()
    save(directory / "argv.json", argv)
    record = {
        "clock": "perf_counter_ns",
        "valid": False,
        "ready_ns": None,
        "completion_ns": None,
        "exit_code": None,
        "includes": "Docker exec client, ctr run, runtime and task exit",
        "excludes": "Python startup, pull, validation and cleanup",
    }
    if include_pull:
        record.update(
            includes="Docker exec client, in-lab shell/taskset, pull, ctr run and task exit",
            excludes="Python startup, reset, validation and cleanup",
        )
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    process = None
    ready_at = None
    pending = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            record["start_ns"] = time.perf_counter_ns()
            process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            deadline = time.monotonic() + timeout
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            selector.register(process.stderr, selectors.EVENT_READ, "stderr")
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise TimeoutError("Trial timed out; remote docker-exec/task state is unknown")
                for key, _ in selector.select(0.1):
                    block = os.read(key.fd, 65536)
                    received = time.perf_counter_ns()
                    if not block:
                        selector.unregister(key.fileobj)
                        continue
                    streams[key.data].extend(block)
                    if key.data == "stdout":
                        pending.extend(block)
                        while b"\n" in pending:
                            line, _, rest = pending.partition(b"\n")
                            pending = bytearray(rest)
                            if bytes(line) + b"\n" == MARKER:
                                if ready_at is not None:
                                    raise ValueError("Duplicate exact Ready marker")
                                ready_at = received
            record["exit_code"] = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            record["end_ns"] = time.perf_counter_ns()
        if record["exit_code"] != 0 or bytes(streams["stdout"]) != MARKER or ready_at is None:
            raise ValueError("Trial requires exact unique Ready stdout and normal exit 0")
        record.update(
            valid=True,
            marker_ns=ready_at,
            ready_ns=ready_at - record["start_ns"],
            completion_ns=record["end_ns"] - record["start_ns"],
        )
        return record
    except BaseException as error:
        record["error"] = str(error)
        raise
    finally:
        if process is not None:
            if process.poll() is None:
                process.kill()  # Local client only; do not claim remote cleanup.
            process.wait(timeout=5)
            process.stdout.close()
            process.stderr.close()
        for name, data in streams.items():
            (directory / f"{name}.txt").write_bytes(data)
        save(directory / "timing.json", record)


def locate_frozen(lab, requested):
    if requested:
        return requested.resolve(strict=True)
    for candidate in sorted(lab.glob("smoke-*/frozen-images.json"), reverse=True):
        result = candidate.with_name("result.json")
        if result.is_file() and json.loads(result.read_text()).get("status") == "passed":
            return candidate
    raise ValueError("Supply --frozen-images from a passed owned-lab smoke")


def verify_lab(args, directory, evidence):
    lab = args.lab.resolve(strict=True)
    manifest = json.loads((lab / "lab.json").read_text())
    source = json.loads((lab / "source-image.json").read_text())
    identity = manifest["id"]
    if not re.fullmatch(r"[0-9a-f]{12}", identity) or Path(manifest["lab"]).resolve() != lab:
        raise ValueError("Lab identity/path mismatch")
    prefix = "/system.slice/pvisor-overlaybd-lab-" + identity
    if manifest.get("owned_cgroup_prefix") != prefix or manifest.get("cgroup_namespace") != "host":
        raise ValueError("Require owned cgroup prefix and host cgroup namespace")
    namespace = manifest["namespace"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", namespace):
        raise ValueError("Invalid private containerd namespace")
    inspected = json.loads(
        evidence.command(["docker", "inspect", manifest["container"]], timeout=20)
    )[0]
    if (
        (inspected["Config"].get("Labels") or {}).get(LABEL) != identity
        or inspected["Image"] != manifest["image_id"]
        or inspected["HostConfig"].get("CgroupnsMode") != "host"
        or inspected["HostConfig"].get("PidMode", "") not in ("", "private")
        or not inspected["State"]["Running"]
    ):
        raise ValueError("Docker label, image, cgroup/PID namespace, or running state mismatch")
    if not any(
        mount.get("Destination") == "/lab"
        and mount.get("RW")
        and Path(mount["Source"]).resolve() == lab
        for mount in inspected["Mounts"]
    ):
        raise ValueError("Docker /lab mount ownership mismatch")
    frozen_path = locate_frozen(lab, args.frozen_images)
    frozen = json.loads(frozen_path.read_text())
    repository = source["repository"]
    port = int(manifest["port"])
    repository_match = re.fullmatch(
        r"127\.0\.0\.1:" + str(port) + r"/([a-z0-9]+(?:[._/-][a-z0-9]+)*)", repository
    )
    if not repository_match or source["source_digest"] != frozen["source_digest"]:
        raise ValueError("Frozen image source differs from the private lab source")
    for name in ("source", "derived"):
        image_digest = frozen[name + "_digest"]
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image_digest):
            raise ValueError("Frozen image requires a SHA-256 digest")
        if frozen[name + "_reference"] != repository + "@" + image_digest:
            raise ValueError("Frozen reference does not match its repository and digest")
        registry_manifest(directory, port, repository_match[1], image_digest, name)
    for name in (
        "lab.json",
        "source-image.json",
        "containerd.toml",
        "snapshotter.json",
        "overlaybd.json",
    ):
        (directory / name).write_bytes((lab / name).read_bytes())
    (directory / "frozen-images.json").write_bytes(frozen_path.read_bytes())
    docker = ["docker", "exec", inspected["Id"]]
    ctr = docker + [CTR, "--address", SOCKET, "--namespace", namespace]
    binary_raw = evidence.command(docker + ["sha256sum", *BINARIES], timeout=20)
    binaries = {}
    for line in binary_raw.decode().splitlines():
        checksum, path = line.split(maxsplit=1)
        binaries[path.strip()] = checksum
    if set(binaries) != set(BINARIES) or not all(
        re.fullmatch(r"[a-f0-9]{64}", value) for value in binaries.values()
    ):
        raise ValueError("Cannot freeze lab binary hashes")
    harness = {}
    for path in (
        Path(__file__),
        Path(__file__).with_name("overlaybd_lab.py"),
        Path(__file__).with_name("overlaybd_smoke.py"),
    ):
        harness[path.name] = digest(path)
        (directory / path.name).write_bytes(path.read_bytes())
    provenance = {
        "lab_id": identity,
        "docker_container_id": inspected["Id"],
        "lab_image_id": manifest["image_id"],
        "namespace": namespace,
        "source_digest": frozen["source_digest"],
        "derived_digest": frozen["derived_digest"],
        "source_reference": frozen["source_reference"],
        "derived_reference": frozen["derived_reference"],
        "frozen_images_sha256": digest(frozen_path),
        "source_manifest_sha256": digest(directory / "source-manifest.json"),
        "derived_manifest_sha256": digest(directory / "derived-manifest.json"),
        "binaries": binaries,
        "harness": harness,
        "configs": {
            name: digest(directory / name)
            for name in ("containerd.toml", "snapshotter.json", "overlaybd.json")
        },
    }
    save(directory / "provenance.json", provenance)
    return manifest, frozen, docker, ctr, prefix, provenance


def no_tasks(evidence, ctr, identities=()):
    for resource in ("tasks", "containers"):
        raw = evidence.command(ctr + [resource, "list"], timeout=20)
        rows = [line.split()[0] for line in raw.decode().splitlines()[1:] if line.split()]
        if (not identities and rows) or any(identity in rows for identity in identities):
            raise RuntimeError(f"Private namespace retains {resource}: {rows}")


def cleanup(evidence, docker, prefix, task_id, baseline, directory, ctr):
    no_tasks(evidence, ctr, (task_id,))
    evidence.command(
        docker
        + [
            "sh",
            "-ec",
            'for d in "$1" "$2"; do test ! -d "$d" || '
            '{ test -z "$(cat "$d/cgroup.procs")"; rmdir -- "$d"; }; done',
            "sh",
            "/sys/fs/cgroup" + prefix + "/" + task_id,
            "/sys/fs/cgroup" + prefix,
        ],
        timeout=20,
    )
    audit_inventory(baseline, directory, wait=15)


def validate_prepare(args, result, provenance, baseline, directory):
    if args.prepare_receipt is None:
        raise ValueError(
            "Run --action prepare once, then supply its result.json with --prepare-receipt"
        )
    receipt = json.loads(args.prepare_receipt.read_text())
    if (
        not receipt.get("valid")
        or not receipt.get("budgets_verified")
        or receipt.get("action") != "prepare"
        or receipt.get("condition") != args.condition
        or receipt.get("cpus") != args.cpus
        or receipt.get("boot_id") != baseline["boot_id"]
        or receipt.get("workload_argv") != ["/bin/sh", "-ec", SHELL]
        or receipt.get("memory_bytes") != 2147483648
        or receipt.get("cpu_quota") != 2
    ):
        raise ValueError("Prepare receipt does not match budgets, CPUs, condition, and host boot")
    old = receipt.get("provenance", {})
    # The independently frozen warm adapter can supply its live-budget proof to
    # this new pair adapter. Its images, binaries, config and container generation
    # must still match; record both harness versions rather than equating them.
    keys = set(provenance) - {"harness"}
    if any(old.get(key) != provenance[key] for key in keys):
        raise ValueError("Prepare receipt differs from the lab, images, binaries, or configuration")
    if args.scenario == "warm-prepared" and old.get("harness") != provenance["harness"]:
        raise ValueError("Warm-prepared receipt must use the same frozen harness")
    (directory / "prepare-receipt.json").write_bytes(args.prepare_receipt.read_bytes())
    result.update(
        prepare_receipt_sha256=digest(args.prepare_receipt),
        budget_preflight_harness=old.get("harness"),
    )


def assert_reset_configuration(lab):
    """Reject layouts whose mutable state is outside the three reset directories."""
    overlaybd = json.loads((lab / "overlaybd.json").read_text())
    snapshotter = json.loads((lab / "snapshotter.json").read_text())
    if (
        overlaybd["cacheConfig"].get("cacheType") != "file"
        or overlaybd["cacheConfig"].get("cacheDir") != "/lab/cache/registry"
        or overlaybd["gzipCacheConfig"].get("cacheDir") != "/lab/cache/gzip"
        or overlaybd["download"].get("enable") is not False
        or overlaybd.get("p2pConfig", {}).get("enable") is not False
        or snapshotter.get("root") != "/lab/containerd-root/io.containerd.snapshotter.v1.overlaybd"
        or snapshotter.get("address") != "/lab/run/overlaybd.sock"
        or snapshotter.get("autoRemoveDev") is not True
    ):
        raise ValueError("Cold reset supports only the private file/block/gzip cache layout")
    config = (lab / "containerd.toml").read_text()
    for key, value in (("root", "/lab/containerd-root"), ("state", "/lab/run/containerd")):
        matches = re.findall(r"^" + key + r'\s*=\s*"([^"]+)"\s*$', config, re.MULTILINE)
        if matches != [value]:
            raise ValueError("Containerd mutable paths differ from the private reset contract")


def all_namespaces_idle(evidence, ctr):
    raw = evidence.command(ctr + ["namespaces", "list"], timeout=20)
    names = [line.split()[0] for line in raw.decode().splitlines()[1:] if line.split()]
    for name in names:
        for resource in ("tasks", "containers", "leases"):
            raw = evidence.command(ctr + ["--namespace", name, resource, "list"], timeout=20)
            if any(line.strip() for line in raw.decode().splitlines()[1:]):
                raise RuntimeError(f"Cold reset refuses namespace {name} retaining {resource}")
    return names


def pin_services(evidence, docker, cpus):
    script = r"""set -eu
cpus=$1
for process in /proc/[0-9]*; do
    exe=$(readlink "$process/exe" 2>/dev/null) || continue
    case "$exe" in
      /bin/registry) service=registry ;;
      /opt/overlaybd/bin/overlaybd-tcmu) service=tcmu ;;
      /opt/overlaybd/snapshotter/overlaybd-snapshotter) service=snapshotter ;;
      /usr/local/bin/containerd) service=containerd ;;
      *) continue ;;
    esac
    pid=${process##*/}
    printf 'SERVICE %s %s\n' "$service" "$pid"
    /lab/tools/taskset -apc "$cpus" "$pid"
    for thread in "$process"/task/[0-9]*; do
        mask=$(awk '/^Cpus_allowed_list:/ {print $2}' "$thread/status")
        printf 'AFFINITY %s %s %s %s\n' "$service" "$pid" "${thread##*/}" "$mask"
    done
done
"""
    raw = evidence.command(
        docker + ["sh", "-ec", script, "sh", ",".join(map(str, cpus))], timeout=30
    )
    services = {}
    threads = []
    for line in raw.decode().splitlines():
        if line.startswith("SERVICE "):
            _, service, pid = line.split()
            if service in services:
                raise ValueError("More than one owned service of the same kind")
            services[service] = int(pid)
        elif line.startswith("AFFINITY "):
            _, service, pid, tid, mask = line.split()
            effective = set()
            for part in mask.split(","):
                low, _, high = part.partition("-")
                effective.update(range(int(low), int(high or low) + 1))
            if effective != set(cpus):
                raise ValueError(f"Service {service} thread {tid} has unexpected affinity {mask}")
            threads.append({"service": service, "pid": int(pid), "tid": int(tid), "cpus": mask})
    expected = {"registry", "tcmu", "snapshotter", "containerd"}
    if set(services) != expected or {item["service"] for item in threads} != expected:
        raise ValueError("Could not restore and read back all four owned service affinities")
    return {"cpus": cpus, "services": services, "threads": threads}


def reset_client(args, manifest, docker, ctr, prefix, baseline, directory, evidence):
    """Archive only this lab's client state after its consumers have stopped."""
    lab = args.lab.resolve()
    assert_reset_configuration(lab)
    if not (lab / "tools/taskset").is_file():
        raise ValueError("Cold restart requires the already installed lab/tools/taskset")
    receipt_path = directory / "reset-receipt.json"
    archive_name = "retired-client-state-" + uuid.uuid4().hex
    worker = "pvisor-overlaybd-reset-" + manifest["id"] + "-" + uuid.uuid4().hex[:8]
    receipt = {
        "valid": False,
        "phase": "preflight",
        "docker_container_id": docker[-1],
        "lab_id": manifest["id"],
        "worker_container": worker,
        "retired_state": str(lab / archive_name),
        "reset_paths": ["containerd-root", "cache", "run"],
        "preserved": ["registry", "conversion", "global host page cache", "other services"],
        "taskset_sha256": digest(lab / "tools/taskset"),
    }
    save(receipt_path, receipt)
    with (directory / "recovery.txt").open("a") as stream:
        stream.write(
            "\nRead-only reset recovery commands:\n"
            + "\n".join(
                (
                    shlex.join(["docker", "inspect", docker[-1]]),
                    shlex.join(["docker", "logs", docker[-1]]),
                    shlex.join(["docker", "inspect", worker]),
                )
            )
            + "\n"
        )
    # Empty task lists are not enough: a pull can hold a lease without a task.
    receipt["namespaces_checked"] = all_namespaces_idle(evidence, ctr)
    evidence.command(
        docker
        + [
            "sh",
            "-ec",
            r"""set -eu
root=/sys/fs/cgroup$1
if test -d "$root"; then
  test -z "$(cat "$root/cgroup.procs")"
  test -z "$(find "$root" -mindepth 1 -maxdepth 1 -type d -print -quit)"
  rmdir -- "$root"
fi
awk '$5 == "/lab/containerd-root" || index($5,"/lab/containerd-root/")==1 {print; bad=1} END {exit bad}' /proc/self/mountinfo
for process in /proc/[0-9]*; do
  exe=$(readlink "$process/exe" 2>/dev/null) || continue
  case "$exe" in
    */ctr|*/convertor|*/runc|*/containerd-shim-runc-v2) printf 'busy: %s %s\n' "$process" "$exe"; exit 1 ;;
  esac
done
""",
            "sh",
            prefix,
        ],
        timeout=20,
    )
    audit_dir = directory / "reset-before-stop"
    audit_dir.mkdir()
    audit_inventory(baseline, audit_dir, wait=15)
    receipt["phase"] = "stopping-owned-lab"
    save(receipt_path, receipt)
    evidence.command(["docker", "stop", "--time", "30", docker[-1]], timeout=45)
    inspected = json.loads(evidence.command(["docker", "inspect", docker[-1]], timeout=20))[0]
    state = inspected["State"]
    if (
        state["Running"]
        or state.get("OOMKilled")
        or state.get("ExitCode") != 0
        or state.get("Error")
    ):
        raise RuntimeError("Owned lab did not stop normally; client state has not been reset")
    audit_dir = directory / "reset-after-stop"
    audit_dir.mkdir()
    audit_inventory(baseline, audit_dir, wait=15)
    processes = evidence.command(["ps", "-eo", "comm="], timeout=20)
    if "overlaybd-tcmu" in [line.strip() for line in processes.decode().splitlines()]:
        raise RuntimeError("Another shared TCMU handler is visible; preserve stopped lab")
    receipt["phase"] = "archiving-private-state"
    save(receipt_path, receipt)
    # This ordinary temporary container mounts only this lab directory. No
    # devices, host cgroups, configfs, network, or Docker socket are exposed.
    script = r"""set -eu
archive=/lab/$1
test ! -e "$archive"
for name in containerd-root cache run registry conversion log; do
  test ! -L "/lab/$name"
done
mkdir "$archive"
mkdir "$archive/service-logs"
for log in /lab/log/*; do
  test -f "$log" && test ! -L "$log" || continue
  cp -p -- "$log" "$archive/service-logs/"
done
stat -c 'PRESERVED %n %d:%i' /lab/registry /lab/conversion
for name in containerd-root cache run; do
  if test -e "/lab/$name"; then
    test -d "/lab/$name"
    mv -- "/lab/$name" "$archive/$name"
    printf 'ARCHIVED %s\n' "$name"
  fi
  mkdir "/lab/$name"
  test -z "$(find "/lab/$name" -mindepth 1 -maxdepth 1 -print -quit)"
  printf 'EMPTY %s\n' "$name"
done
mkdir /lab/cache/registry /lab/cache/gzip
stat -c 'PRESERVED %n %d:%i' /lab/registry /lab/conversion
"""
    raw = evidence.command(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            worker,
            "--label",
            LABEL + "=" + manifest["id"],
            "--label",
            "io.pvisor.overlaybd-role=client-reset",
            "--network",
            "none",
            "--security-opt",
            "label=disable",
            "--cpuset-cpus",
            ",".join(map(str, args.service_cpus)),
            "--cpus",
            "2",
            "--mount",
            "type=bind,src=" + str(lab) + ",dst=/lab",
            "--entrypoint",
            "/bin/sh",
            manifest["image_id"],
            "-ec",
            script,
            "reset-private-client",
            archive_name,
        ],
        timeout=120,
    )
    lines = raw.decode().splitlines()
    if {line for line in lines if line.startswith("EMPTY ")} != {
        "EMPTY containerd-root",
        "EMPTY cache",
        "EMPTY run",
    }:
        raise RuntimeError("Fresh private client directories were not verified empty")
    preserved = [line for line in lines if line.startswith("PRESERVED ")]
    if len(preserved) != 4 or preserved[:2] != preserved[2:]:
        raise RuntimeError("Registry/conversion directory identity changed during reset")
    receipt.update(
        phase="starting-owned-lab",
        empty_directories_verified=True,
        preserved_directory_identity=preserved[:2],
    )
    save(receipt_path, receipt)
    evidence.command(["docker", "start", docker[-1]], timeout=30)
    deadline = time.monotonic() + 60
    while True:
        try:
            raw = evidence.command(ctr + ["plugins", "list"], timeout=10)
            if re.search(rb"snapshotter\S*\s+overlaybd\s+.*\bok\b", raw):
                break
        except RuntimeError:
            pass
        if time.monotonic() >= deadline:
            raise RuntimeError(
                "Restarted owned lab did not become healthy; preserve reset evidence"
            )
        time.sleep(0.2)
    receipt["service_affinity"] = pin_services(evidence, docker, args.service_cpus)
    # Warm the restarted registry's metadata route from the observer, without
    # creating any client image/content state. Server processes are restarted;
    # preserved server artifacts/page cache do not imply preserved Go heap state.
    source = json.loads((lab / "source-image.json").read_text())
    frozen = json.loads((directory / "frozen-images.json").read_text())
    repository = source["repository"].split("/", 1)[1]
    server_dir = directory / "registry-after-restart"
    server_dir.mkdir()
    for name in ("source", "derived"):
        registry_manifest(
            server_dir, int(manifest["port"]), repository, frozen[name + "_digest"], name
        )
    receipt["registry_process_restarted"] = True
    receipt["registry_metadata_rechecked_outside_client"] = True
    # Assert metadata/content are empty before any timed image request. Creating
    # an empty namespace is harness setup; no image is fetched or unpacked here.
    raw = evidence.command(ctr + ["namespaces", "list"], timeout=20)
    namespaces = [line.split()[0] for line in raw.decode().splitlines()[1:] if line.split()]
    if manifest["namespace"] not in namespaces:
        evidence.command(ctr + ["namespaces", "create", manifest["namespace"]], timeout=20)
    for resource in ("images", "content", "tasks", "containers", "leases"):
        raw = evidence.command(ctr + [resource, "list"], timeout=20)
        if any(line.strip() for line in raw.decode().splitlines()[1:]):
            raise RuntimeError(f"Reset private client still has {resource}")
    for snapshotter in ("overlayfs", "overlaybd"):
        raw = evidence.command(
            ctr + ["snapshots", "--snapshotter", snapshotter, "list"], timeout=20
        )
        if any(line.strip() for line in raw.decode().splitlines()[1:]):
            raise RuntimeError("Reset private client still has image snapshots")
    audit_dir = directory / "reset-after-start"
    audit_dir.mkdir()
    audit_inventory(baseline, audit_dir, wait=15)
    tool_hash = (
        evidence.command(docker + ["sha256sum", "/lab/tools/taskset"], timeout=20)
        .decode()
        .split()[0]
    )
    if tool_hash != receipt["taskset_sha256"]:
        raise ValueError("Service/client taskset changed across restart")
    receipt.update(valid=True, phase="empty-client-ready", metadata_and_content_empty=True)
    save(receipt_path, receipt)
    return receipt


def registry_observation(lab, cursor, directory, *, blob_roles=None):
    path = lab / "log/registry.log"
    current = path.stat()
    if cursor[0] != (current.st_dev, current.st_ino) or current.st_size < cursor[1]:
        raise ValueError("Registry log changed identity or was truncated during trial")
    with path.open("rb") as stream:
        stream.seek(cursor[1])
        raw = stream.read()
    (directory / "registry-responses.log").write_bytes(raw)
    requests = []
    # Use only the structured response-completed entry. The registry also emits
    # access lines for the same requests; counting both would double the bytes.
    for line in raw.decode(errors="replace").splitlines():
        if 'msg="response completed"' not in line:
            continue
        fields = {}
        for name in (
            "http.request.method",
            "http.request.uri",
            "http.response.status",
            "http.response.written",
        ):
            match = re.search(re.escape(name) + r'=("(?:\\.|[^"\\])*"|\S+)', line)
            if not match:
                raise ValueError("Cannot audit registry response fields")
            value = match[1]
            fields[name] = json.loads(value) if value.startswith('"') else value
        uri = fields["http.request.uri"]
        status = int(fields["http.response.status"])
        role = "metadata"
        if status >= 400:
            role = "error"
        elif "/blobs/" in uri:
            blob = uri.split("?", 1)[0].rsplit("/", 1)[-1]
            role = (blob_roles or {}).get(blob, "unclassified")
        requests.append(
            {
                "method": fields["http.request.method"],
                "uri": uri,
                "status": status,
                "body_bytes": int(fields["http.response.written"]),
                "role": role,
            }
        )
    blobs = [item for item in requests if "/blobs/" in item["uri"] and item["method"] == "GET"]
    summary = {
        "requests": requests,
        "request_count": len(requests),
        "response_body_bytes": sum(item["body_bytes"] for item in requests),
        "blob_body_bytes": sum(item["body_bytes"] for item in blobs),
        "blob_get_requests": len(blobs),
        "http_206_requests": sum(item["status"] == 206 for item in requests),
        "http_200_requests": sum(item["status"] == 200 for item in requests),
        "content_body_bytes": sum(
            item["body_bytes"] for item in requests if item["role"] == "content"
        ),
        "metadata_body_bytes": sum(
            item["body_bytes"] for item in requests if item["role"] == "metadata"
        ),
        "error_body_bytes": sum(item["body_bytes"] for item in requests if item["role"] == "error"),
        "unclassified_body_bytes": sum(
            item["body_bytes"] for item in requests if item["role"] == "unclassified"
        ),
        "scope": "completion-observation",
        "ready_boundary_available": False,
        "observation_cutoff": "after completion and cleanup audit; no Ready transfer split",
        "accounting": "registry response-body bytes, excludes HTTP/TCP overhead",
    }
    save(directory / "registry-responses.json", summary)
    return summary


def image_blob_roles(directory):
    roles = {}
    for name in ("source", "derived"):
        image = json.loads((directory / f"{name}-manifest.json").read_text())
        roles[image["config"]["digest"]] = "metadata"
        for layer in image["layers"]:
            annotations = layer.get("annotations", {})
            target = annotations.get("containerd.io/snapshot/overlaybd/turbo-oci/target-digest")
            roles[layer["digest"]] = "metadata" if target else "content"
            if target:
                roles[target] = "content"
    return roles


def run_pair(args, manifest, docker, ctr, prefix, baseline, directory, evidence, result):
    if args.service_cpus is None or set(args.service_cpus) & set(args.cpus):
        raise ValueError("Cold pair requires --service-cpus with two CPUs disjoint from --cpus")
    result["metric_scope"] = "Popen before docker exec to Ready/normal exit, including pull and run"
    result["cache_scope"] = (
        "cold private client then warm private client; registry artifacts and host page cache retained"
    )
    result["service_cpus"] = args.service_cpus
    identities = ["image-cold-" + uuid.uuid4().hex[:16], "image-warm-" + uuid.uuid4().hex[:16]]
    (directory / "recovery.txt").write_text(
        recovery_text(docker[-1], ctr, prefix, identities)
        + "\nCold reset journal: reset-receipt.json. An incomplete reset is not rolled back automatically.\n"
        + "The original client state is retained at retired_state. Do not merge it into fresh directories.\n"
        + "Inspect the exact reset worker named in that journal if a Docker client timed out.\n"
        + "If reset fails after stopping the lab, leave it stopped until the journal and archived paths are reviewed.\n"
    )
    result["reset_receipt_file"] = str(directory / "reset-receipt.json")
    try:
        reset_client(args, manifest, docker, ctr, prefix, baseline, directory, evidence)
    finally:
        if (directory / "reset-receipt.json").is_file():
            result["reset_receipt"] = json.loads((directory / "reset-receipt.json").read_text())
            result["reset_receipt_sha256"] = digest(directory / "reset-receipt.json")
    cpus = ",".join(map(str, args.cpus))
    blob_roles = image_blob_roles(directory)
    save(directory / "registry-blob-roles.json", blob_roles)
    for phase, task_id in zip(("cold", "warm"), identities):
        phase_dir = directory / phase
        phase_dir.mkdir()
        save(phase_dir / "host-before.json", baseline)
        pull = ["rpull"] if args.condition == "turbo" else ["images", "pull"]
        pull = (
            ctr[3:]
            + pull
            + ["--plain-http", "--snapshotter", result["snapshotter"], result["image"]]
        )
        run = ctr[3:] + [
            "run",
            "--rm",
            "--snapshotter",
            result["snapshotter"],
            "--cpus",
            "2",
            "--cpuset-cpus",
            cpus,
            "--memory-limit",
            "2147483648",
            "--cgroup",
            prefix + "/" + task_id,
            result["image"],
            task_id,
            "/bin/sh",
            "-ec",
            SHELL,
        ]
        save(phase_dir / "in-lab-argv.json", {"pull": pull, "run": run})
        script = shlex.join(pull) + " 1>&2\nexec " + shlex.join(run)
        argv = docker + ["/lab/tools/taskset", "-c", cpus, "/bin/sh", "-ec", script]
        log = args.lab.resolve() / "log/registry.log"
        stat = log.stat()
        cursor = ((stat.st_dev, stat.st_ino), stat.st_size)
        fields = (
            "condition",
            "cpus",
            "service_cpus",
            "memory_bytes",
            "cpu_quota",
            "snapshotter",
            "image",
            "workload_argv",
            "provenance",
            "boot_id",
            "metric_scope",
            "cache_scope",
            "isolation",
            "prepare_receipt_sha256",
            "budget_preflight_harness",
        )
        phase_result = {name: result[name] for name in fields}
        phase_result.update(
            valid=False,
            ready_ns=None,
            completion_ns=None,
            evidence_dir=str(phase_dir),
            pair_evidence_dir=str(directory),
            phase=phase,
            scenario=phase + "-client",
            task_id=task_id,
            namespace=manifest["namespace"],
            reset_receipt_path=str(directory / "reset-receipt.json"),
            reset_receipt_sha256=result["reset_receipt_sha256"],
        )
        try:
            timing = capture(argv, phase_dir / "trial", args.timeout, include_pull=True)
            phase_result.update(
                ready_ns=timing["ready_ns"],
                completion_ns=timing["completion_ns"],
                exit_code=timing["exit_code"],
            )
            cleanup(evidence, docker, prefix, task_id, baseline, phase_dir, ctr)
            responses = registry_observation(
                args.lab.resolve(), cursor, phase_dir, blob_roles=blob_roles
            )
            phase_result["registry"] = responses
            # Cold starts from proved empty client directories. Warm may check
            # metadata but must not fetch any blob body, including index blobs.
            if phase == "cold" and responses["blob_body_bytes"] <= 0:
                raise ValueError("Cold client transferred no observable registry blob payload")
            if phase == "warm" and responses["blob_body_bytes"] != 0:
                raise ValueError("Warm client unexpectedly fetched registry blob payload")
            if responses["unclassified_body_bytes"]:
                raise ValueError(
                    "Registry blob response could not be assigned to metadata or content"
                )
            phase_result["valid"] = True
        except BaseException as error:
            phase_result["error"] = str(error)
            raise
        finally:
            save(phase_dir / "result.json", phase_result)
            result[phase] = phase_result
            save(directory / "result.json", result)


def execute(args):
    lab = args.lab.resolve(strict=True)
    directory = (
        args.output.resolve()
        if args.output
        else lab
        / (
            "image-perf-"
            + args.action
            + "-"
            + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
            + "-"
            + uuid.uuid4().hex[:8]
        )
    )
    if any(
        directory.is_relative_to(lab / name)
        for name in (
            "containerd-root",
            "cache",
            "run",
            "registry",
            "conversion",
            "log",
            "tools",
            "context",
        )
    ):
        raise ValueError("Evidence must be outside the lab's mutable/service/artifact directories")
    directory.mkdir(parents=True, exist_ok=False)
    evidence = Evidence(directory, args.timeout)
    result = {
        "valid": False,
        "ready_ns": None,
        "completion_ns": None,
        "evidence_dir": str(directory),
        "action": args.action,
        "scenario": args.scenario,
        "condition": args.condition,
        "workload_argv": ["/bin/sh", "-ec", SHELL],
        "cpus": args.cpus,
        "memory_bytes": 2147483648,
        "cpu_quota": 2,
        "metric_scope": "Popen before docker exec to exact Ready and normal exit 0",
        "cache_scope": "prepared client; block/page cache state is not globally flushed",
        "isolation": "containerd/runc shared-host-kernel container; not VM isolation",
    }
    baseline = None
    ctr = None
    try:
        allowed = sorted(os.sched_getaffinity(0))
        if not set(args.cpus).issubset(allowed):
            raise ValueError(f"Requested CPUs are outside observer affinity: {allowed}")
        result["observer_allowed_cpus_before"] = allowed
        os.sched_setaffinity(0, args.cpus)
        result["observer_affinity"] = sorted(os.sched_getaffinity(0))
        manifest, frozen, docker, ctr, prefix, provenance = verify_lab(args, directory, evidence)
        result["provenance"] = provenance
        cpus = ",".join(map(str, args.cpus))
        task_id = "image-perf-" + uuid.uuid4().hex[:16]
        result["task_id"] = task_id
        (directory / "recovery.txt").write_text(recovery_text(docker[-1], ctr, prefix, (task_id,)))
        no_tasks(evidence, ctr)
        baseline = host_inventory()
        save(directory / "host-before.json", baseline)
        if baseline["errors"] or baseline["targets"]:
            raise RuntimeError("Require readable host inventory and no live TCMU targets")
        result["boot_id"] = baseline["boot_id"]
        snapshotter = "overlaybd" if args.condition == "turbo" else "overlayfs"
        image = (
            frozen["derived_reference"] if args.condition == "turbo" else frozen["source_reference"]
        )
        result.update(snapshotter=snapshotter, image=image)
        if args.action == "trial":
            validate_prepare(args, result, provenance, baseline, directory)
        if args.scenario == "cold-warm-pair":
            result.pop("task_id", None)
            run_pair(args, manifest, docker, ctr, prefix, baseline, directory, evidence, result)
        else:
            # Re-confirm image preparation outside the timed Popen interval.
            pull = ["rpull"] if args.condition == "turbo" else ["images", "pull"]
            evidence.command(ctr + pull + ["--plain-http", "--snapshotter", snapshotter, image])
            run = ctr + [
                "run",
                "--rm",
                "--snapshotter",
                snapshotter,
                "--cpus",
                "2",
                "--cpuset-cpus",
                cpus,
                "--memory-limit",
                "2147483648",
                "--cgroup",
                prefix + "/" + task_id,
                image,
                task_id,
                "/bin/sh",
                "-ec",
            ]
            if args.action == "prepare":

                def observe():
                    script = LIVE_CHECK.replace(
                        "cgroup.procs cgroup.events",
                        "cgroup.procs cgroup.events cpuset.cpus.effective",
                    )
                    raw = evidence.command(
                        docker
                        + [
                            "sh",
                            "-ec",
                            script,
                            "sh",
                            "/sys/fs/cgroup" + prefix + "/" + task_id,
                            manifest["namespace"],
                            task_id,
                        ],
                        timeout=15,
                    )
                    (directory / "live-budgets.txt").write_bytes(raw)
                    validate_live(raw, snapshotter, image)
                    match = re.search(rb"=== cpuset.cpus.effective ===\n([^\n]+)", raw)
                    if not match:
                        raise ValueError("Missing live cpuset.cpus.effective")
                    effective = set()
                    for part in match[1].decode().strip().split(","):
                        low, _, high = part.partition("-")
                        effective.update(range(int(low), int(high or low) + 1))
                    if effective != set(args.cpus):
                        raise ValueError("Live task CPU set differs from the selected two CPUs")

                raw = evidence.command(run + [SHELL + "; sleep 2"], observe=observe)
                if raw != MARKER:
                    raise ValueError("Preparation requires exact marker and normal exit 0")
                result["budgets_verified"] = True
            else:
                timing = capture(run + [SHELL], directory / "trial", args.timeout)
                result.update(ready_ns=timing["ready_ns"], completion_ns=timing["completion_ns"])
            cleanup(evidence, docker, prefix, task_id, baseline, directory, ctr)
        result["valid"] = True
    except BaseException as error:
        result["error"] = str(error)
        (directory / "failure.txt").write_text(traceback.format_exc())
        if ctr is not None:
            for resource in ("tasks", "containers"):
                try:
                    evidence.command(ctr + [resource, "list"], timeout=20)
                except Exception as audit_error:
                    result[resource + "_audit_error"] = str(audit_error)
        if baseline is not None:
            try:
                audit_inventory(baseline, directory, wait=0)
            except Exception as audit_error:
                result["cleanup_error"] = str(audit_error)
    finally:
        save(directory / "result.json", result)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["valid"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lab", type=Path, required=True)
    parser.add_argument("--condition", choices=("oci", "turbo"), required=True)
    parser.add_argument(
        "--scenario", choices=("warm-prepared", "cold-warm-pair"), default="warm-prepared"
    )
    parser.add_argument("--action", choices=("prepare", "trial"), default="trial")
    parser.add_argument("--cpus", type=cpu_pair, required=True)
    parser.add_argument(
        "--service-cpus",
        type=cpu_pair,
        help="Cold pair: restore all four owned services to this disjoint CPU pair",
    )
    parser.add_argument(
        "--frozen-images", type=Path, help="Defaults to latest passed lab smoke frozen-images.json"
    )
    parser.add_argument(
        "--prepare-receipt",
        type=Path,
        help="Matching successful prepare/result.json; required for trial",
    )
    parser.add_argument(
        "--output", type=Path, help="New evidence directory; existing paths are refused"
    )
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.scenario == "cold-warm-pair" and (args.action != "trial" or args.service_cpus is None):
        parser.error("cold-warm-pair requires --action trial and explicit --service-cpus")
    return execute(args)


if __name__ == "__main__":
    sys.exit(main())
