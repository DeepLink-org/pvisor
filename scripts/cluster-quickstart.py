#!/usr/bin/env python3
"""Prepare, run and verify a bounded Linux Cluster quickstart with real binaries."""

import argparse
import copy
import hashlib
import json
import os
import re
import secrets
import shlex
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIB = 1024 * 1024
TERMINAL = {"succeeded", "failed", "cancelled", "lost", "suspended"}


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def execute(argv, **kwargs):
    result = subprocess.run(argv, text=True, capture_output=True, timeout=20, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{argv[0]} {argv[1:3]}: {result.stderr.strip()}")
    return result.stdout


def prepare(state, backend, bin_dir, firmware_dir=None, gateway=False):
    if gateway and backend != "host":
        raise ValueError(
            "the offline Gateway quickstart uses --backend host; offline VM snapshot checks use a separate profile"
        )
    if backend == "vm" and (
        firmware_dir is None or not (firmware_dir / "libkrunfw.so.5").is_file()
    ):
        raise ValueError("VM quickstart requires --firmware-dir containing libkrunfw.so.5")
    state.mkdir(parents=True, exist_ok=False, mode=0o700)
    for directory in ["workspace", "inputs", "downloads"]:
        (state / directory).mkdir()
    # Native VM reentry must use the same executable even if Cargo rebuilds it.
    stable_bin = state / "bin"
    stable_bin.mkdir()
    for name in ["pvisor-cluster", "pvisor-worker"]:
        shutil.copy2(bin_dir / name, stable_bin / name)
    bin_dir = stable_bin
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    config = {
        "backend": backend,
        "bin_dir": str(bin_dir),
        "url": f"http://127.0.0.1:{port}",
        "admin": secrets.token_hex(24),
        "worker": secrets.token_hex(24),
        "prefix": f"pvisor-quickstart-{secrets.token_hex(6)}",
        "state": str(state),
        "gateway": gateway,
    }
    if gateway:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            config["model_url"] = f"http://127.0.0.1:{listener.getsockname()[1]}"
        config["model_key"] = secrets.token_hex(24)
    write_json(state / "session.json", config)
    env = {
        "PVISOR_CLUSTER_URL": config["url"],
        "PVISOR_CLUSTER_TOKEN": config["admin"],
        "PVISOR_CLUSTER_WORKER_TOKEN": config["worker"],
        "QS_STATE": str(state),
        "QS_BIN": str(bin_dir),
        "TOKIO_WORKER_THREADS": "2",
    }
    (state / "env.sh").write_text("".join(f"export {k}={shlex.quote(v)}\n" for k, v in env.items()))
    for private in ["session.json", "env.sh"]:
        (state / private).chmod(0o600)
    profile = '[overlaynet]\nmode = "off"\npolicy = "deny"\n'
    if gateway:
        profile = (
            '[overlaynet]\nmode = "auto"\npolicy = "deny"\n\n'
            '[gateway]\nenabled = true\nlevel = "dialogue"\n\n'
            '[[gateway.routes]]\nname = "quickstart-model"\n'
            f"upstream = {json.dumps(config['model_url'] + '/v1')}\n"
            'api_key_env = "PVISOR_QS_MODEL_KEY"\n\n'
            '[[gateway.routes]]\nname = "*"\nforward = "quickstart-model"\n'
        )
    if backend == "vm":
        rootfs = state / "rootfs"
        rootfs.mkdir()
        for program in ["/bin/sh", "/bin/sleep"]:
            libraries = re.findall(r"(?<!\S)/[^\s()]+", execute(["ldd", program]))
            for source in [program, *libraries]:
                target = rootfs / source.lstrip("/")
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
                shutil.copymode(source, target)
        for directory in ["tmp", "proc", "sys", "dev", "root", "etc"]:
            (rootfs / directory).mkdir(exist_ok=True)
        profile = f"lower_layers = [{json.dumps(str(rootfs))}]\n" + profile
        profile += f"\n[vm]\nrootfs = {json.dumps(str(rootfs))}\nlibrary_dir = {json.dumps(str(firmware_dir.resolve()))}\n"
    (state / "worker.toml").write_text(profile)
    memory = (128 if backend == "vm" else 64) * MIB
    task = {
        "version": 1,
        "id": "hello",
        "tenant": "quickstart",
        "run": {
            "run_id": "hello",
            "agent": {"name": "shell"},
            "invocation": {
                "kind": "process",
                "program": "/bin/sh",
                "args": ["-c", "printf 'hello from Cluster\\n'"],
                "inherit_env": False,
                "stdin": "null",
                "stdout": "capture",
                "stderr": "capture",
            },
            "runtime": {
                "timeout_ms": 60000,
                "max_output_bytes": 4096,
                "resource_limits": {
                    "memory_bytes": memory,
                    "cpu_time_ms": 2000,
                    "open_files": 256,
                    "file_size_bytes": MIB,
                },
            },
        },
        "execution": {
            "executor": "virtual_machine" if backend == "vm" else "process",
            "isolation": "virtual_machine" if backend == "vm" else "host_process",
        },
        "resources": {"slots": 1, "memory_bytes": memory, "cpu_millis": 500},
        "labels": {"quickstart-node": "a"},
        "retain_artifacts": {"version": 1, "trace": True, "workspace_upper": backend == "vm"},
    }
    if backend == "host":
        task["run"]["invocation"]["cwd"] = str(state / "workspace")

    def variant(id, command, node="a"):
        value = copy.deepcopy(task)
        value["id"] = value["run"]["run_id"] = id
        value["run"]["invocation"]["args"][1] = command
        value["labels"]["quickstart-node"] = node
        write_json(state / "inputs" / f"{id}.json", value)
        return value

    write_json(state / "inputs/hello.json", task)
    if gateway:
        agent = """import json, os, urllib.request, urllib.error
assert all(k not in os.environ for k in ("PVISOR_CLUSTER_TOKEN", "PVISOR_CLUSTER_WORKER_TOKEN", "PVISOR_QS_MODEL_KEY"))
assert os.environ["OPENAI_API_KEY"].startswith("pvisor-local-")
def call(model):
    request = urllib.request.Request(os.environ["OPENAI_BASE_URL"].rstrip("/") + "/chat/completions", data=json.dumps({"model": model, "messages": [{"role": "user", "content": "Say hello"}], "stream": False}).encode(), headers={"Content-Type": "application/json", "Authorization": "Bearer " + os.environ["OPENAI_API_KEY"]})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)
assert call("quickstart-model")["choices"][0]["message"]["content"] == "hello from offline model"
try:
    call("forbidden-model")
except urllib.error.HTTPError as error:
    assert error.code == 403
else:
    raise AssertionError("unauthorized model was accepted")
print("Gateway request passed; unauthorized model denied; credentials isolated")
"""
        (state / "agent.py").write_text(agent)
        agent_task = copy.deepcopy(task)
        agent_task["id"] = agent_task["run"]["run_id"] = "gateway-agent"
        agent_task["run"]["invocation"].update(
            {"program": "/usr/bin/python3", "args": [str(state / "agent.py")]}
        )
        agent_task["run"]["capabilities"] = {"models": ["quickstart-model"]}
        agent_task["gateway"] = {"version": 1, "level": "dialogue", "models": ["quickstart-model"]}
        write_json(state / "inputs/gateway-agent.json", agent_task)
    variant("other-node", "printf 'node-b\\n'", "b")
    variant("cancel-me", "/bin/sleep 40")
    variant("drained", "printf 'resumed admission\\n'")
    if backend == "host":
        marker = shlex.quote(str(state / "workspace/once.txt"))
        release = shlex.quote(str(state / "workspace/release"))
        command = f"printf 'once\\n' >> {marker}; while [ ! -e {release} ]; do /bin/sleep 0.1; done; printf 'survived restart\\n'"
    else:
        command = "token=survived; /bin/sleep 8; printf '%s restart\\n' \"$token\""
    variant("restart-me", command)
    controls = variant(
        "vm-controls", "token=snapshot-value; /bin/sleep 240; printf '%s\\n' \"$token\""
    )
    controls["run"]["runtime"]["timeout_ms"] = 300000
    write_json(state / "inputs/vm-controls.json", controls)
    nodes = []
    for i in range(3):
        id = f"dag-{i + 1}"
        node = variant(id, f"printf 'step-{i + 1}\\n'")
        nodes.append({"task": node, "depends_on": [] if not i else [f"dag-{i}"]})
    write_json(
        state / "inputs/graph.json",
        {"version": 1, "id": "quickstart-dag", "tenant": "quickstart", "nodes": nodes},
    )
    write_json(
        state / "inputs/fork.json",
        {
            "version": 1,
            "request_id": "fork-1",
            "checkpoint_request_id": "suspend-1",
            "branches": [{"task_id": "vm-child", "run_id": "vm-child"}],
        },
    )
    write_json(
        state / "quotas.json",
        {"quickstart": {"slots": 2, "memory_bytes": 256 * MIB, "cpu_millis": 1000}},
    )
    write_json(
        state / "artifact-limits.json", {"version": 1, "max_bytes": 128 * MIB, "max_objects": 4096}
    )
    return config


class Session:
    def __init__(self, state):
        self.state = state
        self.config = json.loads((state / "session.json").read_text())
        self.env = {
            **os.environ,
            "PVISOR_CLUSTER_URL": self.config["url"],
            "PVISOR_CLUSTER_TOKEN": self.config["admin"],
            "PVISOR_CLUSTER_WORKER_TOKEN": self.config["worker"],
            "TOKIO_WORKER_THREADS": "2",
            "NO_PROXY": "127.0.0.1,localhost",
        }
        self.checks = []
        self.measurements = {}
        if self.config.get("gateway"):
            self.env["PVISOR_QS_MODEL_KEY"] = self.config["model_key"]

    def unit(self, role):
        prefix = self.config["prefix"]
        if not re.fullmatch(r"pvisor-quickstart-[0-9a-f]{12}", prefix):
            raise ValueError("invalid quickstart unit prefix")
        return f"{prefix}-{role}.service"

    def ctl(self, *args):
        return json.loads(
            execute([str(Path(self.config["bin_dir"]) / "pvisor-cluster"), *args], env=self.env)
        )

    def api(self, path):
        request = urllib.request.Request(
            self.config["url"] + path, headers={"Authorization": "Bearer " + self.config["admin"]}
        )
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(
            request, timeout=3
        ) as response:
            return json.load(response)

    def until(self, callback, timeout=60):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                last = callback()
                if last:
                    return last
            except (urllib.error.URLError, RuntimeError) as error:
                last = str(error)
            time.sleep(0.2)
        raise TimeoutError(f"condition not reached: {last}")

    def wait(self, id, phase="terminal"):
        def current():
            record = self.api(f"/v1/tasks/{id}")
            if phase != "terminal" and record["phase"] in TERMINAL and record["phase"] != phase:
                raise ValueError(f"{id} ended before {phase}: {record}")
            return (
                record
                if (
                    record["phase"] in TERMINAL if phase == "terminal" else record["phase"] == phase
                )
                else None
            )

        return self.until(current)

    def launch(self, role):
        binary = Path(self.config["bin_dir"])
        if role == "controller":
            arguments = [
                str(binary / "pvisor-cluster"),
                "serve",
                "--listen",
                self.config["url"].removeprefix("http://"),
                "--journal",
                str(self.state / "journal"),
                "--lease-ms",
                "30000",
                "--quotas",
                str(self.state / "quotas.json"),
                "--max-journal-bytes",
                str(64 * MIB),
                "--max-artifact-bytes",
                str(128 * MIB),
                "--artifact-limits",
                str(self.state / "artifact-limits.json"),
            ]
            memory, cpu = 256 * MIB, "25%"
        elif role == "model":
            arguments = [
                sys.executable,
                str(Path(__file__).resolve()),
                "mock-model",
                "--state",
                str(self.state),
            ]
            memory, cpu = 128 * MIB, "10%"
        else:
            node = role[-1]
            arguments = [
                str(binary / "pvisor-worker"),
                "--id",
                f"qs-{node}",
                "--state",
                str(self.state / role),
                "--backend",
                self.config["backend"],
                "--config",
                str(self.state / "worker.toml"),
                "--slots",
                "1",
                "--memory-bytes",
                str(128 * MIB),
                "--cpu-millis",
                "500",
                "--poll-ms",
                "200",
                "--label",
                f"quickstart-node={node}",
            ]
            memory, cpu = 512 * MIB, "50%"
        execute(
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--service-type=exec",
                f"--unit={self.unit(role)}",
                "--property=MemoryAccounting=yes",
                "--property=CPUAccounting=yes",
                f"--property=MemoryMax={memory}",
                "--property=MemorySwapMax=0",
                f"--property=CPUQuota={cpu}",
                "--property=TasksMax=128",
                "--property=TimeoutStopSec=10",
                f"--property=WorkingDirectory={self.state / 'workspace'}",
                *[
                    f"--setenv={k}={self.env[k]}"
                    for k in (
                        "PVISOR_CLUSTER_URL",
                        "PVISOR_CLUSTER_TOKEN",
                        "PVISOR_CLUSTER_WORKER_TOKEN",
                        "TOKIO_WORKER_THREADS",
                        "NO_PROXY",
                        "PVISOR_QS_MODEL_KEY",
                    )
                    if k in self.env
                ],
                *arguments,
            ],
            env=self.env,
        )

    def start(self):
        if (
            sys.platform != "linux"
            or execute(["systemctl", "--user", "is-system-running"]).strip() != "running"
        ):
            raise RuntimeError(
                "a running Linux user systemd manager is required for hard resource caps"
            )
        available = (
            int(re.search(r"MemAvailable:\s+(\d+)", Path("/proc/meminfo").read_text())[1]) * 1024
        )
        if available < 2 * 1024**3:
            raise RuntimeError("need at least 2 GiB MemAvailable; no workloads started")
        if self.config["backend"] == "vm":
            for device in ["/dev/kvm", "/dev/fuse"]:
                if not os.access(device, os.R_OK | os.W_OK):
                    raise RuntimeError(f"need read/write access to {device}")
        for role in (["model"] if self.config.get("gateway") else []) + [
            "controller",
            "worker-a",
            "worker-b",
        ]:
            if role == "controller":
                self.launch(role)
                self.until(lambda: self.api("/health"), timeout=10)
            else:
                self.launch(role)
        self.until(lambda: len(self.ctl("workers")) == 2, timeout=15)

    def stop(self, role=None):
        for name in [role] if role else ["worker-a", "worker-b", "controller", "model"]:
            subprocess.run(
                ["systemctl", "--user", "stop", self.unit(name)], capture_output=True, timeout=20
            )

    def sample(self):
        for role in ["controller", "worker-a", "worker-b"] + (
            ["model"] if self.config.get("gateway") else []
        ):
            raw = execute(
                [
                    "systemctl",
                    "--user",
                    "show",
                    self.unit(role),
                    "--property=MemoryPeak,MemoryMax,CPUQuotaPerSecUSec,CPUUsageNSec,ControlGroup",
                ]
            )
            self.measurements[role] = dict(
                line.split("=", 1) for line in raw.splitlines() if "=" in line
            )
            expected = (
                256 * MIB if role == "controller" else 128 * MIB if role == "model" else 512 * MIB
            )
            assert int(self.measurements[role]["MemoryMax"]) == expected
            group = Path("/sys/fs/cgroup") / self.measurements[role]["ControlGroup"].lstrip("/")
            assert (group / "memory.max").read_text().strip() == str(expected)
            quota, period = map(int, (group / "cpu.max").read_text().split())
            assert quota / period == (
                0.25 if role == "controller" else 0.1 if role == "model" else 0.5
            )
            assert (group / "memory.swap.max").read_text().strip() == "0"
            events = dict(
                line.split() for line in (group / "memory.events").read_text().splitlines()
            )
            assert events["oom"] == events["oom_kill"] == "0"
            self.measurements[role]["memory_events"] = events

    def check(self, name):
        self.checks.append(name)
        print(f"PASS {name}", flush=True)

    def submit(self, id):
        return self.ctl("submit", str(self.state / f"inputs/{id}.json"))

    def verify(self):
        self.start()
        self.sample()
        self.check("kernel memory/CPU/swap caps")
        for id, output in [("hello", "hello from Cluster\n"), ("other-node", "node-b\n")]:
            self.submit(id)
            record = self.wait(id)
            assert record["phase"] == "succeeded", record
            assert record["result"]["output"]["stdout"] == output, record
            assert record["lease"]["key"]["worker_id"] == ("qs-a" if id == "hello" else "qs-b")
        self.check("real execution and two-node capability placement")
        key = self.ctl("show", "hello")["lease"]["key"]
        assert self.submit("hello")["lease"]["key"] == key
        self.check("immutable submission idempotency")
        self.ctl("artifacts", "hello", "--out", str(self.state / "downloads/hello"))
        assert (self.state / "downloads/hello/run-bundle.json").is_file()
        assert (self.state / "downloads/hello/trace").is_file()
        self.check("verified Bundle and trace download")
        self.ctl("graph", "submit", str(self.state / "inputs/graph.json"))
        for i in range(3):
            assert self.wait(f"dag-{i + 1}")["phase"] == "succeeded"
        assert self.ctl("graph", "show", "quickstart-dag")["phase"] == "succeeded"
        self.check("three-node dependency graph")
        self.submit("cancel-me")
        self.wait("cancel-me", "running")
        self.ctl("cancel", "cancel-me")
        assert self.wait("cancel-me")["phase"] == "cancelled"
        self.check("running cancellation")
        self.ctl("drain", "qs-a")
        self.submit("drained")
        time.sleep(1)
        assert self.ctl("show", "drained")["phase"] == "queued"
        self.ctl("drain", "qs-a", "--resume")
        assert self.wait("drained")["phase"] == "succeeded"
        self.check("drain and resumed admission")
        if self.config.get("gateway"):
            self.submit("gateway-agent")
            agent = self.wait("gateway-agent")
            assert agent["phase"] == "succeeded", agent
            assert "credentials isolated" in agent["result"]["output"]["stdout"]
            self.ctl(
                "artifacts", "gateway-agent", "--out", str(self.state / "downloads/gateway-agent")
            )
            assert len((self.state / "model-requests.jsonl").read_text().splitlines()) == 1
            self.check("Attempt Gateway model request, forbidden model and credential isolation")
        self.submit("restart-me")
        original = self.wait("restart-me", "running")["lease"]["key"]
        if self.config["backend"] == "host":
            self.until(lambda: (self.state / "workspace/once.txt").exists())
        self.until(
            lambda: all(
                w["reserved"]["slots"] == (1 if w["registration"]["id"] == "qs-a" else 0)
                for w in self.ctl("workers")
            )
        )
        time.sleep(0.5)
        size = (self.state / "journal").stat().st_size
        time.sleep(1)
        assert (self.state / "journal").stat().st_size == size
        execute(
            [
                "systemctl",
                "--user",
                "kill",
                "--kill-whom=main",
                "--signal=SIGSTOP",
                self.unit("worker-a"),
            ]
        )
        try:
            self.stop("controller")
            self.launch("controller")
            self.until(lambda: self.api("/health"), timeout=10)
            assert self.ctl("show", "restart-me")["reconciliation_pending"]
        finally:
            execute(
                [
                    "systemctl",
                    "--user",
                    "kill",
                    "--kill-whom=main",
                    "--signal=SIGCONT",
                    self.unit("worker-a"),
                ]
            )
        self.until(lambda: not self.ctl("show", "restart-me")["reconciliation_pending"])
        if self.config["backend"] == "host":
            (self.state / "workspace/release").touch()
        recovered = self.wait("restart-me")
        assert recovered["phase"] == "succeeded", recovered
        assert recovered["lease"]["key"] == original
        if self.config["backend"] == "host":
            assert (self.state / "workspace/once.txt").read_text() == "once\n"
        self.check("no heartbeat writes; same-key Controller restart reconciliation")
        if self.config["backend"] == "vm":
            self.submit("vm-controls")
            self.wait("vm-controls", "running")
            for action in ["pause", "offload", "resume", "suspend"]:
                request = f"{action}-1"
                self.ctl("control", "vm-controls", action, "--request-id", request)

                def observed():
                    controls = self.ctl("show", "vm-controls")["controls"]
                    return next(
                        (
                            c
                            for c in controls
                            if c["command"]["request"]["request_id"] == request
                            and c["phase"] in {"succeeded", "failed", "aborted"}
                        ),
                        None,
                    )

                control = self.until(observed)
                assert control["phase"] == "succeeded", control
            assert self.wait("vm-controls")["phase"] == "suspended"
            self.ctl("fork", "vm-controls", str(self.state / "inputs/fork.json"))
            self.wait("vm-child", "running")
            self.ctl("control", "vm-child", "pause", "--request-id", "child-pause")

            def child_paused():
                record = self.ctl("show", "vm-child")
                assert record["phase"] not in TERMINAL, record
                return record["phase"] == "paused"

            self.until(child_paused)
            self.ctl("cancel", "vm-child")
            assert self.wait("vm-child")["phase"] == "cancelled"
            self.check("native VM pause/offload/resume/suspend and sealed-fork restore")
        self.until(lambda: all(w["reserved"]["slots"] == 0 for w in self.ctl("workers")))
        self.sample()
        self.check("no cgroup OOM; execution reservations released")
        limits = self.ctl("artifact-storage")["limits"]
        assert (
            self.ctl("artifact-storage", "--limits", str(self.state / "artifact-limits.json"))[
                "limits"
            ]
            == limits
        )
        plan = self.ctl("artifact-gc", "--retire-before-ms", str(int(time.time() * 1000) + 1))
        assert any(entry["task_id"] == "hello" for entry in plan["retire"]), plan
        self.ctl("artifact-gc", "--apply", plan["id"])
        try:
            self.api("/v1/tasks/hello/artifacts")
        except urllib.error.HTTPError as error:
            assert error.code == 410
        else:
            raise AssertionError("retired evidence is still downloadable")
        assert self.ctl("show", "hello")["phase"] == "succeeded"
        self.check("online storage policy, evidence retirement and GC")
        report = {
            "schema": "pvisor-cluster-quickstart/v1",
            "passed": True,
            "backend": self.config["backend"],
            "verified_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "max_execution_concurrency": 2,
            "task_memory_bytes": (128 if self.config["backend"] == "vm" else 64) * MIB,
            "task_cpu_time_ms": 2000,
            "gateway": self.config.get("gateway", False),
            "checks": self.checks,
            "cgroups": self.measurements,
            "binaries": {
                name: hashlib.sha256((Path(self.config["bin_dir"]) / name).read_bytes()).hexdigest()
                for name in ["pvisor-cluster", "pvisor-worker"]
            },
        }
        write_json(self.state / "report.json", report)
        print(f"Report: {self.state / 'report.json'}", flush=True)


