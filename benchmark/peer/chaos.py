#!/usr/bin/env python3
"""Benchmark: B-PEER-CHAOS-ENG (benchmark/README.md#b-peer-chaos-eng), diagnostic.

Motivation: verify integrity, fault isolation and recovery during sustained traffic.
Conclusion sought: bounded-window correctness evidence, not a reliability guarantee.
Design: four persistent KVM guests, 512MiB/swap0/one CPU each; 30 minutes with
30 predeclared freeze/stop/crash/recreate faults; fail closed, retain every result.
"""

import argparse
import concurrent.futures
import copy
import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import threading
import time
from pathlib import Path

from four_node import PEERS, REPO, Group, call, foreign_work, save, snapshot


def wait_until(check, timeout=60):
    deadline = time.monotonic() + timeout
    while not check():
        if time.monotonic() >= deadline:
            raise TimeoutError("condition did not become ready")
        time.sleep(0.02)


def owner(config, name):
    for path in (Path(config["store"]) / "instances").glob("*/instance.json"):
        record = json.loads(path.read_text())
        if record.get("name") == name:
            return path, record
    raise RuntimeError(f"missing instance record: {name}")


def process_details(cgroup):
    result = []
    for pid in (cgroup / "cgroup.procs").read_text().split():
        try:
            root = Path("/proc") / pid
            result.append(
                {
                    "pid": int(pid),
                    "comm": (root / "comm").read_text().strip(),
                    "threads": len(list((root / "task").iterdir())),
                    "fds": len(list((root / "fd").iterdir())),
                    "starttime": (root / "stat").read_text().rsplit(")", 1)[1].split()[19],
                }
            )
        except FileNotFoundError:
            continue
    return result


def cgroup_empty(path):
    # systemd may remove the scope between any existence check and open().
    # A removed scope has no remaining tasks, which is the expected crash state.
    try:
        return not (path / "cgroup.procs").read_text().strip()
    except FileNotFoundError:
        return True


