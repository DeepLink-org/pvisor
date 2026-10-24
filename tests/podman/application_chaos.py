#!/usr/bin/env python3
"""Opt-in Linux application chaos for the supported Docker-style CLI profile."""

import argparse
import concurrent.futures
import errno
import hashlib
import http.client
import json
import os
import random
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import uuid
from contextlib import contextmanager
from pathlib import Path

LIMIT = 512 * 1024**2
IMAGE = "mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e"


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def wait_until(check, seconds=10):
    deadline = time.monotonic() + seconds
    while not check():
        if time.monotonic() >= deadline:
            raise TimeoutError("readiness or teardown deadline expired")
        time.sleep(0.025)


def scope_state(path):
    def fields(name):
        return {
            k: int(v) for k, v in (line.split() for line in (path / name).read_text().splitlines())
        }

    return {
        "memory": int((path / "memory.current").read_text()),
        "peak": int((path / "memory.peak").read_text()),
        "swap": int((path / "memory.swap.current").read_text()),
        "events": fields("memory.events"),
        "cpu": fields("cpu.stat"),
        "pids": (path / "cgroup.procs").read_text().split(),
    }


def verify_state(state):
    assert state["peak"] <= LIMIT and state["swap"] == 0, state
    assert all(state["events"].get(k, 0) == 0 for k in ("oom", "oom_kill", "max")), state


def maybe_scope_state(path):
    try:
        return scope_state(path)
    except OSError as error:
        # An already-open cgroup file can return ENODEV after systemd removes
        # the scope. Permission and all other I/O errors remain failures.
        if error.errno not in (errno.ENOENT, errno.ENODEV):
            raise
        return None


def scope_helper(arguments):
    output, binary, *command = arguments
    relative = Path("/proc/self/cgroup").read_text().strip().split(":", 2)[2]
    path = Path("/sys/fs/cgroup") / relative.lstrip("/")
    limits = {
        name: (path / name).read_text().strip()
        for name in ("memory.max", "memory.swap.max", "cpu.max", "pids.max")
    }
    assert int(limits["memory.max"]) == LIMIT and limits["memory.swap.max"] == "0", limits
    quota, period = map(int, limits["cpu.max"].split())
    assert quota == period and limits["pids.max"] == "256", limits
    assert sorted(os.sched_getaffinity(0)) == [4, 5, 6, 7]
    save(Path(output), {"cgroup": str(path), "limits": limits, "initial": scope_state(path)})
    os.execv(binary, [binary, *command])


def start_race(binary, name):
    gate = threading.Barrier(4)

    def one(_):
        gate.wait()
        result = subprocess.run([binary, "start", name], capture_output=True, timeout=40)
        return {
            "code": result.returncode,
            "stdout": result.stdout.decode(),
            "stderr": result.stderr.decode(),
        }

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        results = list(pool.map(one, range(4)))
    print(json.dumps(results), flush=True)
    assert all(r == {"code": 0, "stdout": name + "\n", "stderr": ""} for r in results), results


