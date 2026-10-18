#!/usr/bin/env python3
"""Deploy a private OverlayBD functional-test lab; this is not a benchmark runner.

Uses an existing Docker daemon and official, already unpacked OverlayBD packages.
The privileged lab shares host TCMU/configfs/devices, but keeps configuration,
registry data, containerd state, mounts and caches in its own container/directory.
No host systemd/containerd configuration or global caches are changed.
"""

import argparse
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

LABEL = "io.pvisor.overlaybd-lab"


def run(argv, **kwargs):
    return subprocess.run([str(value) for value in argv], check=True, **kwargs)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def prepare(args):
    lab = args.lab.resolve()
    lab.mkdir(parents=True, exist_ok=False)
    identity = uuid.uuid4().hex[:12]
    context = lab / "context"
    rootfs = context / "rootfs"
    rootfs.mkdir(parents=True)
    for directory in ("run", "log", "cache", "registry", "conversion"):
        (lab / directory).mkdir()
    receipts = []

    def copy_file(source, destination):
        target = rootfs / destination.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target, follow_symlinks=True)
        receipts.append(
            {
                "source": str(Path(source).resolve()),
                "destination": destination,
                "sha256": sha256(target),
            }
        )

    obd = args.overlaybd_root.resolve() / "opt/overlaybd"
    snap = args.snapshotter_root.resolve() / "opt/overlaybd/snapshotter"
    for source in (obd, snap):
        if not source.is_dir():
            raise ValueError(f"Missing unpacked package directory: {source}")
        for path in sorted(source.rglob("*")):
            if path.is_file():
                copy_file(
                    path,
                    "/opt/overlaybd/" + str(path.relative_to(obd))
                    if path.is_relative_to(obd)
                    else "/opt/overlaybd/snapshotter/" + str(path.relative_to(snap)),
                )
    executables = []
    for name in ("containerd", "containerd-shim-runc-v2", "runc"):
        source = shutil.which(name)
        if source is None:
            raise ValueError(f"Missing host executable: {name}")
        destination = "/usr/local/bin/" + name
        copy_file(source, destination)
        executables.append(rootfs / destination.lstrip("/"))
    executables.extend(
        path
        for path in (rootfs / "opt/overlaybd").rglob("*")
        if path.is_file() and os.access(path, os.X_OK)
    )
    libraries = set()
    for executable in executables:
        result = subprocess.run(["ldd", str(executable)], text=True, capture_output=True)
        if "not found" in result.stdout:
            raise ValueError(f"Missing dynamic dependency: {executable}\n{result.stdout}")
        for line in result.stdout.splitlines():
            match = re.search(r"(?:=>\s+)?(/\S+)\s+\(", line)
            if match:
                libraries.add(match.group(1))
    for path in sorted(libraries):
        # The host's dependency paths must be preserved for its ELF interpreter.
        if str(rootfs) not in path:
            copy_file(path, path)
    bundle = next(
        (
            Path(path)
            for path in ("/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/certs/ca-certificates.crt")
            if Path(path).is_file()
        ),
        None,
    )
    if bundle:
        copy_file(bundle, "/etc/ssl/certs/ca-certificates.crt")
    (rootfs / "etc").mkdir(exist_ok=True)
    (rootfs / "etc/nsswitch.conf").write_text("hosts: files dns\n")
    overlaybd = {
        "logConfig": {"logLevel": 1, "logPath": "/lab/log/overlaybd.log"},
        "cacheConfig": {"cacheType": "file", "cacheDir": "/lab/cache/registry", "cacheSizeGB": 4},
        "gzipCacheConfig": {"enable": True, "cacheDir": "/lab/cache/gzip", "cacheSizeGB": 4},
        "credentialConfig": {"mode": "file", "path": "/lab/cred.json"},
        "ioEngine": 0,
        "download": {"enable": False},
        "p2pConfig": {"enable": False},
        "enableAudit": False,
    }
    save(lab / "overlaybd.json", overlaybd)
    (rootfs / "etc/overlaybd").mkdir()
    save(rootfs / "etc/overlaybd/overlaybd.json", overlaybd)
    save(lab / "cred.json", {"auths": {}})
    save(
        lab / "snapshotter.json",
        {
            "root": "/lab/containerd-root/io.containerd.snapshotter.v1.overlaybd",
            "address": "/lab/run/overlaybd.sock",
            "verbose": "info",
            "rwMode": "overlayfs",
            "runtimeType": "containerd",
            "autoRemoveDev": True,
            "mirrorRegistry": [{"host": f"127.0.0.1:{args.port}", "insecure": True}],
        },
    )
    (lab / "containerd.toml").write_text("""version = 2
root = "/lab/containerd-root"
state = "/lab/run/containerd"
disabled_plugins = ["io.containerd.grpc.v1.cri", "io.containerd.cri.v1.runtime", "io.containerd.cri.v1.images"]
[grpc]
  address = "/lab/run/containerd.sock"
[proxy_plugins.overlaybd]
  type = "snapshot"
  address = "/lab/run/overlaybd.sock"
""")
    (lab / "registry.yml").write_text(f"""version: 0.1
log:
  level: info
storage:
  filesystem:
    rootdirectory: /lab/registry
http:
  addr: 0.0.0.0:{args.port}
  secret: {uuid.uuid4().hex}
""")
    (context / "start.sh").write_text("""#!/bin/sh
set -eu
mkdir -p /lab/run /lab/log
/bin/registry serve /lab/registry.yml > /lab/log/registry.log 2>&1 &
registry_pid=$!
/opt/overlaybd/bin/overlaybd-tcmu /lab/overlaybd.json > /lab/log/tcmu.stdout.log 2>&1 &
tcmu_pid=$!
/opt/overlaybd/snapshotter/overlaybd-snapshotter /lab/snapshotter.json > /lab/log/snapshotter.log 2>&1 &
snapshotter_pid=$!
/usr/local/bin/containerd --config /lab/containerd.toml > /lab/log/containerd.log 2>&1 &
containerd_pid=$!
trap 'kill "$containerd_pid" "$snapshotter_pid" "$tcmu_pid" "$registry_pid" 2>/dev/null || true; wait || true; exit 0' TERM INT
while kill -0 "$registry_pid" "$tcmu_pid" "$snapshotter_pid" "$containerd_pid" 2>/dev/null; do
  sleep 1 &
  wait $! || true
done
exit 1
""")
    (context / "Dockerfile").write_text("""FROM registry:3
COPY rootfs/ /
COPY --chmod=0755 start.sh /usr/local/bin/lab-start
ENV PATH="/opt/overlaybd/snapshotter:/opt/overlaybd/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ENTRYPOINT ["/usr/local/bin/lab-start"]
""")
    manifest = {
        "schema": 1,
        "id": identity,
        "container": "pvisor-overlaybd-" + identity,
        "image": "pvisor-overlaybd-lab:" + identity,
        "port": args.port,
        "lab": str(lab),
        "artifact_receipts": receipts,
        "scope": "functional deployment, not benchmark results",
        "background_download": False,
        "namespace": "pvisor-obd-smoke",
        "cgroup_namespace": "host",
        "owned_cgroup_prefix": "/system.slice/pvisor-overlaybd-lab-" + identity,
    }
    save(lab / "lab.json", manifest)
    run(["docker", "build", "--pull=false", "-t", manifest["image"], context])
    image = json.loads(subprocess.check_output(["docker", "image", "inspect", manifest["image"]]))[
        0
    ]
    manifest["image_id"] = image["Id"]
    save(lab / "lab.json", manifest)
    print(lab)


