#!/usr/bin/env python3
"""B-IMAGE-STORAGE-ENG / B-NATIVE-DOCKER-ENG, selected explicitly by --phase.

Question: same-digest OCI inventory, prepared CLI Ready and full idle deployment
memory for V2 versus native Docker/runc, without OpenSandbox/E2B wrappers.
Design: private rootless Docker/containerd overlayfs and V2 stores, disabled
Btrfs directory compression, verified manifests/config/layers, three fresh disk
inventories; runtime 3 warmups/30 randomized pairs and five fresh 0/1/4-instance
groups, 4 CPU/4 GiB/swap0. No existing daemon, host cache or KSM changes.
Semspec owns command evidence, Ready observation, repeat and persistence.
"""

import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import platform
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RUNTIME_PATH = REPO / "tools/semspec/runtime.py"
SPEC = importlib.util.spec_from_file_location("semspec_runtime", RUNTIME_PATH)
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)
sys.path.insert(0, str(REPO / "benchmark/support"))
from resource_budget import ObservationChanged, ResourceBudget  # noqa: E402

SCANNER = r"""
import os,json,sys,stat
root=sys.argv[1];seen=set();rows=[]
def walk(path):
 s=os.lstat(path);key=(s.st_dev,s.st_ino)
 if key in seen:return
 seen.add(key)
 rows.append(dict(path=os.path.relpath(path,root),apparent=s.st_size,
                  allocated=s.st_blocks*512,mode=s.st_mode,inode=s.st_ino,device=s.st_dev))
 if stat.S_ISDIR(s.st_mode):
  with os.scandir(path) as entries:
   for entry in sorted(entries,key=lambda e:e.name):walk(entry.path)
walk(root)
print(json.dumps(dict(apparent=sum(r['apparent'] for r in rows),
 allocated=sum(r['allocated'] for r in rows),inodes=len(rows),rows=rows)))
"""


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def paired(pairs, key):
    differences = [p["pvisor"][key] - p["docker"][key] for p in pairs]
    rng = random.Random(817)
    bootstrap = sorted(
        statistics.median(rng.choices(differences, k=len(pairs))) for _ in range(5000)
    )
    return dict(
        docker_median=statistics.median(p["docker"][key] for p in pairs),
        pvisor_median=statistics.median(p["pvisor"][key] for p in pairs),
        paired_difference=statistics.median(differences),
        ci95=[bootstrap[125], bootstrap[4874]],
    )


