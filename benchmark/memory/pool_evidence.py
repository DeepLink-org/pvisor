"""Read-only wire contracts for retained memory curves.

No process creation, clocks, observers, preparation or campaign loops live here.
The schema identifiers describe saved records, not active experiment versions.
"""

from scanner_evidence import TUNABLES, require_default_ksm
from scanner_evidence import digest as digest

ARMS = (
    "firecracker",
    "docker-rootless",
    "pvisor-off",
    "pvisor-ksm",
    "pvisor-physical",
    "pvisor-compressed",
    "pvisor-local",
)
PATTERNS = ("repeated", "random-shared", "random-unique")
BOUNDARIES = (0, 1, 2, 5, 10, 20, 40, 60)
POST_WRITE_BOUNDARIES = (1, 2, 5, 10, 20, 30)
CURRENT_SCHEMA = "pvisor-pool-curve-condition/v4"
WRITE_SCHEMAS = ("pvisor-pool-curve-condition/v3", CURRENT_SCHEMA)


def values(text):
    return {
        line.split()[0].rstrip(":"): int(line.split()[1])
        for line in text.splitlines()
        if len(line.split()) >= 2 and line.split()[1].isdigit()
    }


def validate_observation(rows, guards, *, tasks_max=128):
    if tasks_max not in (128, 256):
        raise ValueError("unsupported observation task budget")
    if not rows or any("error" in row for row in rows):
        raise ValueError("missing/failed external memory observation")
    for row in rows:
        budget = row["budget"]
        quota, period = map(int, budget["cpu.max"].split())
        if (
            budget["memory.max"] != str(2**31)
            or budget["memory.swap.max"] != "0"
            or budget["memory.swap.current"] != "0"
            or quota != 4 * period
            or period <= 0
            or budget["pids.max"] != str(tasks_max)
        ):
            raise ValueError("installed resource budget mismatch")
    if not guards or any(item.get("sessions", []) for item in guards):
        raise ValueError("foreign VM/build or missing host guard during sampling")


def expected_checks():
    checks, states = {}, {n: 0 for n in range(1, 5)}
    for token in ("ready", "readback"):
        for n in states:
            checks[token, n] = 0
    for percent in (25, 100):
        for n in states:
            checks[f"write-{percent}-{n}", n] = percent
            states[n] = percent
            for peer, state in states.items():
                checks[f"peer-{percent}-{n}", peer] = state
    for n in states:
        checks[f"exit-{n}", n] = 100
        for peer in range(n + 1, 5):
            checks[f"survivor-{n}", peer] = 100
    return checks