def mock_model(session):
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path == "/v1/chat/completions"
            assert self.headers["Authorization"] == "Bearer " + session.config["model_key"]
            data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert data["model"] == "quickstart-model"
            with (session.state / "model-requests.jsonl").open("a") as output:
                output.write(json.dumps({"model": data["model"], "auth_ok": True}) + "\n")
            reply = json.dumps(
                {
                    "id": "quickstart-reply",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "quickstart-model",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "hello from offline model"},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(reply)))
            self.end_headers()
            self.wfile.write(reply)

        def log_message(self, *_):
            pass

    port = int(session.config["model_url"].rsplit(":", 1)[1])
    HTTPServer(("127.0.0.1", port), Handler).serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=[
            "prepare",
            "start",
            "stop",
            "verify",
            "wait",
            "status",
            "limits",
            "restart-controller",
            "mock-model",
        ],
    )
    parser.add_argument(
        "--state", type=Path, required=True, help="new private directory outside the checkout"
    )
    parser.add_argument("--backend", choices=["host", "vm"], default="host")
    parser.add_argument(
        "--firmware-dir", type=Path, help="Linux x86-64 directory containing libkrunfw.so.5"
    )
    parser.add_argument(
        "--gateway",
        action="store_true",
        help="include an offline model and real Attempt Gateway calls (host only)",
    )
    parser.add_argument(
        "--bin-dir",
        type=Path,
        default=Path(os.getenv("CARGO_TARGET_DIR", ROOT / "target")) / "debug",
    )
    parser.add_argument("--task")
    parser.add_argument("--phase", default="terminal")
    args = parser.parse_args()
    state = args.state.resolve()
    if state == ROOT or ROOT in state.parents:
        parser.error("--state must be outside the checkout")
    if args.command in {"prepare", "verify"}:
        prepare(state, args.backend, args.bin_dir.resolve(), args.firmware_dir, args.gateway)
    session = Session(state)
    if args.command == "verify":
        try:
            session.verify()
        finally:
            session.stop()
    elif args.command == "prepare":
        print(f"Prepared {state}; source {state / 'env.sh'}")
    elif args.command == "start":
        try:
            session.start()
        except BaseException:
            session.stop()
            raise
        print(f"Started two 1-slot Workers; use stop --state {state} to clean up")
    elif args.command == "stop":
        session.stop()
    elif args.command == "status":
        print(json.dumps(session.ctl("workers"), indent=2))
    elif args.command == "limits":
        session.sample()
        print(json.dumps(session.measurements, indent=2))
    elif args.command == "restart-controller":
        session.stop("controller")
        session.launch("controller")
        session.until(lambda: session.api("/health"), timeout=10)
        print("Controller restarted with the same journal; Workers reconcile by poll")
    elif args.command == "mock-model":
        mock_model(session)
    elif args.command == "wait":
        if not args.task:
            parser.error("wait requires --task")
        print(json.dumps(session.wait(args.task, args.phase), indent=2))


if __name__ == "__main__":
    main()