def load(lab):
    return json.loads((lab / "lab.json").read_text())


def inspect_container(manifest):
    inspected = json.loads(
        subprocess.check_output(["docker", "inspect", manifest["container"]], timeout=20)
    )[0]
    if (inspected["Config"].get("Labels") or {}).get(LABEL) != manifest["id"]:
        raise ValueError("Container ownership label mismatch")
    return inspected


def host_inventory():
    """Observe shared kernel objects; reading never establishes deletion authority."""
    inventory = {"targets": {}, "block_devices": {}, "uio": {}, "mountinfo": [], "errors": []}
    try:
        inventory["boot_id"] = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        inventory["mountinfo"] = Path("/proc/self/mountinfo").read_text().splitlines()
    except OSError as error:
        inventory["errors"].append(str(error))
    targets = Path("/sys/kernel/config/target/core")
    try:
        if not targets.is_dir():
            raise OSError("configfs target/core is unavailable")
        for group in sorted(targets.iterdir()):
            if not group.name.startswith("user_") or not group.is_dir():
                continue
            for path in sorted(group.iterdir()):
                if not path.is_dir():
                    continue
                record = {"path": str(path), "inode": path.stat().st_ino}
                for name in ("info", "udev_path", "enable"):
                    attribute = path / name
                    try:
                        if attribute.exists():
                            record[name] = attribute.read_text().strip()
                    except OSError as error:
                        record[name + "_error"] = str(error)
                        inventory["errors"].append(str(error))
                inventory["targets"][str(path)] = record
    except OSError as error:
        inventory["errors"].append(str(error))
    for kind, directory in (("block_devices", "/sys/class/block"), ("uio", "/sys/class/uio")):
        try:
            root = Path(directory)
            if not root.exists() and kind == "uio":
                continue
            for path in sorted(root.iterdir()):
                record = {"sysfs_path": str(path.resolve()), "inode": path.stat().st_ino}
                for name in ("dev", "name", "device/vendor", "device/model"):
                    attribute = path / name
                    try:
                        if attribute.exists():
                            record[name] = attribute.read_text().strip()
                    except OSError as error:
                        record[name + "_error"] = str(error)
                inventory[kind][path.name] = record
        except OSError as error:
            inventory["errors"].append(str(error))
    return inventory