class Chaos:
    def __init__(self, config, output):
        self.config, self.output = config, output
        self.group = Group(config, "pvisor", "soak", output / "group")
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.active = {}
        self.monitor_errors = []
        self.udp_processes, self.udp_logs = [], []
        self.hold_processes, self.hold_logs = [], []
        self.results, self.faults, self.udp_results = [], [], []
        self.iteration = 0
        self.udp_round = 0

    def view(self, directory, *, healthy=False):
        group = copy.copy(self.group)
        group.directory = directory
        directory.mkdir(parents=True, exist_ok=True)
        with self.lock:
            group.cgroups = [
                path for node, path in sorted(self.active.items()) if not healthy or node != 1
            ]
        return group

    def monitor(self):
        with (self.output / "resources.jsonl").open("w") as file:
            while not self.stop.is_set():
                with self.lock:
                    active = dict(self.active)
                record = {"time": time.time(), "nodes": {}}
                try:
                    for node, path in active.items():
                        try:
                            state = snapshot(path)
                            assert state["peak"] <= 512 * 1024**2 and state["swap"] == 0
                            assert all(
                                state["events"].get(key, 0) == 0
                                for key in ("oom", "oom_kill", "max")
                            )
                            record["nodes"][node] = {**state, "processes": process_details(path)}
                        except FileNotFoundError:
                            # Removal races are allowed only for the fault target.
                            if node != 1:
                                raise
                    record["foreign_work"] = foreign_work(
                        {str(Path(self.config["pvisor"]).resolve())}
                    )
                    file.write(json.dumps(record) + "\n")
                    file.flush()
                except BaseException as error:
                    self.monitor_errors.append(repr(error))
                    self.stop.set()
                self.stop.wait(1)

    def check(self):
        if self.monitor_errors:
            raise RuntimeError(f"resource monitor failure: {self.monitor_errors}")
        for node, process, path in self.udp_processes:
            if node != 1 and process.poll() is not None:
                raise RuntimeError(f"UDP server {node} died: {path.read_text()}")
        for node, process, path in self.hold_processes:
            if process.poll() is not None and process.returncode != 0:
                raise RuntimeError(f"persistent TCP {node} failed: {path.read_text()}")

    def hold_start(self, seconds):
        for node, target in ((0, 2), (2, 3), (3, 0)):
            path = self.output / f"persistent-tcp-{node}.log"
            log = path.open("w")
            self.hold_logs.append(log)
            process = subprocess.Popen(
                [
                    self.config["pvisor"],
                    "exec",
                    self.group.names[node],
                    "/udpbench",
                    "hold",
                    str(node),
                    str(target),
                    f"10.231.70.{11 + target}",
                    str(seconds),
                ],
                env=self.group.env,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            self.hold_processes.append((node, process, path))

            def ready():
                if process.poll() is not None:
                    raise RuntimeError(f"persistent TCP startup failed: {path.read_text()}")
                return f"HOLD_READY {node}" in path.read_text()

            wait_until(ready)

    def udp_server(self, node, label):
        path = self.output / f"udp-server-{node}-{label}.log"
        log = path.open("w")
        self.udp_logs.append(log)
        process = subprocess.Popen(
            [
                self.config["pvisor"],
                "exec",
                self.group.names[node],
                "/udpbench",
                "serve",
                str(node),
            ],
            env=self.group.env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        self.udp_processes.append((node, process, path))

        def ready():
            if process.poll() is not None:
                raise RuntimeError(f"UDP server {node} failed: {path.read_text()}")
            return f"UDP_READY {node}" in path.read_text()

        wait_until(ready)

    def udp(self, directory):
        directory.mkdir(parents=True, exist_ok=True)
        token = 20261024000000 + self.udp_round * 16
        self.udp_round += 1

        def one(node):
            result = self.group.cli(
                [
                    "exec",
                    self.group.names[node],
                    "/udpbench",
                    "run",
                    str(node),
                    PEERS,
                    str(token + node),
                ],
                timeout=30,
                check=False,
            )
            (directory / f"node-{node}.stdout").write_text(result.stdout)
            (directory / f"node-{node}.stderr").write_text(result.stderr)
            assert result.returncode == 0, (node, result.returncode, result.stderr)
            value = json.loads(result.stdout)
            assert value["verified"] and value["node"] == node and value["requests"] == 540
            assert value["attempts"] >= value["requests"]
            assert value["bytes"] == 318240
            return value

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(one, range(4)))
        save(directory / "result.json", results)
        self.udp_results.extend(results)

    def dns(self, directory):
        directory.mkdir(parents=True, exist_ok=True)
        for node in range(4):
            for target in range(4):
                if node == target:
                    continue
                result = self.group.cli(
                    [
                        "exec",
                        self.group.names[node],
                        "/netbench",
                        "resolve",
                        self.group.names[target],
                        f"10.231.70.{11 + target}",
                    ]
                )
                (directory / f"{node}-{target}.txt").write_text(result.stdout)

    def scenario(self, directory, case, *, healthy=False, after_go=None):
        result = self.view(directory, healthy=healthy).scenario(
            case,
            nodes=[0, 2, 3] if healthy else range(4),
            after_go=after_go,
        )
        self.results.append({"directory": str(directory.relative_to(self.output)), **result})
        self.check()
        return result

    def archive_node(self, directory):
        path, record = owner(self.config, self.group.names[1])
        directory.mkdir(parents=True, exist_ok=False)
        for file in path.parent.iterdir():
            if file.is_file() and (
                file.suffix in {".json", ".log"} or file.name.startswith("boot-")
            ):
                shutil.copy2(file, directory / file.name)
        result = Path(self.config["store"]) / "results" / f"{record['id']}.json"
        if result.exists():
            shutil.copy2(result, directory / "completion.json")
        return record

    def restart(self, directory, index, recreate):
        label = f"r{index}"
        if recreate:
            arguments = [
                "run",
                "--name",
                self.group.names[1],
                "--cpus",
                "1",
                "--memory",
                "384m",
                "--network",
                self.config["pvisor_network"],
                "--ip",
                "10.231.70.12",
                self.config["pvisor_image"],
                "/netbench",
                "serve",
                "1",
                PEERS,
            ]
            log = (directory / "recreated-server.log").open("w")
            self.group.logs.append(log)
            process = subprocess.Popen(
                self.group.scope(1, [self.config["pvisor"], *arguments], label),
                env=self.group.env,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            self.group.processes.append(process)

            def ready():
                if process.poll() is not None:
                    raise RuntimeError(f"recreated node exited {process.returncode}")
                return "SERVER_READY 1" in (directory / "recreated-server.log").read_text()

            wait_until(ready)
        else:
            result = call(
                self.group.scope(1, [self.config["pvisor"], "start", self.group.names[1]], label),
                env=self.group.env,
            )
            (directory / "restart.log").write_text(result.stdout + result.stderr)
        path = self.group.group_path(1, label)
        self.group.cgroups[1] = path
        with self.lock:
            self.active[1] = path
        # Discovery's documented positive/negative cache TTL is 250ms.
        time.sleep(0.4)
        self.udp_server(1, label)
        return owner(self.config, self.group.names[1])[1]

    def fault(self, index, kind):
        directory = self.output / f"fault-{index:02d}-{kind}"
        directory.mkdir()
        started = time.monotonic()
        entry = {"index": index, "kind": kind, "start": time.time()}
        if kind == "freeze":
            path = self.group.cgroups[1]

            def freeze():
                (path / "cgroup.freeze").write_text("1")
                try:
                    wait_until(
                        lambda: "frozen 1" in (path / "cgroup.events").read_text(), timeout=5
                    )
                    frozen = time.monotonic()
                    self.scenario(directory / "during-fault", "healthy", healthy=True)
                    elapsed = time.monotonic() - frozen
                    assert elapsed < 10, "healthy peers stalled during freeze"
                    time.sleep(max(0, 3 - elapsed))
                    entry["frozen_seconds"] = time.monotonic() - frozen
                    entry["frozen_events"] = (path / "cgroup.events").read_text()
                finally:
                    (path / "cgroup.freeze").write_text("0")

            # All 12 connections have written their request header before GO.
            # Freeze immediately after GO, then require those connections to finish.
            self.scenario(directory / "established-connections", "rpc", after_go=freeze)
        else:
            before = self.archive_node(directory / "before-owner")
            with self.lock:
                path = self.active.pop(1)
            if kind == "crash":
                victims = []
                for pid in (path / "cgroup.procs").read_text().split():
                    for fd in (Path("/proc") / pid / "fd").iterdir():
                        try:
                            if "kvm" in os.readlink(fd):
                                victims.append(int(pid))
                                break
                        except FileNotFoundError:
                            continue
                assert len(victims) == 1, victims
                entry["victim"] = process_details(path)
                # A pidfd pins exactly the verified, owned KVM process identity.
                pidfd = os.pidfd_open(victims[0])
                try:
                    signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                finally:
                    os.close(pidfd)
                wait_until(lambda: cgroup_empty(path), 10)
            else:
                self.group.cli(["stop", self.group.names[1]])
            self.archive_node(directory / "stopped-owner")
            if kind == "recreate":
                self.group.cli(["rm", self.group.names[1]])
            self.scenario(directory / "during-fault", "healthy", healthy=True)
            after = self.restart(directory, index, kind == "recreate")
            entry["before_instance"] = before["id"]
            entry["after_instance"] = after["id"]
            entry["before_generation"] = before["generation"]
            entry["after_generation"] = after["generation"]
            assert before["generation"] != after["generation"]
            assert (before["id"] != after["id"]) == (kind == "recreate")
        self.dns(directory / "dns-after")
        self.scenario(directory / "recovered", "rpc")
        self.udp(directory / "udp-recovered")
        self.scenario(directory / "ring-recovered", "ring")
        entry.update(verified=True, seconds=time.monotonic() - started)
        self.faults.append(entry)
        save(directory / "result.json", entry)
        print(f"fault {index + 1}: {kind} verified ({entry['seconds']:.2f}s)", flush=True)

    def run(self, seconds, fault_count):
        started = time.monotonic()
        self.group.start()
        self.active = dict(enumerate(self.group.cgroups))
        for node in range(4):
            self.udp_server(node, "initial")
        monitor = threading.Thread(target=self.monitor)
        monitor.start()
        begin = time.monotonic()
        baseline = {node: process_details(self.group.cgroups[node]) for node in (0, 2, 3)}
        try:
            self.dns(self.output / "dns-initial")
            self.hold_start(seconds)
            next_fault = 0
            while time.monotonic() - begin < seconds or next_fault < fault_count:
                self.check()
                elapsed = time.monotonic() - begin
                if next_fault < fault_count and elapsed >= next_fault * seconds / fault_count:
                    self.fault(next_fault, ("freeze", "stop", "crash", "recreate")[next_fault % 4])
                    next_fault += 1
                directory = self.output / f"traffic-{self.iteration:05d}"
                case = ("rpc", "bulk-long", "ring", "slow")[self.iteration % 4]
                self.scenario(directory, case)
                self.udp(directory / "udp")
                self.iteration += 1
                if self.iteration % 10 == 0:
                    print(
                        f"traffic {self.iteration}, elapsed {time.monotonic() - begin:.1f}s",
                        flush=True,
                    )
            final = {node: process_details(self.group.cgroups[node]) for node in (0, 2, 3)}
            for node, processes in baseline.items():
                identity = {
                    (p["pid"], p["starttime"]) for p in processes if p["comm"].startswith("pvisor")
                }
                assert identity <= {(p["pid"], p["starttime"]) for p in final[node]}, (
                    node,
                    identity,
                )
            self.check()
            persistent_tcp = []
            for node, process, path in self.hold_processes:
                assert process.wait(timeout=15) == 0, path.read_text()
                lines = path.read_text().splitlines()
                assert lines[0] == f"HOLD_READY {node}" and len(lines) == 2
                result = json.loads(lines[1])
                assert (
                    result["verified"] and result["seconds"] >= seconds and result["requests"] > 0
                )
                persistent_tcp.append(result)
            return {
                "verified": True,
                "traffic_seconds": time.monotonic() - begin,
                "total_seconds": time.monotonic() - started,
                "traffic_rounds": self.iteration,
                "faults": self.faults,
                "failures": 0,
                "excluded": 0,
                "tcp_payload_bytes": sum(value["payload_bytes"] for value in self.results),
                "tcp_echo_requests": sum(
                    len(node["rtt_us"])
                    for value in self.results
                    for node in value["nodes"].values()
                ),
                "udp_requests": sum(value["requests"] for value in self.udp_results),
                "udp_attempts": sum(value["attempts"] for value in self.udp_results),
                "udp_timeouts": sum(value["timeouts"] for value in self.udp_results),
                "udp_duplicates": sum(value["duplicates"] for value in self.udp_results),
                "udp_payload_bytes": sum(value["bytes"] for value in self.udp_results),
                "persistent_tcp": persistent_tcp,
                "persistent_processes_before": baseline,
                "persistent_processes_after": final,
            }
        finally:
            self.stop.set()
            monitor.join(timeout=5)

    def close(self):
        for path in self.group.cgroups:
            if path.exists() and (path / "cgroup.freeze").exists():
                (path / "cgroup.freeze").write_text("0")
        self.group.close()
        for _, process, _ in self.udp_processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
        for log in self.udp_logs:
            log.close()
        for _, process, _ in self.hold_processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
        for log in self.hold_logs:
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=1800)
    parser.add_argument("--faults", type=int, default=30)
    args = parser.parse_args()
    assert args.seconds > 0 and args.faults > 0
    config = json.loads(args.config.read_text())
    assert not config.get("profile_library")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    os.sched_setaffinity(0, {8, 9})
    for path, expected in config["artifact_sha256"].items():
        assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == expected, path
    source = output / "harness"
    source.mkdir()
    for name in ("chaos.py", "four_node.py", "four_node_worker.rs", "udp_worker.rs"):
        shutil.copy2(REPO / "benchmark/peer" / name, source / name)
    save(
        output / "manifest.json",
        {
            "benchmark": "B-PEER-CHAOS-ENG",
            "kernel": platform.uname()._asdict(),
            "config": config,
            "seconds": args.seconds,
            "fault_count": args.faults,
            "schedule": [
                ("freeze", "stop", "crash", "recreate")[i % 4] for i in range(args.faults)
            ],
            "harness_sha256": {
                path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                for path in source.iterdir()
            },
            "exclusions": "none; first unexpected failure stops cohort; no automatic retry",
        },
    )
    chaos = Chaos(config, output)
    try:
        summary = chaos.run(args.seconds, args.faults)
    except BaseException as error:
        save(
            output / "failure.json",
            {
                "error": repr(error),
                "completed_faults": chaos.faults,
                "traffic_rounds": chaos.iteration,
            },
        )
        raise
    finally:
        chaos.close()
    save(output / "summary.json", summary)
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