class Evidence:
    def __init__(self, args):
        self.root = args.output.resolve()
        if ".data" not in self.root.parts:
            raise ValueError("retain a new batch under .data")
        self.root.mkdir(parents=True, exist_ok=False, mode=0o700)
        subprocess.run(["chattr", "+m", str(self.root)], check=True)
        flags = subprocess.check_output(["lsattr", "-d", str(self.root)], text=True)
        if "m" not in flags.split()[0]:
            raise ValueError("Btrfs no-compression flag missing")
        self.number = 0
        self.binary = self.root / "pvisor"
        shutil.copy2(args.binary, self.binary)
        for path in [Path(__file__), RUNTIME_PATH, REPO / "benchmark/support/resource_budget.py"]:
            shutil.copy2(path, self.root / path.name)
        self.inputs = json.loads(args.inputs.read_text())
        blobs = self.root / "sources"
        blobs.mkdir()
        for image in self.inputs:
            source = Path(image["source"])
            manifest = Path(image.get("manifest_path", source / "manifest.json"))
            assert sha(manifest) == image["digest"][7:]
            assert json.loads(manifest.read_text()) == image["manifest"]
            for descriptor in [image["manifest"]["config"], *image["manifest"]["layers"]]:
                path = source / descriptor["digest"][7:]
                assert (
                    path.stat().st_size == descriptor["size"]
                    and sha(path) == descriptor["digest"][7:]
                )
                if not (blobs / path.name).exists():
                    shutil.copy2(path, blobs / path.name)
            shutil.copy2(manifest, blobs / image["digest"][7:])
            image.update(source=str(blobs), manifest_path=str(blobs / image["digest"][7:]))
        RUNTIME.save(self.root / "inputs.json", self.inputs)
        self.manifest = dict(
            benchmark_id="B-IMAGE-STORAGE-ENG"
            if args.phase == "storage"
            else "B-NATIVE-DOCKER-ENG",
            phase=args.phase,
            started_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
            host=platform.platform(),
            cpu_affinity=sorted(os.sched_getaffinity(0)),
            host_load=os.getloadavg(),
            filesystem=subprocess.check_output(
                ["findmnt", "-no", "FSTYPE,OPTIONS", "-T", str(self.root)], text=True
            ).strip(),
            directory_flags=flags.strip(),
            binary_sha256=sha(self.binary),
            harness_sha256=sha(__file__),
            runtime_sha256=sha(RUNTIME_PATH),
            inputs=self.inputs,
            source_revision=subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
            ).strip(),
            status="preparing",
            parameters={k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
        )
        (self.root / "source.patch").write_bytes(
            subprocess.check_output(["git", "diff", "HEAD", "--binary"], cwd=REPO)
        )
        self.save()

    def save(self, **updates):
        self.manifest.update(updates)
        RUNTIME.save(self.root / "manifest.json", self.manifest)

    def command(self, argv, env=None, timeout=600, check=True):
        self.number += 1
        path = self.root / "commands" / f"{self.number:05}.json"
        path.parent.mkdir(exist_ok=True)
        try:
            RUNTIME.command_trial(
                path,
                "setup-or-oracle",
                "command",
                self.number,
                dict(argv=[str(v) for v in argv], cwd=str(REPO), env=env or {}, timeout=timeout),
            )
        except ValueError:
            if check:
                raise
        result = json.loads(path.read_text())
        return result["stdout"] if result.get("exit_code") == 0 else None

    def inventory(self, root, destination, pid=None):
        argv = [sys.executable, "-c", SCANNER, str(root)]
        if pid:
            argv = ["nsenter", "-t", str(pid), "-U", "-m", "--", *argv]
        data = json.loads(self.command(argv))
        RUNTIME.save(destination, data)
        return {k: data[k] for k in ["allocated", "apparent", "inodes"]}


class Docker:
    def __init__(self, evidence, directory, slice_unit=None, insecure_registries=()):
        self.e = evidence
        self.directory = directory
        self.runtime = Path(tempfile.mkdtemp(prefix="pvnd-"))
        self.host = "unix://" + str(self.runtime / "d.sock")
        self.argv = ["docker", "--host", self.host]
        self.process = None
        self.unit = None
        config = {"features": {"containerd-snapshotter": True}}
        if insecure_registries:
            config["insecure-registries"] = list(insecure_registries)
        if slice_unit:
            config.update(
                {"cgroup-parent": slice_unit, "exec-opts": ["native.cgroupdriver=systemd"]}
            )
        RUNTIME.save(self.runtime / "daemon.json", config)
        env = dict(
            XDG_RUNTIME_DIR=str(self.runtime),
            DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR=str(self.runtime / "kit"),
            DOCKERD_ROOTLESS_ROOTLESSKIT_NET="pasta",
            DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS="false",
        )
        argv = [
            "dockerd-rootless.sh",
            "--config-file=" + str(self.runtime / "daemon.json"),
            "--host=" + self.host,
            "--data-root=" + str(directory / "docker"),
            "--exec-root=" + str(self.runtime / "exec"),
            "--pidfile=" + str(self.runtime / "d.pid"),
            "--iptables=false",
            "--ip6tables=false",
            "--bridge=none",
            "--userland-proxy=false",
        ]
        try:
            if slice_unit:
                self.unit = slice_unit[:-6] + "-docker.service"
                evidence.command(
                    [
                        "systemd-run",
                        "--user",
                        "--quiet",
                        "--unit=" + self.unit,
                        "--slice=" + slice_unit,
                        "-p",
                        "Delegate=yes",
                        "-p",
                        "TimeoutStopSec=15s",
                        *["--setenv=" + k + "=" + v for k, v in env.items()],
                        "--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/"
                        + str(os.getuid())
                        + "/bus",
                        "taskset",
                        "-c",
                        "0,1,2,3",
                        *argv,
                    ]
                )
            else:
                with (directory / "daemon.log").open("wb") as log:
                    self.process = subprocess.Popen(
                        argv,
                        env=os.environ | env,
                        stdout=log,
                        stderr=subprocess.STDOUT,
                        start_new_session=True,
                    )
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                output = evidence.command(
                    [*self.argv, "info", "--format", "{{json .}}"], timeout=5, check=False
                )
                if output:
                    self.info = json.loads(output)
                    assert self.info["Driver"] == "overlayfs"
                    assert ["driver-type", "io.containerd.snapshotter.v1"] in self.info[
                        "DriverStatus"
                    ]
                    assert "name=rootless" in self.info["SecurityOptions"]
                    self.pid = int((self.runtime / "d.pid").read_text())
                    RUNTIME.save(directory / "docker-info.json", self.info)
                    return
                time.sleep(0.2)
            raise RuntimeError("private rootless Docker did not become ready")
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.unit:
            self.e.command(
                ["journalctl", "--user", "--unit=" + self.unit, "--no-pager"], check=False
            )
            self.e.command(["systemctl", "--user", "stop", self.unit], check=False)
        if self.process and self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=10)
        shutil.rmtree(self.runtime, ignore_errors=True)