def capture_baseline(lab, manifest, phase, note, container_id=None):
    inventory = host_inventory()
    if inventory["errors"]:
        raise RuntimeError("Cannot freeze incomplete host inventory: " + repr(inventory["errors"]))
    baseline = {
        "schema": 1,
        "lab_id": manifest["id"],
        "captured_at_unix": time.time(),
        "phase": phase,
        "predates_start": phase == "pre-start",
        "note": note,
        "container_id": container_id,
        "host": inventory,
    }
    # Never overwrite an earlier observation or disguise a late observation.
    with (lab / "baseline.json").open("x") as stream:
        stream.write(json.dumps(baseline, indent=2) + "\n")
    return baseline


def owned_cgroups(manifest):
    prefix = "/system.slice/pvisor-overlaybd-lab-" + manifest["id"]
    if manifest.get("owned_cgroup_prefix", prefix) != prefix:
        raise ValueError("Owned cgroup prefix does not match the lab identity")
    root = Path("/sys/fs/cgroup") / prefix.lstrip("/")
    result = {
        "prefix": prefix,
        "path": str(root),
        "directories": {},
        "errors": [],
        "verified": False,
        "scope": "caller mount/cgroup namespace",
    }
    try:
        if not Path("/sys/fs/cgroup/system.slice/cgroup.type").is_file():
            result["unverified"] = "Caller cannot observe the host system.slice cgroup hierarchy"
            return result
        if root.exists():
            for path in (root, *(p for p in root.rglob("*") if p.is_dir())):
                record = {}
                for name in ("cgroup.procs", "cgroup.events", "cgroup.type"):
                    record[name] = (path / name).read_text().strip()
                result["directories"][str(path)] = record
        result["verified"] = True
    except OSError as error:
        result["errors"].append(str(error))
    return result


