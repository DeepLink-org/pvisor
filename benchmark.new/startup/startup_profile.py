#!/usr/bin/env python3
"""Benchmark: B-STARTUP-DIAG (diagnostic, never a published startup score).

Question: which frontend/owner/runner/VMM phases dominate startup to an exact
workload Ready line? Collect small independent trials, or inspect existing logs.
Host CLOCK_MONOTONIC checkpoints can be compared across processes on one host;
profile epochs are process-local and must never be aligned or summed together.
"""

import argparse
import hashlib
import json
import os
import selectors
import shlex
import signal
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

PREFIX = "pvisor-startup-profile "
CHECKPOINT = "pvisor-startup "
OBSERVER = "pvisor-startup-observer "
BINDING = "pvisor-owner-request "


def integer(value):
    if type(value) is not int or value < 0:
        raise ValueError(f"expected nonnegative integer: {value!r}")
    return value


def fields(line):
    return dict(field.split("=", 1) for field in shlex.split(line)[1:] if "=" in field)


def analyze(path):
    """Return individual observations; missing records are never inferred zeros."""
    spans, marks, bindings, rows = [], defaultdict(list), [], []
    observer = None
    raw = path.read_bytes()
    for number, line in enumerate(raw.decode("utf-8", errors="replace").splitlines(), 1):
        try:
            if line.startswith(PREFIX):
                record = json.loads(line[len(PREFIX) :])
                if record["schema"] != 1 or record["clock"] not in (
                    "host_monotonic_process_relative",
                    "guest_monotonic_process_relative",
                ):
                    raise ValueError("unsupported profile schema/clock")
                integer(record["pid"])
                start, end, duration = (
                    integer(record[key]) for key in ("start_ns", "end_ns", "duration_ns")
                )
                if end < start or end - start != duration:
                    raise ValueError("inconsistent profile interval")
                if not isinstance(record["stage"], str) or not record["stage"]:
                    raise ValueError("missing stage")
                if record["outcome"] not in ("ok", "success", "unknown"):
                    raise ValueError("unsupported outcome")
                spans.append(record)
            elif line.startswith(CHECKPOINT):
                record = fields(line)
                marks[int(record["pid"])].append(
                    (int(record["monotonic_us"]) * 1000, record["stage"], record["run_id"])
                )
            elif line.startswith(BINDING):
                bindings.append(fields(line))
            elif line.startswith(OBSERVER):
                if observer is not None:
                    raise ValueError("multiple observers in one trial log")
                observer = json.loads(line[len(OBSERVER) :])
                if (
                    observer["exit_code"] != 0
                    or observer["ready_count"] != 1
                    or observer.get("timed_out", False)
                ):
                    raise ValueError("failed trial or missing/duplicate Ready")
                start, ready, end = (
                    integer(observer[key]) for key in ("start_ns", "ready_ns", "end_ns")
                )
                if not start <= ready <= end:
                    raise ValueError("invalid observer clock ordering")
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"{path}:{number}: {error}") from error
    if not spans and not marks:
        raise ValueError(f"{path}: no startup diagnostics")

    for span in spans:
        epoch = span.get("epoch", "runtime-checkpoint")
        rows.append(
            {
                "kind": "span",
                "phase": span["stage"],
                "epoch": epoch,
                "outcome": span["outcome"],
                "duration_ns": span["duration_ns"],
                "pid": span["pid"],
            }
        )
    ready = observer["ready_ns"] if observer else None
    # Without an observer, stop at the last known startup endpoint. Never report
    # wait/workload/teardown intervals as startup merely because they are slow.
    endpoints = {"runner.vmm_built", "vm.wait_begin", "process.spawn_ready", "cli.session_started"}
    for pid, checkpoints in marks.items():
        checkpoints.sort()
        ends = [stamp for stamp, stage, _ in checkpoints if stage in endpoints]
        cutoff = min(ready, max(ends)) if ready is not None and ends else ready
        if cutoff is None and ends:
            cutoff = max(ends)
        if cutoff is None:
            continue
        points = [item for item in checkpoints if item[0] <= cutoff]
        for before, after in zip(points, points[1:]):
            if before[2] != "-" and after[2] != "-" and before[2] != after[2]:
                continue
            rows.append(
                {
                    "kind": "gap",
                    "phase": f"{before[1]} → {after[1]}",
                    "epoch": "host",
                    "outcome": "observed",
                    "duration_ns": after[0] - before[0],
                    "pid": pid,
                }
            )
    # Explicit owner bindings permit this one cross-process interval. Do not
    # match unrelated processes by timestamp or by a process-relative epoch.
    for binding in bindings:
        frontend = marks[int(binding["frontend_pid"])]
        owner = marks[int(binding["owner_pid"])]
        begin = [t for t, stage, _ in frontend if stage == "cli.owner_spawn_begin"]
        end = [t for t, stage, _ in owner if stage == "cli.run_begin"]
        if (
            len(begin) == len(end) == 1
            and begin[0] <= end[0]
            and (ready is None or end[0] <= ready)
        ):
            rows.append(
                {
                    "kind": "handoff",
                    "phase": "owner spawn → owner run (bootstrap included)",
                    "epoch": "host",
                    "outcome": "observed",
                    "duration_ns": end[0] - begin[0],
                    "pid": int(binding["owner_pid"]),
                }
            )
    if observer:
        rows.append(
            {
                "kind": "ready",
                "phase": "Popen → observed workload Ready",
                "epoch": "observer",
                "outcome": "observed",
                "duration_ns": ready - observer["start_ns"],
                "pid": observer["pid"],
            }
        )
        built = [
            t for points in marks.values() for t, stage, _ in points if stage == "runner.vmm_built"
        ]
        if len(built) == 1 and built[0] <= ready:
            rows.append(
                {
                    "kind": "ready",
                    "phase": "VMM built → observed workload Ready",
                    "epoch": "host",
                    "outcome": "observed",
                    "duration_ns": ready - built[0],
                    "pid": observer["pid"],
                }
            )
    return {
        "log": str(path),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "observer": observer,
        "observations": rows,
    }