def pool_details(store):
    pool = store / "images/pool-v2"
    marker = json.loads((pool / "format.json").read_text())
    assert marker["format_version"] == 2 and marker["encoding"] == "zstd-seekable"
    images = json.loads((store / "images.json").read_text())
    assert all(
        i.get("cache") and i["rootfs"] == "" and i["cache"]["handle"].startswith("pvisor-v2:")
        for i in images
    )
    assert all(not (store / "images" / name).exists() for name in ["blobs", "rootfs-v3", "built"])
    return {
        "objects": len([p for p in (pool / "data").rglob("*") if p.is_file()]),
        "packs": len([p for p in (pool / "data/packs").rglob("*") if p.is_file()]),
    }


def import_image(e, image, backend, prefix, env):
    e.command(
        [
            *prefix,
            "pull",
            *(["--platform=linux/amd64"] if backend == "docker" else []),
            image["pinned"],
        ],
        env,
    )
    if backend == "docker":
        actual = json.loads(e.command([*prefix, "image", "inspect", image["pinned"]]))[0]
        config = json.loads(
            (Path(image["source"]) / image["manifest"]["config"]["digest"][7:]).read_text()
        )
        assert actual["Descriptor"]["digest"] == image["digest"]
        assert actual["RootFS"]["Layers"] == config["rootfs"]["diff_ids"]
        assert image["pinned"] in actual["RepoDigests"]
    else:
        images = json.loads((Path(env["PVISOR_DOCKER_HOME"]) / "images.json").read_text())
        actual = next(i for i in images if i["reference"] == image["pinned"])
        assert (
            actual["digest"] == image["digest"]
            and actual["id"] == image["manifest"]["config"]["digest"]
        )


