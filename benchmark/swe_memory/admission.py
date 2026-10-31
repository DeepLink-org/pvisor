"""Common memory admission for pending requests; never gates resume or offload."""

import json
from pathlib import Path

GIB = 2**30


def clean_credit(stat):
    # Do not credit mapped, dirty, writeback, or shmem pages. Half credit only.
    clean = max(
        0,
        min(stat["inactive_file"], stat["file"])
        - stat["file_mapped"]
        - stat["file_dirty"]
        - stat["file_writeback"]
        - stat["shmem"],
    )
    return clean // 2


def pressure_some(text):
    line = next(line for line in text.splitlines() if line.startswith("some "))
    return float(dict(x.split("=") for x in line.split()[1:])["avg10"])


def startup_reservations(entries, now, plan):
    reserved = 0
    for e in entries:
        if now - e["launch_s"] >= plan["startup_reservation_s"] and e.get("scope_seen", False):
            continue
        reserved += max(0, plan["boot_bytes"][e["task"]] - e.get("observed_vm_bytes", 0))
    return reserved


def decision(snapshot, entries, now, task, plan):
    total = snapshot["total"]
    stat = total["memory_stat"]
    credit = clean_credit(stat)
    pending = startup_reservations(entries, now, plan)
    projected = (
        total["memory_current"]
        - credit
        + pending
        + plan["boot_bytes"][task]
        + plan["headroom_bytes"]
    )
    reasons = []
    if now - snapshot["elapsed_s"] > plan["max_snapshot_age_s"]:
        reasons.append("stale_sample")
    if projected > plan["condition_bytes"]:
        reasons.append("memory_budget")
    psi = pressure_some(total["pressure"]["memory"])
    if psi >= plan["memory_psi_avg10_max"]:
        reasons.append("memory_pressure")
    return dict(
        allowed=not reasons,
        reasons=reasons,
        charged_bytes=total["memory_current"],
        clean_cache_credit_bytes=credit,
        pending_boot_bytes=pending,
        new_boot_bytes=plan["boot_bytes"][task],
        headroom_bytes=plan["headroom_bytes"],
        projected_bytes=projected,
        memory_psi_avg10=psi,
    )


def observe(entry):
    p = entry["job"] / "scope.json"
    if not p.exists():
        return
    scope = json.loads(p.read_text())
    g = Path(scope["cgroup"])
    if (g / "memory.current").exists():
        entry["observed_vm_bytes"] = int((g / "memory.current").read_text())
        entry["scope_seen"] = True


def expired(arrival, now, plan):
    limit = plan["max_queue_s"]
    return limit is not None and now - arrival["at_s"] > limit
