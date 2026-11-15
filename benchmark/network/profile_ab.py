#!/usr/bin/env python3
"""Benchmark: B-NETWORK-PROFILE-ENG, engineering A/B.

Motivation: identify which real VM network workloads benefit from speed-oriented
compilation of pvisor-overlaynet and smoltcp, without changing the packet engine.
Conclusion sought: paired latency/goodput differences with bootstrap 95% CIs.
Design: fixed static worker and loopback origin, fresh KVM guest per sample,
seeded random conditions/scenarios, 3 warmups and 30 independent rounds.
This dedicated paired protocol does not execute/publish the unprepared user table.
"""

import argparse
import datetime
import hashlib
import ipaddress
import json
import math
import os
import platform
import random
import shutil
import signal
import socket
import statistics
import subprocess
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCENARIOS = (
    "connect",
    "rpc_fresh",
    "rpc_keepalive",
    "dns",
    "stream",
    "download_1",
    "upload_1",
    "download_8",
    "upload_8",
    "download_32",
    "upload_32",
)
ARMS = ("native", "release", "net_o3")


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def cpu_list(value):
    values = [int(part) for part in value.split(",")]
    if not values or len(values) != len(set(values)) or min(values) < 0:
        raise argparse.ArgumentTypeError("expected distinct comma-separated CPU numbers")
    return values


def builds():
    found = []
    for item in Path("/proc").iterdir():
        if item.name.isdigit():
            try:
                name = (item / "comm").read_text().strip()
                if name in ("cargo", "rustc", "cargo-zigbuild"):
                    found.append(dict(pid=int(item.name), command=name))
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                pass
    return found


def processes(root):
    """Follow all thread child lists; a Tokio worker may have forked a runner."""
    pending, seen = [root], set()
    while pending:
        pid = pending.pop()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            for task in Path(f"/proc/{pid}/task").iterdir():
                pending.extend(int(p) for p in (task / "children").read_text().split())
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return seen


def observe(root):
    snapshot = dict(monotonic_ns=time.monotonic_ns(), loadavg=os.getloadavg(), threads=[])
    for pid in processes(root):
        try:
            for task in Path(f"/proc/{pid}/task").iterdir():
                raw = (task / "stat").read_text()
                name, rest = raw[raw.index("(") + 1 :].rsplit(")", 1)
                fields = rest.split()
                snapshot["threads"].append(
                    dict(
                        pid=pid,
                        tid=int(task.name),
                        name=name,
                        cpu_ticks=int(fields[11]) + int(fields[12]),
                        start_ticks=int(fields[19]),
                    )
                )
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            pass
    return snapshot


def parse_results(text, arm, expected=SCENARIOS):
    records = [
        json.loads(line.removeprefix("NET_RESULT "))
        for line in text.splitlines()
        if line.startswith("NET_RESULT ")
    ]
    names = [r["scenario"] for r in records]
    wanted = set(expected) - ({"dns"} if arm == "native" else set())
    if len(names) != len(set(names)) or set(names) != wanted:
        raise ValueError(f"incomplete/duplicate scenarios: {names}, expected {sorted(wanted)}")
    if text.splitlines().count("NET_COMPLETE") != 1:
        raise ValueError("missing unique completion marker")
    ready = [
        json.loads(line.removeprefix("NET_READY "))
        for line in text.splitlines()
        if line.startswith("NET_READY ")
    ]
    if len(ready) != 1 or ready[0]["kind"] != ("native" if arm == "native" else "vm"):
        raise ValueError("missing correct client readiness")
    for record in records:
        name = record["scenario"]
        if record["elapsed_ns"] <= 0:
            raise ValueError("nonpositive clock")
        if name.startswith(("upload_", "download_")):
            expected_ops = int(name.rsplit("_", 1)[-1])
            if record["bytes"] != 64 * 1024 * 1024 or record["operations"] != expected_ops:
                raise ValueError("incorrect bulk byte/connection count")
        else:
            operations, byte_count, samples = {
                "connect": (128, 0, 128),
                "rpc_fresh": (128, 128 * 2048, 128),
                "rpc_keepalive": (256, 256 * 2048, 256),
                "dns": (128, 0, 128),
                "stream": (32, 2048, 31),
            }[name]
            if (record["operations"], record["bytes"], len(record["samples_ns"])) != (
                operations,
                byte_count,
                samples,
            ):
                raise ValueError("incorrect operation/byte/sample count")
            if any(not isinstance(v, int) or v <= 0 for v in record["samples_ns"]):
                raise ValueError("invalid per-operation clock")
            if name == "stream" and not 0 < record["first_byte_ns"] <= record["elapsed_ns"]:
                raise ValueError("invalid first-byte clock")
    return records