def storage(e, args):
    rows = []
    for repeat in range(args.repeats):
        for label, images in [(i["name"], [i]) for i in e.inputs] + [("combined", e.inputs)]:
            directory = e.root / f"r{repeat + 1}-{label}"
            directory.mkdir()
            docker = Docker(e, directory)
            store = directory / "pvisor"
            env = {"PVISOR_DOCKER_HOME": str(store)}
            result = dict(label=label, repeat=repeat, status="failed")
            try:
                result["docker_empty"] = e.inventory(
                    directory / "docker", directory / "docker-empty.json", docker.pid
                )
                order = random.Random(20261025 + repeat).sample(["docker", "pvisor"], 2)
                result["order"] = order
                for backend in order:
                    for image in images:
                        import_image(
                            e,
                            image,
                            backend,
                            docker.argv if backend == "docker" else [e.binary],
                            {} if backend == "docker" else env,
                        )
                result["pvisor"] = e.inventory(store, directory / "pvisor-inventory.json")
                result["pool"] = pool_details(store)
                result["docker"] = e.inventory(
                    directory / "docker", directory / "docker-inventory.json", docker.pid
                )
                result["docker_incremental_allocated"] = (
                    result["docker"]["allocated"] - result["docker_empty"]["allocated"]
                )
                for name, subdir in [
                    ("docker_content", "io.containerd.content.v1.content"),
                    ("docker_snapshots", "io.containerd.snapshotter.v1.overlayfs"),
                ]:
                    result[name] = e.inventory(
                        directory / "docker/containerd/daemon" / subdir,
                        directory / (name + ".json"),
                        docker.pid,
                    )
                for image in images:
                    outputs = []
                    for backend, prefix, environment in [
                        ("docker", docker.argv, {}),
                        ("pvisor", [e.binary], env),
                    ]:
                        outputs.append(
                            e.command(
                                [
                                    *prefix,
                                    "run",
                                    "--rm",
                                    "--network=none",
                                    "--cpus=1",
                                    "--memory=256m",
                                    image["pinned"],
                                    "/bin/sh",
                                    "-ec",
                                    "sha256sum /etc/os-release /bin/sh",
                                ],
                                environment,
                                timeout=90,
                            )
                        )
                    assert outputs[0] == outputs[1], "same-image content oracle differs"
                before = pool_details(store)
                e.command([e.binary, "pull", images[0]["pinned"]], env)
                assert pool_details(store) == before, "repeat pull added content"
                result["status"] = "passed"
                rows.append(result)
                RUNTIME.save(e.root / "results.json", rows)
                print(
                    "storage",
                    repeat + 1,
                    label,
                    result["pvisor"]["allocated"],
                    result["docker_incremental_allocated"],
                    flush=True,
                )
            finally:
                RUNTIME.save(directory / "result.json", result)
                docker.close()
    summary = {}
    for label in dict.fromkeys(r["label"] for r in rows):
        selected = [r for r in rows if r["label"] == label]
        summary[label] = {
            key: {"median": statistics.median(values), "range": [min(values), max(values)]}
            for key, values in [
                ("pvisor_allocated", [r["pvisor"]["allocated"] for r in selected]),
                (
                    "docker_incremental_allocated",
                    [r["docker_incremental_allocated"] for r in selected],
                ),
                ("docker_empty_allocated", [r["docker_empty"]["allocated"] for r in selected]),
                ("docker_content_allocated", [r["docker_content"]["allocated"] for r in selected]),
                (
                    "docker_snapshots_allocated",
                    [r["docker_snapshots"]["allocated"] for r in selected],
                ),
            ]
        }
    RUNTIME.save(e.root / "summary.json", summary)


