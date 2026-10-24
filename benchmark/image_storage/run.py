#!/usr/bin/env python3
"""Benchmark: B-IMAGE-STORAGE-ENG (benchmark/README.md#b-image-storage-eng).
Role: engineering A/B.
Motivation: quantify the allocated disk cost of the compressed local image pool.
Conclusion sought: same-image and combined-store byte savings, with boundaries.
Design: pinned amd64 manifests, fresh stores, private rootless Docker overlayfs,
uncompressed disk backing, full inventories, runtime checks and retained failures.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

# Run unchanged in the Docker owner's user/mount namespace, which can read image
# files owned by subordinate UIDs. st_blocks includes directory allocation.
SCANNER = r"""
import os,json,stat,sys
root=sys.argv[1]; seen=set(); rows=[]
for directory,dirs,files in os.walk(root,followlinks=False):
 for path in [directory]+[os.path.join(directory,n) for n in files]+[os.path.join(directory,n) for n in dirs if os.path.islink(os.path.join(directory,n))]:
  s=os.lstat(path);key=(s.st_dev,s.st_ino)
  if key in seen: continue
  seen.add(key)
  rows.append(dict(path=os.path.relpath(path,root),apparent=s.st_size,allocated=s.st_blocks*512,mode=s.st_mode,inode=s.st_ino,device=s.st_dev))
