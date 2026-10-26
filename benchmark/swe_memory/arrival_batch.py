"""One immutable staggered arrival batch. Bounded, no retry, exact owned cleanup."""

import argparse
import concurrent.futures
import fcntl
import json
import multiprocessing
import os
import shutil
import signal
import stat
import subprocess
import sys
import time
import traceback
from pathlib import Path

import admission
from analyze_job import analyze_job
from arrival_common import (
    GIB,
    HELPER_CPUS,
    PY,
    ROOT,
    SLICE,
    SSD,
    SUPERVISOR,
    VM_CPUS,
    PhaseTracker,
    cgroup_snapshot,
    cpus,
    load,
    save,
    sha,
)
from audit_shutdown import audit_job


def service_group(unit):
    out = subprocess.check_output(
        ["systemctl", "--user", "show", unit, "-p", "ControlGroup", "--value"],
        text=True,
        timeout=10,
    ).strip()
    assert out.startswith("/user.slice/"), out
    return Path("/sys/fs/cgroup" + out)


def inactive(job):
    owned = load(job / "owned-unit.json")
    unit = owned["unit"]
    assert unit.startswith("pvisor-swe-")
    state = subprocess.run(
        ["systemctl", "--user", "is-active", unit], capture_output=True, text=True, timeout=10
    ).stdout.strip()
    assert state in ["inactive", "failed", "unknown"], f"{unit}: {state}"
    scope = load(job / "scope.json")
    group = Path(scope["cgroup"])
    assert SLICE in group.parts
    if group.exists():
        for p in [group / "cgroup.procs", *group.rglob("cgroup.procs")]:
            if p.exists():
                assert not p.read_text().strip(), "VM cgroup still has processes"
    assert not Path("/proc/" + str(scope["runner"])).exists(), (
        "Recorded runner still exists; retain backing"
    )
    return owned


def finish(job):
    row = analyze_job(ROOT, job)
    save(job / "analysis.json", row)
    assert row.get("passed"), f"Post-run validation failed: {job.name}; inspect analysis.json"
    assert row["max_sample_gap_s"] <= 10, "VM observation gap exceeds existing limit"
    shutdown = audit_job(job)
    save(job / "shutdown-analysis.json", shutdown)
    assert shutdown["passed"], "Shutdown or SSD placement validation failed"
    owned = inactive(job)
    backing = Path(owned["backing"])
    expected = SSD / job.name / "live.ram"
    assert backing == expected and backing.resolve() == expected and backing.is_file()
    s = backing.lstat()
    assert stat.S_ISREG(s.st_mode) and s.st_nlink == 1
    receipt = dict(
        path=str(backing),
        inode=s.st_ino,
        device=s.st_dev,
        logical_bytes=s.st_size,
        allocated_bytes=s.st_blocks * 512,
        unit=owned["unit"],
        unit_inactive=True,
        unix_ns=time.time_ns(),
    )
    # Retain all results/overlays. Only this successful, exited job's RAM artifact is removed.
    save(job / "backing-release-intent.json", receipt)
    backing.unlink()
    receipt["released"] = True
    save(job / "backing-release.json", receipt)
    save(
        job / "evidence-hashes.json",
        {
            str(p.relative_to(job)): sha(p)
            for p in job.rglob("*")
            if p.is_file() and not p.is_symlink()
        },
    )
    return row


