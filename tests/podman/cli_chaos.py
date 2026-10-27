#!/usr/bin/env python3
"""Opt-in seeded CLI/daemon chaos, two small VMs maximum, raw failure receipts."""

import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import select
import signal
import socket
import struct
import subprocess
import threading
import time
import traceback
from pathlib import Path

if not __debug__:
    raise RuntimeError("CLI chaos requires assertions enabled; do not use -O or PYTHONOPTIMIZE")

IMAGE = "mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e"


def save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def pressure():
    fields = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
    psi = {}
    for line in Path("/proc/pressure/memory").read_text().splitlines():
        kind, *values = line.split()
        psi[kind] = dict(value.split("=") for value in values)
    return {
        "unix": time.time(),
        "available": int(fields["MemAvailable"].split()[0]) * 1024,
        "load1": os.getloadavg()[0],
        "psi": psi,
    }


def peer_handle(path):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.connect(str(path))
        pid, uid, _ = struct.unpack(
            "3i", stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
        )
        assert uid == os.geteuid()
        return os.pidfd_open(pid)


class Campaign:
    def __init__(self, args):
        self.args = args
        self.root = args.output.resolve()
        self.root.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.store = self.root / "store"
        self.store.mkdir(mode=0o700)
        self.daemon = self.root / "daemon"
        self.env = dict(
            os.environ, PVISOR_DOCKER_HOME=str(self.store), PVISOR_DAEMON_HOME=str(self.daemon)
        )
        self.rng = random.Random(args.seed)
        self.names = ["cli-chaos-a", "cli-chaos-b"]
        self.volume_name = "cli-chaos-shared"
        self.lock = threading.Lock()
        self.serial = 0
        self.counts = {}
        self.round = 0
        self.generation = [0, 0]
        self.pool = None
        self.pool_lost = False
        self.deadline = None
        self.stopping = threading.Event()
        self.paused = 0
        self.step = "prepare"
        self.started = time.monotonic()
        driver = Path(__file__).read_bytes()
        (self.root / "runner.py").write_bytes(driver)
        save(
            self.root / "manifest.json",
            {
                "seed": args.seed,
                "seconds": args.seconds,
                "image": args.image,
                "binary": str(args.binary),
                "sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                "daemon_sha256": hashlib.sha256(
                    args.binary.with_name("pvisor-daemon").read_bytes()
                ).hexdigest(),
                "driver_sha256": hashlib.sha256(driver).hexdigest(),
                "started_unix": time.time(),
                "pid": os.getpid(),
                "max_vms": 2,
                "guest_mib": 256,
                "guest_cpus": 1,
                "cgroup": Path("/proc/self/cgroup").read_text()
                if Path("/proc/self/cgroup").exists()
                else None,
            },
        )

    def call(self, *args, code=0, data=None, out=None, err=None, timeout=45):
        with self.lock:
            number = self.serial
            self.serial += 1
        directory = self.root / f"cmd-{number:06d}"
        directory.mkdir()
        working_directory = self.root if number % 2 else Path("/tmp").resolve()
        save(
            directory / "command.json",
            {
                "argv": list(map(str, args)),
                "cwd": str(working_directory),
                "step": self.step,
                "round": self.round,
                "stdin_bytes": len(data or b""),
                "stdin_sha256": hashlib.sha256(data or b"").hexdigest(),
                "expected_code": code,
                "expected_stdout_sha256": hashlib.sha256(out).hexdigest()
                if out is not None
                else None,
                "expected_stderr_sha256": hashlib.sha256(err).hexdigest()
                if err is not None
                else None,
            },
        )
        start = time.monotonic()
        child = subprocess.Popen(
            [str(self.args.binary), *map(str, args)],
            env=self.env,
            cwd=working_directory,
            stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
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
            stdout, stderr = child.communicate(timeout=5)
        (directory / "stdout").write_bytes(stdout)
        (directory / "stderr").write_bytes(stderr)
        if data is not None:
            (directory / "stdin").write_bytes(data)
        save(
            directory / "exit.json",
            {"code": child.returncode, "timed_out": timed_out, "seconds": time.monotonic() - start},
        )
        allowed = code if isinstance(code, tuple) else (code,)
        assert not timed_out and child.returncode in allowed, (
            directory.name,
            child.returncode,
            code,
            stderr[-2000:],
        )
        if out is not None:
            assert stdout == out, (directory.name, "stdout", stdout[:400], out[:400])
        if err is not None:
            assert stderr == err, (directory.name, "stderr", stderr[:400], err[:400])
        return stdout, stderr

    def parallel(self, operations):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            return list(pool.map(lambda operation: operation(), operations))

    def launch(self, i, network="none"):
        self.call(
            "run",
            "-d",
            "-M",
            "--name",
            self.names[i],
            "--cpus",
            "1",
            "-m",
            "256m",
            "--network",
            network,
            "-v",
            self.volume_name + ":/shared" + (":ro" if i else ""),
            self.args.image,
            "sh",
            "-c",
            "n=$(cat /boot-count 2>/dev/null || echo 0); n=$((n+1)); echo $n >/boot-count; printf 'boot:%s\\n' $n; printf 'booterr:%s\\n' $n >&2; exec sleep 100000",
        )
        self.generation[i] = 1

    def health(self):
        ids = self.call("ps", "-q")[0].splitlines()
        assert len(ids) == 2, ("expected exactly two owned live containers", ids)
        for i, name in enumerate(self.names):
            self.call(
                "exec", name, "cat", "/boot-count", out=f"{self.generation[i]}\n".encode(), err=b""
            )

    def streams(self):
        exits = [0, 1, 7, 23, 125, 126, 127, 137, 255]
        operations = []
        for i in range(4):
            payload = self.rng.randbytes(self.rng.randint(1, 131072))
            status = self.rng.choice(exits)
            name = self.names[i % 2]
            operations.append(
                lambda payload=payload, status=status, name=name: self.call(
                    "exec",
                    "-i",
                    name,
                    "sh",
                    "-c",
                    f"cat; printf stream-error >&2; exit {status}",
                    data=payload,
                    code=status,
                    out=payload,
                    err=b"stream-error",
                )
            )
        self.parallel(operations)
        self.call("exec", self.names[0], "/nonexistent-cli-chaos", code=127)
        self.call("exec", self.names[1], "/tmp", code=126)

    def environment(self):
        value = self.rng.choice(["", "space argument", "a=b=c", "--flag", "中文⚡"])
        self.call(
            "exec",
            "-e",
            "CLI_CHAOS=" + value,
            self.names[0],
            "sh",
            "-c",
            'printf "%s\\n" "$CLI_CHAOS"; printf "%s\\n" "$@"',
            "sh",
            "-M",
            "",
            "a b",
            out=value.encode() + b"\n-M\n\na b\n",
            err=b"",
        )
        self.call("exec", self.names[0], "sh", "-c", 'test -z "${CLI_CHAOS+x}"', err=b"")

    def volume(self):
        data = self.rng.randbytes(self.rng.randint(0, 98304))
        self.call(
            "exec",
            "-i",
            self.names[0],
            "sh",
            "-c",
            "cat >/shared/new; mv /shared/new /shared/payload",
            data=data,
            out=b"",
            err=b"",
        )
        self.call("exec", self.names[1], "cat", "/shared/payload", out=data, err=b"")
        _, stderr = self.call(
            "exec", self.names[1], "sh", "-c", "printf bad >/shared/payload", code=1, out=b""
        )
        assert b"Read-only" in stderr or b"read-only" in stderr, stderr
        self.call("exec", self.names[0], "cat", "/shared/payload", out=data, err=b"")

    def restart(self):
        i = self.rng.randrange(2)
        name = self.names[i]
        self.call("stop", "-t", "0", name, out=(name + "\n").encode())
        self.parallel(
            [lambda: self.call("start", name, out=(name + "\n").encode()) for _ in range(4)]
        )
        self.generation[i] += 1
        self.health()
        self.call("start", name, out=(name + "\n").encode())
        self.health()

    def manager(self):
        first = json.loads(self.call("daemon", "status")[0])
        inode = Path(first["memory_pool"]["socket"]).stat().st_ino
        if self.rng.choice([False, True]):
            handle = peer_handle(self.daemon / "control.sock")
            try:
                signal.pidfd_send_signal(handle, signal.SIGKILL)
                assert select.select([handle], [], [], 5)[0], "manager did not exit"
            finally:
                os.close(handle)
        else:
            self.call("daemon", "stop")
        self.health()
        joined = self.parallel(
            [lambda: self.call("daemon", "start", "--memory-pool") for _ in range(4)]
        )
        after = json.loads(self.call("daemon", "status")[0])
        assert all(json.loads(stdout) == after for stdout, _ in joined), joined
        assert after["instance_id"] != first["instance_id"]
        assert after["memory_pool"] == first["memory_pool"]
        assert Path(after["memory_pool"]["socket"]).stat().st_ino == inode
        self.health()

    def logs(self):
        baseline = [self.call("logs", name) for name in self.names]
        results = self.parallel(
            [lambda i=i: self.call("logs", self.names[i % 2]) for i in range(4)]
        )
        assert results == [baseline[i % 2] for i in range(4)]

    def batch(self):
        name = self.names[0]
        self.call("stop", "-t", "0", name)
        output, error = self.call("start", name, "cli-chaos-missing", code=1)
        assert output == (name + "\n").encode() and b"no such container" in error.lower(), (
            output,
            error,
        )
        self.generation[0] += 1
        self.health()
        _, error = self.call("rm", name, code=1)
        assert b"running" in error.lower(), error

    def disconnected_client(self):
        directory = self.root / f"abandoned-{self.round:06d}"
        directory.mkdir()
        with (directory / "stdout").open("wb") as out, (directory / "stderr").open("wb") as err:
            child = subprocess.Popen(
                [str(self.args.binary), "exec", self.names[0], "sh", "-c", "printf ready; sleep 5"],
                env=self.env,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=err,
            )
            try:
                deadline = time.monotonic() + 10
                while (directory / "stdout").read_bytes() != b"ready":
                    assert child.poll() is None and time.monotonic() < deadline
                    time.sleep(0.02)
                child.kill()
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
            save(directory / "exit.json", {"code": child.returncode, "injected_client_kill": True})
        self.health()

    def transient(self):
        self.call("rm", "-f", self.names[1])
        try:
            code = self.rng.choice([0, 7, 23, 125, 143])
            output = self.rng.randbytes(8192)
            command = "cat; kill -TERM $$" if code == 143 else f"cat; exit {code}"
            self.call(
                "run",
                "--rm",
                "-M",
                "-i",
                "--name",
                self.names[1],
                "--cpus",
                "1",
                "-m",
                "256m",
                "--network",
                "none",
                self.args.image,
                "sh",
                "-c",
                command,
                data=output,
                code=code,
                out=output,
                err=b"",
            )
            # Public CLI must already allow immediate same-name reuse after --rm.
        finally:
            self.launch(1)

    def invalid(self):
        for args, reason in [
            (("run", "-M", "--memory-pool", "auto", self.args.image), b"cannot be used"),
            (("run", "--unknown-cli-chaos", self.args.image), b"unexpected argument"),
            (("run", "-m", "invalid", self.args.image), b"invalid memory size"),
            (("run", "--cpus", "0", self.args.image), b"invalid VM CPU/memory budget"),
        ]:
            _, stderr = self.call(*args, code=125, out=b"")
            assert reason in stderr, (args, stderr)
        self.health()

    def build(self):
        context = self.root / "build-context"
        context.mkdir(exist_ok=True)
        (context / "Dockerfile").write_text(
            f'FROM {self.args.image}\nRUN printf build-ok >/built\nCMD ["cat","/built"]\n'
        )
        self.call("rm", "-f", self.names[1])
        try:
            self.call(
                "build",
                "--network",
                "none",
                "--cpus",
                "1",
                "-m",
                "256m",
                "-q",
                "-t",
                "cli-chaos:built",
                context,
                timeout=90,
            )
            self.call(
                "run",
                "--rm",
                "-M",
                "--name",
                self.names[1],
                "--cpus",
                "1",
                "-m",
                "256m",
                "--network",
                "none",
                "cli-chaos:built",
                out=b"build-ok",
                err=b"",
            )
            self.call("rmi", "cli-chaos:built")
        finally:
            self.launch(1)

    def network(self):
        name = "cli-chaos-net"
        self.call("network", "create", "--subnet", "192.0.2.0/24", name)
        self.call("rm", "-f", self.names[1])
        try:
            self.launch(1, name)
            inspected = json.loads(self.call("network", "inspect", name)[0])
            assert inspected[0]["Name"] == name and len(inspected[0]["Containers"]) == 1, inspected
            _, error = self.call("network", "rm", name, code=1)
            assert b"attached" in error.lower() or b"use" in error.lower(), error
            self.call("rm", "-f", self.names[1])
            self.call("network", "rm", name)
        finally:
            self.launch(1)

    def sample(self):
        sample = pressure()
        disk = os.statvfs(self.root)
        sample["disk_available"] = disk.f_bavail * disk.f_frsize
        relative = Path("/proc/self/cgroup").read_text().strip().split(":", 2)[2]
        group = Path("/sys/fs/cgroup") / relative.lstrip("/")
        sample["cgroup"] = {
            name: (group / name).read_text().strip()
            for name in [
                "memory.current",
                "memory.peak",
                "memory.events",
                "memory.swap.current",
                "cpu.stat",
                "pids.current",
            ]
        }
        with (self.root / "resources.jsonl").open("a") as stream:
            stream.write(json.dumps(sample) + "\n")
        assert int(sample["cgroup"]["memory.swap.current"]) == 0, sample
        events = dict(line.split() for line in sample["cgroup"]["memory.events"].splitlines())
        assert all(int(events[key]) == 0 for key in ["oom", "oom_kill", "max"]), sample
        return sample

    def pool_loss(self):
        before = json.loads(self.call("daemon", "status")[0])
        assert before["memory_pool"]["config"]["mode"] in ("physical", "physical-pages"), before
        socket_path = Path(before["memory_pool"]["socket"])
        inode = socket_path.stat().st_ino
        manager = peer_handle(self.daemon / "control.sock")
        try:
            signal.pidfd_send_signal(self.pool, signal.SIGKILL)
            assert select.select([self.pool], [], [], 5)[0], "pool did not exit"
            assert select.select([manager], [], [], 10)[0], "manager did not report pool loss"
            self.pool_lost = True
        finally:
            os.close(manager)
        deadline = time.monotonic() + 15
        while self.call("ps", "-q")[0].splitlines():
            assert time.monotonic() < deadline, "physical-pool loss left dependent VMs running"
            time.sleep(0.2)
        for name in self.names:
            _, error = self.call("start", name, code=1)
            assert b"connection refused" in error.lower(), error
        self.call("daemon", "start", "--memory-pool", code=1)
        _, error = self.call(
            "run",
            "--rm",
            "-M",
            "--name",
            "cli-chaos-refused",
            "--cpus",
            "1",
            "-m",
            "256m",
            "--network",
            "none",
            self.args.image,
            "true",
            code=125,
        )
        assert b"memory-pool auto" in error, error
        assert b"existing pool unavailable" in (self.daemon / "manager.log").read_bytes()
        assert socket_path.stat().st_ino == inode, "lost pool socket was silently replaced"
        save(
            self.root / "pool-loss.json",
            {
                "verified": True,
                "retained_socket_inode": inode,
                "after_socket_inode": socket_path.stat().st_ino,
                "pool": before["memory_pool"],
                "dependent_vms_stopped": True,
                "implicit_replacement_refused": True,
            },
        )

    def cleanup(self):
        errors = []
        self.step = "cleanup"
        for name in self.names + (["cli-chaos-refused"] if self.pool_lost else []):
            try:
                _, stderr = self.call("rm", "-f", name, code=(0, 1))
                assert not stderr or b"no such container" in stderr.lower(), stderr
            except Exception as error:
                errors.append(repr(error))
        if not self.pool_lost:
            try:
                self.call("daemon", "stop")
            except Exception as error:
                errors.append(repr(error))
        if self.pool is not None:
            try:
                signal.pidfd_send_signal(self.pool, signal.SIGTERM)
                assert select.select([self.pool], [], [], 5)[0], "pool did not exit"
            except ProcessLookupError:
                pass
            finally:
                os.close(self.pool)
        try:
            self.call("ps", "-a", "-q", out=b"")
        except Exception as error:
            errors.append(repr(error))
        save(self.root / "cleanup.json", {"errors": errors})
        return errors

    def main(self):
        passed = False
        failure = None
        cleanup = []
        try:
            relative = Path("/proc/self/cgroup").read_text().strip().split(":", 2)[2]
            group = Path("/sys/fs/cgroup") / relative.lstrip("/")
            limits = {
                name: (group / name).read_text().strip()
                for name in ["memory.max", "memory.swap.max", "cpu.max", "pids.max"]
            }
            save(self.root / "limits.json", limits)
            assert (
                limits["memory.max"] == str(1536 * 1024**2) and limits["memory.swap.max"] == "0"
            ), limits
            quota, period = map(int, limits["cpu.max"].split())
            assert quota * 2 == period * 3 and limits["pids.max"] == "384", limits
            self.call("pull", self.args.image, timeout=180)
            self.launch(0)
            self.pool = peer_handle(self.daemon / "memory-pool/pool.sock")
            self.call("exec", self.names[0], "sh", "-c", "printf initial >/shared/payload")
            self.launch(1)
            self.health()
            self.started = time.monotonic()
            self.deadline = self.started + self.args.seconds
            save(self.root / "load-start.json", {"unix": time.time(), "seconds": self.args.seconds})
            scenarios = [
                self.streams,
                self.environment,
                self.volume,
                self.restart,
                self.manager,
                self.logs,
                self.batch,
                self.disconnected_client,
                self.transient,
                self.invalid,
                self.build,
                self.network,
            ]
            while time.monotonic() < self.deadline and not self.stopping.is_set():
                sample = self.sample()
                if (
                    sample["available"] < 5 * 1024**3
                    or sample["load1"] > 12
                    or float(sample["psi"]["full"]["avg10"]) > 1
                    or sample["disk_available"] < 5 * 1024**3
                ):
                    self.paused += 1
                    save(self.root / "pressure-pause.json", sample)
                    self.stopping.wait(15)
                    continue
                scenario = (
                    scenarios[self.round]
                    if self.round < len(scenarios)
                    else self.rng.choice(scenarios)
                )
                self.step = scenario.__name__
                scenario()
                self.counts[self.step] = self.counts.get(self.step, 0) + 1
                self.round += 1
                save(
                    self.root / "status.json",
                    {
                        "rounds": self.round,
                        "commands": self.serial,
                        "counts": self.counts,
                        "elapsed": time.monotonic() - self.started,
                        "paused_samples": self.paused,
                        "unix": time.time(),
                    },
                )
                self.stopping.wait(self.args.interval)
            assert all(self.counts.get(scenario.__name__, 0) for scenario in scenarios), (
                "not all scenarios ran"
            )
            if getattr(self.args, "inject_pool_loss", False) and not self.stopping.is_set():
                self.step = "pool_loss"
                self.pool_loss()
                self.counts["pool_loss"] = 1
                self.round += 1
            passed = not self.stopping.is_set()
        except BaseException:
            failure = traceback.format_exc()
            (self.root / "failure.txt").write_text(failure)
        finally:
            cleanup = self.cleanup()
            try:
                self.sample()
            except Exception as error:
                cleanup.append("final resource check: " + repr(error))
            save(
                self.root / "outcome.json",
                {
                    "passed": passed and not cleanup,
                    "failure": failure,
                    "rounds": self.round,
                    "commands": self.serial,
                    "counts": self.counts,
                    "elapsed": time.monotonic() - self.started,
                    "paused_samples": self.paused,
                    "cleanup_errors": cleanup,
                },
            )
        return 0 if passed and not cleanup else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=lambda path: Path(path).resolve(strict=True), required=True
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--image", default=IMAGE)
    parser.add_argument("--seconds", type=int, default=7200)
    parser.add_argument("--seed", type=int, default=10251526)
    parser.add_argument("--interval", type=float, default=3)
    parser.add_argument(
        "--inject-pool-loss",
        action="store_true",
        help="After the window, kill only the owned physical pool and verify fail-closed behavior",
    )
    args = parser.parse_args()
    if not 1 <= args.seconds <= 86400 or args.interval < 0.2:
        parser.error("seconds must be 1..86400 and interval at least 0.2")
    campaign = Campaign(args)
    for sig in [signal.SIGTERM, signal.SIGINT]:
        signal.signal(sig, lambda *_: campaign.stopping.set())
    raise SystemExit(campaign.main())
