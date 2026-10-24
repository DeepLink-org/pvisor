#!/usr/bin/env python3
"""Benchmark: B-OPENSANDBOX-MEMORY-ENG, role engineering A/B.
Motivation: compare full local service and sandbox memory with official OpenSandbox.
Conclusion sought: scoped default/pool/Docker memory, CPU and correctness, including losses.
Design: same Ubuntu/SDK/1 CPU/512 MiB, private product slices, 1/4 idle instances,
four identical 64 MiB allocations for 30 seconds then unique writes for 10 seconds;
independent preflight and five seeded randomized formal rounds, no exclusions.
"""

import argparse
import asyncio
import hashlib
import importlib.metadata
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import uuid
from datetime import timedelta
from pathlib import Path

KEY = "pvisor-memory-comparison-test-key"
BASE = "mirror.gcr.io/library/ubuntu@sha256:a2d43bffc46670d7108bb3c0468c67c5557c4c92e83e1e34cbff1322ad1b94dc"
INIT = "sha256:500d6631531dbaee4762871cc041bb55c36a34f024c7901402f3c9ecac655bcb"
ARMS = ["native", "native-pool", "opensandbox-docker"]
SCRIPT = Path(__file__).resolve()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        while data := f.read(1024 * 1024):
            h.update(data)
    return h.hexdigest()


def call(argv, **kwargs):
    return subprocess.run(
        argv, check=True, capture_output=True, text=True, timeout=120, **kwargs
    ).stdout.strip()


def pids(group):
    result = set()
    for parent, _, _ in os.walk(group):
        try:
            result.update(map(int, (Path(parent) / "cgroup.procs").read_text().split()))
        except FileNotFoundError:
            pass
    return sorted(result)


def observe(group):
    row = {"monotonic": time.monotonic(), "processes": [], "unreadable_pss": []}
    for name in [
        "memory.current",
        "memory.peak",
        "memory.swap.current",
        "memory.stat",
        "memory.events",
        "cpu.stat",
        "pids.current",
        "pids.events",
    ]:
        row[name] = (group / name).read_text().strip()
    for pid in pids(group):
        proc = Path("/proc") / str(pid)
        try:
            values = {}
            try:
                rollup = (proc / "smaps_rollup").read_text()
            except PermissionError:
                # Do not omit a non-dumpable rootless helper from a PSS total.
                # Cgroup charged memory remains complete for the whole subtree.
                row["unreadable_pss"].append(
                    {"pid": pid, "comm": (proc / "comm").read_text().strip()}
                )
                continue
            for line in rollup.splitlines()[1:]:
                key, value = line.split(":", 1)
                values[key] = int(value.split()[0]) * 1024
            row["processes"].append(
                {
                    "pid": pid,
                    "comm": (proc / "comm").read_text().strip(),
                    "pss": values["Pss"],
                    "rss": values["Rss"],
                    "cgroup": (proc / "cgroup").read_text().strip(),
                }
            )
        except (FileNotFoundError, ProcessLookupError):
            pass
    row["readable_pss"] = sum(p["pss"] for p in row["processes"])
    row["pss"] = row["readable_pss"] if not row["unreadable_pss"] else None
    row["rss"] = sum(p["rss"] for p in row["processes"]) if not row["unreadable_pss"] else None
    row["charged"] = int(row["memory.current"])
    row["cpu_usec"] = int(dict(line.split() for line in row["cpu.stat"].splitlines())["usage_usec"])
    return row


def host():
    ksm = Path("/sys/kernel/mm/ksm")
    return {
        "kernel": list(os.uname()),
        "affinity": sorted(os.sched_getaffinity(0)),
        "loadavg": Path("/proc/loadavg").read_text(),
        "processes": call(["ps", "-eo", "pid,ppid,pcpu,pmem,comm", "--sort=-pcpu"]),
        "ksm": {
            p.name: p.read_text().strip()
            for p in ksm.iterdir()
            if p.is_file() and os.access(p, os.R_OK)
        },
        "host_docker_ids": call(["docker", "ps", "-aq", "--no-trunc"]).splitlines(),
    }


