#!/usr/bin/env python3
"""Compare pVisor sandbox launch cost across executors and host options.

Benchmark: B-STARTUP (benchmark/README.md#b-startup), role user-facing.
Motivation: one-shot Agent environments make startup wait part of every task.
Conclusion sought: how many ms host, staged and VM need before the first
command runs, relative to native, Docker and lightweight VMs.
Design: prepared environment, warm caches, matched CPU/memory budget,
interleaved cases, >=30 samples; ready and launch-to-exit reported separately.

The timed interval is process creation through successful completion of a tiny
payload. It includes pVisor bookkeeping and teardown; it is a startup proxy,
not a guest-ready timestamp. Linux /proc sampling is observational and can
miss short peaks.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import random
import re
import resource
import shlex
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

SCHEMA = "pvisor-sandbox-startup/v1"
WORKLOAD = ["/bin/sh", "-c", ":"]
HOST_OPTIONS = {
    "host": [],
    "host_stage": ["--stage", "{stage}"],
    "host_safe": ["--safe", "--stage", "{stage}"],
    "host_net_proxy": [
        "--overlaynet-deny",
        "example.invalid",
        "--overlaynet-listen",
        "127.0.0.1:{port}",
    ],
    "host_net_deny_all": ["--overlaynet-deny-all"],
    "host_safe_net_deny_all": ["--safe", "--stage", "{stage}", "--overlaynet-deny-all"],
    "host_memory_limit": ["--memory", "256MiB"],
    "host_process_limit": ["--max-processes", "64"],
    "host_open_files_limit": ["--max-open-files", "128"],
    "host_gateway": [
        "--gateway-mode",
        "capture",
        "--overlaynet-listen",
        "127.0.0.1:{port}",
        "--gateway-admin-listen",
        "127.0.0.1:{admin_port}",
    ],
    "host_record": ["--record-destination", "{record}"],
}
DEFAULT_CASES = ["direct", *HOST_OPTIONS, "container", "vm"]


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * percent / 100
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    metrics = ("elapsed_ms", "cpu_ms", "peak_tree_rss_bytes", "peak_processes")
    return {
        key: {
            "p50": round(percentile([float(row[key]) for row in rows], 50), 3),
            "p95": round(percentile([float(row[key]) for row in rows], 95), 3),
            "mean": round(statistics.fmean(float(row[key]) for row in rows), 3),
        }
        for key in metrics
    }


def comparisons(rows: dict[str, list[dict[str, Any]]]) -> dict[str, dict[str, Any]]:
    """Compare matched rounds against the nearest host configuration."""
    output: dict[str, dict[str, Any]] = {}
    for name, samples in rows.items():
        values: dict[str, Any] = {}
        if name != "direct" and "direct" in rows:
            values["latency_ratio_vs_direct_p50"] = round(
                percentile([row["elapsed_ms"] for row in samples], 50)
                / percentile([row["elapsed_ms"] for row in rows["direct"]], 50),
                3,
            )
        reference = {
            "host_safe": "host_stage",
            "host_safe_net_deny_all": "host_safe",
        }.get(name, "host" if name.startswith("host_") else None)
        if reference in rows:
            values["reference_case"] = reference
            for metric, label in (
                ("elapsed_ms", "paired_latency_delta_ms_p50"),
                ("cpu_ms", "paired_cpu_delta_ms_p50"),
                ("peak_tree_rss_bytes", "paired_rss_delta_bytes_p50"),
            ):
                deltas = [
                    row[metric] - control[metric]
                    for row, control in zip(samples, rows[reference], strict=True)
                ]
                values[label] = round(percentile(deltas, 50), 3)
        output[name] = values
    return output


def process_tree_sample(pid: int) -> tuple[int, int]:
    """Return summed VmRSS and process count for descendants still attached to pid."""
    if not sys.platform.startswith("linux"):
        return 0, 0
    pending = [pid]
    seen: set[int] = set()
    rss = 0
    live = 0
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        proc = Path("/proc") / str(current)
        try:
            status = (proc / "status").read_text(encoding="ascii")
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        live += 1
        for line in status.splitlines():
            if line.startswith("VmRSS:"):
                rss += int(line.split()[1]) * 1024
                break
        try:
            for task in (proc / "task").iterdir():
                try:
                    children = (task / "children").read_text(encoding="ascii")
                    pending.extend(int(child) for child in children.split())
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    pass
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            pass
    return rss, live


def free_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def expand_command(template: list[str], trial: Path, workload: list[str]) -> list[str]:
    replacements = {
        "{workspace}": str(trial / "workspace"),
        "{stage}": str(trial / "stage"),
        "{record}": str(trial / "record.jsonl"),
        "{run_home}": str(trial / "runs"),
    }
    if any("{port}" in part for part in template):
        replacements["{port}"] = str(free_loopback_port())
    if any("{admin_port}" in part for part in template):
        replacements["{admin_port}"] = str(free_loopback_port())
    result: list[str] = []
    for part in template:
        if part == "{workload}":
            result.extend(workload)
        else:
            for name, value in replacements.items():
                part = part.replace(name, value)
            result.append(part)
    return result


def pvisor_command(name: str, args: argparse.Namespace) -> list[str]:
    base = [str(args.pvisor), "run", "--no-config", "--stdio", "capture"]
    if args.common_cpu:
        base += ["--cpu", str(args.common_cpu)]
    if args.common_memory:
        base += ["--memory", args.common_memory]
    if name in HOST_OPTIONS:
        return [*base, "--executor", "host", *HOST_OPTIONS[name], "--", "{workload}"]
    if name == "container":
        extra = ["--container-runtime", args.container_runtime]
        if args.container_rootfs:
            extra += ["--container-rootfs", str(args.container_rootfs)]
        else:
            extra += ["--container-image", args.container_image]
        if args.container_pvisor_binary:
            extra += ["--container-pvisor-binary", str(args.container_pvisor_binary)]
        extra += ["--container-network", args.container_network]
        return [*base, "--executor", "container", *extra, "--", "{workload}"]
    if name == "vm":
        return [
            *base,
            "--executor",
            "vm",
            "--rootfs",
            args.vm_rootfs,
            "--overlaynet",
            "off",
            "--",
            "{workload}",
        ]
    if name == "direct":
        return ["{workload}"]
    raise ValueError(f"unknown built-in case: {name}")


def validate_bundle(trial: Path, name: str) -> str:
    bundles = list(trial.glob("**/run-bundle.json"))
    if len(bundles) != 1:
        raise RuntimeError(f"{name}: expected one Run Bundle, found {len(bundles)}")
    document = json.loads(bundles[0].read_text(encoding="utf-8"))
    run = document.get("run", {})
    if run.get("state") != "completed" or run.get("exit_code") != 0:
        raise RuntimeError(f"{name}: Run Bundle is not completed with exit code 0")
    executor = run.get("executor") or {}
    observed = executor.get("isolation", "unknown")
    expected = {"container": "container", "vm": "virtual_machine"}.get(name)
    if expected and observed != expected:
        raise RuntimeError(f"{name}: observed isolation {observed!r}, expected {expected!r}")
    if name.startswith("host_safe") and observed != "rootless_process":
        raise RuntimeError(f"{name}: expected rootless_process, observed {observed!r}")
    return observed


def run_trial(
    name: str,
    template: list[str],
    *,
    phase: str,
    workload: list[str],
    scratch: Path,
    interval_ms: float,
    timeout_s: float,
    verify_pvisor: bool,
    keep: bool,
) -> dict[str, Any]:
    trial = Path(tempfile.mkdtemp(prefix=f"{name}-{phase}-", dir=scratch))
    (trial / "workspace").mkdir()
    (trial / "runs").mkdir()
    (trial / "config").mkdir()
    env = os.environ.copy()
    env["PVISOR_RUN_HOME"] = str(trial / "runs")
    env["XDG_CONFIG_HOME"] = str(trial / "config")
    command = expand_command(template, trial, workload)
    log = trial / "stderr.log"
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.perf_counter_ns()
    peak_rss = 0
    peak_processes = 0
    process: subprocess.Popen[bytes] | None = None
    finished = threading.Event()
    finished_at: list[int] = []

    def wait_for_exit() -> None:
        assert process is not None
        process.wait()
        finished_at.append(time.perf_counter_ns())
        finished.set()

    try:
        with log.open("wb") as stderr:
            process = subprocess.Popen(
                command,
                cwd=trial / "workspace",
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=stderr,
                start_new_session=True,
            )
            waiter = threading.Thread(target=wait_for_exit, daemon=True)
            waiter.start()
            while not finished.is_set():
                rss, count = process_tree_sample(process.pid)
                peak_rss = max(peak_rss, rss)
                peak_processes = max(peak_processes, count)
                if (time.perf_counter_ns() - started) / 1e9 > timeout_s:
                    try:
                        os.killpg(process.pid, 15)
                    except ProcessLookupError:
                        pass
                    if not finished.wait(2):
                        try:
                            os.killpg(process.pid, 9)
                        except ProcessLookupError:
                            pass
                        finished.wait()
                    raise TimeoutError(f"{name}: exceeded {timeout_s}s")
                finished.wait(interval_ms / 1000)
            elapsed_ms = (finished_at[0] - started) / 1e6
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        if process.returncode != 0:
            raise RuntimeError(
                f"{name}: exit {process.returncode}; stderr: "
                f"{log.read_text(errors='replace')[-2000:]}"
            )
        observed = validate_bundle(trial, name) if verify_pvisor else None
        return {
            "elapsed_ms": round(elapsed_ms, 3),
            "cpu_ms": round(
                ((after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)) * 1000,
                3,
            ),
            "peak_tree_rss_bytes": peak_rss,
            "peak_processes": peak_processes,
            "observed_isolation": observed,
        }
    except Exception:
        print(f"failed trial retained at {trial}; command: {shlex.join(command)}", file=sys.stderr)
        raise
    finally:
        if process is not None and not finished.is_set():
            process.kill()
            finished.wait()
        if not keep and sys.exc_info()[0] is None:
            shutil.rmtree(trial)


def load_adapter(path: Path) -> dict[str, list[str]]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema") != "pvisor-startup-adapter/v1":
        raise ValueError("adapter schema must be pvisor-startup-adapter/v1")
    cases: dict[str, list[str]] = {}
    if not isinstance(document.get("cases"), list):
        raise ValueError("adapter cases must be a JSON array")
    for item in document["cases"]:
        if not isinstance(item, dict) or "name" not in item or "command" not in item:
            raise ValueError("each adapter case needs name and command")
        name, command = item["name"], item["command"]
        if (
            not isinstance(name, str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", name)
            or name in DEFAULT_CASES
            or name in cases
        ):
            raise ValueError(f"invalid or duplicate adapter case: {name!r}")
        if not isinstance(command, list) or not all(isinstance(x, str) for x in command):
            raise ValueError(f"{name}: command must be a JSON argv array")
        if command.count("{workload}") != 1:
            raise ValueError(f"{name}: command must contain one {{workload}} token")
        cases[name] = command
    return cases


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pvisor", type=Path, default=Path("target/release/pvisor"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--shell", default="/bin/sh", help="shell path inside every environment")
    parser.add_argument(
        "--scratch-root",
        type=Path,
        default=Path(tempfile.gettempdir()),
        help="parent directory for isolated trial workspaces (default: system temp)",
    )
    parser.add_argument("--cases", default=",".join(DEFAULT_CASES))
    parser.add_argument("--adapter", type=Path, help="JSON argv templates for other runtimes")
    parser.add_argument("--container-rootfs", type=Path)
    parser.add_argument("--container-image")
    parser.add_argument("--container-pvisor-binary", type=Path)
    parser.add_argument("--container-runtime", default="crun")
    parser.add_argument("--container-network", choices=("host", "none"), default="none")
    parser.add_argument("--vm-rootfs", help="prepared path, image=REF, or host on Linux")
    parser.add_argument(
        "--common-cpu", type=int, help="request this CPU count for every pVisor case"
    )
    parser.add_argument("--common-memory", help="request this memory cap for every pVisor case")
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--interval-ms", type=float, default=2)
    parser.add_argument("--resource-hold-ms", type=int, default=200)
    parser.add_argument("--timeout-s", type=float, default=60)
    parser.add_argument("--seed", type=int, default=20260926)
    parser.add_argument("--keep-trials", action="store_true")
    args = parser.parse_args()
    if (
        args.warmups < 0
        or args.samples < 1
        or args.interval_ms <= 0
        or args.timeout_s <= 0
        or args.resource_hold_ms < 1
    ):
        parser.error(
            "warmups must be >= 0; samples, interval-ms, resource-hold-ms and timeout-s must be > 0"
        )
    args.pvisor = args.pvisor.resolve()
    if args.container_rootfs:
        args.container_rootfs = args.container_rootfs.resolve()
    if args.container_pvisor_binary:
        args.container_pvisor_binary = args.container_pvisor_binary.resolve()
    if args.vm_rootfs and args.vm_rootfs != "host" and not args.vm_rootfs.startswith("image="):
        args.vm_rootfs = str(Path(args.vm_rootfs).resolve())
    args.output = args.output.resolve()
    args.scratch_root = args.scratch_root.resolve()
    if not args.scratch_root.is_dir():
        parser.error(f"scratch root is not a directory: {args.scratch_root}")
    args.adapter_cases = load_adapter(args.adapter) if args.adapter else {}
    args.selected = [case.strip() for case in args.cases.split(",")]
    if len(args.selected) != len(set(args.selected)) or any(not x for x in args.selected):
        parser.error("--cases must be a non-empty list of unique case names")
    unknown = set(args.selected) - set(DEFAULT_CASES) - set(args.adapter_cases)
    if unknown:
        parser.error(f"unknown cases: {', '.join(sorted(unknown))}")
    if "container" in args.selected and bool(args.container_rootfs) == bool(args.container_image):
        parser.error("container needs exactly one of --container-rootfs or --container-image")
    if "vm" in args.selected and not args.vm_rootfs:
        parser.error("vm needs --vm-rootfs")
    if args.common_cpu is not None and args.common_cpu < 1:
        parser.error("--common-cpu must be positive")
    if args.common_memory and "host_memory_limit" in args.selected:
        parser.error("--common-memory conflicts with the host_memory_limit option case")
    if args.container_rootfs and not args.container_rootfs.is_dir():
        parser.error(f"container rootfs is not a directory: {args.container_rootfs}")
    if args.container_pvisor_binary and not args.container_pvisor_binary.is_file():
        parser.error(f"container pVisor binary does not exist: {args.container_pvisor_binary}")
    if args.vm_rootfs and args.vm_rootfs != "host" and not args.vm_rootfs.startswith("image="):
        if not Path(args.vm_rootfs).is_dir():
            parser.error(f"VM rootfs is not a directory: {args.vm_rootfs}")
    if any(case not in args.adapter_cases and case != "direct" for case in args.selected):
        if not args.pvisor.is_file():
            parser.error(f"pVisor binary does not exist: {args.pvisor}")
    if not sys.platform.startswith("linux"):
        parser.error("this version requires Linux /proc for process-tree resource sampling")
    return args


def main() -> int:
    args = parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="pvisor-startup-", dir=args.scratch_root))
    print(f"trial workspace: {scratch}", flush=True)
    templates = {
        name: args.adapter_cases.get(name) or pvisor_command(name, args) for name in args.selected
    }
    rows: dict[str, list[dict[str, Any]]] = {name: [] for name in args.selected}
    rng = random.Random(args.seed)
    startup_workload = [args.shell, "-c", ":"]
    hold_workload = [args.shell, "-c", f"sleep {args.resource_hold_ms / 1000:g}"]
    for round_number in range(args.warmups + args.samples):
        order = args.selected.copy()
        rng.shuffle(order)
        for name in order:
            print(f"round {round_number + 1}/{args.warmups + args.samples}: {name}", flush=True)
            startup = run_trial(
                name,
                templates[name],
                phase="startup",
                workload=startup_workload,
                scratch=scratch,
                interval_ms=args.interval_ms,
                timeout_s=args.timeout_s,
                verify_pvisor=name not in args.adapter_cases and name != "direct",
                keep=args.keep_trials,
            )
            occupancy = run_trial(
                name,
                templates[name],
                phase="occupancy",
                workload=hold_workload,
                scratch=scratch,
                interval_ms=args.interval_ms,
                timeout_s=args.timeout_s,
                verify_pvisor=name not in args.adapter_cases and name != "direct",
                keep=args.keep_trials,
            )
            if round_number >= args.warmups:
                rows[name].append(
                    {
                        **startup,
                        "occupancy_elapsed_ms": occupancy["elapsed_ms"],
                        "occupancy_cpu_ms": occupancy["cpu_ms"],
                        "peak_tree_rss_bytes": occupancy["peak_tree_rss_bytes"],
                        "peak_processes": occupancy["peak_processes"],
                    }
                )
    result = {
        "schema": SCHEMA,
        "environment": {
            "recorded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "os": platform.platform(),
            "machine": platform.machine(),
            "processor": cpu_name(),
            "logical_cpus": os.cpu_count(),
            "memory_total_kib": memory_total_kib(),
            "pvisor_binary": str(args.pvisor),
            "pvisor_sha256": sha256_file(args.pvisor) if args.pvisor.is_file() else None,
            "git_commit": git_value("rev-parse", "HEAD"),
            "git_dirty": bool(git_value("status", "--porcelain")),
            "container_runtime": args.container_runtime,
            "container_network": args.container_network,
            "scratch_root": str(args.scratch_root),
            "common_cpu": args.common_cpu,
            "common_memory": args.common_memory,
            "container_rootfs": str(args.container_rootfs) if args.container_rootfs else None,
            "container_image": args.container_image,
            "vm_rootfs": args.vm_rootfs,
        },
        "protocol": {
            "workload": startup_workload,
            "occupancy_workload": hold_workload,
            "warmups": args.warmups,
            "samples": args.samples,
            "sample_interval_ms": args.interval_ms,
            "timeout_s": args.timeout_s,
            "seed": args.seed,
            "metric_boundary": "Popen start to zero-exit command completion",
            "rss_source": "sampled /proc process tree VmRSS sum during occupancy workload",
            "cpu_source": "RUSAGE_CHILDREN delta",
        },
        "cases": {
            name: {
                "command_template": templates[name],
                "raw": rows[name],
                "summary": summarize(rows[name]),
            }
            for name in args.selected
        },
        "comparisons": comparisons(rows),
    }
    (args.output / "startup.json").write_text(json.dumps(result, indent=2) + "\n")
    (args.output / "startup.md").write_text(render_markdown(result))
    print(f"wrote {args.output / 'startup.json'} and startup.md")
    if not args.keep_trials:
        shutil.rmtree(scratch)
    return 0


def sha256_file(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_value(*arguments: str) -> str | None:
    completed = subprocess.run(
        ["git", *arguments],
        cwd=Path(__file__).parents[2],
        capture_output=True,
        text=True,
        check=False,
    )
    return completed.stdout.strip() if completed.returncode == 0 else None


def cpu_name() -> str:
    for line in Path("/proc/cpuinfo").read_text().splitlines():
        if line.startswith("model name"):
            return line.split(":", 1)[1].strip()
    return platform.processor() or "unknown"


def memory_total_kib() -> int | None:
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemTotal:"):
            return int(line.split()[1])
    return None


def render_markdown(result: dict[str, Any]) -> str:
    lines = [
        "# Sandbox startup benchmark",
        "",
        "Wall time covers launch through completion of "
        f"`{shlex.join(result['protocol']['workload'])}`.",
        "CPU is child rusage for that startup trial. RSS and process count are sampled",
        "during the separate, short `sleep` occupancy trial. Sampling misses short peaks;",
        "summed RSS counts shared pages more than once.",
        "",
        "| Case | Isolation observed | Latency p50 / p95 (ms) | vs direct | CPU p50 (ms) | RSS p50 / p95 (MiB) | Processes p50 |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for name, case in result["cases"].items():
        summary = case["summary"]
        observed = sorted(
            {
                row["observed_isolation"] or ("direct" if name == "direct" else "unverified")
                for row in case["raw"]
            }
        )
        latency = summary["elapsed_ms"]
        rss = summary["peak_tree_rss_bytes"]
        comparison = result["comparisons"][name]
        ratio = comparison.get("latency_ratio_vs_direct_p50")
        lines.append(
            f"| {name} | {', '.join(observed)} | {latency['p50']:.2f} / {latency['p95']:.2f} "
            f"| {f'{ratio:.2f}x' if ratio is not None else '—'} "
            f"| {summary['cpu_ms']['p50']:.2f} | {rss['p50'] / 1048576:.2f} / "
            f"{rss['p95'] / 1048576:.2f} | {summary['peak_processes']['p50']:.0f} |"
        )
    option_rows = [
        (name, comparison)
        for name, comparison in result["comparisons"].items()
        if "reference_case" in comparison
    ]
    if option_rows:
        lines.extend(
            [
                "",
                "## Host option effects (paired by round)",
                "",
                "| Option | Reference | Δ latency p50 (ms) | Δ CPU p50 (ms) | Δ sampled RSS p50 (MiB) |",
                "| --- | --- | ---: | ---: | ---: |",
            ]
        )
        for name, comparison in option_rows:
            lines.append(
                f"| {name} | {comparison['reference_case']} "
                f"| {comparison['paired_latency_delta_ms_p50']:+.2f} "
                f"| {comparison['paired_cpu_delta_ms_p50']:+.2f} "
                f"| {comparison['paired_rss_delta_bytes_p50'] / 1048576:+.2f} |"
            )
    return "\n".join([*lines, ""])


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, TimeoutError) as error:
        print(f"benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
