"""Shared memory savings contracts for active JSON benchmarks.

Extracted from the retired runners; no historical campaign entry point.
"""

import json
import os
import subprocess
import threading
import time
import uuid
from pathlib import Path

from linux_cold_runtime import competing_jobs, digest, settle_unit


def host_identity():
    return dict(
        kernel=os.uname().release,
        cpu_affinity=sorted(os.sched_getaffinity(0)),
        cpu_model=next(
            line.split(":", 1)[1].strip()
            for line in Path("/proc/cpuinfo").read_text().splitlines()
            if line.startswith("model name")
        ),
        cpu_cores_requested=[0, 1, 2, 3],
        host_cp_sha256=digest(Path("/usr/bin/cp")),
        ksm={
            name: (Path("/sys/kernel/mm/ksm") / name).read_text().strip()
            for name in ("run", "pages_to_scan", "sleep_millisecs")
        },
    )


def validate(raw, condition):
    if raw.get("schema") != "pvisor-memory-savings/v1" or raw.get("correctness") != "passed":
        raise ValueError("missing complete successful result")
    for key in ("mode", "pattern", "wait"):
        if raw.get(key) != condition[key]:
            raise ValueError("wrong condition: " + key)
    if raw.get("cleanup") is not True or raw.get("exit_code") != 0:
        raise ValueError("missing successful native reap")
    if (raw.get("memory_mib"), raw.get("cpus"), raw.get("payload_bytes")) != (
        condition.get("memory_mib", 256),
        2,
        64 * 1024**2,
    ):
        raise ValueError("wrong guest budget")
    phases = {row["name"]: row for row in raw["phases"]}
    required = {"before", "active", "parked", f"idle-{condition['wait']}", "restored", "after"}
    if not required <= phases.keys() or len(phases) != len(raw["phases"]):
        raise ValueError("missing/duplicate phase")
    groups = set()
    previous = 0
    for row in raw["phases"]:
        mem = row["memory"]
        if condition.get("memory_mib") == 512:
            processes = mem.get("processes", [])
            if not processes or len({p["pid"] for p in processes}) != len(processes):
                raise ValueError("missing/duplicate resident process accounting")
            for process in processes:
                line = next(
                    (
                        line
                        for line in process["smaps_rollup"].splitlines()
                        if line.startswith("Pss:")
                    ),
                    None,
                )
                if line is None or process["pss_bytes"] != int(line.split()[1]) * 1024:
                    raise ValueError("resident PSS evidence mismatch")
            if mem.get("pss_bytes") != sum(p["pss_bytes"] for p in processes):
                raise ValueError("incomplete resident physical memory")
        groups.add(mem["cgroup"])
        if not {"anon", "file", "kernel"} <= mem["stat"].keys():
            raise ValueError("incomplete memory accounting")
        if mem["current"] <= 0 or mem["cpu"]["usage_usec"] < previous:
            raise ValueError("invalid memory/CPU counter")
        previous = mem["cpu"]["usage_usec"]
        if any(mem["events"].get(key, 0) for key in ("oom", "oom_kill", "max")):
            raise ValueError("memory budget failure")
    if len(groups) != 1:
        raise ValueError("phase escaped group")
    tasks = {row["token"]: row for row in raw["tasks"]}
    if len(tasks) != len(raw["tasks"]) or not {"ready", "restored", "exit"} <= tasks.keys():
        raise ValueError("missing/duplicate integrity proof")
    for token in ("ready", "restored", "exit"):
        row = tasks[token]
        if row["digest"] != raw["expected_digest"] or row["bytes"] != raw["payload_bytes"]:
            raise ValueError("payload integrity mismatch")
    if condition["mode"] == "release" and tasks.get("free", {}).get("bytes") != 0:
        raise ValueError("missing release proof")
    idle = phases[f"idle-{condition['wait']}"]
    if idle["elapsed_ms"] - phases["parked"]["elapsed_ms"] < condition["wait"] * 1000:
        raise ValueError("idle window too short")
    return phases, tasks


def wait_for_quiet(output, quiet_seconds=30, deadline_seconds=180, allow_builds=False):
    deadline, quiet_since, rows = time.monotonic() + deadline_seconds, None, []
    while True:
        row = competing_jobs(Path("/sys/fs/cgroup/pvisor-no-owned-service"))
        rows.append(row)
        (output / "prelaunch-wait.json").write_text(json.dumps(rows) + "\n")
        now = time.monotonic()
        jobs = [job for job in row["jobs"] if not allow_builds or job.get("kind") != "build/test"]
        if quiet_seconds == 0:
            return not jobs
        if jobs:
            quiet_since = None
        elif quiet_since is None:
            quiet_since = now
        elif now - quiet_since >= quiet_seconds:
            return True
        if now >= deadline:
            return False
        time.sleep(min(1, deadline - now))


def validate_observation(rows, guards, allow_builds=False, memory_max=2147483648):
    complete = [row for row in rows if "error" not in row]
    if not complete or any("error" in row for row in rows):
        raise ValueError("missing/failed external memory observation")
    for row in complete:
        budget = row["budget"]
        quota, period = map(int, budget["cpu.max"].split())
        if (
            budget["memory.max"] != str(memory_max)
            or budget["memory.swap.max"] != "0"
            or budget["memory.swap.current"] != "0"
            or quota != 4 * period
            or period <= 0
            or budget["pids.max"] != "128"
        ):
            raise ValueError("installed resource budget mismatch")
    if not guards or any(
        job
        for item in guards
        for job in item.get("jobs", [])
        if not allow_builds or job.get("kind") != "build/test"
    ):
        raise ValueError("foreign VM/build or missing host guard during sampling")