class Product:
    def __init__(self, root):
        self.root = root
        self.name = "pvom" + uuid.uuid4().hex
        self.slice = self.name + ".slice"
        self.services = []
        call(
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--collect",
                "--unit=" + self.name + "anchor",
                "--slice=" + self.slice,
                "-p",
                "RuntimeMaxSec=240",
                "-p",
                "CPUAffinity=0 1 2 3",
                "/usr/bin/sleep",
                "240",
            ]
        )
        try:
            call(
                [
                    "systemctl",
                    "--user",
                    "set-property",
                    "--runtime",
                    self.slice,
                    "MemoryMax=4294967296",
                    "MemorySwapMax=0",
                    "CPUQuota=400%",
                    "TasksMax=2048",
                ]
            )
            self.group = Path("/sys/fs/cgroup") / call(
                ["systemctl", "--user", "show", self.slice, "-p", "ControlGroup", "--value"]
            ).lstrip("/")
            assert (self.group / "memory.max").read_text().strip() == "4294967296"
            assert (self.group / "memory.swap.max").read_text().strip() == "0"
            assert (self.group / "cpu.max").read_text().split() == ["400000", "100000"]
            save(
                root / "product-group.json",
                {
                    "slice": self.slice,
                    "group": str(self.group),
                    "memory_max": 4294967296,
                    "cpu_max": "400000 100000",
                    "swap_max": 0,
                },
            )
        except BaseException:
            self.stop()
            raise

    def launch(self, label, argv, env=None, native=False):
        config = self.root / (label + "-exec.json")
        save(
            config,
            {
                "argv": list(map(str, argv)),
                "env": env or {},
                "native": native,
                "log": str(self.root / (label + ".log")),
            },
        )
        unit = self.name + label
        call(
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--collect",
                "--unit=" + unit,
                "--slice=" + self.slice,
                "-p",
                "Delegate=yes",
                "-p",
                "CPUAffinity=0 1 2 3",
                "-p",
                "TimeoutStopSec=15",
                sys.executable,
                str(SCRIPT),
                "--exec",
                str(config),
            ]
        )
        self.services.append(unit)

    def stop(self):
        call(["systemctl", "--user", "stop", self.slice])
        if hasattr(self, "group") and self.group.exists():
            assert not pids(self.group), "product group leaked processes"


