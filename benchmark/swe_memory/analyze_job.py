"""Read completed calibration only. Partial/failing evidence stays visible."""

import json
from pathlib import Path

from summary_reference import distribution, lines, summarize
from trace_checks import compare_timeouts


def load(p):
    return json.loads(p.read_text())


def integral(samples, begin, end):
    out = dict(duration_s=0, ram_gib_s=0, service_gib_s=0, cpu_s=0)
    assert samples[0]["host_monotonic_ns"] <= begin < end <= samples[-1]["host_monotonic_ns"]
    for a, b in zip(samples, samples[1:]):
        ta, tb = a["host_monotonic_ns"], b["host_monotonic_ns"]
        assert tb > ta
        lo, hi = max(ta, begin), min(tb, end)
        if hi <= lo:
            continue
        dt = (hi - lo) / 1e9
        out["duration_s"] += dt
        for field, av, bv in [
            ("ram_gib_s", a["guest_ram"]["pss_kib"] * 1024, b["guest_ram"]["pss_kib"] * 1024),
            ("service_gib_s", a["memory_current"], b["memory_current"]),
        ]:
            vl = av + (bv - av) * (lo - ta) / (tb - ta)
            vh = av + (bv - av) * (hi - ta) / (tb - ta)
            out[field] += (vl + vh) / 2 * dt / 2**30
        out["cpu_s"] += (
            (b["cpu"]["usage_usec"] - a["cpu"]["usage_usec"]) / 1e6 * (hi - lo) / (tb - ta)
        )
    out["mean_ram_mib"] = out["ram_gib_s"] * 1024 / out["duration_s"]
    out["mean_service_mib"] = out["service_gib_s"] * 1024 / out["duration_s"]
    return out


