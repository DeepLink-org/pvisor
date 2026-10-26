"""Render English review artifacts from existing evidence; never starts a VM."""

import argparse
import hashlib
import html
import json
import math
import re
from pathlib import Path

POLICIES = ("baseline", "offload")
LABELS = {"baseline": "Management disabled", "offload": "SSD offload"}


def load(path):
    return json.loads(path.read_text())


def relative_change(base, value):
    if base == 0:
        return "0.0%" if value == 0 else "N/A (zero baseline)"
    return f"{100 * (value / base - 1):+.1f}%"


def step_mean(samples, field, end=1800):
    if samples[0]["elapsed_s"] > 3 or samples[-1]["elapsed_s"] < end:
        raise ValueError("Samples do not cover the common arrival window")
    integral = 0
    for a, b in zip(samples, samples[1:]):
        dt = max(0, min(end, b["elapsed_s"]) - a["elapsed_s"])
        value = a["phases"]["agent"] if field == "agent" else a[field]
        integral += value * dt
    return integral / end


def metrics(result, samples):
    if (
        not result["offline_batch_passed"]
        or not result["safety_audit_passed"]
        or result["verified"] != 60
        or len(result["jobs"]) != 60
        or result["rejected_count"]
    ):
        raise ValueError("A partial/failed cohort cannot produce a matched-work report")
    if len(samples) < 2:
        raise ValueError("Missing time series")
    integral = 0
    for a, b in zip(samples, samples[1:]):
        dt = (b["host_monotonic_ns"] - a["host_monotonic_ns"]) / 1e9
        if not 0 < dt <= 10 or b["elapsed_s"] <= a["elapsed_s"]:
            raise ValueError("Non-monotonic or gapped samples")
        integral += (
            sum(x[k]["memory_current"] for x in (a, b) for k in ("total", "supervisor"))
            / 2
            / 2**30
            * dt
        )
    if not math.isclose(integral, result["combined_memory_gib_s"], abs_tol=0.001, rel_tol=0):
        raise ValueError("Memory integral differs from final acceptance")
    for s in samples:
        if s["phases"]["agent"] < s["phases"]["model_wait"] + s["phases"]["tool_exec"]:
            raise ValueError("Phase counts overlap")
    return [
        ("Verified tasks", result["verified"], 0),
        ("Batch makespan (min)", result["total_wall_s"] / 60, 2),
        (
            "Effective throughput including drain (tasks/min)",
            result["throughput_per_min_including_drain"],
            3,
        ),
        ("Mean active agents, common first 30 min", step_mean(samples, "agent"), 2),
        ("Peak active agents (observed)", max(s["phases"]["agent"] for s in samples), 0),
        ("Mean queued requests, common first 30 min", step_mean(samples, "queued"), 2),
        ("Peak queued requests", max(s["queued"] for s in samples), 0),
        ("Total memory-time integral (GiB min)", integral / 60, 2),
        ("Queue wait P50 (min)", result["queue_wait_s"]["p50"] / 60, 2),
        ("Queue wait P95 (min)", result["queue_wait_s"]["p95"] / 60, 2),
        ("Queue wait maximum (min)", result["queue_wait_s"]["maximum"] / 60, 2),
        (
            "Scheduled arrival to verification P95 (min)",
            result["verified_task_latency_s"]["p95"] / 60,
            2,
        ),
    ]