def stop_owned(job):
    p = job / "owned-unit.json"
    if p.exists():
        owned = load(p)
        unit = owned["unit"]
        assert unit.startswith("pvisor-swe-")
        assert job.parent == ROOT / "jobs" and Path(owned["output"]).resolve().is_relative_to(
            job.resolve()
        ), "Ownership mismatch"
        state = subprocess.run(
            ["systemctl", "--user", "is-active", unit], capture_output=True, text=True, timeout=10
        ).stdout.strip()
        if state in ["inactive", "failed", "unknown"]:
            return
        if (job / "scope.json").exists():
            assert SLICE in Path(load(job / "scope.json")["cgroup"]).parts, "Foreign VM slice"
        subprocess.run(
            ["systemctl", "--user", "stop", unit],
            check=True,
            timeout=45,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def budget(group):
    assert int((group / "memory.max").read_text()) == 34091302912, "Experiment memory limit missing"
    assert int((group / "memory.swap.max").read_text()) == 0, "Experiment swap must be disabled"
    quota, period = map(int, (group / "cpu.max").read_text().split())
    assert quota * 100 == period * 5975, "Fixed total CPU budget missing"


def resource_guard(snapshot):
    mem = {
        line.split(":")[0]: int(line.split()[1]) * 1024 for line in snapshot["meminfo"].splitlines()
    }
    assert mem["MemAvailable"] > 48 * GIB, "Host memory reserve breached"
    assert snapshot["ssd_free"] > 100 * GIB, "SSD reserve breached"
    assert snapshot["hdd_free"] > 500 * GIB, "HDD evidence reserve breached"
    assert snapshot["total"]["memory_events"]["oom"] == snapshot["oom_initial"], (
        "Experiment OOM detected"
    )
    assert snapshot["total"]["swap_current"] == 0, "Experiment used swap"
    if "supervisor" in snapshot:
        assert (
            snapshot["supervisor"]["swap_current"] == 0
            and snapshot["supervisor"]["memory_events"]["oom"] == 0
        ), "Supervisor resource failure"
    # Retained global guard; the smaller treatment memory.max enforces local pressure.
    return snapshot["total"]["memory_current"] < 360 * GIB and snapshot["ssd_free"] > 110 * GIB


def interrupted(signum, frame):
    raise RuntimeError(f"Interrupted by {signum}")


def run(spec_path, policy):
    spec = load(spec_path)
    ident = spec["id"] + "-" + policy
    assert policy in ["baseline", "offload"] and all(c.isalnum() or c == "-" for c in ident)
    out = ROOT / "batches" / ident
    out.mkdir(parents=True, exist_ok=False)
    save(out / "spec.json", dict(**spec, policy=policy, source=str(spec_path)))
    assert load(ROOT / "advance-progress.json")["status"] == "all-calibration-passed"
    assert os.sched_getaffinity(0) == cpus(HELPER_CPUS), "Helper placement differs"
    group = service_group(SLICE)
    budget(group)
    # Every helper descends from this controller inside the same capped slice.
    own = Path("/proc/self/cgroup").read_text()
    assert "/" + SLICE + "/" in own, own
    own_path = Path("/sys/fs/cgroup" + own.strip().split("::", 1)[1])
    supervisor_group = service_group(SUPERVISOR)
    assert int((supervisor_group / "memory.max").read_text()) == 256 * 2**20
    sq, sp = map(int, (supervisor_group / "cpu.max").read_text().split())
    assert sq * 4 == sp
    initial = cgroup_snapshot(group)
    active = {}
    pending = {}
    completed = []
    failed = []
    next_i = 0
    admission_holds = 0
    launched_count = 0
    rejected = []
    admission_plan = load(ROOT / "admission-plan.json")
    last_admission_sample = None
    schedule = spec["arrivals"]
    start = time.monotonic()
    started_ns = time.monotonic_ns()
    save(
        out / "started.json",
        dict(
            unix_ns=time.time_ns(),
            host_monotonic_ns=started_ns,
            group=str(group),
            helper_group=str(own_path),
            vm_cpus=VM_CPUS,
            helper_cpus=HELPER_CPUS,
            total_memory_max=34091302912,
            combined_total_memory_max=32 * GIB,
            initial=initial,
        ),
    )
    stop_arrivals = False

    def progress(status, **extra):
        save(
            out / "progress.json",
            dict(
                status=status,
                launched=launched_count,
                total_planned=len(schedule),
                active=len(active),
                validating=len(pending),
                completed=completed,
                failed=failed,
                rejected=rejected,
                processed_arrivals=next_i,
                queued=max(
                    0, sum(a["at_s"] <= time.monotonic() - start for a in schedule) - next_i
                ),
                elapsed_s=time.monotonic() - start,
                **extra,
            ),
        )

    executor = concurrent.futures.ProcessPoolExecutor(
        max_workers=2, mp_context=multiprocessing.get_context("spawn")
    )
    samples = (out / "resources.jsonl").open("w")
    decisions = (out / "admission.jsonl").open("w")
    last_sample = -10.0
    admit = True
    try:
        while active or pending or (next_i < len(schedule) and not stop_arrivals):
            elapsed = time.monotonic() - start
            assert elapsed < spec["arrival_duration_s"] + spec["drain_timeout_s"], (
                "Batch exceeded bounded drain deadline"
            )
            if elapsed - last_sample >= 2:
                phases = {
                    k: 0
                    for k in [
                        "vm",
                        "agent",
                        "model_wait",
                        "tool_exec",
                        "state_not_sampled",
                        "log_lag_bytes",
                    ]
                }
                for entry in active.values():
                    admission.observe(entry)
                    for k, v in entry["tracker"].update().items():
                        phases[k] += v
                snap = dict(
                    phases=phases,
                    unix_ns=time.time_ns(),
                    host_monotonic_ns=time.monotonic_ns(),
                    elapsed_s=elapsed,
                    total=cgroup_snapshot(group),
                    helpers=cgroup_snapshot(own_path),
                    supervisor=cgroup_snapshot(supervisor_group),
                    meminfo=Path("/proc/meminfo").read_text(),
                    host_stat=Path("/proc/stat").read_text(),
                    diskstats=Path("/proc/diskstats").read_text(),
                    host_pressure={
                        k: Path("/proc/pressure/" + k).read_text() for k in ["cpu", "memory", "io"]
                    },
                    ssd_free=shutil.disk_usage(SSD).free,
                    hdd_free=shutil.disk_usage(ROOT).free,
                    oom_initial=initial["memory_events"]["oom"],
                    launched=launched_count,
                    active_workers=len(active),
                    verified=len(completed),
                    failures=len(failed),
                    arrived=sum(a["at_s"] <= elapsed for a in schedule),
                    queued=max(0, sum(a["at_s"] <= elapsed for a in schedule) - next_i),
                    rejected=len(rejected),
                )
                samples.write(json.dumps(snap) + "\n")
                samples.flush()
                last_sample = elapsed
                admit = resource_guard(snap)
                progress(
                    "draining-after-failure" if stop_arrivals else "running",
                    admission_holds=admission_holds,
                )
            for pid, entry in list(active.items()):
                proc = entry["process"]
                if proc.poll() is None:
                    assert elapsed - entry["launch_s"] < spec["job_timeout_s"], (
                        "Worker wall timeout: " + entry["job"].name
                    )
                    continue
                entry["log"].close()
                del active[pid]
                launch_record = dict(
                    exit=proc.returncode,
                    scheduled_s=entry["scheduled_s"],
                    launched_s=entry["launch_s"],
                    finished_s=elapsed,
                    wall_s=elapsed - entry["launch_s"],
                    scheduled_to_finished_s=elapsed - entry["scheduled_s"],
                )
                save(out / (entry["job"].name + "-launcher.json"), launch_record)
                if entry["job"].exists():
                    save(entry["job"] / "launcher.json", launch_record)
                if proc.returncode != 0:
                    stop_owned(entry["job"])
                    failed.append(
                        dict(job=entry["job"].name, error="Worker failed", exit=proc.returncode)
                    )
                    stop_arrivals = True
                else:
                    pending[executor.submit(finish, entry["job"])] = entry
            for future, entry in list(pending.items()):
                if not future.done():
                    continue
                del pending[future]
                try:
                    row = future.result()
                    completed.append(
                        dict(
                            job=entry["job"].name,
                            task=row["task"],
                            agent_s=row["agent_s"],
                            verified_at_s=time.monotonic() - start,
                        )
                    )
                except Exception as e:
                    failed.append(
                        dict(job=entry["job"].name, error=str(e), traceback=traceback.format_exc())
                    )
                    stop_arrivals = True
            # FIFO pending requests consume no VM. Real timer/resume paths are untouched.
            if not stop_arrivals and next_i < len(schedule) and schedule[next_i]["at_s"] <= elapsed:
                a = schedule[next_i]
                if admission.expired(a, elapsed, admission_plan):
                    rejected.append(
                        dict(
                            index=next_i,
                            task=a["task"],
                            scheduled_s=a["at_s"],
                            at_s=elapsed,
                            queue_s=elapsed - a["at_s"],
                            reason="queue_deadline",
                        )
                    )
                    next_i += 1
                elif snap["host_monotonic_ns"] != last_admission_sample:
                    last_admission_sample = snap["host_monotonic_ns"]
                    decision = admission.decision(
                        snap, list(active.values()), elapsed, a["task"], admission_plan
                    )
                    decision.update(
                        index=next_i,
                        task=a["task"],
                        at_s=elapsed,
                        queue_s=elapsed - a["at_s"],
                        global_guard=admit,
                    )
                    if not admit:
                        decision["reasons"].append("host_or_storage_guard")
                    if len(active) >= spec["protective_worker_limit"]:
                        decision["reasons"].append("protective_worker_limit")
                    decision["allowed"] = not decision["reasons"]
                    decisions.write(json.dumps(decision) + "\n")
                    decisions.flush()
                    if not decision["allowed"]:
                        admission_holds += 1
                    else:
                        job_id = f"{ident}-{next_i:05d}"
                        job = ROOT / "jobs" / job_id
                        assert not job.exists(), "Existing job must not be overwritten/retried"
                        log = (ROOT / "logs" / f"{job_id}.log").open("xb")
                        env = dict(os.environ, DENSITY_VM_CPUS=VM_CPUS)
                        proc = subprocess.Popen(
                            [
                                PY,
                                str(Path(__file__).with_name("worker.py")),
                                "replay",
                                a["task"],
                                policy,
                                job_id,
                            ],
                            stdout=log,
                            stderr=log,
                            env=env,
                            start_new_session=True,
                        )
                        active[proc.pid] = dict(
                            process=proc,
                            job=job,
                            task=a["task"],
                            log=log,
                            launch_s=time.monotonic() - start,
                            scheduled_s=a["at_s"],
                            tracker=PhaseTracker(job / "runs" / a["task"] / "vm"),
                        )
                        next_i += 1
                        launched_count += 1
            time.sleep(0.1)
        progress(
            "failed" if failed else ("completed-with-rejections" if rejected else "completed"),
            admission_holds=admission_holds,
            unlaunched=schedule[next_i:],
        )
        return 1 if failed else 0
    except BaseException as e:
        progress(
            "aborted", error=str(e), traceback=traceback.format_exc(), unlaunched=schedule[next_i:]
        )
        raise
    finally:
        for entry in active.values():
            p = entry["process"]
            if p.poll() is None:
                os.killpg(p.pid, signal.SIGTERM)
        for entry in active.values():
            p = entry["process"]
            try:
                p.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
            stop_owned(entry["job"])
            entry["log"].close()
        executor.shutdown(wait=True)
        samples.close()
        decisions.close()


if __name__ == "__main__":
    assert os.environ.get("PVISOR_SWE_CONTEXT"), "Explicit prepared context required"
    assert not sys.flags.optimize, "Assertions must remain enabled"
    os.umask(0o077)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    parser = argparse.ArgumentParser()
    parser.add_argument("spec", type=Path)
    parser.add_argument("policy", choices=["baseline", "offload"])
    args = parser.parse_args()
    with (ROOT / "arrival-controller.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        raise SystemExit(run(args.spec, args.policy))
