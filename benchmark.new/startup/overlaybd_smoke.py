#!/usr/bin/env python3
"""Run one owned-lab OCI/OverlayBD functional smoke, without performance scores.

All subprocess arguments and output, registry manifests, live resource checks,
and cleanup audits are retained in a new lab/smoke-* directory, also on failure.
Timeouts stop the local Docker client; they do not imply remote task cleanup.
"""

import argparse
import hashlib
import http.client
import json
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

CTR = "/opt/overlaybd/snapshotter/ctr"
SOCKET = "/lab/run/containerd.sock"
MARKER = b"LAZY_READY ubuntu 26.04\n"
SHELL = (
    '. /etc/os-release; test "$ID" = ubuntu; '
    'test "$VERSION_ID" = 26.04; '
    "printf 'LAZY_READY ubuntu 26.04\\n'; sleep 2"
)
LIVE_CHECK = (
    r"""set -eu
leaf=$1
test -d "$leaf"
for attr in memory.max cpu.max cgroup.procs cgroup.events; do
    printf '\n=== %s ===\n' "$attr"
    cat "$leaf/$attr"
done
printf '\n=== tcmu-targets ===\n'
for target in /sys/kernel/config/target/core/user_*/*; do
    test -d "$target" || continue
    test -f "$target/info" || continue
    printf '%s\n' "$target"
    cat "$target/info"
done
printf '\n=== block-devices ===\n'
ls -l /dev/sd* 2>/dev/null || true
printf '\n=== mountinfo ===\n'
cat /proc/self/mountinfo
printf '\n=== container-info ===\n'
"""
    + CTR
    + r""" --address /lab/run/containerd.sock --namespace "$2" containers info "$3"
"""
)


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