class ApplicationChaos:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.store = self.output / "store"
        self.env = dict(os.environ, PVISOR_DOCKER_HOME=str(self.store))
        self.rng = random.Random(args.seed)
        self.prefix = "app-" + uuid.uuid4().hex[:8]
        self.web, self.client, self.observer, self.spare = [
            self.prefix + "-" + role for role in ("web", "client", "observer", "spare")
        ]
        self.names = [self.web, self.client, self.observer, self.spare]
        self.network, self.isolated = self.prefix + "-net", self.prefix + "-isolated"
        self.volume = self.prefix + "-data"
        self.image = self.prefix + ":app"
        self.expected = b"initial\n"
        self.allowed_hashes = {hashlib.sha256(self.expected).hexdigest()}
        self.faulting = False
        self.fault_epoch = 0
        self.web_version = self.tag_version = b"version-1\n"
        self.port = None
        self.serial = 0
        self.lock = threading.Lock()
        self.active = {}
        self.units = []
        self.stop_monitor = threading.Event()
        self.monitor_error = []
        self.counts = {}
        self.step = "setup"
        self.deadline = time.monotonic() + args.seconds

    def call(self, *arguments, code=0, data=b"", scoped=None, timeout=45, allow_missing=False):
        with self.lock:
            number = self.serial
            self.serial += 1
        directory = self.output / "commands" / f"{number:05d}"
        directory.mkdir(parents=True)
        command = [self.args.binary, *arguments]
        if scoped is not None:
            unit = f"{self.prefix}-{number}"
            self.units.append(unit + ".scope")
            command = [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                f"--unit={unit}",
                "-p",
                "MemoryMax=512M",
                "-p",
                "MemorySwapMax=0",
                "-p",
                "CPUQuota=100%",
                "-p",
                "TasksMax=256",
                "taskset",
                "-c",
                "4-7",
                sys.executable,
                str(Path(__file__).resolve()),
                "_scope",
                str(directory / "scope.json"),
                *command,
            ]
        save(
            directory / "command.json",
            {
                "arguments": arguments,
                "step": self.step,
                "scope_role": scoped,
                "stdin_bytes": len(data),
                "stdin_sha256": hashlib.sha256(data).hexdigest(),
            },
        )
        started = time.monotonic()
        child = subprocess.Popen(
            command,
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        timed_out = False
        try:
            stdout, stderr = child.communicate(data, timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(child.pid, signal.SIGKILL)
            stdout, stderr = child.communicate()
        (directory / "stdout").write_bytes(stdout)
        (directory / "stderr").write_bytes(stderr)
        save(
            directory / "exit.json",
            {
                "code": child.returncode,
                "seconds": time.monotonic() - started,
                "timed_out": timed_out,
            },
        )
        missing = allow_missing and b"no such container" in stderr.lower()
        if timed_out or (code is not None and child.returncode != code and not missing):
            raise RuntimeError(
                f"{arguments}: exit {child.returncode}, expected {code}, timed_out={timed_out}; evidence {directory}; stderr={stderr[-2000:]!r}"
            )
        if scoped is not None and (directory / "scope.json").exists():
            state = json.loads((directory / "scope.json").read_text())
            path = Path(state["cgroup"])
            state = maybe_scope_state(path)
            if state is not None and state["pids"]:
                verify_state(state)
                with self.lock:
                    self.active[scoped] = path
        return stdout, stderr

    def parallel(self, *operations):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            pending = [pool.submit(operation) for operation in operations]
            errors = []
            results = []
            for result in pending:
                try:
                    results.append(result.result())
                except BaseException as error:
                    errors.append(repr(error))
            if errors:
                raise RuntimeError("; ".join(errors))
            return results

    def build_context(self, version):
        context = self.output / f"context-{version}"
        context.mkdir(exist_ok=True)
        shutil.copy2(Path(__file__).with_name("application_fixture.sh"), context / "app.sh")
        shutil.copy2(self.args.http_binary, context / "app-http")
        (context / "image-version").write_bytes(f"version-{version}\n".encode())
        (context / "state").write_bytes(b"initial\n")
        (context / "Dockerfile").write_text(
            f"FROM {self.args.image}\nCOPY app.sh /app.sh\nCOPY --chmod=0755 app-http /app-http\nCOPY image-version /image-version\nCOPY state /srv/state\n"
        )
        return context

    def launch(self, name, role, address, extra=(), network=None, image=None, command=None):
        arguments = [
            "run",
            "-d",
            "--name",
            name,
            "--cpus",
            "1",
            "--memory",
            "384m",
            "--network",
            network or self.network,
            "--ip",
            address,
            *extra,
            image or self.image,
            *(command or ["/bin/sh", "/app.sh", role]),
        ]
        stdout, stderr = self.call(*arguments, scoped=name)
        assert re.fullmatch(rb"[0-9a-f]{64}\n", stdout) and not stderr, (stdout, stderr)
        assert name in self.active, f"live container escaped its verified scope: {name}"

    def setup(self):
        self.call("pull", self.args.image, timeout=120)
        self.call(
            "build", "--network=none", "-t", self.image, str(self.build_context(1)), timeout=120
        )
        self.call("network", "create", "--subnet", "172.29.91.0/24", self.network)
        self.call("network", "create", "--subnet", "172.29.92.0/24", self.isolated)
        self.launch(
            self.web, "web", "172.29.91.11", ["-v", self.volume + ":/srv", "-p", "127.0.0.1:0:8080"]
        )
        self.launch(self.client, "idle", "172.29.91.12")
        self.launch(self.observer, "idle", "172.29.91.13", ["-v", self.volume + ":/srv:ro"])
        self.call("exec", self.web, "sh", "-c", "printf upper-retained > /upper-token")
        self.ready()
        self.healthy()

    def discover_port(self):
        stdout, _ = self.call("ps")
        lines = [
            line
            for line in stdout.decode().splitlines()
            if line.split() and line.split()[-1] == self.web
        ]
        assert len(lines) == 1, stdout
        match = re.search(r"127\.0\.0\.1:(\d+)->8080/tcp", lines[0])
        assert match, lines
        port = int(match[1])
        assert self.port is None or self.port == port, (self.port, port)
        self.port = port

    def http(self, expected=None, path="/state"):
        client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        started = time.monotonic()
        try:
            client.request("GET", path)
            response = client.getresponse()
            payload = response.read()
            valid = response.status == 200 and (expected is None or payload == expected)
            with self.lock:
                with (self.output / "http.jsonl").open("a") as log:
                    log.write(
                        json.dumps(
                            {
                                "step": self.step,
                                "bytes": len(payload),
                                "sha256": hashlib.sha256(payload).hexdigest(),
                                "status": response.status,
                                "verified": valid,
                                "seconds": time.monotonic() - started,
                            }
                        )
                        + "\n"
                    )
            if not valid:
                (self.output / f"http-failure-{uuid.uuid4().hex}.bin").write_bytes(payload)
                raise AssertionError(
                    (response.status, len(payload), hashlib.sha256(payload).hexdigest())
                )
            return payload
        finally:
            client.close()

    def ready(self):
        self.discover_port()
        attempts = []

        def check():
            try:
                self.http(self.expected)
                return True
            except (OSError, http.client.HTTPException) as error:
                attempts.append(repr(error))
                return False

        wait_until(check)
        save(self.output / f"ready-{self.serial}.json", {"attempts": attempts, "port": self.port})
        # Membership's documented generation/negative-cache TTL is 250ms.
        time.sleep(0.3)

    def guest_http(self, expected=None):
        stdout, stderr = self.call(
            "exec", self.client, "wget", "-T", "5", "-qO-", f"http://{self.web}:8080/state"
        )
        assert stdout == (self.expected if expected is None else expected) and not stderr, (
            len(stdout),
            stderr,
        )

    def healthy(self):
        if self.monitor_error:
            raise RuntimeError(self.monitor_error)
        self.parallel(
            lambda: self.http(self.expected),
            self.guest_http,
            lambda: self.check_file(self.observer, "/srv/state", self.expected),
        )
        self.check_file(self.web, "/upper-token", b"upper-retained")
        self.check_file(self.web, "/image-version", self.web_version)
        output, _ = self.call("network", "inspect", self.network)
        members = json.loads(output)[0]["Containers"]
        assert sorted(v["Name"] for v in members.values()) == sorted(
            [self.web, self.client, self.observer]
        ), members

    def check_file(self, name, path, value):
        stdout, stderr = self.call("exec", name, "cat", path)
        assert stdout == value and not stderr, (name, path, stdout[:100], stderr)

    def write_state(self, value):
        with self.lock:
            self.allowed_hashes.add(hashlib.sha256(value).hexdigest())
        self.call(
            "exec",
            "-i",
            self.web,
            "sh",
            "-c",
            "cat > /srv/.next && mv /srv/.next /srv/state",
            data=value,
        )
        self.expected = value

    def streams(self):
        payload = self.rng.randbytes(262147)

        def echo(name, code):
            stdout, stderr = self.call(
                "exec",
                "-i",
                name,
                "sh",
                "-c",
                f"cat; printf stream-{code} >&2; exit {code}",
                code=code,
                data=payload,
            )
            assert stdout == payload and stderr == f"stream-{code}".encode()

        self.parallel(
            lambda: echo(self.web, 17),
            lambda: echo(self.client, 29),
            lambda: self.call("exec", self.web, "/missing-application-chaos", code=127),
            lambda: self.http(self.expected),
        )

    def atomic_volume(self):
        before = self.expected
        after = self.rng.randbytes(self.rng.choice([31, 8193, 131077, 524291]))
        with self.lock:
            self.allowed_hashes.add(hashlib.sha256(after).hexdigest())
        gate = threading.Barrier(4)

        def write():
            gate.wait()
            self.call(
                "exec",
                "-i",
                self.web,
                "sh",
                "-c",
                "cat > /srv/.next && mv /srv/.next /srv/state",
                data=after,
            )

        def read_http():
            gate.wait()
            for _ in range(6):
                assert self.http() in (before, after), "HTTP observed a partial atomic replacement"

        def read_guest(name, via_http):
            gate.wait()
            for _ in range(3):
                if via_http:
                    out, _ = self.call(
                        "exec", name, "wget", "-T", "5", "-qO-", f"http://{self.web}:8080/state"
                    )
                else:
                    out, _ = self.call("exec", name, "cat", "/srv/state")
                assert out in (before, after), "shared reader observed a partial atomic replacement"

        self.parallel(
            write,
            read_http,
            lambda: read_guest(self.client, True),
            lambda: read_guest(self.observer, False),
        )
        self.expected = after
        self.call(
            "exec",
            self.observer,
            "sh",
            "-c",
            "for op in 'echo bad > /srv/state' 'rm /srv/state' 'mv /srv/state /srv/bad' 'touch /srv/forbidden'; do if sh -c \"$op\"; then exit 1; fi; done",
        )
        self.check_file(self.web, "/srv/state", after)
        self.call("exec", self.web, "test", "!", "-e", "/srv/forbidden")

    def boots(self):
        output, _ = self.call("exec", self.web, "cat", "/boots")
        return int(output)

    def remove_active(self, name):
        with self.lock:
            return self.active.pop(name)

    def port_free(self):
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", self.port))

    def restart(self):
        count = self.boots()
        self.remove_active(self.web)
        self.call("stop", "-t", "1", self.web)
        self.port_free()
        # Four start clients share one scope and one expected generation.
        binary = self.args.binary
        self.call_helper_race(binary)
        self.ready()
        assert self.boots() == count + 1, "entrypoint ran more than once"

    def call_helper_race(self, binary):
        # Reuse call's scope/provenance path while selecting a trusted helper.
        original = self.args.binary
        try:
            self.args.binary = sys.executable
            output, stderr = self.call(
                str(Path(__file__).resolve()), "_race", binary, self.web, scoped=self.web
            )
            assert len(json.loads(output)) == 4 and not stderr
        finally:
            self.args.binary = original

    def victim(self, path, role):
        found = []
        for pid in (path / "cgroup.procs").read_text().split():
            root = Path("/proc") / pid
            try:
                command = (root / "cmdline").read_bytes().split(b"\0")
                if (
                    f"--pvisor-internal-docker-{role}".encode() in command
                    and str(self.store).encode() in command
                ):
                    found.append(int(pid))
            except FileNotFoundError:
                continue
        assert len(found) == 1, (role, found)
        return found[0]

    def crash(self, role):
        count = self.boots()
        path = self.remove_active(self.web)
        pid = self.victim(path, role)
        descriptor = os.pidfd_open(pid)
        try:
            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
        finally:
            os.close(descriptor)

        def gone():
            state = maybe_scope_state(path)
            return state is None or not state["pids"]

        wait_until(gone)
        self.port_free()
        self.parallel(
            lambda: self.call("exec", self.client, "true"),
            lambda: self.check_file(self.observer, "/srv/state", self.expected),
        )
        self.call("start", self.web, scoped=self.web)
        self.ready()
        assert self.boots() == count + 1

    def crash_vm(self):
        self.crash("vm")

    def crash_owner(self):
        self.crash("owner")

    def recreate(self):
        self.remove_active(self.web)
        self.call("rm", "-f", self.web)
        self.port_free()
        # Same managed volume/name/IP/host port, but the upper must be fresh.
        self.launch(
            self.web,
            "web",
            "172.29.91.11",
            ["-v", self.volume + ":/srv", "-p", f"127.0.0.1:{self.port}:8080"],
        )
        self.web_version = self.tag_version
        self.ready()
        assert self.boots() == 1
        self.call("exec", self.web, "test", "!", "-e", "/upper-token")
        self.call("exec", self.web, "sh", "-c", "printf upper-retained > /upper-token")

    def abort_clients(self):
        for _ in range(8):
            client = socket.create_connection(("127.0.0.1", self.port), timeout=5)
            client.sendall(b"GET /state HTTP/1.1\r\nHost: app\r\n")
            client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
            client.close()
        marker = "/disconnect-ready"
        self.call("exec", self.client, "rm", "-f", marker)
        log = (self.output / f"abandoned-exec-{self.serial}.log").open("wb")
        child = subprocess.Popen(
            [
                self.args.binary,
                "exec",
                self.client,
                "sh",
                "-c",
                f"echo ready > {marker}; exec sleep 30",
            ],
            env=self.env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        try:
            self.call(
                "exec",
                self.client,
                "sh",
                "-c",
                f"for i in $(seq 1 100); do test -f {marker} && exit 0; sleep .025; done; exit 1",
            )
        finally:
            child.kill()
            child.wait(timeout=5)
            log.close()
        self.http(self.expected)

    def exec_restart(self):
        self.call("exec", self.web, "rm", "-f", "/old-exec-ready")
        directory = self.output / f"old-exec-{self.serial}"
        directory.mkdir()
        with (directory / "stdout").open("wb") as out, (directory / "stderr").open("wb") as err:
            child = subprocess.Popen(
                [
                    self.args.binary,
                    "exec",
                    self.web,
                    "sh",
                    "-c",
                    "printf 'old-generation\\n'; echo ready > /old-exec-ready; exec sleep 300",
                ],
                env=self.env,
                stdout=out,
                stderr=err,
            )
            try:
                self.call(
                    "exec",
                    self.web,
                    "sh",
                    "-c",
                    "for i in $(seq 1 100); do test -f /old-exec-ready && exit 0; sleep .025; done; exit 1",
                )
                self.restart()
                code = child.wait(timeout=8)
                assert code != 0, "old exec incorrectly succeeded after VM termination"
                save(directory / "exit.json", {"code": code})
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
        assert (directory / "stdout").read_bytes() == b"old-generation\n"
        self.streams()

    def slow_http(self):
        self.write_state(self.rng.randbytes(524291))
        with socket.create_connection(("127.0.0.1", self.port), timeout=5) as client:
            client.settimeout(5)
            client.sendall(b"GET /state HTTP/1.1\r\nHost: app\r\nConnection: close\r\n\r\n")
            assert client.recv(1) == b"H"
            # This connection deliberately stops reading while others must finish.
            self.parallel(
                lambda: self.http(self.expected), self.guest_http, lambda: self.http(self.expected)
            )
            client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))

    def logs(self):
        before = self.call("logs", self.web)
        assert before[0].startswith(b"APP_BOOT 1\n") and before[1].startswith(b"APP_ERR 1\n"), (
            before
        )
        values = self.parallel(*[lambda: self.call("logs", self.web) for _ in range(4)])
        assert all(value == before for value in values), "logs were consumed or cross-routed"

    def conflicts(self):
        stdout, stderr = self.call(
            "run",
            "-d",
            "--name",
            self.spare,
            "--cpus",
            "1",
            "--memory",
            "384m",
            "--network",
            self.network,
            "--ip",
            "172.29.91.11",
            self.image,
            "true",
            code=125,
            scoped=self.spare,
        )
        assert not stdout and b"IPv4 address already allocated" in stderr, (stdout, stderr)
        stdout, stderr = self.call(
            "run",
            "-d",
            "--name",
            self.spare,
            "--cpus",
            "1",
            "--memory",
            "384m",
            "--network",
            self.network,
            "-p",
            f"127.0.0.1:{self.port}:8080",
            self.image,
            "true",
            code=125,
            scoped=self.spare,
        )
        assert not stdout and b"address already in use" in stderr.lower(), (stdout, stderr)
        self.call("rm", "-f", self.spare)
        output, _ = self.call("network", "inspect", self.network)
        assert len(json.loads(output)[0]["Containers"]) == 3

    def isolated_network(self):
        self.launch(self.spare, "idle", "172.29.92.11", network=self.isolated)
        try:
            self.call(
                "exec",
                self.spare,
                "sh",
                "-c",
                f"if wget -T 1 -qO- http://{self.web}:8080/state; then exit 1; fi; if wget -T 1 -qO- http://172.29.91.11:8080/state; then exit 1; fi",
            )
            self.guest_http()
        finally:
            self.remove_active(self.spare)
            self.call("rm", "-f", self.spare)

    def auto_remove(self):
        payload = self.rng.randbytes(65539)
        stdout, stderr = self.call(
            "run",
            "--rm",
            "-i",
            "--name",
            self.spare,
            "--cpus",
            "1",
            "--memory",
            "384m",
            "--network",
            "none",
            self.image,
            "sh",
            "-c",
            "cat; exit 23",
            code=23,
            data=payload,
            scoped=self.spare,
        )
        assert stdout == payload and not stderr
        self.launch(self.spare, "idle", "172.29.91.14")
        self.remove_active(self.spare)
        self.call("rm", "-f", self.spare)

    def image_replace(self):
        # Failure must leave the existing tag and existing VM pin intact.
        context = self.output / f"failed-build-{self.serial}"
        context.mkdir()
        (context / "Dockerfile").write_text(f"FROM {self.args.image}\nRUN exit 7\n")
        _, stderr = self.call(
            "build",
            "--network=none",
            "-t",
            self.image,
            str(context),
            code=1,
            scoped="build",
            timeout=90,
        )
        assert b"RUN exited with status 7" in stderr, stderr
        version = 1 if self.tag_version == b"version-2\n" else 2
        self.call(
            "build",
            "--network=none",
            "-t",
            self.image,
            str(self.build_context(version)),
            scoped="build",
            timeout=90,
        )
        self.tag_version = f"version-{version}\n".encode()
        self.check_file(self.web, "/image-version", self.web_version)
        stdout, stderr = self.call(
            "run",
            "--rm",
            "--name",
            self.spare,
            "--cpus",
            "1",
            "--memory",
            "384m",
            "--network",
            "none",
            self.image,
            "cat",
            "/image-version",
            scoped=self.spare,
        )
        assert stdout == self.tag_version and not stderr

    def monitor(self):
        with (self.output / "resources.jsonl").open("w") as file:
            while not self.stop_monitor.is_set():
                with self.lock:
                    active = dict(self.active)
                sample = {"time": time.time(), "step": self.step, "instances": {}}
                try:
                    live = 0
                    for name, path in active.items():
                        state = maybe_scope_state(path)
                        if state is None:
                            continue
                        verify_state(state)
                        if not state["pids"]:
                            continue
                        live += 1
                        processes = []
                        for pid in state["pids"]:
                            root = Path("/proc") / pid
                            try:
                                processes.append(
                                    {
                                        "pid": pid,
                                        "fds": len(list((root / "fd").iterdir())),
                                        "threads": len(list((root / "task").iterdir())),
                                    }
                                )
                                for task in (root / "task").iterdir():
                                    assert sorted(os.sched_getaffinity(int(task.name))) == [
                                        4,
                                        5,
                                        6,
                                        7,
                                    ]
                            except ProcessLookupError:
                                continue
                            except FileNotFoundError:
                                continue
                        sample["instances"][name] = {**state, "processes": processes}
                    assert live <= 4, live
                    file.write(json.dumps(sample) + "\n")
                    file.flush()
                except BaseException as error:
                    self.monitor_error.append(repr(error))
                    return
                self.stop_monitor.wait(0.2)

    @contextmanager
    def expected_fault(self):
        with self.lock:
            self.fault_epoch += 1
            self.faulting = True
        try:
            yield
        finally:
            with self.lock:
                self.faulting = False

    def background_http(self):
        with (self.output / "background-http.jsonl").open("w") as log:
            while not self.stop_monitor.is_set():
                with self.lock:
                    epoch, already_faulting = self.fault_epoch, self.faulting
                record = {"time": time.time(), "step": self.step, "epoch": epoch}
                try:
                    payload = self.http()
                    digest = hashlib.sha256(payload).hexdigest()
                    with self.lock:
                        valid = digest in self.allowed_hashes
                    assert valid, "background HTTP received uncommitted or corrupted content"
                    record.update(verified=True, sha256=digest, bytes=len(payload))
                except (OSError, http.client.HTTPException) as error:
                    with self.lock:
                        expected = already_faulting or self.faulting or epoch != self.fault_epoch
                    record.update(error=repr(error), expected_fault=expected)
                    if not expected:
                        self.monitor_error.append(
                            f"background HTTP outside fault window: {error!r}"
                        )
                except BaseException as error:
                    record.update(error=repr(error), expected_fault=False)
                    self.monitor_error.append(f"background HTTP: {error!r}")
                log.write(json.dumps(record) + "\n")
                log.flush()
                if self.monitor_error:
                    return
                self.stop_monitor.wait(0.025)

    def run(self, schedule):
        for index, case in enumerate(schedule):
            assert time.monotonic() < self.deadline, "application chaos time budget exceeded"
            self.step = f"{index:03d}-{case}"
            started = time.monotonic()
            if case in {"restart", "crash_vm", "crash_owner", "recreate", "exec_restart"}:
                with self.expected_fault():
                    getattr(self, case)()
            else:
                getattr(self, case)()
            self.healthy()
            assert not self.monitor_error, self.monitor_error
            self.counts[case] = self.counts.get(case, 0) + 1
            save(
                self.output / "rounds" / f"{self.step}.json",
                {
                    "verified": True,
                    "seconds": time.monotonic() - started,
                    "state_bytes": len(self.expected),
                    "state_sha256": hashlib.sha256(self.expected).hexdigest(),
                },
            )
            print(f"round {index + 1}/{len(schedule)}: {case} passed", flush=True)

    def archive(self):
        for entry in (self.store / "instances").glob("*/instance.json"):
            record = json.loads(entry.read_text())
            directory = self.output / "owner-records" / record["name"]
            directory.mkdir(parents=True, exist_ok=True)
            for path in entry.parent.iterdir():
                if path.is_file() and (
                    path.suffix in {".log", ".json"} or path.name.startswith("boot-")
                ):
                    shutil.copy2(path, directory / path.name)
            completion = self.store / "results" / (record["id"] + ".json")
            if completion.exists():
                shutil.copy2(completion, directory / "completion.json")

    def cleanup(self):
        errors = []
        self.archive()
        for name in self.names:
            try:
                self.call("rm", "-f", "-v", name, allow_missing=True)
            except BaseException as error:
                errors.append(repr(error))
        for network in (self.network, self.isolated):
            try:
                self.call("network", "rm", "-f", network)
            except BaseException as error:
                errors.append(repr(error))
        try:
            stdout, _ = self.call("ps", "-aq")
            assert stdout == b"", stdout
        except BaseException as error:
            errors.append(repr(error))
        for receipt in (self.output / "commands").glob("*/scope.json"):
            try:
                state = maybe_scope_state(Path(json.loads(receipt.read_text())["cgroup"]))
                assert state is None or not state["pids"], (str(receipt), state)
            except BaseException as error:
                errors.append(repr(error))
        save(self.output / "cleanup.json", {"errors": errors})
        if errors:
            raise RuntimeError(errors)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "_scope":
        scope_helper(sys.argv[2:])
        return
    if len(sys.argv) > 1 and sys.argv[1] == "_race":
        start_race(*sys.argv[2:])
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--http-binary", required=True)
    parser.add_argument("--image", default=IMAGE)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=11)
    parser.add_argument("--rounds", type=int, default=36)
    parser.add_argument("--seconds", type=int, default=900)
    args = parser.parse_args()
    assert sys.platform == "linux" and "@sha256:" in args.image
    args.binary = str(Path(args.binary).resolve(strict=True))
    args.http_binary = str(Path(args.http_binary).resolve(strict=True))
    cases = [
        "streams",
        "atomic_volume",
        "restart",
        "crash_vm",
        "crash_owner",
        "recreate",
        "abort_clients",
        "logs",
        "conflicts",
        "isolated_network",
        "auto_remove",
        "image_replace",
        "exec_restart",
        "slow_http",
    ]
    assert args.rounds >= len(cases) and args.seconds > 0
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    os.sched_setaffinity(0, {8, 9})
    rng = random.Random(args.seed)
    schedule = cases + rng.choices(cases, k=args.rounds - len(cases))
    save(
        args.output / "manifest.json",
        {
            "binary": args.binary,
            "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(),
            "http_binary": args.http_binary,
            "http_binary_sha256": hashlib.sha256(Path(args.http_binary).read_bytes()).hexdigest(),
            "image": args.image,
            "seed": args.seed,
            "schedule": schedule,
            "seconds": args.seconds,
            "limits": {
                "max_vms": 4,
                "per_vm_memory_max": LIMIT,
                "swap": 0,
                "guest_mib": 384,
                "cpu_quota": 1,
            },
            "failures": "retain first unexpected failure; no automatic retry or exclusions",
        },
    )
    for name in ("application_chaos.py", "application_fixture.sh", "application_http.rs"):
        shutil.copy2(Path(__file__).with_name(name), args.output / name)
    chaos = ApplicationChaos(args)
    monitor = threading.Thread(target=chaos.monitor)
    traffic = threading.Thread(target=chaos.background_http)
    summary = {"passed": False, "seed": args.seed}
    try:
        chaos.setup()
        monitor.start()
        traffic.start()
        chaos.run(schedule)
        summary["passed"] = True
    except BaseException as error:
        save(
            args.output / "failure.json",
            {"step": chaos.step, "error": repr(error), "completed": chaos.counts},
        )
        raise
    finally:
        chaos.stop_monitor.set()
        if monitor.ident is not None:
            monitor.join(timeout=5)
        if traffic.ident is not None:
            traffic.join(timeout=6)
        if chaos.monitor_error:
            summary.update(passed=False, monitor_errors=chaos.monitor_error)
        try:
            chaos.cleanup()
        except BaseException as error:
            summary.update(passed=False, cleanup_error=repr(error))
            raise
        finally:
            summary.update(
                cases=chaos.counts, commands=chaos.serial, monitor_errors=chaos.monitor_error
            )
            save(args.output / "summary.json", summary)
    if not summary["passed"]:
        save(
            args.output / "failure.json",
            {
                "step": chaos.step,
                "error": "late monitor failure",
                "monitor_errors": chaos.monitor_error,
            },
        )
        raise RuntimeError(summary)


if __name__ == "__main__":
    main()
