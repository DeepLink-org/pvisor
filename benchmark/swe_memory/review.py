"""Recheck immutable acceptance receipts and summarize one completed condition."""

import json

from analyze_batch import summarize
from arrival_batch import inactive
from arrival_common import CONTEXT, GIB, ROOT, load, quantiles, save, sha
from audit_shutdown import audit_job


def review():
    spec = load(ROOT / "spec.json")
    policy = CONTEXT["policy"]
    ident = spec["id"] + "-" + policy
    result = summarize(ident)
    if (
        not result["all_passed"]
        or result["verified"] != len(spec["arrivals"])
        or result["rejected_count"]
    ):
        raise ValueError("All scheduled tasks must strictly complete without rejection")
    controls = offloads = 0
    deliveries = []
    for done in result["jobs"]:
        job = ROOT / "jobs" / done["job"]
        for relative, expected in load(job / "evidence-hashes.json").items():
            path = job / relative
            if path.resolve().is_relative_to(job.resolve()) is False or sha(path) != expected:
                raise ValueError("Verified evidence changed: " + relative)
        owned = inactive(job)
        release = load(job / "backing-release.json")
        scope = load(job / "scope.json")
        if (
            not release["released"]
            or not release["unit_inactive"]
            or release["path"] != owned["backing"]
            or release["unit"] != owned["unit"]
        ):
            raise ValueError("Invalid post-exit backing-release receipt")
        if release["device"] != scope["placement_devices"]["ram"]:
            raise ValueError("Backing device differs from original scope")
        audit_job(job, backing_device=release["device"])
        analysis = load(job / "analysis.json")
        if not analysis["passed"] or analysis["max_sample_gap_s"] > 10:
            raise ValueError("Job acceptance failed")
        controls += sum(analysis["control_counts"].values())
        offloads += len(analysis["reclaims"])
        events = [
            json.loads(x)
            for x in (job / "runs" / done["task"] / "vm/events.jsonl").read_text().splitlines()
        ]
        ends = [e["host_monotonic_ns"] for e in events if e["event"] == "model_request_end"]
        fixture = [json.loads(x) for x in (job / "fixture.jsonl").read_text().splitlines()]
        if len(ends) != len(fixture):
            raise ValueError("Incomplete model delivery evidence")
        deliveries += [(end - f["body_ready_monotonic_ns"]) / 1e6 for end, f in zip(ends, fixture)]
    if policy == "baseline" and controls != 0:
        raise ValueError("Baseline performed active management")
    if policy == "offload" and offloads == 0:
        raise ValueError("Offload condition never offloaded")
    samples = [
        json.loads(x)
        for x in (ROOT / "batches" / ident / "resources.jsonl").read_text().splitlines()
    ]
    integral = 0
    for a, b in zip(samples, samples[1:]):
        dt = (b["host_monotonic_ns"] - a["host_monotonic_ns"]) / 1e9
        if not 0 < dt <= 10:
            raise ValueError("Invalid observation gap")
        integral += (
            sum(x[k]["memory_current"] for x in (a, b) for k in ("total", "supervisor"))
            / 2
            / GIB
            * dt
        )
    for sample in samples:
        for key in ("total", "supervisor"):
            if sample[key]["swap_current"] or any(
                sample[key]["memory_events"][e] for e in ("oom", "oom_kill")
            ):
                raise ValueError("OOM or swap in sampled condition")
    result.update(
        offline_batch_passed=True,
        safety_audit_passed=True,
        combined_memory_gib_s=integral,
        actual_offloads=offloads,
        model_delivery_ms=quantiles(deliveries),
    )
    save(ROOT / "condition-review.json", result)
    return result


if __name__ == "__main__":
    review()