print(json.dumps(dict(apparent=sum(r['apparent'] for r in rows),allocated=sum(r['allocated'] for r in rows),inodes=len(rows),rows=rows)))
"""


def command(argv, log, env=None, timeout=600):
    with log.open("ab") as output:
        output.write((json.dumps(argv) + "\n").encode())
        output.flush()
        result = subprocess.run(
            argv, env=env, stdout=subprocess.PIPE, stderr=output, timeout=timeout
        )
        output.write(result.stdout)
    if result.returncode:
        raise RuntimeError(f"exit {result.returncode}: {argv}; see {log}")
    return result.stdout


def inventory(path, destination, pid=None):
    argv = ["python3", "-c", SCANNER, str(path)]
    if pid:
        argv = ["nsenter", "-t", str(pid), "-U", "-m", "--", *argv]
    data = json.loads(subprocess.check_output(argv))
    destination.write_text(json.dumps(data, indent=2) + "\n")
    return {k: data[k] for k in ("allocated", "apparent", "inodes")}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            value.update(block)
    return value.hexdigest()


def start_docker(directory, runtime, log):
    data = directory / "docker"
    config = runtime / "daemon.json"
    config.write_text(json.dumps({"features": {"containerd-snapshotter": True}}))
    env = dict(
        os.environ,
        XDG_RUNTIME_DIR=str(runtime),
        DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR=str(runtime / "kit"),
        DOCKERD_ROOTLESS_ROOTLESSKIT_NET="pasta",
        DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS="false",
    )
    argv = [
        "dockerd-rootless.sh",
        "--config-file=" + str(config),
        "--host=unix://" + str(runtime / "d.sock"),
        "--data-root=" + str(data),
        "--exec-root=" + str(runtime / "exec"),
        "--pidfile=" + str(runtime / "d.pid"),
        "--iptables=false",
        "--ip6tables=false",
        "--bridge=none",
        "--userland-proxy=false",
    ]
    output = log.open("wb")
    process = subprocess.Popen(
        argv, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True
    )
    output.close()
    client = ["docker", "--host", "unix://" + str(runtime / "d.sock")]
    deadline = time.monotonic() + 30
    try:
        while time.monotonic() < deadline:
            probe = subprocess.run([*client, "info", "--format", "{{json .}}"], capture_output=True)
            if probe.returncode == 0:
                info = json.loads(probe.stdout)
                assert info["Driver"] == "overlayfs", info["Driver"]
                assert ["driver-type", "io.containerd.snapshotter.v1"] in info["DriverStatus"]
                (directory / "docker-info.json").write_text(json.dumps(info, indent=2) + "\n")
                return process, client, int((runtime / "d.pid").read_text())
            if process.poll() is not None:
                raise RuntimeError(f"private Docker startup failed: {log}")
            time.sleep(0.1)
        raise RuntimeError(f"private Docker startup timeout: {log}")
    except BaseException:
        stop_docker(process)
        raise


def stop_docker(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)


def pool_details(store):
    pool = store / "images/pool-v1"
    encoding = json.loads((pool / "format.json").read_text())["encoding"]
    assert encoding == "pvisor-local-pool-v1"
    images = json.loads((store / "images.json").read_text())
    assert all(i.get("cache") and i["rootfs"] == "" for i in images)
    for legacy in ["blobs", "rootfs-v3", "built"]:
        assert not (store / "images" / legacy).exists(), legacy
    content = dict(objects=0, raw_bytes=0, encoded_bytes=0, compressed_objects=0)
    for path in (pool / "data").rglob("*"):
        if not path.is_file():
            continue
        with path.open("rb") as stream:
            header = stream.read(13)
        assert header[:8] == b"PVPOOL1\0"
        content["objects"] += 1
        content["raw_bytes"] += int.from_bytes(header[9:13], "little")
        content["encoded_bytes"] += path.stat().st_size
        content["compressed_objects"] += int(header[8] == 1)
    return content


def measure_case(root, inputs, repeat, label):
    directory = root / "trials" / f"r{repeat}-{label}"
    directory.mkdir(parents=True)
    log = directory / "commands.log"
    runtime = Path(tempfile.mkdtemp(prefix="pvimg-"))
    process = None
    result = dict(label=label, repeat=repeat, images=[i["reference"] for i in inputs])
    try:
        process, docker, pid = start_docker(directory, runtime, directory / "daemon.log")
        result["docker_empty"] = inventory(
            directory / "docker", directory / "docker-empty.json", pid
        )
        store = directory / "pvisor"
        env = dict(os.environ, PVISOR_DOCKER_HOME=str(store))
        # Prespecified randomized backend order; disk totals, not timings.
        order = ["docker", "pvisor"]
        random.Random(20261024 + repeat).shuffle(order)
        result["order"] = order
        for backend in order:
            for image in inputs:
                if backend == "docker":
                    command([*docker, "pull", "--platform=linux/amd64", image["pinned"]], log)
                    inspection = json.loads(
                        command([*docker, "image", "inspect", image["pinned"]], log)
                    )[0]
                    # Docker 29's containerd image ID is the manifest digest,
                    # rather than the config digest used by classic Docker.
                    assert inspection["Descriptor"]["digest"] == image["digest"]
                    config = json.loads(
                        (
                            Path(image["source"]) / image["manifest"]["config"]["digest"][7:]
                        ).read_text()
                    )
                    assert inspection["RootFS"]["Layers"] == config["rootfs"]["diff_ids"]
                    assert image["pinned"] in inspection["RepoDigests"]
                else:
                    command([str(root / "runtime/pvisor"), "pull", image["pinned"]], log, env)
                    catalog = json.loads((store / "images.json").read_text())
                    record = next(i for i in catalog if i["reference"] == image["pinned"])
                    assert record["digest"] == image["digest"]
                    assert record["id"] == image["manifest"]["config"]["digest"]
        # Measure before runtime checks create any writable containers.
        result["pvisor"] = inventory(store, directory / "pvisor-inventory.json")
        result["pool"] = pool_details(store)
        result["docker"] = inventory(directory / "docker", directory / "docker-inventory.json", pid)
        for name, subdir in {
            "docker_content": "containerd/daemon/io.containerd.content.v1.content",
            "docker_snapshots": "containerd/daemon/io.containerd.snapshotter.v1.overlayfs",
        }.items():
            result[name] = inventory(
                directory / "docker" / subdir, directory / (name + ".json"), pid
            )
        result["docker_incremental_allocated"] = (
            result["docker"]["allocated"] - result["docker_empty"]["allocated"]
        )
        for image in inputs:
            for backend in ["pvisor", "docker"]:
                prefix = [str(root / "runtime/pvisor")] if backend == "pvisor" else docker
                argv = [*prefix, "run", "--rm", "--network=none"]
                if backend == "pvisor":
                    argv += ["--cpus=1", "--memory=128m"]
                argv += [image["pinned"], "/bin/cat", "/etc/os-release"]
                output = command(argv, log, env if backend == "pvisor" else None, timeout=90)
                expected = (
                    b"ID=alpine"
                    if image["name"].startswith("alpine")
                    else b"ID=ubuntu"
                    if image["name"].startswith("ubuntu")
                    else b"ID=debian"
                )
                assert expected in output, (backend, image["name"], output)
        before = result["pool"].copy()
        command([str(root / "runtime/pvisor"), "pull", inputs[0]["pinned"]], log, env)
        assert pool_details(store) == before, "repeat pull added content"
        result["pvisor_after_validation"] = inventory(
            store, directory / "pvisor-after-validation.json"
        )
        result["status"] = "passed"
        print(
            "passed",
            repeat,
            label,
            "pvisor",
            result["pvisor"]["allocated"],
            "docker incremental",
            result["docker_incremental_allocated"],
            flush=True,
        )
    except BaseException as error:
        result.update(status="failed", error=repr(error))
        raise
    finally:
        (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        if process:
            stop_docker(process)
        # Runtime socket paths live on tmpfs, outside the measured disk stores.
        shutil.rmtree(runtime, ignore_errors=True)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("batch", type=Path)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    root = args.batch.resolve()
    assert args.repeats > 0
    inputs = json.loads((root / "inputs.json").read_text())
    for image in inputs:
        source = Path(image["source"])
        assert digest(source / "manifest.json") == image["digest"][7:]
        for desc in [image["manifest"]["config"], *image["manifest"]["layers"]]:
            path = source / desc["digest"][7:]
            assert path.stat().st_size == desc["size"] and digest(path) == desc["digest"][7:]
    binary = root / "runtime/pvisor"
    manifest = dict(
        benchmark="B-IMAGE-STORAGE-ENG",
        host=platform.platform(),
        binary_sha256=digest(binary),
        harness_sha256=digest(Path(__file__)),
        filesystem=subprocess.check_output(["findmnt", "-J", "-T", str(root)]).decode(),
        compression_flags=subprocess.check_output(["lsattr", "-d", str(root)]).decode(),
        started_at=time.time(),
        inputs=inputs,
        repeats=args.repeats,
        limitations=[
            "allocated blocks omit global filesystem metadata",
            "rootless Docker; no iptables/bridge",
            "disk inventory, not latency measurement",
        ],
    )
    assert "m" in manifest["compression_flags"].split()[0], "Btrfs no-compression flag missing"
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    shutil.copy2(__file__, root / "harness.py")
    results = []
    for repeat in range(1, args.repeats + 1):
        for image in inputs:
            results.append(measure_case(root, [image], repeat, image["name"]))
        results.append(measure_case(root, inputs, repeat, "combined"))
        (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    assert digest(binary) == manifest["binary_sha256"]
    manifest.update(finished_at=time.time(), status="passed")
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