def metric(record, name=None):
    name = name or record["scenario"]
    if name.startswith(("upload_", "download_")):
        return record["bytes"] / (1024 * 1024) * 1e9 / record["elapsed_ns"], "MiB/s"
    if name == "stream":
        return record["elapsed_ns"] / 1e6, "ms"
    if name == "stream_first_byte":
        return record["first_byte_ns"] / 1e6, "ms"
    return statistics.median(record["samples_ns"]) / 1e6, "ms"


def validate_bundle(bundle, port):
    run = bundle["run"]
    if (
        run["executor"]["kind"] != "virtual_machine"
        or run["state"] != "completed"
        or run["exit_code"] != 0
        or run["metrics"]["resource.vm_memory_bytes"] != 256 * 1024 * 1024
        or not bundle["safety"]["network_non_bypassable"]
        or bundle["network"]["interception"]["driver"] != "vm-smoltcp"
        or bundle["network"]["interception"]["strength"] != "non-bypassable"
    ):
        raise ValueError("VM/network boundary or completed execution not observed")
    policy = bundle["network"]["policy"]
    if (
        policy["mode"] != "allowlist"
        or policy["limits"]
        or policy["rules"]
        != [dict(host="localhost.localdomain", ports=[port], allow_private_ips=True)]
    ):
        raise ValueError("unexpected endpoint policy or bandwidth limit")
    counters = bundle["network"]["intercepted"]
    # Exact independent byte oracle: three 64 MiB bulk transfers per direction,
    # RPC payloads, 212 request headers, 32 stream events and 41 upload receipts.
    expected = dict(
        policy_allowed=340,
        policy_denied=0,
        failures=0,
        dns_queries=129,
        dns_answers=129,
        tcp_flows_opened=340,
        tcp_flows_denied=0,
        tcp_connect_failures=0,
        bytes_guest_to_host=201723200,
        bytes_host_to_guest=201722184,
        unsupported_packets=0,
        active_tcp_flows=0,
    )
    if any(counters[key] != value for key, value in expected.items()):
        raise ValueError(f"incomplete/unexpected network accounting: {counters}")
    return counters


def clusters(values):
    """Predeclared descriptive split, not outlier removal or a significance test."""
    ordered = sorted(values)
    threshold = max(0.005, 0.1 * statistics.median(ordered))
    splits = [i for i in range(3, len(ordered) - 2) if ordered[i] - ordered[i - 1] > threshold]
    groups, previous = [], 0
    for stop in splits + [len(ordered)]:
        if stop - previous >= 3:
            groups.append(ordered[previous:stop])
            previous = stop
    if previous != len(ordered):
        return [ordered]
    return groups or [ordered]


def paired_ci(left, right, seed=20261115):
    if len(left) != len(right) or not left:
        raise ValueError("nonempty paired observations required")
    deltas = [b - a for a, b in zip(left, right)]
    rng = random.Random(seed)
    bootstrap = sorted(statistics.median(rng.choices(deltas, k=len(deltas))) for _ in range(5000))
    return dict(
        median_delta=statistics.median(deltas),
        ci95=[bootstrap[124], bootstrap[4874]],
        median_paired_percent=statistics.median((b / a - 1) * 100 for a, b in zip(left, right)),
    )


