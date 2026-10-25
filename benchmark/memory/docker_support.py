"""Private B-MEMORY-SCALE rootless Docker lifecycle and ownership evidence.

Each condition owns a daemon, runtime directory, image-store copy and systemd
slice. No command uses the user's default Docker socket, containers or images.
"""

import json
import os
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

from scanner_compare import digest, save


def group_pids(group):
    """Read the entire owned cgroup subtree, including Docker systemd scopes."""
    found = set()
    for directory, _dirs, _files in os.walk(group):
        try:
            found.update(int(pid) for pid in (Path(directory) / "cgroup.procs").read_text().split())
        except FileNotFoundError:
            continue
    return sorted(found)


def belongs(cgroup_text, relative):
    entries = [line[3:] for line in cgroup_text.splitlines() if line.startswith("0::")]
    return len(entries) == 1 and (entries[0] == relative or entries[0].startswith(relative + "/"))


def create_slice(root):
    """Install limits before starting the product; anchor bounds orphan lifetime."""
    name = "pvisordockermemory" + uuid.uuid4().hex
    unit = name + ".slice"
    anchor = name + ".service"
    # The timer also stops owned container scopes if the coordinator disappears.
    script = (
        "import subprocess,time; time.sleep(240); "
        'subprocess.run(["systemctl","--user","stop","--no-block",' + repr(unit) + "],check=True)"
    )
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--unit=" + anchor,
        "--slice=" + unit,
        "-p",
        "CPUAffinity=0 1 2 3",
        "-p",
        "TimeoutStopSec=10",
        "/usr/bin/python3",
        "-c",
        script,
    ]
    save(root / "slice-launch.json", dict(unit=unit, anchor=anchor, command=command))
    subprocess.run(command, check=True, capture_output=True, timeout=15)
    try:
        subprocess.run(
            [
                "systemctl",
                "--user",
                "set-property",
                "--runtime",
                unit,
                "MemoryMax=2147483648",
                "MemorySwapMax=0",
                "CPUQuota=400%",
                "TasksMax=256",
            ],
            check=True,
            capture_output=True,
            timeout=15,
        )
        relative = subprocess.check_output(
            ["systemctl", "--user", "show", unit, "-p", "ControlGroup", "--value"],
            text=True,
            timeout=5,
        ).strip()
        group = Path("/sys/fs/cgroup") / relative.lstrip("/")
        if not relative or group.name != unit:
            raise ValueError("owned Docker slice identity missing")
        return unit, group
    except BaseException:
        subprocess.run(["systemctl", "--user", "stop", unit], capture_output=True, timeout=15)
        raise


