#!/usr/bin/env python3
"""Benchmark: B-FOUR-PEER-ENG (benchmark/README.md#b-four-peer-eng), engineering A/B.

Motivation: assess four real pVisor VMs against four Docker bridge containers.
Conclusion sought: integrity, recovery, verified goodput/RTT, bounded memory and CPU.
Design: identical static worker; 512MiB hard host limits, no swap, one CPU per node;
3 warmups and 30 seeded randomized pairs; fresh four-node groups; retain failures.
"""

import argparse
import hashlib
import json
import os
import platform
import queue
import random
import re
import shutil
import statistics
import subprocess
import threading
import time
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PEERS = ",".join(f"10.231.70.{i}" for i in range(11, 15))
LIMIT = 512 * 1024 * 1024


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def call(command, *, env=None, timeout=60, check=True):
    result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{command}: exit {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result


def mapping(path):
    return {
        key: int(value) for key, value in (line.split() for line in path.read_text().splitlines())
    }


def snapshot(path):
    return {
        "cpu": mapping(path / "cpu.stat"),
        "memory": int((path / "memory.current").read_text()),
        "peak": int((path / "memory.peak").read_text()),
        "events": mapping(path / "memory.events"),
        "swap": int((path / "memory.swap.current").read_text()),
        "pids": (path / "cgroup.procs").read_text().split(),
    }


def daemon_snapshot(pids):
    result = {}
    for name, pid in pids.items():
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            result[name] = {
                "pid": pid,
                "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
                "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),
            }
        except (OSError, ValueError) as error:
            result[name] = {"pid": pid, "unavailable": repr(error)}
    return result


class Group:
    def __init__(self, config, arm, label, directory):
        self.config, self.arm, self.directory = config, arm, directory
        self.prefix = f"{config['prefix']}-{label}"
        self.names = [f"{self.prefix}-{node}" for node in range(4)]
        self.processes, self.units, self.cgroups, self.logs = [], [], [], []
        self.env = dict(os.environ, PVISOR_DOCKER_HOME=config["store"])

    def cli(self, arguments, **kwargs):
        command = [self.config["pvisor"] if self.arm == "pvisor" else "docker", *arguments]
        return call(command, env=self.env, **kwargs)

    def scope(self, node, command, suffix=""):
        unit = self.names[node] + suffix
        self.units.append(unit + ".scope")
        return [
            "systemd-run",
            "--user",
            f"--unit={unit}",
            "--scope",
            "--quiet",
            "-p",
            "MemoryMax=512M",
            "-p",
            "MemorySwapMax=0",
            "-p",
            "TasksMax=256",
            "-p",
            "CPUQuota=100%",
            "-p",
            "AllowedCPUs=4-7",
            "taskset",
            "-c",
            "4-7",
            *command,
        ]

    def group_path(self, node, suffix=""):
        if self.arm == "pvisor":
            result = call(
                [
                    "systemctl",
                    "--user",
                    "show",
                    self.names[node] + suffix + ".scope",
                    "--value",
                    "-p",
                    "ControlGroup",
                ]
            )
            relative = result.stdout.strip()
        else:
            record = json.loads(self.cli(["inspect", self.names[node]]).stdout)[0]
            assert record["HostConfig"]["Memory"] == LIMIT
            assert record["HostConfig"]["MemorySwap"] == LIMIT
            assert record["HostConfig"]["NanoCpus"] == 1000000000
            assert record["HostConfig"]["CpusetCpus"] == "4-7"
            pid = record["State"]["Pid"]
            relative = Path(f"/proc/{pid}/cgroup").read_text().strip().split(":", 2)[2]
            save(self.directory / f"inspect-{node}{suffix}.json", record)
        path = Path("/sys/fs/cgroup") / relative.lstrip("/")
        assert int((path / "memory.max").read_text()) == LIMIT
        assert (path / "memory.swap.max").read_text().strip() == "0"
        quota, period = map(int, (path / "cpu.max").read_text().split())
        assert quota == period
        cpuset = path / "cpuset.cpus.effective"
        if cpuset.exists():
            assert cpuset.read_text().strip() == "4-7"
        # The user's systemd subtree may not delegate the cpuset controller.
        # In that case taskset still constrains every inherited thread; verify
        # the real affinity rather than accepting an uninstalled property.
        affinities = {}
        for pid in (path / "cgroup.procs").read_text().split():
            for task in Path(f"/proc/{pid}/task").iterdir():
                try:
                    affinity = sorted(os.sched_getaffinity(int(task.name)))
                except ProcessLookupError:
                    continue
                assert affinity == [4, 5, 6, 7], (pid, task.name, affinity)
                affinities[task.name] = affinity
        save(
            self.directory / f"limits-{node}{suffix}.json",
            {
                "cgroup": str(path),
                "memory.max": LIMIT,
                "memory.swap.max": 0,
                "cpu.max": [quota, period],
                "cpuset": "4-7",
                "cpuset_controller_installed": cpuset.exists(),
                "verified_thread_affinities": affinities,
                "initial": snapshot(path),
            },
        )
        return path

    def start(self):
        self.directory.mkdir(parents=True, exist_ok=True)
        for node, name in enumerate(self.names):
            arguments = [
                "run",
                "--name",
                name,
                "--cpus",
                "1",
                "--memory",
                "384m" if self.arm == "pvisor" else "512m",
                "--network",
                self.config[f"{self.arm}_network"],
                "--ip",
                f"10.231.70.{11 + node}",
            ]
            if self.arm == "docker":
                arguments += [
                    "--memory-swap",
                    "512m",
                    "--cpuset-cpus",
                    "4-7",
                    "--pids-limit",
                    "256",
                ]
            arguments += [self.config[f"{self.arm}_image"], "/netbench", "serve", str(node), PEERS]
            command = [self.config["pvisor"] if self.arm == "pvisor" else "docker", *arguments]
            if self.arm == "pvisor":
                command = self.scope(node, command)
            log = (self.directory / f"server-{node}.log").open("w")
            self.logs.append(log)
            self.processes.append(
                subprocess.Popen(
                    command, env=self.env, stdout=log, stderr=subprocess.STDOUT, text=True
                )
            )
        deadline = time.monotonic() + 60
        for node, process in enumerate(self.processes):
            while f"SERVER_READY {node}" not in (self.directory / f"server-{node}.log").read_text():
                if process.poll() is not None:
                    raise RuntimeError(
                        f"server exited {process.returncode}: {(self.directory / f'server-{node}.log').read_text()}"
                    )
                if time.monotonic() > deadline:
                    raise TimeoutError("server startup timeout")
                time.sleep(0.05)
            info = self.cli(["exec", self.names[node], "/netbench", "info"]).stdout
            (self.directory / f"guest-{node}.txt").write_text(info)
            assert re.search(r"MTU:\s+1500", info), info
            if self.arm == "pvisor":
                memory_kib = int(re.search(r"MemTotal:\s+(\d+)", info)[1])
                assert memory_kib * 1024 <= LIMIT
                assert "Linux version 6." in info
            self.cgroups.append(self.group_path(node))

    def scenario(self, case, nodes=range(4)):
        directory = self.directory / case
        directory.mkdir()
        events = queue.Queue()
        processes, threads, errors = [], [], []

        def collect(process, node, file):
            with file.open("w") as output:
                for line in process.stdout:
                    output.write(line)
                    output.flush()
                    events.put((node, line.strip()))

        stop = threading.Event()
        samples = []

        def monitor():
            while not stop.is_set():
                samples.append(
                    {"time": time.monotonic(), "nodes": [snapshot(path) for path in self.cgroups]}
                )
                stop.wait(0.05)

        try:
            for node in nodes:
                command = [
                    self.config["pvisor"] if self.arm == "pvisor" else "docker",
                    "exec",
                    "-i",
                    self.names[node],
                    "/netbench",
                    "run",
                    str(node),
                    PEERS,
                    case,
                ]
                error = (directory / f"client-{node}.stderr").open("w")
                errors.append(error)
                process = subprocess.Popen(
                    command,
                    env=self.env,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=error,
                    text=True,
                )
                processes.append((node, process))
                thread = threading.Thread(
                    target=collect, args=(process, node, directory / f"client-{node}.stdout")
                )
                thread.start()
                threads.append(thread)
            ready, results = set(), {}
            deadline = time.monotonic() + 60
            while len(ready) != len(processes):
                if time.monotonic() > deadline:
                    raise TimeoutError("client readiness timeout")
                try:
                    node, line = events.get(timeout=0.1)
                    assert line == f"READY {node} {case}", line
                    ready.add(node)
                except queue.Empty:
                    if any(process.poll() is not None for _, process in processes):
                        raise RuntimeError("client failed before GO")
            before = [snapshot(path) for path in self.cgroups]
            daemon_before = daemon_snapshot(self.config["daemon_pids"])
            monitor_thread = threading.Thread(target=monitor)
            monitor_thread.start()
            go = time.monotonic()
            for _, process in processes:
                process.stdin.write("GO\n")
                process.stdin.flush()
                process.stdin.close()
            go_skew = time.monotonic() - go
            for node, process in processes:
                status = process.wait(timeout=max(1, deadline - time.monotonic()))
                if status:
                    raise RuntimeError(f"client {node} exit {status}")
            for thread in threads:
                thread.join(timeout=2)
            while not events.empty():
                node, line = events.get_nowait()
                results[node] = json.loads(line)
            assert len(results) == len(processes)
            assert all(
                result["verified"] and result["scenario"] == case for result in results.values()
            )
            after = [snapshot(path) for path in self.cgroups]
            daemon_after = daemon_snapshot(self.config["daemon_pids"])
            for previous, current in zip(before, after):
                assert current["peak"] <= LIMIT and current["swap"] == 0
                for key in ("oom", "oom_kill", "max"):
                    assert current["events"].get(key, 0) == previous["events"].get(key, 0) == 0
            stop.set()
            monitor_thread.join()
            rtts = sorted(value for result in results.values() for value in result["rtt_us"])
            seconds = max(result["seconds"] for result in results.values())
            payload = sum(result["bytes"] for result in results.values())
            expected = {
                "rpc": (1500 * 256, 1500),
                "bulk": (3 * 4194304, 0),
                "ring": (200 * 256, 200),
                "slow": (500 * 256, 500),
                "healthy": (500 * 256, 500),
            }
            for result in results.values():
                assert (result["bytes"], len(result["rtt_us"])) == expected[case]
            summary = {
                "case": case,
                "nodes": results,
                "payload_bytes": payload,
                "seconds": seconds,
                "goodput_mbps": payload * 8 / seconds / 1e6,
                "rtt_p50_us": statistics.median(rtts) if rtts else None,
                "rtt_p95_us": rtts[int(len(rtts) * 0.95)] if rtts else None,
                "cpu_seconds": sum(
                    current["cpu"]["usage_usec"] - previous["cpu"]["usage_usec"]
                    for previous, current in zip(before, after)
                )
                / 1e6,
                "memory_peak_bytes": [current["peak"] for current in after],
                "memory_current_bytes": [current["memory"] for current in after],
                "go_skew_seconds": go_skew,
                "before": before,
                "after": after,
                "daemon_before": daemon_before,
                "daemon_after": daemon_after,
            }
            save(directory / "samples.json", samples)
            save(directory / "result.json", summary)
            return summary
        finally:
            stop.set()
            if "monitor_thread" in locals():
                monitor_thread.join(timeout=2)
            for _, process in processes:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
            for error in errors:
                error.close()
            for thread in threads:
                thread.join(timeout=2)

    def fault_checks(self):
        for node in range(4):
            for target in range(4):
                if node != target:
                    result = self.cli(
                        [
                            "exec",
                            self.names[node],
                            "/netbench",
                            "resolve",
                            self.names[target],
                            f"10.231.70.{11 + target}",
                        ]
                    )
                    (self.directory / f"dns-{node}-{target}.txt").write_text(result.stdout)
        self.scenario("slow")
        self.cli(["stop", self.names[1]])
        # The offline cgroup disappears; healthy nodes must still communicate.
        original = self.cgroups.copy()
        offline = original[1]
        self.cgroups = [path for index, path in enumerate(original) if index != 1]
        self.scenario("healthy", nodes=[0, 2, 3])
        self.cgroups = original
        if self.arm == "pvisor":
            result = call(
                self.scope(1, [self.config["pvisor"], "start", self.names[1]], "r"), env=self.env
            )
            (self.directory / "restart.log").write_text(result.stdout + result.stderr)
            self.cgroups[1] = self.group_path(1, "r")
        else:
            self.cli(["start", self.names[1]])
            self.cgroups[1] = self.group_path(1, "r")
        assert self.cgroups[1] != offline or self.arm == "docker"
        time.sleep(0.4)
        self.scenario("rpc")

    def close(self):
        failures = []
        for name in self.names:
            result = self.cli(["rm", "-f", name], check=False, timeout=30)
            if result.returncode:
                failures.append(result.stderr)
        for process in self.processes:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for log in self.logs:
            log.close()
        save(self.directory / "cleanup.json", {"failures": failures})
        if failures:
            raise RuntimeError(f"cleanup failed: {failures}")


def analyze(pairs):
    rng = random.Random(4096)
    result = {}
    for case in ("rpc", "bulk", "ring"):
        result[case] = {}
        for metric in ("goodput_mbps", "rtt_p50_us", "rtt_p95_us", "cpu_seconds"):
            a = [pair["pvisor"][case][metric] for pair in pairs]
            b = [pair["docker"][case][metric] for pair in pairs]
            if a[0] is None:
                continue
            differences = [left - right for left, right in zip(a, b)]
            intervals = sorted(
                statistics.median(rng.choices(differences, k=len(differences))) for _ in range(5000)
            )
            result[case][metric] = {
                "pvisor_median": statistics.median(a),
                "docker_median": statistics.median(b),
                "pvisor_minus_docker_paired_median": statistics.median(differences),
                "ci95": [intervals[125], intervals[4874]],
                "pvisor_range": [min(a), max(a)],
                "docker_range": [min(b), max(b)],
            }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--preflight", action="store_true")
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    os.sched_setaffinity(0, {8, 9})
    rng = random.Random(20261024)
    schedule = []
    for index in range(1 if args.preflight else 33):
        order = ["pvisor", "docker"]
        rng.shuffle(order)
        schedule.append(
            {"index": index, "warmup": not args.preflight and index < 3, "order": order}
        )
    source = output / "source"
    shutil.copytree(
        REPO / "benchmark/peer",
        source / "benchmark/peer",
        ignore=shutil.ignore_patterns(".data", "__pycache__"),
    )
    for crate in (
        "pvisor",
        "pvisor-cli",
        "pvisor-vm",
        "pvisor-overlaynet",
        "pvisor-guest",
        "pvisor-core",
        "pvisor-overlay-core",
        "pvisor-overlayfs",
        "pvisor-journal",
        "pvisor-gateway",
    ):
        shutil.copytree(
            REPO / f"crates/{crate}",
            source / f"crates/{crate}",
            ignore=shutil.ignore_patterns("target"),
        )
    for filename in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(REPO / filename, source / filename)
    hashes = {
        str(path.relative_to(source)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in source.rglob("*")
        if path.is_file()
    }
    for path, expected in config["artifact_sha256"].items():
        assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected, path
    save(
        output / "manifest.json",
        {
            "benchmark": "B-FOUR-PEER-ENG",
            "kernel": platform.uname()._asdict(),
            "config": config,
            "schedule": schedule,
            "source_sha256": hashes,
            "cpuinfo": Path("/proc/cpuinfo").read_text(),
            "initial_loadavg": Path("/proc/loadavg").read_text(),
            "initial_processes": call(["ps", "-eo", "pid,comm,pcpu,pmem"]).stdout,
            "exclusions": "none; any failed correctness/resource check stops batch and retains previous diagnostics",
            "scope": "fresh four-node groups, no concurrent builds/tests; controller CPU8-9; nodes CPU4-7, each one-CPU quota/512MiB hard limit/swap0",
        },
    )
    pairs = []
    try:
        for entry in schedule:
            pair = {}
            for arm in entry["order"]:
                directory = output / f"{entry['index']:02d}-{arm}"
                group = Group(config, arm, f"{uuid.uuid4().hex[:6]}", directory)
                try:
                    group.start()
                    if args.preflight:
                        group.fault_checks()
                        # Fault checks already used the rpc result directory.
                        pair[arm] = {"bulk": group.scenario("bulk"), "ring": group.scenario("ring")}
                    else:
                        pair[arm] = {case: group.scenario(case) for case in ("rpc", "bulk", "ring")}
                finally:
                    group.close()
                save(directory / "result.json", pair[arm])
            if not entry["warmup"]:
                pairs.append(pair)
            print(f"completed round {entry['index'] + 1}/{len(schedule)}", flush=True)
        save(
            output / "summary.json",
            {
                "pairs": len(pairs),
                "failures": 0,
                "excluded": 0,
                "metrics": analyze(pairs) if not args.preflight else {"preflight": "passed"},
            },
        )
        print((output / "summary.json").read_text(), flush=True)
    except BaseException as error:
        save(output / "failure.json", {"error": repr(error)})
        raise


if __name__ == "__main__":
    main()