def summarize(trials, rounds):
    formal = [t for t in trials if t["phase"] == "formal"]
    if len(formal) != rounds * len(ARMS):
        raise ValueError("incomplete formal cohort")
    by_arm = {arm: {} for arm in ARMS}
    for trial in formal:
        arm = by_arm[trial["arm"]]
        if trial["round"] in arm:
            raise ValueError("duplicate formal round")
        arm[trial["round"]] = {r["scenario"]: r for r in trial["results"]}
    for arm in ARMS:
        if set(by_arm[arm]) != set(range(rounds)):
            raise ValueError("missing formal round")
    rows = []
    for name in (*SCENARIOS, "stream_first_byte", "stream_gap"):
        values, unit = {}, None
        for arm in ARMS:
            if name == "dns" and arm == "native":
                continue
            values[arm] = []
            for index in range(rounds):
                key = "stream" if name.startswith("stream_") else name
                value, unit = metric(by_arm[arm][index][key], name)
                values[arm].append(value)
        statistics_by_arm = {}
        for arm, numbers in values.items():
            groups = clusters(numbers)
            statistics_by_arm[arm] = dict(
                n=len(numbers),
                median=statistics.median(numbers),
                p95_reference=sorted(numbers)[math.ceil(len(numbers) * 0.95) - 1],
                clusters=[dict(n=len(g), median=statistics.median(g)) for g in groups],
                values=numbers,
            )
        rows.append(
            dict(
                scenario=name,
                unit=unit,
                arms=statistics_by_arm,
                comparison=paired_ci(values["release"], values["net_o3"]),
            )
        )
    return dict(schema=1, benchmark="B-NETWORK-PROFILE-ENG", valid=True, rounds=rounds, rows=rows)


def report(summary, destination):
    lines = [
        "# 网络热路径编译优化：真实 KVM 场景对照",
        "",
        "## Motivation",
        "",
        "比较默认 release 与仅将 pvisor-overlaynet、smoltcp 设为 opt-level=3 的制品。",
        "原生条件仅作绝对参考；网络内部时钟不包含 VM 启停。",
        "",
        "## 实验设计",
        "",
        f"每条件 {summary['rounds']} 个独立正式样本；3 轮预热不计统计。",
        "同轮随机交替、同负载与亲和性，2 vCPU/256 MiB；无 TLS/WAN/peer。",
        "正值差异表示候选值更高；MiB/s 越高越好，ms 越低越好。",
        "CI 为 5,000 次配对 bootstrap；跨零写未检出差异。",
        "多簇条件报告各簇数量与中位数，不用单个中位数概括。",
        "",
        "## 实验数据和分析",
        "",
        "| 场景 | 单位 | 原生参考 | release | 网络 O3 | 配对差异 [95% CI] | 配对变化 |",
        "|---|---|---:|---:|---:|---:|---:|",
    ]
    for row in summary["rows"]:

        def level(arm):
            data = row["arms"].get(arm)
            if data is None:
                return "—"
            groups = data["clusters"]
            return "；".join(f"{g['median']:.3f} ({g['n']})" for g in groups)

        comp = row["comparison"]
        lo, hi = comp["ci95"]
        interpretation = "未检出差异" if lo <= 0 <= hi else f"{comp['median_paired_percent']:+.1f}%"
        lines.append(
            f"| {row['scenario']} | {row['unit']} | {level('native')} | {level('release')} | "
            f"{level('net_o3')} | {comp['median_delta']:+.3f} [{lo:+.3f}, {hi:+.3f}] | {interpretation} |"
        )
    lines += [
        "",
        "完整逐请求时钟、逐样本 JSON、命令日志、源码/制品摘要和宿主观察见本目录。",
        "CPU 观察为 /proc 周期快照，不是完整进程 CPU accounting；其他宿主负载并未被隔离。",
        "不将该批次与历史数字拼接，不由局部编译变化推导替换协议栈或完整 performance profile 的收益。",
        "",
    ]
    destination.write_text("\n".join(lines))


def clean_env(folder):
    env = os.environ.copy()
    for name in list(env):
        if name.lower().endswith("_proxy") or name.startswith(
            ("PVISOR_", "CLAUDE_", "ANTHROPIC_", "OPENAI_")
        ):
            env.pop(name)
    for name, child in (
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("PVISOR_RUN_HOME", "runs"),
    ):
        path = folder / child
        path.mkdir(mode=0o700)
        env[name] = str(path)
    return env


