#!/usr/bin/env python3
"""Opt-in, bounded container chaos; independent of ordinary test discovery."""

import argparse
import concurrent.futures
import json
import os
import random
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path


def host_sample():
    load = os.getloadavg()[0]
    memory = subprocess.run(
        ["memory_pressure"], capture_output=True, text=True, timeout=5, check=True
    )
    match = re.search(r"System-wide memory free percentage: (\d+)%", memory.stdout)
    if not match:
        raise RuntimeError("cannot read macOS memory pressure")
    return {"load1": load, "memory_free_percent": int(match[1])}


class Chaos:
    def __init__(self, args, directory):
        self.args = args
        self.directory = directory
        self.env = {**os.environ, "PVISOR_DOCKER_HOME": str(directory / "store")}
        self.names = ["chaos-a", "chaos-b"]
        self.counts = {}
        self.commands = 0
        self.max_latency = 0
        self.samples = []
        self.random = random.Random(args.seed)
        self.metrics_lock = threading.Lock()
        self.deadline = time.monotonic() + args.seconds

    def call(self, *args, code=0, data=b""):
        start = time.monotonic()
        with subprocess.Popen(
            [self.args.binary, *args],
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        ) as child:
            try:
                timeout = 35 if self.deadline is None else min(35, max(0.1, self.deadline - start))
                out, err = child.communicate(data, timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.communicate()
                raise RuntimeError(f"command timed out: {args}") from None
        elapsed = time.monotonic() - start
        with self.metrics_lock:
            self.commands += 1
            self.max_latency = max(self.max_latency, elapsed)
        if child.returncode != code:
            raise RuntimeError(
                f"{args}: exit={child.returncode}, want={code}, out={out!r}, err={err!r}"
            )
        return out, err

    def parallel(self, *commands):
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            pending = [pool.submit(command) for command in commands]
            # Observe every worker, including when another one failed.
            errors = []
            for result in pending:
                try:
                    result.result()
                except Exception as error:
                    errors.append(str(error))
            if errors:
                raise RuntimeError("; ".join(errors))

    def setup(self):
        self.call("pull", self.args.image)
        self.call(
            "run",
            "-d",
            "--name",
            self.names[0],
            "--cpus",
            "1",
            "-m",
            "256m",
            "-v",
            "shared:/data",
            "-p",
            "127.0.0.1:0:8080",
            self.args.image,
            "sh",
            "-c",
            "echo boot; exec busybox nc -lk -s 127.0.0.1 -p 8080 -e cat",
        )
        self.call(
            "run",
            "-d",
            "--name",
            self.names[1],
            "--cpus",
            "1",
            "-m",
            "256m",
            "--network",
            "none",
            "-v",
            "shared:/data",
            self.args.image,
            "sleep",
            "600",
        )
        self.call("exec", self.names[0], "sh", "-c", "printf retained > /retained")

    def streams(self):
        token = self.random.randrange(1_000_000)

        def echo(name, status):
            payload = (f"{token}-{name}:".encode() + b"\x00\xff") * 1024
            out, err = self.call(
                "exec",
                "-i",
                name,
                "sh",
                "-c",
                f"cat; printf error >&2; exit {status}",
                code=status,
                data=payload,
            )
            assert out == payload and err == b"error", (len(out), len(payload), err)

        self.parallel(
            lambda: echo(self.names[0], 17),
            lambda: echo(self.names[1], 29),
            lambda: self.call("exec", self.names[0], "/missing", code=127),
        )

    def shared_volume(self):
        token = f"value-{self.random.randrange(1_000_000)}"
        self.call(
            "exec",
            self.names[0],
            "sh",
            "-c",
            f"printf {token} > /data/next; mv /data/next /data/value",
        )
        out, _ = self.call("exec", self.names[1], "cat", "/data/value")
        assert out == token.encode(), out
        self.parallel(
            lambda: self.call("exec", self.names[0], "sh", "-c", "printf first > /data/a"),
            lambda: self.call("exec", self.names[1], "sh", "-c", "printf second > /data/b"),
        )
        out, _ = self.call("exec", self.names[1], "sh", "-c", "cat /data/a /data/b")
        assert out == b"firstsecond", out

    def restart(self):
        self.call("stop", "-t", "0", self.names[0])
        self.parallel(*[lambda: self.call("start", self.names[0]) for _ in range(2)])
        out, _ = self.call("exec", self.names[0], "cat", "/retained")
        assert out == b"retained", out
        self.call("exec", self.names[1], "true")

    def disconnect(self):
        # Abandon a live CLI client, then prove the guest survives and can be stopped.
        self.call("exec", self.names[1], "rm", "-f", "/attached")
        child = subprocess.Popen(
            [self.args.binary, "exec", self.names[1], "sh", "-c", "touch /attached; exec sleep 20"],
            env=self.env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            self.call(
                "exec",
                self.names[1],
                "sh",
                "-c",
                "for i in $(seq 1 80); do test -f /attached && exit 0; sleep .05; done; exit 1",
            )
        finally:
            child.kill()
            child.wait(timeout=5)
        self.call("stop", "-t", "0", self.names[1])
        self.call("start", self.names[1])
        self.call("exec", self.names[1], "true")

    def tcp(self):
        self.call(
            "exec",
            self.names[0],
            "sh",
            "-c",
            "for i in $(seq 1 80); do grep -q ':1F90 ' /proc/net/tcp && exit 0; sleep .05; done; exit 1",
        )
        listing, _ = self.call("ps")
        port = int(re.search(rb"127\.0\.0\.1:(\d+)->8080/tcp", listing)[1])
        # An immediately closed peer must not poison subsequent connections.
        socket.create_connection(("127.0.0.1", port), timeout=3).close()
        for i in range(2):
            payload = (f"tcp-{i}".encode() + b"\x00") * 256
            with socket.create_connection(("127.0.0.1", port), timeout=3) as client:
                client.settimeout(3)
                client.sendall(payload)
                client.shutdown(socket.SHUT_WR)
                response = bytearray()
                while block := client.recv(8192):
                    response.extend(block)
                assert response == payload, (len(response), len(payload))

    def logs(self):
        before, before_err = self.call("logs", self.names[0])
        assert before.startswith(b"boot\n"), before

        def read():
            out, err = self.call("logs", self.names[0])
            assert (out, err) == (before, before_err), (before, before_err, out, err)

        self.parallel(read, read, read)
        after, err = self.call("logs", self.names[0])
        assert (after, err) == (before, before_err), (before, before_err, after, err)

    def run(self):
        cases = [
            self.streams,
            self.shared_volume,
            self.restart,
            self.disconnect,
            self.tcp,
            self.logs,
        ]
        for step in range(self.args.rounds):
            sample = host_sample()
            processes = subprocess.run(
                ["ps", "-axo", "pid=,rss=,args="],
                capture_output=True,
                text=True,
                check=True,
                timeout=5,
            )
            owned = [
                line.split(None, 2)
                for line in processes.stdout.splitlines()
                if str(self.directory / "store") in line
            ]
            sample["owner_vmm_rss_kib"] = sum(int(line[1]) for line in owned)
            sample["owner_vmm_processes"] = len(owned)
            self.samples.append(sample)
            if sample["load1"] > self.args.max_load or sample["memory_free_percent"] < 20:
                raise RuntimeError(f"host pressure stop: {sample}")
            if time.monotonic() >= self.deadline:
                raise RuntimeError("run reached its time budget before finishing rounds")
            case = cases[step] if step < len(cases) else self.random.choice(cases)
            print(
                f"seed={self.args.seed} step={step} case={case.__name__} host={sample}", flush=True
            )
            case()
            self.counts[case.__name__] = self.counts.get(case.__name__, 0) + 1
            self.call("exec", self.names[0], "true")
            self.call("exec", self.names[1], "true")
            time.sleep(0.2)

    def cleanup(self):
        self.deadline = None
        errors = []
        for name in self.names:
            try:
                self.call("rm", "-f", "-v", name)
            except RuntimeError as error:
                if "No such container" not in str(error):
                    errors.append(str(error))
        if errors:
            raise RuntimeError("; ".join(errors))
        out, _ = self.call("ps", "-aq")
        assert out == b"", out


def main():
    started = time.monotonic()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=os.environ.get("PVISOR_BINARY", "pvisor"))
    parser.add_argument("--image", default=os.environ.get("PVISOR_TEST_IMAGE", "alpine:3.20"))
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--rounds", type=int, choices=range(6, 61), default=24)
    parser.add_argument("--seconds", type=int, choices=range(30, 301), default=180)
    parser.add_argument("--max-load", type=float, default=max(1, (os.cpu_count() or 1) * 0.75))
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = {
        "seed": args.seed,
        "binary": args.binary,
        "image": args.image,
        "requested_rounds": args.rounds,
        "time_budget_seconds": args.seconds,
        "max_host_load": args.max_load,
        "limits": {"vms": 2, "vcpus": 2, "guest_ram_mib": 512, "cli_concurrency": 3},
        "status": "failed",
    }
    directory = Path(tempfile.mkdtemp(prefix="pvisor-chaos-")).resolve()
    chaos = Chaos(args, directory)
    try:
        sample = host_sample()
        if sample["load1"] > args.max_load or sample["memory_free_percent"] < 20:
            raise RuntimeError(f"host too busy to start: {sample}")
        chaos.setup()
        chaos.run()
        report["status"] = "passed"
    except Exception as error:
        report["error"] = str(error)
        raise
    finally:
        try:
            chaos.cleanup()
            shutil.rmtree(directory)
        except Exception as error:
            report["status"] = "failed"
            report["cleanup_error"] = str(error)
            report["retained_directory"] = str(directory)
            raise
        finally:
            report.update(
                elapsed_seconds=time.monotonic() - started,
                commands=chaos.commands,
                max_command_seconds=chaos.max_latency,
                cases=chaos.counts,
                host_samples=chaos.samples,
            )
            args.report.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