def summarize(trials):
    groups = defaultdict(list)
    present = defaultdict(set)
    for index, trial in enumerate(trials):
        for row in trial["observations"]:
            key = tuple(row[field] for field in ("kind", "epoch", "phase", "outcome"))
            groups[key].append(row["duration_ns"] / 1e6)
            present[key].add(index)
    return [
        {
            "kind": key[0],
            "epoch": key[1],
            "phase": key[2],
            "outcome": key[3],
            "calls": len(values),
            "trials": len(present[key]),
            "missing_trials": len(trials) - len(present[key]),
            "median_ms": statistics.median(values),
            "min_ms": min(values),
            "max_ms": max(values),
        }
        for key, values in sorted(groups.items(), key=lambda item: -statistics.median(item[1]))
    ]


def collect(directory, command, runs, ready_line, timeout):
    """Collect stdout/stderr independently; preserve failed trials before raising."""
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    manifest = {
        "benchmark": "B-STARTUP-DIAG",
        "command": command,
        "cwd": str(Path.cwd()),
        "runs": runs,
        "ready_line": ready_line,
        "timeout_seconds": timeout,
        "platform": sys.platform,
        "python": sys.version,
        "kvm_present": Path("/dev/kvm").exists(),
        "cpu_affinity": sorted(os.sched_getaffinity(0))
        if hasattr(os, "sched_getaffinity")
        else None,
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }
    binary = Path(command[0])
    if binary.is_file():
        manifest["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    (directory / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    logs = []
    for index in range(runs):
        trial = directory / f"trial-{index:03d}"
        trial.mkdir(mode=0o700)
        env = {
            **os.environ,
            "PVISOR_STARTUP_PROFILE": "1",
            "PVISOR_STARTUP_TIMING": "1",
            "PVISOR_RUN_HOME": str(trial.resolve() / "runs"),
        }
        # Keep diagnostic routing and stdout under this collector's ownership.
        for key in (
            "PVISOR_DIAGNOSTICS_FD",
            "PVISOR_UI_CHILD",
            "PVISOR_UI_LOG_FILE",
            "PVISOR_UI_STAGE_FILE",
        ):
            env.pop(key, None)
        count, ready, pending = 0, None, b""
        log = trial / "stderr.log"
        timed_out = False
        with log.open("wb") as stderr, (trial / "stdout.log").open("wb") as stdout:
            with selectors.DefaultSelector() as selector:
                start = time.monotonic_ns()
                process = subprocess.Popen(
                    command, stdout=subprocess.PIPE, stderr=stderr, env=env, start_new_session=True
                )
                selector.register(process.stdout, selectors.EVENT_READ)
                deadline = time.monotonic() + timeout
                try:
                    while selector.get_map():
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            timed_out = True
                            break
                        for key, _ in selector.select(min(remaining, 0.1)):
                            data = os.read(key.fd, 65536)
                            observed = time.monotonic_ns()
                            if not data:
                                selector.unregister(key.fileobj)
                                continue
                            stdout.write(data)
                            pending += data
                            while b"\n" in pending:
                                line, pending = pending.split(b"\n", 1)
                                if line == ready_line.encode():
                                    count += 1
                                    ready = observed
                            if len(pending) > 1024 * 1024:
                                raise ValueError("stdout line exceeds 1 MiB")
                    if not timed_out:
                        try:
                            process.wait(timeout=max(0.001, deadline - time.monotonic()))
                        except subprocess.TimeoutExpired:
                            timed_out = True
                finally:
                    if process.poll() is None:
                        # Give the frontend time to cancel/drain its owner before escalation.
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                    process.stdout.close()
                observation = {
                    "pid": process.pid,
                    "start_ns": start,
                    "ready_ns": ready,
                    "end_ns": time.monotonic_ns(),
                    "ready_count": count,
                    "exit_code": process.returncode,
                    "timed_out": timed_out,
                }
                stderr.write((OBSERVER + json.dumps(observation) + "\n").encode())
        if timed_out or process.returncode != 0 or count != 1:
            raise ValueError(f"{trial}: failed/timeout/Ready count {count}; evidence retained")
        logs.append(log)
    return logs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="*", type=Path)
    parser.add_argument("--collect", type=Path, help="new private diagnostic directory")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--ready-line", default="PVISOR_STARTUP_READY")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--json", type=Path, help="write full observations and summary")
    argv = sys.argv[1:]
    command = argv[argv.index("--") + 1 :] if "--" in argv else []
    args = parser.parse_args(argv[: argv.index("--")] if "--" in argv else argv)
    if args.runs <= 0 or args.timeout <= 0 or not args.ready_line or "\n" in args.ready_line:
        parser.error("positive runs/timeout and one nonempty Ready line required")
    if args.collect:
        if not command or args.logs:
            parser.error("--collect needs -- COMMAND and no log paths")
        args.logs = collect(
            args.collect.resolve(), command, args.runs, args.ready_line, args.timeout
        )
    elif command or not args.logs:
        parser.error("provide log paths, or --collect DIR -- COMMAND")
    trials = [analyze(path) for path in args.logs]
    summary = summarize(trials)
    result = {"benchmark": "B-STARTUP-DIAG", "trials": trials, "summary": summary}
    destination = args.json or (args.collect / "summary.json" if args.collect else None)
    if destination:
        destination.write_text(json.dumps(result, indent=2) + "\n")
    print("Diagnostic timings; gaps, spans and Ready overlap. Do not add rows.")
    print("Without an observer, Ready and unmarked startup time remain unknown.")
    for kind in ("ready", "handoff", "gap", "span"):
        print(f"\n{kind}: median ms [min, max]; trials present/total; calls; outcome")
        for row in (item for item in summary if item["kind"] == kind):
            print(
                f"{row['median_ms']:9.3f} [{row['min_ms']:.3f}, {row['max_ms']:.3f}] "
                f"{row['trials']}/{len(trials)} {row['calls']} {row['outcome']}: "
                f"{row['phase']} ({row['epoch']})"
            )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        sys.exit(str(error))
