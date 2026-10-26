"""Summarize a completed arrival batch without erasing failed/unlaunched work."""

import sys
from collections import defaultdict

from arrival_common import GIB, ROOT, load, quantiles, save


def offload_counts(offload_points):
    current = peak_offloaded = 0
    offload_timeline = []
    grouped = defaultdict(int)
    for at, delta in offload_points:
        grouped[at] += delta
    for at, delta in sorted(grouped.items()):
        current += delta
        assert current >= 0
        peak_offloaded = max(peak_offloaded, current)
        offload_timeline.append([at, current])
    assert current == 0
    return peak_offloaded, offload_timeline


def summarize(ident):
    out = ROOT / "batches" / ident
    spec = load(out / "spec.json")
    progress = load(out / "progress.json")
    started = load(out / "started.json")
    samples = [
        __import__("json").loads(line)
        for line in (out / "resources.jsonl").read_text().splitlines()
    ]
    jobs = []
    offload_points = []
    for done in progress["completed"]:
        job = ROOT / "jobs" / done["job"]
        a = load(job / "analysis.json")
        launch = load(job / "launcher.json")
        reference = spec["latency_reference"][a["task"]]
        launch["verified_at_s"] = done["verified_at_s"]
        for begin, end in a["offload_intervals_unix_s"]:
            assert begin <= end, "Reversed offload interval"
            offload_points.extend(
                [(begin - started["unix_ns"] / 1e9, 1), (end - started["unix_ns"] / 1e9, -1)]
            )
        jobs.append(
            dict(
                verified_at_s=launch["verified_at_s"],
                scheduled_to_verified_s=launch["verified_at_s"] - launch["scheduled_s"],
                job=job.name,
                task=a["task"],
                scheduled_s=launch["scheduled_s"],
                launched_s=launch["launched_s"],
                finished_s=launch["finished_s"],
                scheduled_to_finished_s=launch["scheduled_to_finished_s"],
                launch_delay_s=launch["launched_s"] - launch["scheduled_s"],
                agent_s=a["agent_s"],
                tool_s=a["tool_s"],
                task_latency_ratio=launch["scheduled_to_finished_s"] / reference["launcher_wall_s"],
                agent_latency_ratio=a["agent_s"] / reference["agent_s"],
                model_delivery_ms=a["ready_to_host_agent_ms"],
                provider_oversleep_ms=a["provider_oversleep_ms"],
                mean_service_mib=a["agent"]["mean_service_mib"],
                mean_ram_mib=a["agent"]["mean_ram_mib"],
                actual_offloads=len(a["reclaims"]),
                vm_max_sample_gap_s=a["max_sample_gap_s"],
                vm_service_gib_s=a["whole_vm"]["service_gib_s"],
                guest_ram_gib_s=a["whole_vm"]["ram_gib_s"],
                vm_cpu_s=a["whole_vm"]["cpu_s"],
            )
        )
    peak_offloaded, offload_timeline = offload_counts(offload_points)
    total_gib_s = helper_gib_s = 0
    for a, b in zip(samples, samples[1:]):
        dt = (b["host_monotonic_ns"] - a["host_monotonic_ns"]) / 1e9
        total_gib_s += (a["total"]["memory_current"] + b["total"]["memory_current"]) / 2 / GIB * dt
        helper_gib_s += (
            (a["helpers"]["memory_current"] + b["helpers"]["memory_current"]) / 2 / GIB * dt
        )
    duration = (samples[-1]["host_monotonic_ns"] - samples[0]["host_monotonic_ns"]) / 1e9
    total_cpu = (
        samples[-1]["total"]["cpu"]["usage_usec"] - samples[0]["total"]["cpu"]["usage_usec"]
    ) / 1e6
    helper_cpu = (
        samples[-1]["helpers"]["cpu"]["usage_usec"] - samples[0]["helpers"]["cpu"]["usage_usec"]
    ) / 1e6
    window_start = max(v["launcher_wall_s"] for v in spec["latency_reference"].values())
    window_end = spec["arrival_duration_s"]
    steady_n = sum(window_start <= j["verified_at_s"] < window_end for j in jobs)
    window = max(0, window_end - window_start)
    rolling = []
    for s in samples:
        t = s["elapsed_s"]
        complete = sum(j["verified_at_s"] <= t for j in jobs)
        arrived = sum(a["at_s"] <= t for a in spec["arrivals"])
        rolling.append(
            dict(
                at_s=t,
                arrived=arrived,
                completed=complete,
                backlog=arrived - complete,
                active_workers=s["active_workers"],
            )
        )
    result = dict(
        id=ident,
        status=progress["status"],
        all_passed=progress["status"] == "completed" and len(jobs) == len(spec["arrivals"]),
        planned=len(spec["arrivals"]),
        launched=progress["launched"],
        verified=len(jobs),
        failures=progress["failed"],
        admission_holds=progress["admission_holds"],
        rate_per_min=spec["rate_per_min"],
        total_wall_s=progress["elapsed_s"],
        throughput_per_min_including_drain=len(jobs) * 60 / progress["elapsed_s"],
        steady_window_s=[window_start, window_end],
        steady_verified_per_min=steady_n * 60 / window if window else None,
        scheduled_to_finished_s=quantiles([j["scheduled_to_finished_s"] for j in jobs]),
        launch_delay_s=quantiles([j["launch_delay_s"] for j in jobs]),
        task_latency_ratio=quantiles([j["task_latency_ratio"] for j in jobs]),
        agent_latency_ratio=quantiles([j["agent_latency_ratio"] for j in jobs]),
        peak_total_memory_gib=max(s["total"]["memory_current"] for s in samples) / GIB,
        mean_total_memory_gib=total_gib_s / duration,
        total_memory_gib_s=total_gib_s,
        helper_memory_gib_s=helper_gib_s,
        cpu_core_s=total_cpu,
        helper_cpu_core_s=helper_cpu,
        mean_cpu_cores=total_cpu / duration,
        mean_helper_cpu_cores=helper_cpu / duration,
        peak_active_workers=max(s["active_workers"] for s in samples),
        peak_live_vms=max(s["phases"]["vm"] for s in samples),
        peak_live_agents=max(s["phases"]["agent"] for s in samples),
        peak_offloaded_vms=peak_offloaded,
        offload_timeline=offload_timeline,
        max_phase_log_lag_bytes=max(s["phases"]["log_lag_bytes"] for s in samples),
        max_sample_gap_s=max(
            (b["host_monotonic_ns"] - a["host_monotonic_ns"]) / 1e9
            for a, b in zip(samples, samples[1:])
        ),
        jobs=jobs,
        backlog_curve=rolling,
        limitations=[
            "Single batch per policy at this arrival rate; exploration, not boundary confirmation.",
            "Active workers include setup/verification/finalization; not identical to Agent-only density.",
            "cgroup memory includes helper, page cache and kernel charges; guest RAM PSS reported separately.",
            "No per-backing BPF latency attribution in this batch.",
            "Offloaded VM counts reconstructed after exit from control-event timestamps, not polled live.",
        ],
    )
    result["max_vm_sample_gap_s"] = max(j["vm_max_sample_gap_s"] for j in jobs) if jobs else None
    result.update(
        sum_vm_service_gib_s=sum(j["vm_service_gib_s"] for j in jobs),
        sum_guest_ram_gib_s=sum(j["guest_ram_gib_s"] for j in jobs),
        scoped_vm_plus_helper_gib_s=sum(j["vm_service_gib_s"] for j in jobs) + helper_gib_s,
        initial_parent_memory_gib=samples[0]["total"]["memory_current"] / GIB,
        initial_parent_file_cache_gib=samples[0]["total"]["memory_stat"].get("file", 0) / GIB,
    )
    result["limitations"].append(
        "Parent slice includes retained prior-job cache charges. Scoped VM plus helper integral covers their observed lifetimes; it excludes charges retained in parent after VM exit. Report both, not raw parent difference as policy savings."
    )
    result.update(
        rejected=progress.get("rejected", []),
        rejected_count=len(progress.get("rejected", [])),
        all_admitted_passed=progress["status"] in ["completed", "completed-with-rejections"]
        and len(jobs) == progress["launched"]
        and not progress["failed"],
        verified_task_latency_s=quantiles([j["scheduled_to_verified_s"] for j in jobs]),
        queue_wait_s=quantiles([j["launch_delay_s"] for j in jobs]),
        peak_pending_requests=max(s.get("queued", 0) for s in samples),
        service_success_fraction=len(jobs) / len(spec["arrivals"]),
    )
    result["limitations"].append(
        "This cohort never expires queued requests. Every scheduled task must strictly complete; any VM or experiment abort is failure. Throughput includes queue and drain; queues are not live Agents."
    )
    save(out / "summary.json", result)
    return result


if __name__ == "__main__":
    import os

    assert os.environ.get("PVISOR_SWE_CONTEXT"), "Explicit prepared context required"
    r = summarize(sys.argv[1])
    print(
        __import__("json").dumps(
            {k: v for k, v in r.items() if k not in ["jobs", "backlog_curve"]}, indent=2
        )
    )
