"""Concurrent environment density, reliability and memory.

Benchmark: B-DENSITY (benchmark/README.md#b-density), role user-facing.
Motivation: parallel Agents accumulate memory and startup cost; users need
how many environments one machine sustains.
Conclusion sought: with X GiB, how many staged and VM environments run
reliably, memory per environment, and where and how failures begin.
Design: concurrency sweep 1-128 recording success rate, startup wait, RSS and
cgroup memory; Podman/Docker as the control; idle and tool-loaded probes
reported separately; failures reported with resources, never dropped.
"""

import concurrent.futures
import resource
import threading
import time
from pathlib import Path


def snapshot(roots):
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            fields = (path / "stat").read_text().rsplit(") ", 1)[1].split()
            rss = next(
                int(line.split()[1])
                for line in (path / "status").read_text().splitlines()
                if line.startswith("VmRSS:")
            )
            processes[int(path.name)] = (int(fields[1]), rss)
        except (OSError, ValueError, StopIteration):
            continue
    owned = set(roots)
    while True:
        additions = {pid for pid, (parent, _) in processes.items() if parent in owned} - owned
        if not additions:
            break
        owned.update(additions)
    return sum(processes[pid][1] for pid in owned if pid in processes), len(
        owned & processes.keys()
    )


def run(ctx):
    backends = ctx.args.density_backends.split(",")
    # Workers all hold for 1s after recording ready; this is an occupancy probe.
    payload = ["/bin/sh", "-c", "printf PVISOR_DENSITY_READY; sleep 1"]
    original_run = ctx.run
    for backend in backends:
        for concurrency in map(int, ctx.args.density_concurrencies.split(",")):
            available = next(
                int(line.split()[1])
                for line in Path("/proc/meminfo").read_text().splitlines()
                if line.startswith("MemAvailable:")
            )
            single = [
                x["peak_tree_rss_kib"]
                for x in ctx.rows
                if x["suite"] == "density" and x["backend"] == "vm" and x["concurrency"] == 1
            ]
            estimate = max(128 * 1024, max(single) * 1.5) if single else 256 * 1024
            if backend == "vm" and concurrency * estimate + 2 * 1024 * 1024 > available:
                ctx.capabilities[f"density/{backend}/{concurrency}"] = {
                    "state": "not-measured",
                    "reason": f"memory guard: {estimate / 1024:.1f} MiB estimated RSS/VM plus 2 GiB reserve exceeds available memory",
                    "available_kib": available,
                    "estimated_per_job_kib": estimate,
                }
                ctx.save()
                continue
            for trial in range(min(ctx.args.samples, 5)):
                print(f"density {backend} C={concurrency}: {trial}", flush=True)
                # Native Popen tracking uses one monkeypatch local to this sequential suite.
                import subprocess

                Popen = subprocess.Popen
                roots = set()
                lock = threading.Lock()
                stop = threading.Event()
                peaks = [0, 0]

                def tracked(*args, **kwargs):
                    process = Popen(*args, **kwargs)
                    with lock:
                        roots.add(process.pid)
                    return process

                def monitor():
                    while not stop.is_set():
                        with lock:
                            live = set(roots)
                        rss, count = snapshot(live)
                        peaks[0] = max(peaks[0], rss)
                        peaks[1] = max(peaks[1], count)
                        stop.wait(0.02)

                def job(i):
                    root = ctx.fresh(f"density-{backend}-{concurrency}-{i}")
                    work = root / "workspace"
                    work.mkdir()
                    stage = root / "stage"
                    runs = root / "runs"
                    command = ctx.command(backend, work, stage, payload)
                    if backend == "vm":
                        command[command.index("--memory") + 1] = "128MiB"
                    home = root / "home"
                    home.mkdir()
                    env = {
                        "HOME": str(home),
                        "PVISOR_RUN_HOME": str(runs),
                        "XDG_CONFIG_HOME": str(root / "config"),
                    }
                    try:
                        wall, stdout, _ = original_run(command, cwd=work, env=env, timeout=120)
                    except (RuntimeError, TimeoutError) as error:
                        return {"error": str(error), "logs": str(root)}
                    bundle = ctx.validate_bundle(backend, runs, stage)
                    if bundle:
                        stdout = bundle["run"]["output"]["stdout"]
                    assert stdout == "PVISOR_DENSITY_READY"
                    return {"wall_ms": wall}

                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                start = time.perf_counter_ns()
                sampler = threading.Thread(target=monitor, daemon=True)
                subprocess.Popen = tracked
                sampler.start()
                try:
                    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
                        outcomes = list(pool.map(job, range(concurrency)))
                        latencies = [item["wall_ms"] for item in outcomes if "wall_ms" in item]
                finally:
                    subprocess.Popen = Popen
                    stop.set()
                    sampler.join()
                wall = (time.perf_counter_ns() - start) / 1e6
                after = resource.getrusage(resource.RUSAGE_CHILDREN)
                errors = [item for item in outcomes if "error" in item]
                if errors:
                    key = f"density/{backend}/{concurrency}"
                    capability = ctx.capabilities.setdefault(
                        key, {"state": "failed", "batches": []}
                    )
                    capability["batches"].append(
                        {
                            "trial": trial,
                            "attempted": concurrency,
                            "completed": len(latencies),
                            "failed": len(errors),
                            "errors": errors,
                        }
                    )
                    ctx.save()
                    continue
                ctx.record(
                    dict(
                        suite="density",
                        workload="hold-1s",
                        backend=backend,
                        concurrency=concurrency,
                        trial=trial,
                        wall_ms=wall,
                        job_wall_ms=latencies,
                        peak_tree_rss_kib=peaks[0],
                        peak_tree_processes=peaks[1],
                        child_cpu_ms=(
                            (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)
                        )
                        * 1000,
                        completed=len(latencies),
                        correctness="passed",
                        memory_sampling_interval_ms=20,
                    )
                )