def record_baseline(args):
    """Explicit migration path for a lab already started before baseline support."""
    manifest = load(args.lab)
    inspected = inspect_container(manifest)
    if not inspected["State"]["Running"]:
        raise RuntimeError(
            "Late baseline requires a running owned lab; preserve a stopped lab for recovery"
        )
    baseline = capture_baseline(
        args.lab, manifest, "late-observation", args.baseline_note, inspected["Id"]
    )
    print(
        json.dumps(
            {
                "baseline": str(args.lab / "baseline.json"),
                "phase": baseline["phase"],
                "predates_start": False,
                "note": "This cannot reconstruct resources present before the initial launch.",
            },
            indent=2,
        )
    )


def recovery(manifest):
    ctr = ["docker", "exec", manifest["container"], "ctr", "--address", "/lab/run/containerd.sock"]
    return [
        shlex.join(["docker", "inspect", manifest["container"]]),
        shlex.join(["docker", "logs", manifest["container"]]),
        shlex.join(ctr + ["namespaces", "ls"]),
        "While the lab is running, list each namespace's tasks, containers, images, leases and snapshots; remove only explicitly identified lab resources through the same private ctr socket.",
        "Inspect preserved host target/device/mount deltas and config paths. Never delete unrelated configfs objects or devices.",
        "Reconcile and remove only the lab's explicit cgroup prefix after its tasks exit: "
        + manifest.get(
            "owned_cgroup_prefix", "/system.slice/pvisor-overlaybd-lab-" + manifest["id"]
        ),
        "If the container exited unexpectedly, preserve it and the lab directory; recover the owned backend and reconcile kernel objects before any explicit manual removal.",
    ]


def lifecycle_record(lab, manifest, status, **details):
    record = {
        "status": status,
        "lab_id": manifest["id"],
        "recorded_at_unix": time.time(),
        **details,
    }
    path = lab / f"lifecycle-{time.time_ns()}-{status}.json"
    save(path, record)
    save(lab / "lifecycle.json", {"status": status, "record": str(path)})
    return path


def kernel_audit(baseline, manifest, require_host_cgroups=False):
    current = host_inventory()
    original = baseline["host"]
    delta = {}
    for kind in ("targets", "block_devices", "uio"):
        before, after = original[kind], current[kind]
        delta[kind] = {
            "added": {name: value for name, value in after.items() if name not in before},
            "changed": {
                name: value
                for name, value in after.items()
                if name in before and value != before[name]
            },
            "removed": sorted(set(before) - set(after)),
        }
    matches = {}
    for name, target in current["targets"].items():
        text = "\n".join(str(value) for value in target.values())
        # A matching configuration is evidence to review, not authority to unlink.
        if manifest["lab"] in text or "/lab/containerd-root/" in text:
            matches[name] = target
    added_mounts = sorted(set(current["mountinfo"]) - set(original["mountinfo"]))
    removed_mounts = sorted(set(original["mountinfo"]) - set(current["mountinfo"]))
    owned_mounts = [
        line for line in added_mounts if manifest["lab"] in line or "/lab/containerd-root/" in line
    ]
    blockers = list(current["errors"])
    if original.get("boot_id") != current.get("boot_id"):
        blockers.append("host boot identity differs from baseline")
    # The handler is shared. Any remaining user target makes stopping unsafe,
    # including an unknown target or one already present at a late observation.
    if current["targets"]:
        blockers.append(
            "user-backed TCMU objects remain; no automatic object deletion is permitted"
        )
    for kind in ("block_devices", "uio"):
        if delta[kind]["added"] or delta[kind]["changed"]:
            blockers.append("new or changed host " + kind + " remain; ownership must be reconciled")
    if owned_mounts:
        blockers.append("new mounts referencing the lab remain")
    cgroups = owned_cgroups(manifest)
    blockers.extend(cgroups["errors"])
    if require_host_cgroups and not cgroups["verified"]:
        blockers.append(
            "post-stop host cgroup inspection is unverified; preserve the stopped container for manual recovery"
        )
    if cgroups["directories"]:
        blockers.append(
            "owned workload cgroup prefix remains; reconcile it before stopping the lab"
        )
    return {
        "host": current,
        "delta": delta,
        "configuration_matches_lab": matches,
        "mount_delta": {
            "added": added_mounts,
            "removed": removed_mounts,
            "referencing_lab": owned_mounts,
        },
        "owned_cgroups": cgroups,
        "baseline_phase": baseline["phase"],
        "blockers": blockers,
    }