def analyze_job(ROOT, job, diagnostic=False):
    meta_path = job / "started.json"
    if not meta_path.exists():
        raise ValueError("Missing job start record")
    meta = load(meta_path)
    row = {
        "job": job.name,
        "mode": meta["mode"],
        "task": meta["task"],
        "policy": meta["policy"],
        "completed": (job / "completed.json").exists(),
        "error": load(job / "error.json") if (job / "error.json").exists() else None,
    }
    if (not row["completed"] and not diagnostic) or meta["mode"] == "oracle":
        row["check"] = load(job / "check.json") if (job / "check.json").exists() else None
        return row
    try:
        trial = job / "runs" / meta["task"]
        vm = trial / "vm"
        base = summarize(trial)
        ev = lines(vm / "events.jsonl")
        samples = lines(vm / "resources.jsonl")
        rt = load(vm / "runtime-events.json")
        live = lines(vm / "runtime-events.jsonl")
        phases = {e["event"]: e["host_monotonic_ns"] for e in ev}
        begin, end = phases["trial.agent_start"], phases["trial.agent_end"]
        row.update(
            {
                k: base[k]
                for k in [
                    "agent_s",
                    "model_calls",
                    "model_wait_s",
                    "control_counts",
                    "paused_s",
                    "offloaded_s",
                    "longest_offloaded_s",
                    "unclosed_pause",
                    "reclaims",
                    "driver_state",
                    "driver_exit_code",
                    "model_errors",
                    "delivery_errors",
                    "notifier_errors",
                    "max_memory_events",
                    "max_swap_bytes",
                    "stderr_truncated",
                    "max_sample_gap_s",
                ]
            }
        )
        row["agent"] = integral(samples, begin, end)
        row["whole_vm"] = integral(
            samples, samples[0]["host_monotonic_ns"], samples[-1]["host_monotonic_ns"]
        )
        row["vm_lifecycle_s"] = (phases["vm_stop"] - phases["vm_start"]) / 1e9
        row["verifier_s"] = (
            phases["trial.verification_end"] - phases["trial.verification_start"]
        ) / 1e9
        row["peak_service_mib"] = max(s["memory_current"] for s in samples) / 2**20
        row["peak_ram_pss_mib"] = max(s["guest_ram"]["pss_kib"] for s in samples) / 1024
        row["tool_s"] = 0
        pending = {}
        for e in ev:
            if not begin <= e["host_monotonic_ns"] <= end:
                continue
            if e["event"] == "tool_start":
                pending[e["execution_id"]] = e["host_monotonic_ns"]
            elif e["event"] == "tool_end":
                row["tool_s"] += (e["host_monotonic_ns"] - pending.pop(e["execution_id"])) / 1e9
        assert not pending
        starts = [e for e in ev if e["event"] == "model_request_start"]
        ends = [e for e in ev if e["event"] == "model_request_end"]
        fixture = lines(job / "fixture.jsonl")
        plan = load(ROOT / "plans" / f"{meta['task']}.json")
        assert len(starts) == len(ends) == len(fixture) == len(plan)
        ready = []
        late = []
        for i, (a, b, f, p) in enumerate(zip(starts, ends, fixture, plan)):
            assert (
                f["index"] == i + 1
                and f["status"] == "delivered"
                and f["target_wait_s"] == p["wait_s"]
            )
            ready.append((b["host_monotonic_ns"] - f["body_ready_monotonic_ns"]) / 1e6)
            late.append(
                (f["body_ready_monotonic_ns"] - f["wait_start_monotonic_ns"]) / 1e6
                - p["wait_s"] * 1000
            )
        row["ready_to_host_agent_ms"] = distribution(ready)
        row["provider_oversleep_ms"] = distribution(late)
        offset = meta["unix_ns"] - meta["host_monotonic_ns"]
        until = phases["trial.verification_end"] + offset
        row["control_failures"] = [
            e["data"]["payload"] for e in rt if e.get("data", {}).get("name") == "vm.control_failed"
        ]
        row["channel_failures_before_verifier_end"] = [
            e["data"]["payload"]
            for e in rt
            if e.get("data", {}).get("name") == "vm.idle_channel_failure"
            and e["observed_at_unix_ms"] * 1e6 <= until
        ]
        row["tool_timeouts"] = sum(
            e["event"] == "tool_end" and bool(e.get("timed_out")) for e in ev
        )
        source = Path(load(ROOT / "manifest.json")["tasks"][meta["task"]]["source_trial"])
        row["timeout_replay_check"] = compare_timeouts(ev, lines(source / "vm/events.jsonl"))
        row["tool_transport_errors"] = sum(e["event"] == "tool_transport_error" for e in ev)
        row["live_events_equal_final"] = live == rt
        offload_begin = None
        row["offload_intervals_unix_s"] = []
        for e in rt:
            d = e.get("data", {})
            if d.get("name") != "vm.control_completed":
                continue
            v = d["payload"]["value"]["vm"]["state"]
            op = d["payload"]["operation"]["kind"]["op"]
            t = e["observed_at_unix_ms"] / 1000
            if op == "run.offload" and v == "offloaded":
                assert offload_begin is None
                offload_begin = t
            elif op == "run.resume" and v == "running" and offload_begin is not None:
                row["offload_intervals_unix_s"].append([offload_begin, t])
                offload_begin = None
        assert offload_begin is None, "Unclosed offload interval"

        row["check"] = load(job / "check.json")
        backing = Path(load(job / "owned-unit.json")["backing"])
        stat = backing.stat()
        row["backing"] = {"logical_bytes": stat.st_size, "allocated_bytes": stat.st_blocks * 512}
        row["passed"] = all(
            [
                row["check"]["passed"],
                base["driver_state"] == "completed",
                base["driver_exit_code"] == 0,
                not base["model_errors"],
                not base["delivery_errors"],
                not base["notifier_errors"],
                not base["unclosed_pause"],
                not row["control_failures"],
                not row["channel_failures_before_verifier_end"],
                row["timeout_replay_check"]["passed"],
                not row["tool_transport_errors"],
                live == rt,
                base["max_swap_bytes"] == 0,
                base["max_memory_events"]["oom"] == 0,
                base["max_memory_events"]["oom_kill"] == 0,
            ]
        )
    except Exception as e:
        row["analysis_error"] = type(e).__name__ + ":" + str(e)
        row["passed"] = False
    return row