def trial(args, arm, phase, index, order, port, server, output):
    # Sampling is Linux-only; keep offline contracts importable on other hosts.
    import resource

    folder = output / f"{phase}-{index:02d}-{arm}"
    folder.mkdir()
    workspace = folder / "workspace"
    workspace.mkdir()
    env = clean_env(folder)
    workload = ["client", "native" if arm == "native" else "vm", str(port), ",".join(order)]
    if arm == "native":
        command = [str(output / "frozen/worker"), *workload]
    else:
        rootfs = folder / "rootfs"
        (rootfs / "bench").mkdir(parents=True)
        shutil.copy2(output / "frozen/worker", rootfs / "bench/net-worker")
        command = [
            str(output / f"frozen/{arm}"),
            "spawn",
            "--no-agent-defaults",
            "--vm",
            "--rootfs",
            str(rootfs),
            "--vm-library-dir",
            str(args.firmware),
            "--cpu",
            "2",
            "--memory",
            "256MiB",
            "--stdio",
            "inherit",
            "--timeout",
            "120s",
            "--stage",
            str(folder / "stage"),
            "--overlaynet",
            "auto",
            "--overlaynet-policy",
            "allowlist",
            "--overlaynet-rule",
            f'host = "localhost.localdomain", ports = [{port}], allow_private_ips = true',
            "--",
            "/bench/net-worker",
            *workload,
        ]
    command = ["taskset", "-c", ",".join(map(str, args.cpus)), *command]
    save(folder / "command.json", command)
    observations, stop, interference = [], threading.Event(), []
    started = time.monotonic_ns()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    with (folder / "stdout.log").open("wb") as stdout, (folder / "stderr.log").open("wb") as stderr:
        process = subprocess.Popen(
            command, cwd=workspace, env=env, stdout=stdout, stderr=stderr, start_new_session=True
        )

        def monitor():
            while not stop.is_set():
                observations.append(observe(process.pid))
                active = builds()
                if active:
                    interference.append(active)
                stop.wait(0.1)

        observer = threading.Thread(target=monitor)
        observer.start()
        try:
            deadline = time.monotonic() + 150
            while process.poll() is None:
                if server.poll() is not None:
                    raise RuntimeError("origin server failed")
                if interference:
                    raise RuntimeError(f"build interference: {interference[0]}")
                if time.monotonic() > deadline:
                    raise TimeoutError("trial deadline exceeded")
                # This is a bounded workload supervisor, not agent/job polling.
                stop.wait(0.05)
            if process.returncode != 0:
                raise RuntimeError(f"{arm} exited {process.returncode}; see {folder}/stderr.log")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            stop.set()
            observer.join()
            save(folder / "observations.json", observations)
            save(folder / "interference.json", interference)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    records = parse_results((folder / "stdout.log").read_text(), arm)
    result = dict(
        phase=phase,
        round=index,
        arm=arm,
        results=records,
        process_wall_ns=time.monotonic_ns() - started,
        reaped_child_cpu_seconds=after.ru_utime
        + after.ru_stime
        - before.ru_utime
        - before.ru_stime,
    )
    if arm != "native":
        bundle = json.loads((folder / "stage/run-bundle.json").read_text())
        result["network_accounting"] = validate_bundle(bundle, port)
    save(folder / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument(
        "--build-receipt", type=Path, help="directory with original build commands/logs"
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpus", type=cpu_list, default=[0, 1, 2, 3])
    parser.add_argument("--server-cpus", type=cpu_list, default=[4, 5])
    parser.add_argument("--rounds", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--seed", type=int, default=20261115)
    parser.add_argument(
        "--preflight", action="store_true", help="one round, explicitly not formal evidence"
    )
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        parser.error("this real-guest protocol requires Linux x86_64/KVM")
    if len(args.cpus) != 4 or len(args.server_cpus) != 2:
        parser.error("expected four client CPUs and two origin CPUs")
    if set(args.cpus) & set(args.server_cpus):
        parser.error("origin/client affinity must be disjoint")
    if not set(args.cpus + args.server_cpus) <= os.sched_getaffinity(0):
        parser.error("CPU outside available affinity")
    if not args.preflight and (args.rounds < 30 or args.warmups != 3):
        parser.error("formal protocol requires at least 30 rounds and exactly 3 warmups")
    addresses = {
        record[4][0]
        for record in socket.getaddrinfo("localhost.localdomain", 80, type=socket.SOCK_STREAM)
    }
    if not addresses or not all(ipaddress.ip_address(ip).is_loopback for ip in addresses):
        parser.error("localhost.localdomain must resolve exclusively to host loopback")
    if builds():
        parser.error(f"wait for existing builds before measuring: {builds()}")
    with Path("/dev/kvm").open("rb+"):
        pass
    output = args.output.resolve()
    output.mkdir(parents=True)
    frozen = output / "frozen"
    frozen.mkdir()
    artifacts = {}
    for name, source in (
        ("release", args.baseline),
        ("net_o3", args.candidate),
        ("worker", args.worker),
    ):
        source = source.resolve(strict=True)
        shutil.copy2(source, frozen / name)
        artifacts[name] = dict(
            source=str(source), sha256=sha(frozen / name), bytes=source.stat().st_size
        )
    for source in Path(__file__).parent.glob("*"):
        if source.is_file():
            shutil.copy2(source, frozen / source.name)
    args.firmware = args.firmware.resolve(strict=True)
    firmware = {}
    firmware_copy = frozen / "firmware"
    firmware_copy.mkdir()
    for path in args.firmware.glob("libkrunfw*"):
        if path.is_file():
            firmware[path.name] = sha(path)
            shutil.copy2(path, firmware_copy / path.name)
    if not firmware:
        parser.error("no firmware library in supplied directory")
    soname = firmware_copy / "libkrunfw.so.5"
    if not soname.exists():
        libraries = list(firmware_copy.glob("libkrunfw.so.5.*"))
        if len(libraries) != 1:
            parser.error("expected one versioned libkrunfw.so.5 library")
        soname.symlink_to(libraries[0].name)
    args.firmware = firmware_copy
    if args.build_receipt:
        receipt = frozen / "build-receipt"
        receipt.mkdir()
        for source in args.build_receipt.iterdir():
            if source.suffix in (".txt", ".log", ".jsonl"):
                shutil.copy2(source, receipt / source.name)
    save(
        output / "manifest.json",
        dict(
            schema=1,
            benchmark="B-NETWORK-PROFILE-ENG",
            preflight=args.preflight,
            created=datetime.datetime.now(datetime.timezone.utc).isoformat(),
            uname=list(platform.uname()),
            cpus=args.cpus,
            server_cpus=args.server_cpus,
            rounds=args.rounds,
            warmups=args.warmups,
            seed=args.seed,
            loopback_resolution=sorted(addresses),
            firmware=firmware,
            artifacts=artifacts,
            build_receipt=str(args.build_receipt) if args.build_receipt else None,
            source_head=subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
            ).strip(),
            cpuinfo=Path("/proc/cpuinfo").read_text(),
            meminfo=Path("/proc/meminfo").read_text(),
            cpustat=Path("/proc/stat").read_text(),
            harness={p.name: sha(p) for p in frozen.iterdir() if p.is_file()},
        ),
    )
    server_log = (output / "server.stderr").open("wb")
    server = subprocess.Popen(
        ["taskset", "-c", ",".join(map(str, args.server_cpus)), str(frozen / "worker"), "server"],
        stdout=subprocess.PIPE,
        stderr=server_log,
        text=True,
        start_new_session=True,
    )
    trials = []
    try:
        # The origin opens no upstream connection and emits readiness once.
        ready = server.stdout.readline().strip()
        if not ready.startswith("READY "):
            raise RuntimeError("origin startup failed")
        port = int(ready.split()[1])
        save(output / "server.json", dict(pid=server.pid, address=f"127.0.0.1:{port}"))
        rng = random.Random(args.seed)
        schedule = []
        phases = (
            [("preflight", 1)]
            if args.preflight
            else [("warmup", args.warmups), ("formal", args.rounds)]
        )
        for phase, count in phases:
            for index in range(count):
                arms, scenarios = list(ARMS), list(SCENARIOS)
                rng.shuffle(arms)
                rng.shuffle(scenarios)
                schedule.append(dict(phase=phase, round=index, arms=arms, scenarios=scenarios))
        save(output / "schedule.json", schedule)
        for item in schedule:
            for arm in item["arms"]:
                active = builds()
                if active:
                    raise RuntimeError(f"build interference before trial: {active}")
                print(f"{item['phase']} {item['round']:02d} {arm}", flush=True)
                trials.append(
                    trial(
                        args,
                        arm,
                        item["phase"],
                        item["round"],
                        item["scenarios"],
                        port,
                        server,
                        output,
                    )
                )
                save(output / "completed.json", trials)
        if server.poll() is not None:
            raise RuntimeError("origin failed after the final trial")
        if not args.preflight:
            summary = summarize(trials, args.rounds)
            save(output / "summary.json", summary)
            report(summary, output / "report.md")
        save(
            output / "validation.json",
            dict(valid=True, preflight=args.preflight, trials=len(trials)),
        )
    except BaseException as error:
        save(
            output / "validation.json", dict(valid=False, error=repr(error), completed=len(trials))
        )
        raise
    finally:
        if server.poll() is None:
            os.killpg(server.pid, signal.SIGTERM)
        server.wait(timeout=5)
        server_log.close()


if __name__ == "__main__":
    main()