def ctr_output(manifest, *arguments):
    return subprocess.check_output(
        [
            "docker",
            "exec",
            manifest["container"],
            "ctr",
            "--address",
            "/lab/run/containerd.sock",
            *arguments,
        ],
        text=True,
        timeout=20,
    )


def container_namespace_audit(manifest):
    """Read the owned container's actual cgroup and mount views before stopping."""
    prefix = "/system.slice/pvisor-overlaybd-lab-" + manifest["id"]
    if manifest.get("owned_cgroup_prefix", prefix) != prefix:
        raise ValueError("Owned cgroup prefix does not match the lab identity")
    inspected = inspect_container(manifest)
    if not inspected["State"]["Running"] or inspected["HostConfig"].get("CgroupnsMode") != "host":
        raise RuntimeError(
            "A running owned container with host cgroup namespace is required for cgroup audit"
        )
    # A successful visibility check is required before absence of the prefix can
    # mean empty. The shell only reads files and enumerates this exact prefix.
    command = """set -eu
test -r /sys/fs/cgroup/cgroup.controllers
test -r /sys/fs/cgroup/system.slice/cgroup.type
root=/sys/fs/cgroup$1
if test -d "$root"; then
  find "$root" -type d -print
fi
"""
    directories = subprocess.check_output(
        [
            "docker",
            "exec",
            manifest["container"],
            "/bin/sh",
            "-c",
            command,
            "owned-cgroup-audit",
            prefix,
        ],
        text=True,
        timeout=20,
    ).splitlines()
    mountinfo = subprocess.check_output(
        ["docker", "exec", manifest["container"], "/bin/cat", "/proc/self/mountinfo"],
        text=True,
        timeout=20,
    ).splitlines()
    mounts = []
    for line in mountinfo:
        before, separator, after = line.partition(" - ")
        if not separator or len(before.split()) < 5 or not after.split():
            raise ValueError("Malformed owned-container mountinfo")
        mountpoint = before.split()[4].replace("\\040", " ").replace("\\134", "\\")
        if mountpoint == "/lab/containerd-root" or mountpoint.startswith("/lab/containerd-root/"):
            mounts.append(
                {"mountpoint": mountpoint, "filesystem": after.split()[0], "record": line}
            )
    blockers = []
    if directories:
        blockers.append(
            "owned container still observes workload cgroup directories under " + prefix
        )
    if mounts:
        blockers.append("owned container still observes mounts below /lab/containerd-root")
    return {
        "scope": "owned container host cgroup namespace and private mount namespace",
        "cgroups_verified": True,
        "owned_cgroup_prefix": prefix,
        "cgroup_directories": directories,
        "mountinfo": mountinfo,
        "containerd_mounts": mounts,
        "blockers": blockers,
    }


def table_rows(output, header):
    lines = [line for line in output.splitlines() if line.strip()]
    if not lines or lines[0].split()[0] != header:
        raise ValueError("Unexpected ctr table header: " + repr(output[:300]))
    return [line.split() for line in lines[1:]]


