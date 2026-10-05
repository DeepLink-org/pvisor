#!/usr/bin/env python3
"""Render measured Cluster scaling curves and CSV summaries; requires matplotlib."""

import argparse
import csv
import json
import statistics
from pathlib import Path

from evidence_tsv import load as load_evidence

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[2]
MIB = 1024**2
BLUE, GREEN, ORANGE, GRAY = "#2563eb", "#0f766e", "#d97706", "#64748b"


def percentile(values, probability):
    values = sorted(values)
    pos = (len(values) - 1) * probability
    lower = int(pos)
    upper = min(lower + 1, len(values) - 1)
    return values[lower] + (values[upper] - values[lower]) * (pos - lower)


def summarize_execution(report):
    if not report["passed"] or any(not row["passed"] for row in report["batches"]):
        raise ValueError("failed/incomplete reports cannot be rendered as successful curves")
    rows = []
    for size in sorted(report["conditions"]["sizes"]):
        batches = [r for r in report["batches"] if r["sandboxes"] == size and not r["warmup"]]
        assert len(batches) == report["conditions"]["repetitions"]
        ready = [t["ready_ms"] for b in batches for t in b["tasks"]]
        memory = [
            statistics.median(
                sum(v["current_bytes"] for v in s.values()) / MIB for s in b["plateau"]
            )
            for b in batches
        ]
        rate = [b["launches_per_second"] for b in batches]
        row = {
            "sandboxes": size,
            "batches": len(batches),
            "guest_samples": len(ready),
            "ready_p50_ms": statistics.median(ready),
            "ready_p95_ms": percentile(ready, 0.95),
            "ready_min_ms": min(ready),
            "ready_max_ms": max(ready),
            "all_ready_p50_ms": statistics.median(b["all_ready_ms"] for b in batches),
            "cgroup_current_p50_mib": statistics.median(memory),
            "cgroup_current_min_mib": min(memory),
            "cgroup_current_max_mib": max(memory),
            "cgroup_anon_p50_mib": statistics.median(
                statistics.median(
                    sum(v["anon_bytes"] for v in s.values()) / MIB for s in b["plateau"]
                )
                for b in batches
            ),
            "cgroup_file_p50_mib": statistics.median(
                statistics.median(
                    sum(v["file_bytes"] for v in s.values()) / MIB for s in b["plateau"]
                )
                for b in batches
            ),
            "native_pss_sum_p50_mib": statistics.median(
                sum(
                    p["pss_bytes"]
                    for processes in b["native_identities"].values()
                    for p in processes
                )
                / MIB
                for b in batches
            ),
            "launch_rate_p50_per_second": statistics.median(rate),
            "launch_rate_min_per_second": min(rate),
            "launch_rate_max_per_second": max(rate),
        }
        rows.append(row)
    return rows


def summarize_controller(data):
    rows = []
    for size in [1000, 10000, 100000, 1000000]:
        report = load_evidence(data / f"controller-history-{size}.tsv")
        assert report["source_identity_unchanged_during_measurement"]
        r = report["rows"][0]
        assert r["tasks"] == size and r["ready_tasks_before_poll"] == 1
        rows.append(
            {
                "records": size,
                "indexed_p50_ns": r["indexed_counts"]["p50"],
                "indexed_p95_ns": r["indexed_counts"]["p95"],
                "scan_reference_p50_ns": r["full_task_record_scan_reference"]["p50"],
                "process_fixture_rss_mib": r["controller_plus_id_fixture_memory_before_reference"][
                    "VmRSS_kib"
                ]
                / 1024,
                "journal_mib": r["wal_bytes_before_poll"] / MIB,
                "warm_reopen_seconds": r["reopen_us"] / 1e6,
                "assignment_poll_ms": r["current_poll_us_including_wal_and_admission"] / 1000,
                "assignment_polls": r["current_polls_until_assignment"],
            }
        )
    return rows