def observer(unit, stop, output):
    rows, guards, group = [], [], None
    last_guard = 0
    try:
        while not stop.wait(0.05):
            now = time.monotonic()
            if group is None:
                result = subprocess.run(
                    ["systemctl", "--user", "show", unit, "-p", "ControlGroup", "--value"],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if result.stdout.strip():
                    group = Path("/sys/fs/cgroup") / result.stdout.strip().lstrip("/")
            if group is not None and group.exists():
                try:
                    rows.append(
                        dict(
                            time_ns=time.monotonic_ns(),
                            group=str(group),
                            memory_current=int((group / "memory.current").read_text()),
                            memory_peak=int((group / "memory.peak").read_text()),
                            cpu_stat=(group / "cpu.stat").read_text(),
                            memory_events=(group / "memory.events").read_text(),
                            memory_pressure=(group / "memory.pressure").read_text(),
                            budget={
                                name: (group / name).read_text().strip()
                                for name in (
                                    "cpu.max",
                                    "memory.max",
                                    "memory.swap.max",
                                    "memory.swap.current",
                                    "pids.max",
                                )
                            },
                        )
                    )
                except OSError as error:
                    # Collected services legitimately disappear after native
                    # reap. A read gap while the group still exists is invalid.
                    if group.exists():
                        rows.append(dict(error=str(error)))
            if now - last_guard >= 0.5:
                guards.append(
                    competing_jobs(group or Path("/sys/fs/cgroup/pvisor-no-owned-service"))
                )
                last_guard = now
    except Exception as error:
        rows.append(dict(error=str(error)))
    finally:
        guards.append(competing_jobs(group or Path("/sys/fs/cgroup/pvisor-no-owned-service")))
        (output / "monitor.json").write_text(json.dumps(rows) + "\n")
        (output / "guard.json").write_text(json.dumps(guards, indent=2) + "\n")


def run(args, binary, output, condition, round_id, warmup, validator=validate):
    output.mkdir()
    worker = output / "w"
    unit = "pvisor-memory-savings-" + uuid.uuid4().hex + ".service"
    row = dict(condition=condition, round=round_id, warmup=warmup, unit=unit, status="failed")
    if not wait_for_quiet(
        output,
        quiet_seconds=getattr(args, "quiet_seconds", 30),
        allow_builds=getattr(args, "static", False),
    ):
        row["error"] = "quiet admission failed"
        return row
    cmd = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit=" + unit,
        "-p",
        "MemoryAccounting=yes",
        "-p",
        "CPUAccounting=yes",
        "-p",
        "MemoryMax=" + str(condition.get("group_memory_max", 2147483648)),
        "-p",
        "MemorySwapMax=0",
        "-p",
        "CPUQuota=400%",
        "-p",
        "CPUAffinity=0 1 2 3",
        "-p",
        "TasksMax=128",
        "-p",
        "RuntimeMaxSec=240",
        "-p",
        "TimeoutStopSec=10",
        "-p",
        "KillMode=control-group",
        str(binary),
        "--rootfs",
        str(args.rootfs),
        "--firmware",
        str(args.firmware),
        "--output",
        str(worker),
    ]
    for key, value in condition.items():
        cmd += [
            "--" + key.replace("_", "-"),
            str(value).lower() if isinstance(value, bool) else str(value),
        ]
    row["command"] = cmd
    stop = threading.Event()
    watch = threading.Thread(target=observer, args=(unit, stop, output))
    watch.start()
    try:
        env = os.environ.copy()
        for key in (
            "PVISOR_EXPERIMENTAL_MEMORY_METRICS",
            "PVISOR_EXPERIMENTAL_MEMORY_POOL",
            "PVISOR_TEST_ALLOW_NO_USERNS",
        ):
            env.pop(key, None)
        env.update(PVISOR_FS_PROFILE="0", PVISOR_STARTUP_TIMING="0", PVISOR_STARTUP_PROFILE="0")
        with (output / "stdout").open("wb") as stdout, (output / "stderr").open("wb") as stderr:
            result = subprocess.run(cmd, env=env, stdout=stdout, stderr=stderr, timeout=270)
        row["returncode"] = result.returncode
        raw = json.loads((worker / "raw.json").read_text())
        validator(raw, condition)
        if result.returncode:
            raise ValueError("worker exited nonzero")
        row["raw_sha256"] = digest(worker / "raw.json")
        row["raw"] = str(worker / "raw.json")
        row["status"] = "successful"
    except Exception as error:
        row["error"] = str(error)
    finally:
        row["unit_quiescent"] = settle_unit(unit, output)
        stop.set()
        watch.join()
        guards = json.loads((output / "guard.json").read_text())
        try:
            validate_observation(
                json.loads((output / "monitor.json").read_text()),
                guards,
                allow_builds=getattr(args, "static", False),
                memory_max=condition.get("group_memory_max", 2147483648),
            )
        except ValueError as error:
            row.update(status="failed", error=str(error))
        if not row["unit_quiescent"]:
            row.update(status="failed", error="owned cleanup not proven")
        if row["status"] == "successful" and (worker / "live.ram").is_file():
            backing = worker / "live.ram"
            info = backing.stat()
            row["retired_backing"] = dict(
                logical_bytes=info.st_size,
                allocated_bytes=info.st_blocks * 512,
                sha256=digest(backing),
                policy="removed after native reap and owned-unit quiescence",
            )
            backing.unlink()
    (output / "result.json").write_text(json.dumps(row, indent=2) + "\n")
    return row