def containerd_audit(manifest):
    plugins = ctr_output(manifest, "plugins", "ls")
    snapshotters = []
    for row in table_rows(plugins, "TYPE"):
        if len(row) < 3 or "snapshotter" not in row[0]:
            continue
        if row[-1] == "error":
            raise RuntimeError("Cannot inspect an unhealthy snapshotter: " + " ".join(row))
        if row[-1] == "ok":
            snapshotters.append(row[1])
    if "overlaybd" not in snapshotters:
        raise RuntimeError("Owned overlaybd snapshotter is not healthy")
    namespaces = [row[0] for row in table_rows(ctr_output(manifest, "namespaces", "ls"), "NAME")]
    audit = {"plugins": plugins, "snapshotters": snapshotters, "namespaces": {}, "blockers": []}
    for namespace in namespaces:
        resources = {}
        for entity, header in (
            ("tasks", "TASK"),
            ("containers", "CONTAINER"),
            ("images", "REF"),
            ("leases", "ID"),
        ):
            output = ctr_output(manifest, "--namespace", namespace, entity, "ls")
            rows = table_rows(output, header)
            resources[entity] = output
            if rows:
                audit["blockers"].append(f"namespace {namespace} retains {entity}")
        resources["snapshots"] = {}
        for snapshotter in snapshotters:
            output = ctr_output(
                manifest, "--namespace", namespace, "snapshots", "--snapshotter", snapshotter, "ls"
            )
            resources["snapshots"][snapshotter] = output
            if table_rows(output, "KEY"):
                audit["blockers"].append(f"namespace {namespace} retains {snapshotter} snapshots")
        audit["namespaces"][namespace] = resources
    return audit


def start(args):
    manifest = load(args.lab)
    manifest["cgroup_namespace"] = "host"
    manifest.setdefault(
        "owned_cgroup_prefix", "/system.slice/pvisor-overlaybd-lab-" + manifest["id"]
    )
    owned_cgroups(manifest)  # Validate the recorded prefix before exposing cgroupns.
    save(args.lab / "lab.json", manifest)
    for module in ("target_core_user", "tcm_loop"):
        if not Path("/sys/module", module).is_dir():
            raise ValueError(f"Host kernel module not loaded: {module}")
    targets = Path("/sys/kernel/config/target/core")
    if not targets.is_dir():
        raise ValueError("Host configfs target/core is unavailable")
    # TCMU handler registration is shared by all containers. Require exclusive use.
    if any(path.is_dir() for path in targets.glob("user_*/*")):
        raise ValueError("Existing user-backed TCMU targets found; refusing another handler")
    processes = subprocess.check_output(["ps", "-eo", "comm="], text=True).splitlines()
    if any(name.strip() == "overlaybd-tcmu" for name in processes):
        raise ValueError("Existing overlaybd-tcmu process found; refusing another handler")
    capture_baseline(args.lab, manifest, "pre-start", "captured before docker run")
    port = manifest["port"]
    try:
        lifecycle_record(args.lab, manifest, "starting")
        run(
            [
                "docker",
                "run",
                "-d",
                "--name",
                manifest["container"],
                "--label",
                LABEL + "=" + manifest["id"],
                "--privileged",
                "--cgroupns",
                "host",
                "--security-opt",
                "label=disable",
                "--stop-timeout",
                "20",
                "--mount",
                "type=bind,src=" + manifest["lab"] + ",dst=/lab",
                "--mount",
                "type=bind,src=/dev,dst=/dev",
                "--mount",
                "type=bind,src=/sys/kernel/config,dst=/sys/kernel/config",
                "-p",
                f"127.0.0.1:{port}:{port}",
                manifest["image_id"],
            ],
            timeout=60,
        )
        for _ in range(100):
            state = inspect_container(manifest)["State"]
            if not state["Running"]:
                raise RuntimeError(f"Lab exited; inspect {args.lab}/log: {state}")
            result = subprocess.run(
                [
                    "docker",
                    "exec",
                    manifest["container"],
                    "ctr",
                    "--address",
                    "/lab/run/containerd.sock",
                    "plugins",
                    "ls",
                ],
                capture_output=True,
                text=True,
                timeout=10,
            )
            if result.returncode == 0 and re.search(
                r"snapshotter\S*\s+overlaybd\s+.*\bok\b", result.stdout
            ):
                save(
                    args.lab / "startup-check.json",
                    {
                        "plugins": result.stdout,
                        "checked_at_unix": time.time(),
                        "container_state": state,
                    },
                )
                lifecycle_record(args.lab, manifest, "running", container_state=state)
                print(result.stdout)
                return
            time.sleep(0.2)
        raise RuntimeError(f"Lab did not become ready; inspect {args.lab}/log")
    except Exception as error:
        path = lifecycle_record(
            args.lab,
            manifest,
            "start-failed",
            error=repr(error),
            host=host_inventory(),
            recovery=recovery(manifest),
        )
        print(f"Startup failed; container and data preserved. See {path}", file=sys.stderr)
        print("\n".join(recovery(manifest)), file=sys.stderr)
        raise