def csv_write(path, rows):
    with path.open("w") as out:
        writer = csv.DictWriter(out, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def save(fig, output, name):
    fig.savefig(
        output / f"{name}.svg", bbox_inches="tight", facecolor="white", metadata={"Date": None}
    )
    fig.savefig(output / f"{name}.png", bbox_inches="tight", facecolor="white", dpi=160)
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=ROOT / "docs/src/assets/benchmarks/cluster-scalability-20261005",
    )
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args()
    output = args.output_dir or args.data_dir
    output.mkdir(parents=True, exist_ok=True)
    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "font.size": 10,
            "svg.fonttype": "none",
            "axes.spines.top": False,
            "axes.spines.right": False,
            "text.color": "#1e293b",
            "axes.labelcolor": "#475569",
        }
    )
    report = load_evidence(args.data_dir / "vm.tsv")
    execution = summarize_execution(report)
    control = summarize_controller(args.data_dir)
    csv_write(output / "vm-summary.csv", execution)
    csv_write(output / "controller-summary.csv", control)
    fig, axes = plt.subplots(1, 3, figsize=(15, 5.1))
    x = [r["sandboxes"] for r in execution]
    for ax in axes:
        ax.set_xticks(x)
        ax.set_xlabel("Simultaneous real KVM guests")
        ax.grid(alpha=0.18)
        ax.set_axisbelow(True)
    ax = axes[0]
    ax.fill_between(
        x,
        [r["cgroup_current_min_mib"] for r in execution],
        [r["cgroup_current_max_mib"] for r in execution],
        color=BLUE,
        alpha=0.13,
    )
    for field, label, color in [
        ("cgroup_current_p50_mib", "Total cgroup charge", BLUE),
        ("cgroup_anon_p50_mib", "Anonymous memory", GREEN),
        ("cgroup_file_p50_mib", "File / shmem charge", GRAY),
    ]:
        ax.plot(x, [r[field] for r in execution], "o-", color=color, label=label)
    ax.set(title="Live memory footprint", ylabel="Controller + Workers (MiB)", ylim=(0, None))
    ax.legend(frameon=False, fontsize=9)
    ax = axes[1]
    ax.fill_between(
        x,
        [r["ready_min_ms"] for r in execution],
        [r["ready_max_ms"] for r in execution],
        color=GREEN,
        alpha=0.13,
        label="Observed min / max",
    )
    for field, label, color in [
        ("ready_p50_ms", "Per-guest P50", GREEN),
        ("ready_p95_ms", "Empirical P95", ORANGE),
    ]:
        ax.plot(x, [r[field] for r in execution], "o-", color=color, label=label)
    ax.set(title="Submit to guest readiness", ylabel="Latency (ms)", ylim=(0, None))
    ax.legend(frameon=False, fontsize=9)
    ax = axes[2]
    values = [r["launch_rate_p50_per_second"] for r in execution]
    ax.fill_between(
        x,
        [r["launch_rate_min_per_second"] for r in execution],
        [r["launch_rate_max_per_second"] for r in execution],
        color=BLUE,
        alpha=0.13,
    )
    ax.plot(x, values, "o-", color=BLUE, label="Measured launch rate")
    ax.plot(
        x, [values[0] * n / x[0] for n in x], "--", color=GRAY, label="Linear reference from N=1"
    )
    ax.set(title="Burst launch capacity", ylabel="Guests / time-to-all-ready (s)", ylim=(0, None))
    ax.legend(frameon=False, fontsize=9)
    fig.suptitle("Cluster execution scaling: measured N=1, 2, 4", fontsize=16, fontweight="bold")
    fig.text(
        0.5,
        0.015,
        "5 measured batches per point + excluded warmup | 128 MiB / 1 vCPU per guest | 0.5 core / 512 MiB per Worker\nShared Linux host; prepared rootfs; warm caches. Bands show observed ranges, not confidence intervals. Launch rate is not Agent throughput.",
        ha="center",
        fontsize=9,
        color=GRAY,
    )
    fig.tight_layout(rect=(0, 0.10, 1, 0.93))
    save(fig, output, "execution")
    fig, axes = plt.subplots(1, 3, figsize=(15, 5.1))
    x = [r["records"] for r in control]
    for ax in axes:
        ax.set_xscale("log")
        ax.set_xticks(x, ["1k", "10k", "100k", "1M"])
        ax.set_xlabel("Retained task records (one ready task)")
        ax.grid(alpha=0.18)
        ax.set_axisbelow(True)
    ax = axes[0]
    ax.set_yscale("log")
    ax.plot(
        x, [r["indexed_p50_ns"] for r in control], "o-", color=GREEN, label="Indexed counts P50"
    )
    ax.plot(
        x,
        [r["scan_reference_p50_ns"] for r in control],
        "o-",
        color=ORANGE,
        label="Full-scan algorithm P50",
    )
    ax.set(title="Counts query cost", ylabel="Nanoseconds / call (log scale)")
    ax.legend(frameon=False, fontsize=9)
    ax = axes[1]
    ax.set_yscale("log")
    ax.plot(
        x,
        [r["process_fixture_rss_mib"] for r in control],
        "o-",
        color=BLUE,
        label="Process + ID fixture RSS",
    )
    ax.plot(
        x, [r["journal_mib"] for r in control], "o-", color=GRAY, label="Intent / receipt journal"
    )
    ax.set(title="Retention still costs memory / disk", ylabel="MiB (log scale)")
    ax.legend(frameon=False, fontsize=9)
    ax = axes[2]
    ax.plot(x, [r["warm_reopen_seconds"] for r in control], "o-", color=ORANGE)
    ax.set(
        title="Restart still scales with history",
        ylabel="Warm local journal replay (s)",
        ylim=(0, None),
    )
    for r in control:
        ax.annotate(
            f"{r['warm_reopen_seconds']:.2f}s",
            (r["records"], r["warm_reopen_seconds"]),
            xytext=(-4, 8),
            textcoords="offset points",
            ha="right",
            fontsize=9,
        )
    fig.suptitle(
        "Controller history scaling: archived typed-API measurements",
        fontsize=16,
        fontweight="bold",
    )
    fig.text(
        0.5,
        0.015,
        "2026-10-05 v3 archive | Separate process per size; release binary; one pinned CPU | No guests or HTTP load\n20 query samples; replay/RSS are single observations. Scan is an algorithm reference on the same records, not an old-binary benchmark.",
        ha="center",
        fontsize=9,
        color=GRAY,
    )
    fig.tight_layout(rect=(0, 0.10, 1, 0.93))
    save(fig, output, "controller")
    print(json.dumps({"execution": execution, "controller": control}, indent=2))


if __name__ == "__main__":
    main()