class Evidence:
    def __init__(self, directory, timeout):
        self.directory = directory
        self.timeout = timeout
        self.index = 0

    def command(self, argv, *, observe=None, timeout=None):
        """Drain both pipes; run one observation after the complete Ready line."""
        self.index += 1
        directory = self.directory / f"{self.index:02d}-command"
        directory.mkdir()
        argv = [str(part) for part in argv]
        save(directory / "argv.json", argv)
        status = {
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "timed_out": False,
            "observation_performed": False,
        }
        output = {"stdout": bytearray(), "stderr": bytearray()}
        process = None
        observation_error = None
        try:
            process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            deadline = time.monotonic() + (timeout or self.timeout)
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, "stdout")
                selector.register(process.stderr, selectors.EVENT_READ, "stderr")
                while selector.get_map():
                    if time.monotonic() >= deadline:
                        status["timed_out"] = True
                        process.kill()
                        # A remote docker-exec process might still be running.
                        break
                    for key, _ in selector.select(timeout=0.1):
                        chunk = key.fileobj.read1(65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        output[key.data].extend(chunk)
                    if (
                        observe
                        and not status["observation_performed"]
                        and MARKER in output["stdout"]
                    ):
                        status["observation_performed"] = True
                        try:
                            observe()
                        except Exception as error:
                            observation_error = error
            process.wait(timeout=5)
            status["returncode"] = process.returncode
        except BaseException as error:
            status["error"] = str(error)
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            raise
        finally:
            for name, data in output.items():
                (directory / f"{name}.txt").write_bytes(data)
            if process is not None:
                for stream in (process.stdout, process.stderr):
                    stream.close()
            save(directory / "status.json", status)
        if status["timed_out"] or status["returncode"] != 0:
            raise RuntimeError(f"Command failed or timed out; see {directory}")
        if observation_error is not None:
            raise RuntimeError(f"Live resource check failed: {observation_error}")
        if observe and not status["observation_performed"]:
            raise RuntimeError("Ready marker was not observed; live budgets were not verified")
        return bytes(output["stdout"])


def registry_manifest(evidence, port, repository, reference, name):
    path = f"/v2/{repository}/manifests/{reference}"
    receipt = {"method": "GET", "url": f"http://127.0.0.1:{port}{path}"}
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    try:
        connection.request(
            "GET",
            path,
            headers={
                "Accept": ", ".join(
                    (
                        "application/vnd.oci.image.manifest.v1+json",
                        "application/vnd.docker.distribution.manifest.v2+json",
                    )
                )
            },
        )
        response = connection.getresponse()
        receipt.update(status=response.status, headers=response.getheaders())
        body = response.read(8 * 1024 * 1024 + 1)
        (evidence / f"{name}-manifest.json").write_bytes(body)
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        receipt["computed_digest"] = digest
        declared = response.getheader("Docker-Content-Digest")
        if response.status != 200 or len(body) > 8 * 1024 * 1024:
            raise ValueError(f"Registry manifest request failed: {response.status}")
        if declared != digest:
            raise ValueError("Registry Docker-Content-Digest does not match response SHA-256")
        if reference.startswith("sha256:") and digest != reference:
            raise ValueError("Source manifest does not match the frozen source digest")
        parsed = json.loads(body)
        if "config" not in parsed or "layers" not in parsed or "manifests" in parsed:
            raise ValueError("Smoke requires a single-platform image manifest")
        return digest
    finally:
        connection.close()
        save(evidence / f"{name}-registry-receipt.json", receipt)


def inventory_delta(before, after):
    delta = {}
    for kind in ("targets", "block_devices", "uio"):
        old, new = before[kind], after[kind]
        delta[kind] = {
            "added": sorted(new.keys() - old.keys()),
            "removed": sorted(old.keys() - new.keys()),
            "changed": sorted(key for key in old.keys() & new.keys() if old[key] != new[key]),
        }
    delta["boot_changed"] = before.get("boot_id") != after.get("boot_id")
    delta["errors"] = after["errors"]
    return delta


def audit_inventory(before, directory, *, wait):
    deadline = time.monotonic() + wait
    while True:
        after = host_inventory()
        delta = inventory_delta(before, after)
        clean = (
            not delta["boot_changed"]
            and not delta["errors"]
            and all(
                not values
                for kind in ("targets", "block_devices", "uio")
                for values in delta[kind].values()
            )
        )
        if clean or time.monotonic() >= deadline:
            save(directory / "host-after.json", after)
            save(directory / "host-delta.json", delta)
            if not clean:
                raise RuntimeError(
                    "Host TCMU/block/UIO inventory differs; preserve lab and inspect host-delta.json"
                )
            return
        time.sleep(0.25)


def validate_live(raw, snapshotter, reference):
    sections = {}
    current = None
    for line in raw.decode().splitlines():
        if line.startswith("=== ") and line.endswith(" ==="):
            current = line[4:-4]
            sections[current] = []
        elif current:
            sections[current].append(line)

    def text(name):
        return "\n".join(sections[name]).strip()

    if text("memory.max") != "2147483648":
        raise ValueError("Live cgroup memory.max is not 2 GiB")
    quota, period = text("cpu.max").split()
    if quota == "max" or int(period) <= 0 or int(quota) != 2 * int(period):
        raise ValueError("Live cgroup cpu.max is not 2 CPUs")
    processes = text("cgroup.procs").split()
    if not processes or not all(value.isdigit() for value in processes):
        raise ValueError("Live task is absent from the owned cgroup")
    info = json.loads(text("container-info"))
    if info.get("Snapshotter") != snapshotter or info.get("Image") != reference:
        raise ValueError("Container info does not match the frozen image and snapshotter")
    if snapshotter == "overlaybd" and "/sys/kernel/config/target/core/user_" not in text(
        "tcmu-targets"
    ):
        raise ValueError("No live TCMU target was observed for OverlayBD")


def recovery_text(container, ctr, prefix, identities):
    lines = [
        "Functional smoke only. Preserve this directory and lab logs on failure.",
        "A timed-out Docker client does not prove the remote command/task stopped.",
        "Inspect first; run deletion commands only for the exact smoke IDs below.",
        shlex.join(["docker", "logs", container]),
        shlex.join(["docker", "top", container, "-eo", "pid,args"]),
        shlex.join(ctr + ["tasks", "list"]),
        shlex.join(ctr + ["containers", "list"]),
    ]
    for identity in identities:
        lines += [
            f"# Only if this smoke's task/container {identity} remains:",
            shlex.join(ctr + ["tasks", "kill", "--signal", "SIGKILL", identity]),
            shlex.join(ctr + ["tasks", "delete", "--force", identity]),
            shlex.join(ctr + ["containers", "delete", identity]),
            "# After the task is absent, remove only this empty owned leaf:",
            shlex.join(
                [
                    "docker",
                    "exec",
                    container,
                    "sh",
                    "-ec",
                    'test -z "$(cat "$1/cgroup.procs")"; rmdir -- "$1"',
                    "sh",
                    "/sys/fs/cgroup" + prefix + "/" + identity,
                ]
            ),
        ]
    lines += [
        "# After all owned tasks/leaves are absent, remove only the empty owned parent:",
        shlex.join(
            [
                "docker",
                "exec",
                container,
                "sh",
                "-ec",
                'test -z "$(cat "$1/cgroup.procs")"; rmdir -- "$1"',
                "sh",
                "/sys/fs/cgroup" + prefix,
            ]
        ),
        "Do not remove global TCMU targets, block/UIO devices, snapshots, images, or other tasks.",
        "For a lingering pull, inspect docker top and the exact recorded reference before manual recovery.",
        "Re-run overlaybd_lab.py status --lab PATH and inspect inventories before stopping the lab.",
    ]
    return "\n".join(lines) + "\n"


def smoke(args):
    lab = args.lab.resolve(strict=True)
    directory = lab / (
        "smoke-"
        + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        + "-"
        + uuid.uuid4().hex[:8]
    )
    directory.mkdir(exist_ok=False)
    evidence = Evidence(directory, args.timeout)
    result = {
        "scope": "functional smoke only; no cache-state or performance claim",
        "status": "failed",
        "evidence": str(directory),
        "cases": [],
    }
    baseline = None
    docker = ctr = None
    (directory / "recovery.txt").write_text(
        "Inspect failure.txt and command evidence before retrying. No deletion is authorized "
        "until Docker ownership and the exact smoke task IDs are established.\n"
    )
    try:
        inputs = {}
        for name in ("lab.json", "source-image.json"):
            data = (lab / name).read_bytes()
            (directory / name).write_bytes(data)
            inputs[name] = json.loads(data)
        manifest, source = inputs["lab.json"], inputs["source-image.json"]
        for path in (Path(__file__), Path(__file__).with_name("overlaybd_lab.py")):
            data = path.read_bytes()
            (directory / path.name).write_bytes(data)
        identity = manifest["id"]
        if not re.fullmatch(r"[a-f0-9]{12}", identity) or Path(manifest["lab"]).resolve() != lab:
            raise ValueError("Invalid lab identity/path")
        prefix = "/system.slice/pvisor-overlaybd-lab-" + identity
        if (
            manifest.get("owned_cgroup_prefix") != prefix
            or manifest.get("cgroup_namespace") != "host"
        ):
            raise ValueError(
                "Lab must explicitly own its cgroup prefix and use host cgroup namespace"
            )
        namespace = manifest["namespace"]
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", namespace):
            raise ValueError("Invalid private containerd namespace")
        inspected = json.loads(
            evidence.command(["docker", "inspect", manifest["container"]], timeout=20)
        )[0]
        if (inspected["Config"].get("Labels") or {}).get(LABEL) != identity:
            raise ValueError("Docker lab ownership label mismatch")
        if (
            not inspected["State"]["Running"]
            or inspected["Image"] != manifest["image_id"]
            or inspected["HostConfig"].get("CgroupnsMode") != "host"
        ):
            raise ValueError("Docker lab runtime/image/cgroup namespace differs from manifest")
        if not any(
            mount.get("Destination") == "/lab"
            and mount.get("RW")
            and Path(mount["Source"]).resolve() == lab
            for mount in inspected["Mounts"]
        ):
            raise ValueError("Docker /lab bind does not match the requested owned lab")
        container = inspected["Id"]
        docker = ["docker", "exec", container]
        ctr = docker + [CTR, "--address", SOCKET, "--namespace", namespace]
        suffix = uuid.uuid4().hex[:12]
        identities = ["smoke-oci-" + suffix, "smoke-turbo-" + suffix]
        (directory / "recovery.txt").write_text(recovery_text(container, ctr, prefix, identities))
        port = int(manifest["port"])
        repository = source["repository"]
        match = re.fullmatch(
            r"127\.0\.0\.1:" + str(port) + r"/([a-z0-9]+(?:[._/-][a-z0-9]+)*)", repository
        )
        if not match or not re.fullmatch(r"sha256:[a-f0-9]{64}", source["source_digest"]):
            raise ValueError("Source must use the owned loopback registry and a SHA-256 digest")
        source_digest = registry_manifest(
            directory, port, match[1], source["source_digest"], "source"
        )
        turbo_digest = registry_manifest(directory, port, match[1], "turbo", "turbo")
        result.update(
            source_digest=source_digest,
            derived_digest=turbo_digest,
            source_reference=repository + "@" + source_digest,
            derived_reference=repository + "@" + turbo_digest,
            docker_container_id=container,
            namespace=namespace,
        )
        save(
            directory / "frozen-images.json",
            {key: value for key, value in result.items() if key.endswith(("digest", "reference"))},
        )
        baseline = host_inventory()
        save(directory / "host-before.json", baseline)
        if baseline["errors"]:
            raise RuntimeError("Cannot establish host kernel-object baseline")
        # All cgroup reads occur in the host-cgroup-namespace lab container.
        evidence.command(
            docker
            + [
                "sh",
                "-ec",
                'test ! -e "$1" || { test -z "$(cat "$1/cgroup.procs")"; '
                'test -z "$(find "$1" -mindepth 1 -maxdepth 1 -type d -print -quit)"; }',
                "sh",
                "/sys/fs/cgroup" + prefix,
            ],
            timeout=20,
        )
        for name, snapshotter, digest, task_id in (
            ("oci", "overlayfs", source_digest, identities[0]),
            ("turbo", "overlaybd", turbo_digest, identities[1]),
        ):
            case = {
                "name": name,
                "snapshotter": snapshotter,
                "task_id": task_id,
                "image": repository + "@" + digest,
                "status": "failed",
            }
            result["cases"].append(case)
            if name == "oci":
                pull = [
                    "images",
                    "pull",
                    "--plain-http",
                    "--snapshotter",
                    snapshotter,
                    case["image"],
                ]
            else:
                pull = ["rpull", "--plain-http", "--snapshotter", snapshotter, case["image"]]
            evidence.command(ctr + pull)

            def observe():
                raw = evidence.command(
                    docker
                    + [
                        "sh",
                        "-ec",
                        LIVE_CHECK,
                        "sh",
                        "/sys/fs/cgroup" + prefix + "/" + task_id,
                        namespace,
                        task_id,
                    ],
                    timeout=15,
                )
                (directory / f"{name}-live.txt").write_bytes(raw)
                validate_live(raw, snapshotter, case["image"])

            raw = evidence.command(
                ctr
                + [
                    "run",
                    "--rm",
                    "--snapshotter",
                    snapshotter,
                    "--cpus",
                    "2",
                    "--memory-limit",
                    "2147483648",
                    "--cgroup",
                    prefix + "/" + task_id,
                    case["image"],
                    task_id,
                    "/bin/sh",
                    "-ec",
                    SHELL,
                ],
                observe=observe,
            )
            if raw != MARKER:
                raise ValueError(
                    "Smoke stdout must be exactly LAZY_READY ubuntu 26.04 followed by newline"
                )
            for resource in ("tasks", "containers"):
                raw = evidence.command(ctr + [resource, "list"], timeout=20)
                if task_id in [
                    line.split()[0] for line in raw.decode().splitlines() if line.split()
                ]:
                    raise RuntimeError(f"Smoke {resource} object remains: {task_id}")
            # rmdir can remove only the exact empty leaf/parent owned by this lab.
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
            case_directory = directory / name
            case_directory.mkdir()
            audit_inventory(baseline, case_directory, wait=15)
            case["status"] = "passed"
            case["budgets_verified"] = {"memory_bytes": 2147483648, "cpu_quota": 2}
            save(directory / "result.json", result)
        result["status"] = "passed"
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
                result["audit_error"] = str(audit_error)
        result["recovery"] = str(directory / "recovery.txt")
    finally:
        save(directory / "result.json", result)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result["status"] == "passed" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--lab", type=Path, required=True, help="Existing owned OverlayBD lab directory"
    )
    parser.add_argument(
        "--timeout", type=int, default=600, help="Per pull/run timeout in seconds (default: 600)"
    )
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    return smoke(args)


if __name__ == "__main__":
    sys.exit(main())