class Product:
    def __init__(self, e, directory, backend):
        self.e = e
        self.directory = directory
        self.backend = backend
        self.slice = "pvnative" + uuid.uuid4().hex[:12] + ".slice"
        self.client_unit = self.slice[:-6] + "-clients.service"
        self.docker = None
        self.store = directory / "pvisor"
        self.env = (
            {"PVISOR_DOCKER_HOME": str(self.store), "PVISOR_STARTUP_TIMING": "0"}
            if backend == "pvisor"
            else {}
        )
        # Without delegated cpuset, runc restores init's unrestricted CPU mask.
        # Both arms execute the image's taskset first, using their CPU namespaces.
        self.payload = ["/usr/bin/taskset", "-c", "0,1,2,3" if backend == "docker" else "0"]
        properties = [
            ("CPUAccounting", "b", "true"),
            ("MemoryAccounting", "b", "true"),
            ("CPUQuotaPerSecUSec", "t", "4000000"),
            ("MemoryMax", "t", str(4 * 1024**3)),
            ("MemorySwapMax", "t", "0"),
        ]
        e.command(
            [
                "busctl",
                "--user",
                "--quiet",
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                self.slice,
                "fail",
                str(len(properties)),
                *[v for p in properties for v in p],
                "0",
            ]
        )
        try:
            self.group = self.group_for(self.slice)
            self.budget = ResourceBudget(
                self.group, 4 * 1024**3, frozenset(range(4)), cpu_count=4, cpu_placement="affinity"
            )
            e.command(
                [
                    "systemd-run",
                    "--user",
                    "--quiet",
                    "--unit=" + self.client_unit,
                    "--slice=" + self.slice,
                    "-p",
                    "Delegate=yes",
                    "taskset",
                    "-c",
                    "0,1,2,3",
                    "/usr/bin/sleep",
                    "infinity",
                ]
            )
            leaf = self.group_for(self.client_unit)
            self.launch = [e.root / "enter-group", leaf, "taskset", "-c", "0,1,2,3"]
            if backend == "docker":
                self.docker = Docker(e, directory, self.slice)
                self.prefix = [*self.launch, *self.docker.argv]
            else:
                self.prefix = [*self.launch, e.binary]
            self.budget.read()
        except BaseException:
            self.close()
            raise

    def group_for(self, unit):
        path = self.e.command(
            ["systemctl", "--user", "show", unit, "--property=ControlGroup", "--value"]
        ).strip()
        assert path.startswith("/user.slice/") and self.slice in path.split("/")
        return Path("/sys/fs/cgroup") / path.lstrip("/")

    def command(self, args, check=True):
        return self.e.command([*self.prefix, *args], self.env, check=check)

    def witness(self, names):
        if self.backend == "docker":
            required = [self.docker.pid]
            for name in names:
                required.append(
                    int(self.command(["inspect", "--format", "{{.State.Pid}}", name]).strip())
                )
        else:
            ids = [
                json.loads(p.read_text())["id"]
                for p in self.store.glob("instances/*/instance.json")
            ]
            required = []
            for proc in Path("/proc").iterdir():
                if not proc.name.isdigit():
                    continue
                try:
                    argv = (proc / "cmdline").read_bytes().split(b"\0")
                except OSError:
                    continue
                if (
                    argv
                    and argv[0] == os.fsencode(self.e.binary)
                    and any(os.fsencode(i) in argv for i in ids)
                ):
                    required.append(int(proc.name))
            assert len(required) >= 2 * len(names), "missing live owner/VMM scope"
        if not required:
            return {"members": self.budget.members()}
        for attempt in range(3):
            try:
                return self.budget.witness_all_members(required)
            except ObservationChanged:
                if attempt == 2:
                    raise
                time.sleep(0.05)

    def observe(self, names, label):
        samples = []
        start = time.monotonic()
        while time.monotonic() - start < 3:
            observation = self.budget.read()
            assert not any(observation["memory_events"].values()), (
                "memory pressure/OOM invalidates group"
            )
            assert (self.group / "memory.swap.current").read_text().strip() == "0"
            samples.append(dict(elapsed_s=time.monotonic() - start, **observation))
            time.sleep(0.2)
        self.witness(names)
        result = dict(
            memory_current_bytes=statistics.median(
                s["memory_current_bytes"] for s in samples if s["elapsed_s"] >= 1
            ),
            samples=samples,
        )
        RUNTIME.save(self.directory / (label + ".json"), result)
        RUNTIME.save(self.directory / (label + "-witness.json"), self.witness(names))
        return result["memory_current_bytes"]

    def close(self):
        if hasattr(self, "prefix"):
            ids = self.command(["ps", "-aq"], check=False)
            if ids and ids.strip():
                self.command(["rm", "-f", *ids.split()], check=False)
        if self.docker:
            self.docker.close()
        self.e.command(["systemctl", "--user", "stop", self.client_unit, self.slice], check=False)


