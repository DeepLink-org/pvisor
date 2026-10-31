"""Private SWE engineering evidence summary. Never equates API wait with pure reasoning."""

import collections
import json
import statistics


def lines(path):
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def distribution(xs):
    return (
        dict(n=len(xs), median=statistics.median(xs), minimum=min(xs), maximum=max(xs))
        if xs
        else dict(n=0)
    )


def summarize(trial):
    vm = trial / "vm"
    ev = lines(vm / "events.jsonl")
    result = json.loads((trial / "result.json").read_text())
    runtime = json.loads((vm / "runtime-events.json").read_text())
    resources = lines(vm / "resources.jsonl")
    waits, start, errors = [], None, 0
    for e in ev:
        if e["event"] == "model_request_start":
            start = e["host_monotonic_ns"]
        elif e["event"] in ("model_request_end", "model_request_error"):
            if start is None:
                raise ValueError("unpaired model event")
            waits.append((start, e["host_monotonic_ns"]))
            start = None
            errors += e["event"] == "model_request_error"
    if start is not None:
        raise ValueError("model request has no terminal event")
    controls = []
    parked_start = offload_start = None
    parked, offloaded, reclaims = [], [], []
    # Observation timestamps bound intervals; they are not instruction-level timers.
    for e in runtime:
        if e["data"].get("name") != "vm.control_completed":
            continue
        p = e["data"]["payload"]
        t = e["observed_at_unix_ms"] / 1000
        v = p["value"]["vm"]
        op = p["operation"]["kind"]["op"]
        controls.append((op, v["state"]))
        if op == "run.pause" and v["state"] == "paused":
            if parked_start is not None:
                raise ValueError("overlapping pause intervals")
            parked_start = t
        if op == "run.offload" and v["state"] == "offloaded":
            offload_start = t
            m = v["memory"]
            reclaims.append(
                {k: m[k] for k in ("resident_before_bytes", "resident_after_bytes", "backed_bytes")}
            )
        if op == "run.resume" and v["state"] == "running":
            if parked_start is not None:
                parked.append(t - parked_start)
                parked_start = None
            if offload_start is not None:
                offloaded.append(t - offload_start)
                offload_start = None
    weighted = collections.defaultdict(float)
    wait_samples = []
    for a, b in zip(resources, resources[1:]):
        lo, hi = a["host_monotonic_ns"], b["host_monotonic_ns"]
        overlap = sum(max(0, min(hi, y) - max(lo, x)) for x, y in waits) / 1e9
        if not overlap:
            continue
        weighted["covered_s"] += overlap
        for name, av, bv in (
            ("cgroup_mib_s", a["memory_current"] / 2**20, b["memory_current"] / 2**20),
            ("ram_pss_mib_s", a["guest_ram"]["pss_kib"] / 1024, b["guest_ram"]["pss_kib"] / 1024),
            (
                "cgroup_file_mib_s",
                a["memory_stat"]["file"] / 2**20,
                b["memory_stat"]["file"] / 2**20,
            ),
        ):
            weighted[name] += (av + bv) / 2 * overlap
        weighted["cpu_s"] += (
            (b["cpu"]["usage_usec"] - a["cpu"]["usage_usec"]) / 1e6 * overlap / ((hi - lo) / 1e9)
        )
        wait_samples.append(b["guest_ram"]["pss_kib"] / 1024)
    deliveries = []
    for line in (vm / "driver.log").read_text(errors="replace").splitlines():
        if line.startswith("pvisor-swe-delivery "):
            deliveries.append(json.loads(line.split(" ", 1)[1]))
    driver = json.loads((vm / "run-result.json").read_text())
    outcome_path = trial / "agent/outcome.json"
    outcome = json.loads(outcome_path.read_text()) if outcome_path.exists() else None
    stages = {e["event"]: e["host_monotonic_ns"] for e in ev if e["event"].startswith("trial.")}
    agent_s = (stages["trial.agent_end"] - stages["trial.agent_start"]) / 1e9
    covered = weighted["covered_s"]
    return dict(
        condition=trial.parent.name,
        task=trial.name,
        reward=(result.get("verifier_result") or {}).get("rewards"),
        exception=result.get("exception_info"),
        driver_state=driver["state"],
        agent_outcome=outcome,
        driver_exit_code=driver["exit_code"],
        model_errors=errors,
        agent_s=agent_s,
        model_calls=len(waits),
        model_wait_s=sum((b - a) / 1e9 for a, b in waits),
        model_wait_duration_s=distribution([(b - a) / 1e9 for a, b in waits]),
        control_counts={str(k): v for k, v in collections.Counter(controls).items()},
        paused_s=sum(parked),
        offloaded_s=sum(offloaded),
        longest_offloaded_s=max(offloaded, default=0),
        unclosed_pause=parked_start is not None,
        reclaims=reclaims,
        wait_mean_cgroup_mib=weighted["cgroup_mib_s"] / covered if covered else None,
        wait_mean_cgroup_file_mib=weighted["cgroup_file_mib_s"] / covered if covered else None,
        wait_mean_ram_pss_mib=weighted["ram_pss_mib_s"] / covered if covered else None,
        wait_min_ram_pss_mib=min(wait_samples, default=None),
        wait_vm_cpu_s=weighted["cpu_s"],
        sample_covered_wait_s=covered,
        max_sample_gap_s=max(
            (b["host_monotonic_ns"] - a["host_monotonic_ns"]) / 1e9
            for a, b in zip(resources, resources[1:])
        ),
        delivery_barrier_ms=distribution([x["delivery_barrier_ns"] / 1e6 for x in deliveries]),
        delivery_errors=sum(
            not x["delivery_barrier_ok"] or not x["upstream_read_ok"] for x in deliveries
        ),
        max_memory_events={
            k: max(x["memory_events"][k] for x in resources) for k in resources[0]["memory_events"]
        },
        max_swap_bytes=max(x["swap_current"] for x in resources),
        notifier_errors=[
            e.get("error") for e in ev if e["event"] == "notifier_stop" and e.get("error")
        ],
        stderr_truncated=driver["output"]["stderr_truncated"],
    )