class Engine:
    """One private daemon, always accessed through its explicit Unix socket."""

    def __init__(self, root, slice_unit, data):
        self.root, self.slice_unit, self.data = root, slice_unit, data
        self.runtime = Path(tempfile.mkdtemp(prefix="pvdc-", dir="/tmp"))
        self.socket = self.runtime / "docker.sock"
        self.cli = ["/usr/bin/docker", "--host=unix://" + str(self.socket)]
        self.process = None
        self.logs = []
        self.containers = []
        self.clients = []

    def call(self, args, *, timeout=30, **kwargs):
        return subprocess.run(
            self.cli + args, check=True, capture_output=True, timeout=timeout, **kwargs
        )

    def start(self):
        config = self.root / "docker-daemon.json"
        save(config, {})
        env = os.environ.copy()
        # A unique state directory avoids any pre-existing rootless daemon.
        env.update(
            DOCKERD_ROOTLESS_ROOTLESSKIT_STATE_DIR=str(self.runtime / "kit"),
            DOCKERD_ROOTLESS_ROOTLESSKIT_DETACH_NETNS="false",
            DOCKERD_ROOTLESS_ROOTLESSKIT_NET="none",
            DOCKERD_ROOTLESS_ROOTLESSKIT_PORT_DRIVER="none",
        )
        command = [
            "/usr/bin/dockerd-rootless.sh",
            "--config-file=" + str(config),
            "--data-root=" + str(self.data),
            "--exec-root=" + str(self.runtime / "exec"),
            "--pidfile=" + str(self.runtime / "daemon.pid"),
            "--host=unix://" + str(self.socket),
            "--bridge=none",
            "--iptables=false",
            "--ip6tables=false",
            "--ip-forward=false",
            "--ip-masq=false",
            "--userland-proxy=false",
            "--cgroup-parent=" + self.slice_unit,
        ]
        save(
            self.root / "docker-launch.json",
            dict(
                command=command,
                runtime=str(self.runtime),
                rootlesskit_network="none",
                rootlesskit_port_driver="none",
            ),
        )
        out = (self.root / "docker.stdout").open("wb")
        err = (self.root / "docker.stderr").open("wb")
        self.logs.extend([out, err])
        self.process = subprocess.Popen(command, stdout=out, stderr=err, env=env)
        deadline = time.monotonic() + 20
        while self.process.poll() is None and time.monotonic() < deadline:
            response = subprocess.run(
                self.cli + ["info", "--format", "{{json .}}"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if response.returncode == 0:
                info = json.loads(response.stdout)
                save(
                    self.root / "docker-info.json",
                    {
                        key: info.get(key)
                        for key in (
                            "CgroupDriver",
                            "CgroupVersion",
                            "Driver",
                            "SecurityOptions",
                            "ServerVersion",
                            "DockerRootDir",
                            "MemoryLimit",
                            "SwapLimit",
                            "CpuCfsQuota",
                            "Cpuset",
                        )
                    },
                )
                if (
                    info["CgroupDriver"] != "systemd"
                    or info["CgroupVersion"] != "2"
                    or "name=rootless" not in info["SecurityOptions"]
                    or info["Driver"] != "overlayfs"
                ):
                    raise ValueError("Docker rootless/systemd/v2/overlayfs capabilities differ")
                return json.loads((self.root / "docker-info.json").read_text())
            time.sleep(0.1)
        raise RuntimeError("private Docker daemon failed to become ready")

    def inspect(self, identity):
        return json.loads(self.call(["inspect", identity], text=True).stdout)[0]

    def create(self, cfg, identity, image, error):
        library = "library_variant" in cfg
        oracle = (
            [
                "/usr/bin/python3",
                "-B",
                "-u",
                "/library-worker.py",
                "random-shared",
                str(identity),
                "rootfs",
            ]
            if library
            else ["/worker", "--native", cfg["pattern"], str(identity)]
        )
        command = [
            "create",
            "--name",
            "memory-" + str(identity),
            "--cgroup-parent=" + self.slice_unit,
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=256m",
            "--memory-swap=256m",
            "--cpus=1",
            "--pids-limit=16",
            "-i",
            image,
            "/memory-init",
            *oracle,
        ]
        cid = self.call(command, text=True).stdout.strip()
        self.containers.append(cid)
        client = subprocess.Popen(
            self.cli + ["start", "-ai", cid],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=error,
        )
        self.clients.append(client)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            item = self.inspect(cid)
            if item["State"]["Running"]:
                pid = item["State"]["Pid"]
                proc = Path("/proc") / str(pid)
                # RootlessKit uses the host PID namespace here. Fail if it changes.
                if not proc.exists():
                    raise ValueError("Docker container PID is not observable on host")
                save(self.root / f"docker-{identity}-created.json", item)
                return ContainerProcess(self, cid, client, pid), self.cli + command
            if client.poll() is not None:
                raise RuntimeError("Docker container failed before running")
            time.sleep(0.02)
        raise TimeoutError("Docker container start deadline")

    def stop(self):
        errors = []
        for cid in self.containers:
            try:
                self.call(["rm", "-f", cid], timeout=10)
            except Exception as error:
                errors.append(str(error))
        for client in self.clients:
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)
                errors.append("attach client shutdown deadline")
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                errors.append("daemon shutdown deadline")
        for stream in self.logs:
            stream.close()
        save(self.root / "docker-cleanup.json", dict(errors=errors, containers=self.containers))
        return not errors


class ContainerProcess:
    """Expose the oracle's host PID while retaining the Docker attach transport."""

    def __init__(self, engine, cid, client, pid):
        self.engine, self.cid, self.client, self.pid = engine, cid, client, pid
        self.stdin, self.stdout = client.stdin, client.stdout

    def poll(self):
        return self.client.poll()

    def wait(self, timeout):
        code = self.client.wait(timeout=timeout)
        item = self.engine.inspect(self.cid)
        save(self.engine.root / ("docker-" + self.cid + "-exit.json"), item)
        if (
            item["State"]["Running"]
            or item["State"]["OOMKilled"]
            or item["State"]["ExitCode"] != code
        ):
            raise ValueError("Docker native exit/transport/OOM evidence mismatch")
        return code

    def kill(self):
        if self.engine.inspect(self.cid)["State"]["Running"]:
            try:
                self.engine.call(["kill", self.cid], timeout=10)
            except subprocess.CalledProcessError:
                if self.engine.inspect(self.cid)["State"]["Running"]:
                    raise


def verify_inputs(root, receipt):
    from prepare_pool import tree_manifest

    if any(digest(root / name) != value for name, value in receipt["hashes"].items()):
        raise ValueError("frozen Docker inputs changed")
    if tree_manifest(root / "seed") != json.loads((root / "seed-manifest.json").read_text()):
        raise ValueError("frozen Docker image-store seed changed")
    for name, value in receipt["runtime_sha256"].items():
        if digest(Path(name)) != value:
            raise ValueError("Docker runtime binary changed")


def prepare_case(inputs, root):
    receipt = json.loads((inputs / "receipt.json").read_text())
    verify_inputs(inputs, receipt)
    # Reflinks keep per-condition image metadata private without a full disk copy.
    subprocess.run(
        ["cp", "-a", "--reflink=always", str(inputs / "seed"), str(root / "docker-data")],
        check=True,
        capture_output=True,
        timeout=60,
    )
    return receipt


def validate_case(raw, cfg, root):
    """Require actual container identity, leaf limits and complete runtime scope."""
    receipt = json.loads((Path(cfg["docker_inputs"]) / "receipt.json").read_text())
    if raw.get("docker_image_id") != receipt["image_id"]:
        raise ValueError("Docker image identity mismatch")
    info = raw.get("docker_info", {})
    if (
        info.get("CgroupDriver") != "systemd"
        or info.get("CgroupVersion") != "2"
        or info.get("Driver") != "overlayfs"
        or "name=rootless" not in info.get("SecurityOptions", [])
    ):
        raise ValueError("Docker engine capabilities missing")
    group = cfg["accounting_group"]
    pids = []
    for n in range(1, 5):
        item = json.loads((root / f"docker-{n}-created.json").read_text())
        host = item["HostConfig"]
        if (
            item["Image"] != receipt["image_id"]
            or not item["State"]["Running"]
            or host["CgroupParent"] != cfg["docker_slice"]
            or host["Memory"] != 256 * 1024**2
            or host["MemorySwap"] != 256 * 1024**2
            or host["NanoCpus"] != 10**9
            or host["PidsLimit"] != 16
            or not host["ReadonlyRootfs"]
            or host["NetworkMode"] != "none"
            or host["CapDrop"] != ["ALL"]
            or not any(s.startswith("no-new-privileges") for s in host["SecurityOpt"])
            or item["Config"]["Cmd"][0] != "/memory-init"
        ):
            raise ValueError("Docker installed container configuration mismatch")
        pids.append(item["State"]["Pid"])
        exited = json.loads((root / ("docker-" + item["Id"] + "-exit.json")).read_text())
        if (
            exited["Id"] != item["Id"]
            or exited["State"]["Running"]
            or exited["State"]["OOMKilled"]
            or exited["State"]["ExitCode"] != 0
        ):
            raise ValueError("Docker native exit missing")
    for phase in [raw["before"], *raw["phases"], raw["after"]]:
        if phase["cgroup"] != group:
            raise ValueError("Docker accounting escaped full slice")
        if phase in raw["phases"]:
            if phase["guest_pids"] != pids:
                raise ValueError("Docker host PID/inspection mismatch")
            if not {"dockerd", "containerd"} <= {p["comm"] for p in phase["processes"]}:
                raise ValueError("Docker runtime omitted from group accounting")
            processes = {p["pid"]: p for p in phase["processes"]}
            for pid in pids:
                limits = processes[pid].get("container_limits", {})
                quota = limits.get("cpu.max", "").split()
                if (
                    limits.get("memory.max") != str(256 * 1024**2)
                    or limits.get("memory.swap.max") != "0"
                    or limits.get("memory.swap.current") != "0"
                    or limits.get("pids.max") != "16"
                    or len(quota) != 2
                    or quota[0] != quota[1]
                ):
                    raise ValueError("Docker actual leaf resource limits mismatch")
    if json.loads((root / "docker-cleanup.json").read_text())["errors"]:
        raise ValueError("Docker cleanup incomplete")