def render(results_path, sample_paths, output, historical=False):
    results = load(results_path)
    protocol = load(Path(__file__).with_name("protocol.json"))
    expected = sorted((a["at_s"], a["task"]) for a in protocol["arrival_spec"]["arrivals"])
    for policy in POLICIES:
        actual = sorted((j["scheduled_s"], j["task"]) for j in results[policy]["jobs"])
        if actual != expected or len({j["job"] for j in results[policy]["jobs"]}) != len(expected):
            raise ValueError("Report does not match the frozen complete workload/arrival table")
    data = {
        p: [json.loads(line) for line in sample_paths[p].read_text().splitlines()] for p in POLICIES
    }
    rows = {p: metrics(results[p], data[p]) for p in POLICIES}
    output.mkdir(parents=True, exist_ok=False)
    table = [
        "| Metric | Management disabled | SSD offload | Change vs. baseline |",
        "|---|---:|---:|---:|",
    ]
    for a, b in zip(rows["baseline"], rows["offload"]):
        name, base, precision = a
        table.append(
            f"| {name} | {base:,.{precision}f} | {b[1]:,.{precision}f} | {relative_change(base, b[1])} |"
        )
    description = """# Agent density under a fixed memory budget: SWE drain comparison

Historical engineering evidence from gpu02, 2026-10-26. This is one
baseline-first/offload-second pair using a frozen runtime, not a measurement
of current main or a rerun of the refactored harness.

Both arms use the same 60 tasks and arrivals, two vCPUs, 250% whole-VM CPU,
32 GiB scoped RAM, total CPU 6000%, swap 0 and unrestricted offload concurrency.
FIFO admission reserves 8 GiB, credits half of clean unmapped inactive cache,
and keeps startup reservations. Queue wait never expires; execution and
experiment fault bounds still apply. No real model is called.

"""
    caveats = """

![Memory, live agents, pending requests and verified completions](memory-density.svg)

Changes are descriptive `(offload / baseline - 1) * 100%`, computed before
rounding. They are not randomized causal estimates or confidence intervals.
Both arms completed all 60 tasks with no rejection, VM failure, OOM or swap.
Active agents include tool execution and replayed model waits; requests waiting
for VM creation are excluded. Occupancy peaks are not sustainable capacity.

Total memory-time integrates condition plus coordinator memory, including
private image, overlay and backing caches. Guest RAM PSS is a separate measure
and must not be substituted for total memory savings. Common OS/shared-code
cache is outside the scoped budget; physical SSD placement is fixed without a
hard IOPS quota. Memory is plotted only while observed, not extrapolated to zero
after completion. Both arms accumulate queues; batch drainage is not proof of
sustainable throughput at this arrival rate. The original online exploration
criteria did not pass; this report evaluates the offline all-tasks-complete goal.

The frozen binary SHA256 is
`08da082749523a93f7a75a8e329c7753a8254c16d74064c1f00365908fc1fc2f`.
The historical acceptance recorded 120 actual VM exits and 7676 unchanged frozen
files. Local harness tests are reported separately from these historical runs.
"""
    delivery = {p: results[p]["model_delivery_ms"]["p99"] for p in POLICIES}
    caveats += f"\nThe delivery cost remains visible: replay response-delivery P99 was {delivery['baseline']:.1f} ms versus {delivery['offload']:.1f} ms. The table emphasizes queue-inclusive latency; this does not mean resume is free.\n"
    if not historical:
        description = description.replace(
            "Historical engineering evidence from gpu02, 2026-10-26. This is one\nbaseline-first/offload-second pair using a frozen runtime, not a measurement\nof current main or a rerun of the refactored harness.",
            "Engineering evidence from one baseline-first/offload-second pair using the\nexplicitly frozen runtime. This does not measure current main.",
        )
        caveats = caveats.replace(
            "The historical acceptance recorded 120 actual VM exits and 7676 unchanged frozen\nfiles. Local harness tests are reported separately from these historical runs.",
            "Actual exits and frozen inputs are checked by condition review and postflight.\nLocal harness tests are separate from measured VM runs.",
        )
    text = description + "\n".join(table) + caveats
    (output / "RESULTS.md").write_text(text)
    provenance = {
        p.name: hashlib.sha256(p.read_bytes()).hexdigest()
        for p in [results_path, *sample_paths.values()]
    }
    (output / "sources.json").write_text(json.dumps(provenance, indent=2) + "\n")
    plot(results, data, output)
    table_html = (
        "<table><thead><tr>"
        + "".join("<th>" + html.escape(c.strip()) + "</th>" for c in table[0].strip("|").split("|"))
        + "</tr></thead><tbody>"
    )
    for line in table[2:]:
        table_html += (
            "<tr>"
            + "".join("<td>" + html.escape(c.strip()) + "</td>" for c in line.strip("|").split("|"))
            + "</tr>"
        )
    table_html += "</tbody></table>"
    (output / "report.html").write_text(
        '<!doctype html><meta charset="utf-8"><title>SWE memory-budget comparison</title><style>body{font:16px system-ui;max-width:1200px;margin:40px auto;padding:20px}table{border-collapse:collapse;width:100%}th,td{padding:9px;border-bottom:1px solid #ddd;text-align:right}th:first-child,td:first-child{text-align:left}img{width:100%}pre{white-space:pre-wrap;line-height:1.5}</style><h1>Agent density under a fixed memory budget</h1><pre>'
        + html.escape(description.split("\n", 1)[1])
        + "</pre>"
        + table_html
        + '<img src="memory-density.svg" alt="Four measured time series"><pre>'
        + html.escape(caveats.split("\n\n", 2)[-1])
        + "</pre>"
    )