def validate_worker(raw, cfg):
    from docker_support import belongs

    if cfg["arm"] not in ARMS:
        raise ValueError("unsupported arm")
    if cfg.get("schema") not in (None, "pvisor-pool-curve-condition/v2", *WRITE_SCHEMAS):
        raise ValueError("unsupported condition schema")
    if raw.get("status") != "passed" or raw.get("condition") != cfg:
        raise ValueError("worker/condition mismatch")
    phases = raw.get("phases", [])
    names = [f"idle-{n}" for n in BOUNDARIES] + ["readback", "cow-25", "cow-100"]
    if cfg.get("schema") in WRITE_SCHEMAS:
        names += [f"post-write-{n}" for n in POST_WRITE_BOUNDARIES]
    if [p.get("name") for p in phases] != names:
        raise ValueError("missing or reordered memory phases")
    if phases[len(BOUNDARIES) - 1]["monotonic_ns"] - raw["all_ready_ns"] < 60_000_000_000:
        raise ValueError("idle scan window too short")
    if cfg.get("schema") in WRITE_SCHEMAS:
        origin = raw["post_write_origin_ns"]
        cow = next(p for p in phases if p["name"] == "cow-100")
        if origin > cow["monotonic_ns"] or phases[-1]["monotonic_ns"] - origin < 30_000_000_000:
            raise ValueError("post-write scan window too short or origin misplaced")
    expected_pids = phases[0]["guest_pids"]
    if len(expected_pids) != 4 or len(set(expected_pids)) != 4:
        raise ValueError("four distinct simultaneous VMs required")
    cpu, clock = 0, 0
    for phase in [raw["before"], *phases, raw["after"]]:
        c = phase["counters"]
        quota, period = c["cpu.max"].split()
        if (
            c["memory.max"].strip() != str(2**31)
            or c["memory.swap.max"].strip() != "0"
            or int(quota) != 4 * int(period)
        ):
            raise ValueError("phase escaped resource budget")
        if cfg.get("schema") == CURRENT_SCHEMA and c.get("pids.max", "").strip() != "256":
            raise ValueError("phase escaped current task budget")
        if cfg.get("schema") == CURRENT_SCHEMA and values(c["pids.events"]).get("max", 0):
            raise ValueError("task budget exhausted during condition")
        if any(values(c["memory.events"]).get(n, 0) for n in ("oom", "oom_kill", "max")) or int(
            c["memory.swap.current"]
        ):
            raise ValueError("OOM/swap/budget failure")
        usage = values(c["cpu.stat"])["usage_usec"]
        if usage < cpu or phase["monotonic_ns"] < clock:
            raise ValueError("nonmonotonic accounting")
        cpu, clock = usage, phase["monotonic_ns"]
        if phase["name"].startswith(("idle-", "post-write-")) or phase["name"] in (
            "readback",
            "cow-25",
            "cow-100",
        ):
            if phase["guest_pids"] != expected_pids:
                raise ValueError("VM missing from live phase")
            pids = [p["pid"] for p in phase["processes"]]
            if len(pids) != len(set(pids)) or not set(expected_pids) <= set(pids):
                raise ValueError("incomplete process PSS accounting")
        for process in phase["processes"]:
            if process["pss_bytes"] != values(process["smaps_rollup"])["Pss"] * 1024:
                raise ValueError("PSS evidence mismatch")
        if cfg.get("schema") in ("pvisor-pool-curve-condition/v2", *WRITE_SCHEMAS):
            require_default_ksm(phase["ksm"])
            if any(phase["ksm"]["files"][n] != raw["kernel_before"]["files"][n] for n in TUNABLES):
                raise ValueError("KSM changed during observation")
            expected_group = phase["cgroup"].removeprefix("/sys/fs/cgroup")
            for process in phase["processes"]:
                placed = (
                    belongs(process["cgroup"], expected_group)
                    if cfg["arm"] == "docker-rootless"
                    else f"0::{expected_group}" in process["cgroup"].splitlines()
                )
                if process["cpu_affinity"] != [0, 1, 2, 3] or not placed:
                    raise ValueError("process escaped CPU/cgroup placement")
    checks = raw.get("checks", [])
    expected = expected_checks()
    if len(checks) != len(expected) or {(c["token"], c["instance"]) for c in checks} != set(
        expected
    ):
        raise ValueError("missing/duplicated payload or isolation acknowledgement")
    for c in checks:
        percent = expected[c["token"], c["instance"]]
        if c["percent"] != percent or c["sha256"] != cfg["hashes"][f"{c['instance']}:{percent}"]:
            raise ValueError("full payload oracle mismatch")
    if {x["pid"] for x in raw["cleanup"] if x.get("reaped")} != set(expected_pids) or len(
        raw["cleanup"]
    ) != 4:
        raise ValueError("native reaping incomplete")
    for state in (raw["kernel_before"], raw["kernel_after"]):
        require_default_ksm(state)
    if any(raw["kernel_before"]["files"][n] != raw["kernel_after"]["files"][n] for n in TUNABLES):
        raise ValueError("KSM configuration changed")
    for phase in phases:
        mapped = {p["pid"]: p["mapping_evidence"]["mergeable_bytes"] for p in phase["processes"]}
        if cfg["arm"] == "pvisor-ksm":
            if not all(mapped[pid] > 0 for pid in expected_pids):
                raise ValueError("KSM advice missing from a VM")
        elif any(mapped[pid] for pid in expected_pids):
            raise ValueError("unexpected KSM participation")