def status(args):
    manifest = load(args.lab)
    inspected = inspect_container(manifest)
    print(
        json.dumps(
            {
                "container": manifest["container"],
                "state": inspected["State"],
                "registry": f"http://127.0.0.1:{manifest['port']}",
                "lab": manifest["lab"],
            },
            indent=2,
        )
    )


def publish(args):
    """Upload exact OCI-layout blobs and manifest to the private local registry."""
    manifest = load(args.lab)
    inspect_container(manifest)
    index = json.loads((args.oci_layout / "index.json").read_text())
    descriptors = index["manifests"]
    if len(descriptors) != 1:
        raise ValueError("Require a single-platform, single-image OCI layout")
    descriptor = descriptors[0]

    def blob(descriptor):
        algorithm, digest = descriptor["digest"].split(":")
        if algorithm != "sha256":
            raise ValueError("Require sha256 source blobs")
        path = args.oci_layout / "blobs/sha256" / digest
        if path.stat().st_size != descriptor["size"] or sha256(path) != digest:
            raise ValueError(f"Source digest/size mismatch: {path}")
        return path.read_bytes()

    source = blob(descriptor)
    image = json.loads(source)
    base = f"http://127.0.0.1:{manifest['port']}"
    endpoint = base + "/v2/" + args.repository
    uploaded = []
    for entry in [image["config"], *image["layers"]]:
        data = blob(entry)
        request = urllib.request.Request(endpoint + "/blobs/uploads/", method="POST", data=b"")
        with urllib.request.urlopen(request, timeout=30) as response:
            location = urllib.parse.urljoin(base, response.headers["Location"])
        separator = "&" if "?" in location else "?"
        location += separator + urllib.parse.urlencode({"digest": entry["digest"]})
        request = urllib.request.Request(
            location, method="PUT", data=data, headers={"Content-Type": "application/octet-stream"}
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            if response.status != 201:
                raise ValueError(f"Unexpected registry upload response: {response.status}")
        uploaded.append({"digest": entry["digest"], "bytes": len(data)})
    request = urllib.request.Request(
        endpoint + "/manifests/source",
        method="PUT",
        data=source,
        headers={"Content-Type": descriptor["mediaType"]},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        if response.headers["Docker-Content-Digest"] != descriptor["digest"]:
            raise ValueError("Published registry manifest digest mismatch")
    receipt = {
        "source_digest": descriptor["digest"],
        "source_layout": str(args.oci_layout.resolve()),
        "repository": f"127.0.0.1:{manifest['port']}/{args.repository}",
        "blobs": uploaded,
    }
    save(args.lab / "source-image.json", receipt)
    print(json.dumps(receipt, indent=2))


def stop(args):
    manifest = load(args.lab)
    details = {}
    try:
        inspected = inspect_container(manifest)
        details["container_state"] = inspected["State"]
        baseline = json.loads((args.lab / "baseline.json").read_text())
        if baseline.get("lab_id") != manifest["id"]:
            raise ValueError("Baseline lab identity mismatch")
        details["kernel_before_stop"] = kernel_audit(baseline, manifest)
        if not inspected["State"]["Running"]:
            raise RuntimeError(
                "Container exited outside this stop operation; automatic removal is refused"
            )
        if baseline.get("container_id") and baseline["container_id"] != inspected["Id"]:
            raise ValueError("Late baseline belongs to a different container generation")
        details["containerd"] = containerd_audit(manifest)
        details["container_namespaces"] = container_namespace_audit(manifest)
        blockers = (
            details["kernel_before_stop"]["blockers"]
            + details["containerd"]["blockers"]
            + details["container_namespaces"]["blockers"]
        )
        if blockers:
            raise RuntimeError("; ".join(blockers))
        lifecycle_record(args.lab, manifest, "stopping", **details)
        run(["docker", "stop", manifest["container"]], timeout=45)
        stopped = inspect_container(manifest)
        details["container_after_stop"] = stopped["State"]
        details["kernel_after_stop"] = kernel_audit(baseline, manifest)
        # The owned container proved the exact prefix absent and every private
        # namespace empty immediately before stopping. Preserve that evidence
        # when the caller cannot repeat the cgroup read after the observer exits;
        # do not turn an invisible hierarchy into a claimed post-stop observation.
        details["post_stop_cgroup_proof"] = {
            "post_stop_reverified": details["kernel_after_stop"]["owned_cgroups"]["verified"],
            "pre_stop_owned_prefix_verified_absent": True,
            "pre_stop_all_private_namespaces_empty": True,
            "limitation": "If caller visibility is unavailable, cgroup cleanup relies on the preserved pre-stop audit; only targets/devices/mount deltas were rechecked after stop.",
        }
        if (
            stopped["State"]["Running"]
            or stopped["State"].get("OOMKilled")
            or stopped["State"].get("ExitCode") != 0
            or stopped["State"].get("Error")
        ):
            raise RuntimeError("Lab did not stop cleanly; automatic removal is refused")
        if details["kernel_after_stop"]["blockers"]:
            raise RuntimeError("; ".join(details["kernel_after_stop"]["blockers"]))
        run(["docker", "rm", manifest["container"]], timeout=30)
        lifecycle_record(args.lab, manifest, "removed", **details)
        print(
            "Owned lab container removed after audits; private data retained; host modules left loaded."
        )
    except Exception as error:
        path = lifecycle_record(
            args.lab,
            manifest,
            "stop-refused",
            error=repr(error),
            **details,
            recovery=recovery(manifest),
        )
        print(f"Stop/removal refused; preserved lab state: {path}", file=sys.stderr)
        print("\n".join(recovery(manifest)), file=sys.stderr)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action", choices=("prepare", "start", "status", "publish", "stop", "record-baseline")
    )
    parser.add_argument("--lab", type=Path, required=True)
    parser.add_argument("--overlaybd-root", type=Path)
    parser.add_argument("--snapshotter-root", type=Path)
    parser.add_argument("--port", type=int, default=55118)
    parser.add_argument("--oci-layout", type=Path)
    parser.add_argument("--repository", default="pvisor-lab/shell")
    parser.add_argument(
        "--baseline-note",
        default="late observation of an already running lab; not a pre-start inventory",
    )
    args = parser.parse_args()
    if args.action == "prepare" and (args.overlaybd_root is None or args.snapshotter_root is None):
        parser.error("prepare requires --overlaybd-root and --snapshotter-root")
    if args.action == "publish" and args.oci_layout is None:
        parser.error("publish requires --oci-layout")
    if not 1024 <= args.port <= 65535:
        parser.error("--port must be between 1024 and 65535")
    globals()[args.action.replace("-", "_")](args)


if __name__ == "__main__":
    main()