def runtime(e, args):
    image = next(i for i in e.inputs if i["name"] == "ubuntu-24.04")
    launcher = REPO / "benchmark/storage/enter_group.rs"
    shutil.copy2(launcher, e.root / launcher.name)
    e.command(["rustc", "--edition=2024", "-O", launcher, "-o", e.root / "enter-group"])
    e.save(
        launcher_sha256=sha(e.root / "enter-group"),
        cpus=[0, 1, 2, 3],
        runs=args.runs,
        warmup=3,
        memory_rounds=args.memory_rounds,
    )
    products = {}
    try:
        for backend in ["docker", "pvisor"]:
            directory = e.root / ("startup-" + backend)
            directory.mkdir()
            product = Product(e, directory, backend)
            products[backend] = product
            import_image(e, image, backend, product.prefix, product.env)
        rng = random.Random(819)

        def trial(phase, index):
            pair = {}
            for backend in rng.sample(["docker", "pvisor"], 2):
                product = products[backend]
                name = f"pvready-{phase}-{index}"
                directory = e.root / f"{phase}-{index:03}-{backend}"
                directory.mkdir()
                argv = [
                    str(v)
                    for v in [
                        *product.prefix,
                        "run",
                        "--rm",
                        "--name",
                        name,
                        "--network=none",
                        "--cpus=1",
                        "--memory=256m",
                        image["pinned"],
                        *product.payload,
                        "/bin/sh",
                        "-ec",
                        'test ! -e /tmp/pv-private; printf check >/tmp/pv-private; test "$(cat /tmp/pv-private)" = check; printf "REFERENCE_READY\\n"',
                    ]
                ]
                result = RUNTIME.capture(
                    argv, REPO, os.environ | product.env, directory / "stdout", timeout=30
                )
                assert (directory / "stdout").read_text() == "REFERENCE_READY\n"
                assert not product.command(["ps", "-aq"]).strip(), "container cleanup failed"
                pair[backend] = {
                    "ready_ns": result["ready_duration_ns"],
                    "exit_ns": result["exit_duration_ns"],
                }
            return pair

        pairs = RUNTIME.repeat(3, args.runs, trial)
        RUNTIME.save(e.root / "startup-pairs.json", pairs)
        RUNTIME.save(
            e.root / "startup-summary.json",
            {key: paired(pairs, key) for key in ["ready_ns", "exit_ns"]},
        )
    finally:
        for p in products.values():
            p.close()
    memory = []
    for repeat in range(args.memory_rounds):
        pair = {}
        for backend in random.Random(821 + repeat).sample(["docker", "pvisor"], 2):
            directory = e.root / f"memory-{repeat:02}-{backend}"
            directory.mkdir()
            product = Product(e, directory, backend)
            names = []
            try:
                import_image(e, image, backend, product.prefix, product.env)
                pair[backend] = {"empty": product.observe(names, "empty")}
                for index in range(4):
                    name = f"pvmem-{index}"
                    names.append(name)
                    product.command(
                        [
                            "run",
                            "-d",
                            "--name",
                            name,
                            "--network=none",
                            "--cpus=1",
                            "--memory=256m",
                            image["pinned"],
                            *product.payload,
                            "/bin/sh",
                            "-ec",
                            f'test ! -e /tmp/pv-private; printf "{name}" >/tmp/pv-private; printf "{name}\\n"; exec /bin/sleep 180',
                        ]
                    )
                    deadline = time.monotonic() + 5
                    while product.command(["logs", name]) != name + "\n":
                        if time.monotonic() >= deadline:
                            raise ValueError("memory subject did not emit its exact token")
                        time.sleep(0.05)
                    for peer in names:
                        assert (
                            product.command(
                                ["exec", peer, *product.payload, "/bin/cat", "/tmp/pv-private"]
                            )
                            == peer
                        )
                    if index in [0, 3]:
                        key = "one" if index == 0 else "four"
                        pair[backend][key] = product.observe(names, key)
                for count, key in [(1, "one"), (4, "four")]:
                    pair[backend][key + "_increment"] = (
                        pair[backend][key] - pair[backend]["empty"]
                    ) / count
            finally:
                product.close()
        memory.append(pair)
        RUNTIME.save(e.root / "memory-pairs.json", memory)
        print("memory", repeat + 1, pair, flush=True)
    RUNTIME.save(
        e.root / "memory-summary.json",
        {
            key: paired(memory, key)
            for key in ["empty", "one", "four", "one_increment", "four_increment"]
        },
    )


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--phase", choices=["storage", "runtime"], required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--inputs", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--repeats", type=int, default=3)
    p.add_argument("--runs", type=int, default=30)
    p.add_argument("--memory-rounds", type=int, default=5)
    args = p.parse_args()
    if min(args.repeats, args.runs, args.memory_rounds) < 1:
        p.error("repeats, runs and memory-rounds must be positive")
    e = Evidence(args)
    try:
        (storage if args.phase == "storage" else runtime)(e, args)
        assert sha(e.binary) == e.manifest["binary_sha256"]
        e.save(
            status="complete", finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat()
        )
    except BaseException as error:
        e.save(status="failed", error=repr(error), error_evidence=getattr(error, "evidence", None))
        raise


if __name__ == "__main__":
    main()