def plot(results, samples, output):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update(
        {
            "svg.fonttype": "none",
            "font.size": 11,
            "axes.spines.top": False,
            "axes.spines.right": False,
        }
    )
    figure, axes = plt.subplots(2, 2, figsize=(14, 9), sharex=True)
    end = max(v["total_wall_s"] / 60 for v in results.values())
    limit = math.ceil(end / 5) * 5
    for policy, color in zip(POLICIES, ("#2b65ad", "#df742a")):
        series = samples[policy]
        t = [s["elapsed_s"] / 60 for s in series]
        done = results[policy]["total_wall_s"] / 60
        axes[0, 0].plot(
            t,
            [
                (s["total"]["memory_current"] + s["supervisor"]["memory_current"]) / 2**30
                for s in series
            ],
            color=color,
            label=LABELS[policy],
        )
        axes[0, 1].step(
            t + [done], [s["phases"]["agent"] for s in series] + [0], where="post", color=color
        )
        axes[1, 0].step(t + [done], [s["queued"] for s in series] + [0], where="post", color=color)
        verified = sorted(j["verified_at_s"] / 60 for j in results[policy]["jobs"])
        axes[1, 1].step(
            [0] + verified + [limit],
            [0] + list(range(1, len(verified) + 1)) + [len(verified)],
            where="post",
            color=color,
        )
    protocol = load(Path(__file__).with_name("protocol.json"))
    arrivals = protocol["arrival_spec"]["arrivals"]
    axes[1, 1].step(
        [0] + [a["at_s"] / 60 for a in arrivals] + [limit],
        [0] + list(range(1, len(arrivals) + 1)) + [len(arrivals)],
        where="post",
        color="#89939b",
        linestyle="--",
        label="Scheduled arrivals",
    )
    axes[0, 0].axhline(32, linestyle="--", color="#777", label="32 GiB scoped budget")
    axes[0, 0].legend(fontsize=9, loc="lower left")
    axes[1, 1].legend(fontsize=9)
    for ax, title, ylabel in zip(
        axes.flat,
        (
            "Total memory: condition + coordinator",
            "Active agents: tools or replayed model waits",
            "Pending requests: no VM created",
            "Cumulative strictly verified completions",
        ),
        ("Memory (GiB)", "Active agents", "Queued requests", "Completed tasks"),
    ):
        ax.set_title(title, loc="left", fontsize=12)
        ax.set_ylabel(ylabel)
        ax.set_ylim(bottom=0)
        ax.set_xlim(0, limit)
        ax.grid(alpha=0.2)
        ax.axvline(30, linestyle=":", color="#aaa")
    for ax in axes[1]:
        ax.set_xlabel("Time since batch start (min)")
    figure.suptitle(
        "Same 60 SWE tasks, same 32 GiB: memory use and observed Agent density", fontsize=16
    )
    figure.text(
        0.08,
        0.025,
        "Single sequential pair; observed occupancy is not sustainable capacity. Private caches included; no post-run memory extrapolation.",
        fontsize=10,
    )
    figure.tight_layout(rect=(0, 0.06, 1, 0.95))
    figure.savefig(output / "memory-density.svg")
    svg = output / "memory-density.svg"
    # Keep vector coordinates intact while avoiding thousands of whitespace-only
    # path continuation lines in a reviewable source diff.
    source = re.sub(
        r'd="([^"]*)"', lambda m: 'd="' + " ".join(m.group(1).split()) + '"', svg.read_text()
    )
    svg.write_text("\n".join(line.rstrip() for line in source.splitlines()) + "\n")
    figure.savefig(output / "memory-density.png", dpi=160)
    plt.close(figure)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--baseline-samples", type=Path, required=True)
    parser.add_argument("--offload-samples", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--historical", action="store_true")
    args = parser.parse_args()
    render(
        args.results,
        {"baseline": args.baseline_samples, "offload": args.offload_samples},
        args.output,
        historical=args.historical,
    )