def execute(path):
    cfg = json.loads(path.read_text())
    argv = cfg["argv"]
    if cfg["native"]:
        group = Path("/sys/fs/cgroup") / Path(
            Path("/proc/self/cgroup").read_text().strip().split(":", 2)[2]
        ).relative_to("/")
        (group / "api").mkdir()
        (group / "api/cgroup.procs").write_text(str(os.getpid()))
        (group / "cgroup.subtree_control").write_text("+cpu +memory +pids")
        argv += ["--cgroup-root", str(group)]
    fd = os.open(cfg["log"], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.dup2(fd, 1)
    os.dup2(fd, 2)
    os.close(fd)
    os.execvpe(argv[0], argv, {**os.environ, **cfg["env"]})


class Docker:
    def __init__(self, product, data):
        self.product = product
        self.runtime = Path(tempfile.mkdtemp(prefix="pvomd-"))
        self.cli = ["docker", "--host=unix://" + str(self.runtime / "docker.sock")]
        config = self.runtime / "daemon.json"
        save(config, {"features": {"containerd-snapshotter": True}})
        env = {
            "DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR": str(self.runtime / "kit"),
            "DOCKERD_ROOTLESS_ROOTLESSKIT_NET": "pasta",
            "DOCKERD_ROOTLESS_ROOTLESSKIT_PORT_DRIVER": "pesto",
            "DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS": "false",
        }
        product.launch(
            "docker",
            [
                "/usr/bin/dockerd-rootless.sh",
                "--config-file=" + str(config),
                "--host=unix://" + str(self.runtime / "docker.sock"),
                "--data-root=" + str(data),
                "--exec-root=" + str(self.runtime / "exec"),
                "--pidfile=" + str(self.runtime / "pid"),
                "--cgroup-parent=" + product.slice,
                "--bridge=none",
                "--iptables=false",
                "--ip6tables=false",
                "--userland-proxy=true",
            ],
            env,
        )
        until = time.monotonic() + 30
        while time.monotonic() < until:
            try:
                info = json.loads(self.call(["info", "--format", "{{json .}} "]))
                assert info["CgroupDriver"] == "systemd" and info["CgroupVersion"] == "2"
                assert "name=rootless" in info["SecurityOptions"] and info["Driver"] == "overlayfs"
                save(product.root / "docker-info.json", info)
                return
            except subprocess.CalledProcessError:
                time.sleep(0.1)
        raise TimeoutError("private Docker startup")

    def call(self, args):
        return call(self.cli + args)


def prepare(args):
    root = args.inputs.resolve()
    root.mkdir()
    source = Path(__file__).parent
    shutil.copy2(source / "memory_worker.rs", root / "memory_worker.rs")
    argv = [
        "rustc",
        "--edition=2021",
        "--target",
        "x86_64-unknown-linux-musl",
        "-C",
        "opt-level=2",
        str(root / "memory_worker.rs"),
        "-o",
        str(root / "memory-worker"),
    ]
    call(argv)
    shutil.copy2(args.daemon, root / "pvisor-daemon")
    shutil.copy2(Path(args.images_dir) / "native-ubuntu.json", root / "native-ubuntu.json")
    for name in ["source.tar.gz", "source-receipt.json", "validation.json"]:
        shutil.copy2(args.source_evidence / name, root / name)
    shutil.copy2(Path(__file__), root / "memory.py")
    call(["docker", "image", "save", "--output", str(root / "images.tar"), BASE, INIT])
    save(root / "host-images.json", json.loads(call(["docker", "image", "inspect", BASE, INIT])))
    product = Product(root)
    try:
        docker = Docker(product, root / "seed")
        docker.call(["load", "--input", str(root / "images.tar")])
        image = json.loads((root / "host-images.json").read_text())[0]["Id"]
        docker.call(["tag", image, "pvom-base:frozen"])
        docker.call(["tag", INIT, "pvom-execd:frozen"])
        docker.call(
            [
                "network",
                "create",
                "--subnet=10.98.0.0/24",
                "--opt",
                "com.docker.network.bridge.enable_ip_masquerade=false",
                "pvom-network",
            ]
        )
        save(
            root / "private-images.json",
            json.loads(docker.call(["image", "inspect", "pvom-base:frozen", "pvom-execd:frozen"])),
        )
    finally:
        product.stop()
    save(
        root / "receipt.json",
        {
            "benchmark": "B-OPENSANDBOX-MEMORY-ENG",
            "base": BASE,
            "execd_image": INIT,
            "build": argv,
            "rustc": call(["rustc", "-Vv"]),
            "files": {
                n: digest(root / n)
                for n in [
                    "memory_worker.rs",
                    "memory-worker",
                    "memory.py",
                    "pvisor-daemon",
                    "native-ubuntu.json",
                    "source.tar.gz",
                    "source-receipt.json",
                    "validation.json",
                    "images.tar",
                    "host-images.json",
                    "private-images.json",
                ]
            },
        },
    )
    print("PREPARED", root, flush=True)


def verify_inputs(root):
    receipt = json.loads((root / "receipt.json").read_text())
    assert all(digest(root / name) == value for name, value in receipt["files"].items()), (
        "frozen input changed"
    )
    return receipt


async def trial(args, root, arm, repeat):
    import httpx
    from opensandbox.config import ConnectionConfig
    from opensandbox.models.execd import RunCommandOpts
    from opensandbox.models.filesystem import WriteEntry
    from opensandbox.sandbox import Sandbox

    product = Product(root)
    state = Path(tempfile.mkdtemp(prefix="pvoms-"))
    sandboxes = []
    row = {
        "arm": arm,
        "repeat": repeat,
        "success": False,
        "phases": {},
        "instances": [],
        "checks": [],
    }
    domain = "127.0.0.1:18540"
    connection = ConnectionConfig(
        domain=domain, protocol="http", api_key=KEY, use_server_proxy=True
    ).with_transport_if_missing()
    observer_stop = threading.Event()
    observer_error = []
    samples = []
    thread = None
    cli = None

    def monitor():
        try:
            with (root / "observations.jsonl").open("w") as output:
                while not observer_stop.is_set():
                    sample = observe(product.group)
                    output.write(json.dumps(sample) + "\n")
                    output.flush()
                    samples.append(sample)
                    observer_stop.wait(0.5)
        except BaseException as error:
            observer_error.append(repr(error))

    async def phase(name, seconds):
        begin = time.monotonic()
        first = observe(product.group)
        await asyncio.sleep(seconds)
        last = observe(product.group)
        selected = [
            s for s in samples if begin + max(0, seconds - 2) <= s["monotonic"] <= last["monotonic"]
        ] + [last]
        row["phases"][name] = {
            "start": first,
            "end": last,
            "pss": statistics.median(s["pss"] for s in selected)
            if all(s["pss"] is not None for s in selected)
            else None,
            "charged": statistics.median(s["charged"] for s in selected),
            "cpu_seconds": (last["cpu_usec"] - first["cpu_usec"]) / 1e6,
            "seconds": last["monotonic"] - first["monotonic"],
            "samples": len(selected),
        }
        assert not observer_error, observer_error
        assert int(last["memory.swap.current"]) == 0
        events = dict(line.split() for line in last["memory.events"].splitlines())
        assert events["oom"] == events["oom_kill"] == "0"
        save(root / "result.json", row)

    async def command(sandbox, text):
        result = await sandbox.commands.run(text)
        assert result.exit_code == 0
        return "".join(item.text for item in result.logs.stdout)

    try:
        if arm == "opensandbox-docker":
            call(
                [
                    "rootlesskit",
                    "--net=host",
                    "--state-dir=" + tempfile.mkdtemp(prefix="pvomcopy-"),
                    "/usr/bin/cp",
                    "-a",
                    "--reflink=auto",
                    str(args.inputs / "seed"),
                    str(root / "docker-data"),
                ]
            )
            docker = Docker(product, root / "docker-data")
            cli = docker.cli
            config = root / "server.toml"
            config.write_text(
                '[server]\nhost="127.0.0.1"\nport=18540\n[log]\nlevel="INFO"\n[runtime]\ntype="docker"\nexecd_image="pvom-execd:frozen"\n[store]\ntype="sqlite"\npath='
                + json.dumps(str(root / "official.db"))
                + '\n[proxy]\nresolve_internal=false\n[docker]\nnetwork_mode="pvom-network"\n[ingress]\nmode="direct"\n'
            )
            product.launch(
                "api",
                [
                    str(Path(sys.executable).with_name("opensandbox-server")),
                    "--config",
                    str(config),
                ],
                {
                    "DOCKER_HOST": "unix://" + str(docker.runtime / "docker.sock"),
                    "PYTHONPATH": str(args.upstream / "server"),
                    "OPENSANDBOX_SERVER_API_KEY": KEY,
                },
            )
            image = "pvom-base:frozen"
        else:
            images = root / "images"
            images.mkdir()
            shutil.copy2(args.inputs / "native-ubuntu.json", images / "native-ubuntu.json")
            argv = [
                str(args.inputs / "pvisor-daemon"),
                "serve",
                "--sandbox",
                "--images-dir",
                str(images),
                "--state",
                str(state),
                "--listen",
                domain,
            ]
            if arm == "native-pool":
                argv += ["--memory-pool"]
            product.launch("api", argv, {"OPEN_SANDBOX_API_KEY": KEY}, native=True)
            image = "native-ubuntu"
        async with httpx.AsyncClient(
            base_url="http://" + domain, headers={"OPEN-SANDBOX-API-KEY": KEY}, trust_env=False
        ) as client:
            until = time.monotonic() + 40
            while time.monotonic() < until:
                try:
                    response = await client.get("/v1/sandboxes")
                    if response.status_code == 200:
                        assert response.json()["items"] == []
                        break
                except httpx.HTTPError:
                    pass
                await asyncio.sleep(0.05)
            else:
                raise TimeoutError("API readiness")
        thread = threading.Thread(target=monitor)
        thread.start()
        await phase("service", 3)
        for index in range(4):
            sandbox = await Sandbox.create(
                image,
                connection_config=connection,
                resource={"cpu": "1", "memory": "512Mi"},
                entrypoint=["/bin/sleep", "infinity"],
                env={"MEMORY_INSTANCE": str(index)},
                ready_timeout=timedelta(seconds=120),
            )
            sandboxes.append(sandbox)
            assert await command(
                sandbox,
                'test ! -e /tmp/private-memory && printf %s "$MEMORY_INSTANCE" >/tmp/private-memory && cat /tmp/private-memory',
            ) == str(index)
            if cli:
                ids = call(cli + ["ps", "-q"]).splitlines()
                inspected = json.loads(call(cli + ["inspect", *ids]))
                matched = [c for c in inspected if sandbox.id in c["Config"]["Labels"].values()]
                assert len(matched) == 1
                item = matched[0]
                assert (
                    item["HostConfig"]["Memory"] == 536870912
                    and item["HostConfig"]["NanoCpus"] == 10**9
                )
                pid = item["State"]["Pid"]
                relative = Path("/proc", str(pid), "cgroup").read_text().strip().split(":", 2)[2]
                leaf = Path("/sys/fs/cgroup") / relative.lstrip("/")
                assert str(leaf).startswith(str(product.group) + "/")
                assert (leaf / "memory.max").read_text().strip() == "536870912"
                assert (leaf / "cpu.max").read_text().split() == ["100000", "100000"]
                save(root / f"container-{index}.json", item)
                row["instances"].append({"id": sandbox.id, "pid": pid, "cgroup": str(leaf)})
            else:
                identity = json.loads((state / sandbox.id / "identity.json").read_text())
                assert identity["image"]["native_guest"]
                assert (identity.get("memory_pool") is not None) == (arm == "native-pool")
                leaf = Path(identity["cgroup"])
                assert str(leaf).startswith(str(product.group) + "/")
                assert (leaf / "memory.max").read_text().strip() == "536870912"
                assert (leaf / "memory.swap.max").read_text().strip() == "0"
                assert (leaf / "cpu.max").read_text().split() == ["100000", "100000"]
                row["instances"].append({"id": sandbox.id, "cgroup": str(leaf)})
            if index == 0:
                await phase("idle-1", 3)
        await phase("idle-4", 5)
        for sandbox in sandboxes:
            await sandbox.files.write_files(
                [
                    WriteEntry(
                        path="/tmp/memory-worker",
                        data=(args.inputs / "memory-worker").read_bytes(),
                        mode=755,
                    )
                ]
            )
            execution = await sandbox.commands.run(
                "/tmp/memory-worker serve /tmp/memory.sock /tmp/memory-ready 123456789",
                opts=RunCommandOpts(background=True),
            )
            row["checks"].append({"background_id": execution.id})
        for sandbox in sandboxes:
            until = time.monotonic() + 15
            while time.monotonic() < until:
                try:
                    ready = json.loads(await sandbox.files.read_file("/tmp/memory-ready"))
                    assert ready["verified"] and ready["bytes"] == 64 * 1024**2
                    row["checks"].append(ready)
                    break
                except Exception:
                    await asyncio.sleep(0.05)
            else:
                raise TimeoutError("64 MiB worker readiness")
        await phase("shared-4", 30)
        before = time.monotonic()
        for sandbox in sandboxes:
            result = json.loads(
                await command(sandbox, "/tmp/memory-worker client /tmp/memory.sock check")
            )
            assert result["seed"] == 123456789 and result["verified"]
            row["checks"].append(result)
        row["readback_seconds"] = time.monotonic() - before
        for index, sandbox in enumerate(sandboxes):
            seed = 987654321 + index * 100000000
            result = json.loads(
                await command(sandbox, f"/tmp/memory-worker client /tmp/memory.sock unique {seed}")
            )
            assert result["seed"] == seed and result["verified"]
            row["checks"].append(result)
        await phase("unique-4", 10)
        for index, sandbox in enumerate(sandboxes):
            result = json.loads(
                await command(sandbox, "/tmp/memory-worker client /tmp/memory.sock quit")
            )
            assert result["seed"] == 987654321 + index * 100000000 and result["verified"]
            assert await sandbox.files.read_file("/tmp/private-memory") == str(index)
            row["checks"].append(result)
        row["success"] = True
    except BaseException as error:
        row.update(error=repr(error), traceback=traceback.format_exc())
        if isinstance(error, subprocess.CalledProcessError):
            row["subprocess_stderr"] = error.stderr
        raise
    finally:
        for sandbox in sandboxes:
            try:
                await sandbox.kill()
                async with httpx.AsyncClient(
                    base_url="http://" + domain,
                    headers={"OPEN-SANDBOX-API-KEY": KEY},
                    trust_env=False,
                ) as client:
                    assert (await client.get("/v1/sandboxes/" + sandbox.id)).status_code == 404
                await sandbox.close()
            except BaseException as error:
                row.update(success=False, cleanup_error=repr(error))
        # A failed SDK readiness call may leave an accepted sandbox without a handle.
        # The API and its initially empty inventory belong exclusively to this trial.
        try:
            async with httpx.AsyncClient(
                base_url="http://" + domain,
                headers={"OPEN-SANDBOX-API-KEY": KEY},
                trust_env=False,
                timeout=120,
            ) as client:
                inventory = await client.get("/v1/sandboxes")
                inventory.raise_for_status()
                for item in inventory.json()["items"]:
                    response = await client.delete("/v1/sandboxes/" + item["id"])
                    assert response.status_code in [204, 404]
                assert (await client.get("/v1/sandboxes")).json()["items"] == []
                row["api_inventory_empty"] = True
        except Exception as error:
            row.update(success=False, inventory_cleanup_error=repr(error))
        await connection.close_transport_if_owned()
        observer_stop.set()
        if thread:
            thread.join(timeout=10)
            assert not thread.is_alive()
        row["observer_errors"] = observer_error
        try:
            if cli:
                assert call(cli + ["ps", "-aq"]) == ""
            row["all_deleted"] = all(not Path(i["cgroup"]).exists() for i in row["instances"])
            assert row["all_deleted"]
        finally:
            product.stop()
            save(root / "result.json", row)
    assert row["success"] and not observer_error, row
    print(
        "PASSED",
        repeat,
        arm,
        {k: round(v["charged"] / 1024**2, 2) for k, v in row["phases"].items()},
        flush=True,
    )
    return row


async def campaign(args):
    global SCRIPT
    assert importlib.metadata.version("opensandbox") == "1.1.0"
    receipt = verify_inputs(args.inputs)
    args.output.mkdir()
    validation = json.loads((args.inputs / "validation.json").read_text())
    assert receipt["files"]["pvisor-daemon"] == validation["daemon_sha256"]
    assert (
        call(["git", "rev-parse", "HEAD"], cwd=args.upstream)
        == "b1a29cf93a823a95913f7943010febb3f29de05c"
    )
    assert call(["git", "status", "--porcelain"], cwd=args.upstream) == ""
    with (args.output / "upstream-source.tar.gz").open("wb") as output:
        subprocess.run(
            [
                "git",
                "archive",
                "--format=tar.gz",
                "HEAD",
                "components/execd",
                "server",
                "sdks/sandbox/python",
                "specs",
            ],
            cwd=args.upstream,
            stdout=output,
            check=True,
        )
    runtime = [
        "/usr/bin/" + name
        for name in [
            "docker",
            "dockerd",
            "dockerd-rootless.sh",
            "rootlesskit",
            "pasta",
            "pesto",
            "containerd",
            "containerd-shim-runc-v2",
            "runc",
        ]
    ]
    save(
        args.output / "execution-receipt.json",
        {
            "input_receipt_sha256": digest(args.inputs / "receipt.json"),
            "harness_sha256": digest(Path(__file__)),
            "upstream_sha256": digest(args.output / "upstream-source.tar.gz"),
            "runtime_sha256": {name: digest(name) for name in runtime},
            "sdk": importlib.metadata.version("opensandbox"),
            "python": sys.version,
            "preflight": args.preflight,
        },
    )
    shutil.copy2(Path(__file__), args.output / "harness.py")
    SCRIPT = args.output / "harness.py"
    save(args.output / "input-receipt.json", receipt)
    (args.output / "QUESTION.txt").write_text(
        "B-OPENSANDBOX-MEMORY-ENG: Compare complete local OpenSandbox deployment PSS/cgroup memory for default native, explicit physical pool, and official server/private rootless Docker. Empty API, 1/4 idle instances, four identical 64 MiB allocations for 30 seconds, then unique 64 MiB allocations for 10 seconds; no host cache/KSM changes; no density or full-host physical savings claim.\n"
    )
    save(args.output / "host-before.json", host())
    rng = random.Random(args.seed)
    order = []
    for repeat in range(1 if args.preflight else args.rounds):
        arms = ARMS.copy()
        rng.shuffle(arms)
        order.extend({"repeat": repeat, "arm": arm} for arm in arms)
    save(args.output / "order.json", order)
    rows = []
    try:
        for item in order:
            root = args.output / (str(item["repeat"]) + "-" + item["arm"])
            root.mkdir()
            rows.append(await trial(args, root, **item))
            save(args.output / "results.json", rows)
    finally:
        after = host()
        save(args.output / "host-after.json", after)
        before = json.loads((args.output / "host-before.json").read_text())
        assert sorted(before["host_docker_ids"]) == sorted(after["host_docker_ids"])
        verify_inputs(args.inputs)
        assert (
            call(["git", "rev-parse", "HEAD"], cwd=args.upstream)
            == "b1a29cf93a823a95913f7943010febb3f29de05c"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--daemon", type=Path)
    parser.add_argument("--images-dir", type=Path)
    parser.add_argument("--source-evidence", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--upstream", type=Path)
    parser.add_argument("--preflight", action="store_true")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--seed", type=int, default=20261024)
    parser.add_argument("--exec", type=Path)
    args = parser.parse_args()
    for name in ["inputs", "output", "daemon", "images_dir", "source_evidence", "upstream", "exec"]:
        value = getattr(args, name)
        if value is not None:
            setattr(args, name, value.resolve())
    if args.exec:
        execute(args.exec)
    elif args.prepare:
        prepare(args)
    else:
        assert args.preflight or args.rounds >= 5
        asyncio.run(campaign(args))


if __name__ == "__main__":
    main()
