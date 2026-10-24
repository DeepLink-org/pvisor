#!/usr/bin/env python3
"""Plot retained startup, image-inventory and memory engineering cohorts.

Serves B-OPENSANDBOX-ENG, B-IMAGE-STORAGE-ENG and
B-OPENSANDBOX-MEMORY-ENG. Reads existing measurements only; never launches a
sandbox, pools cohorts, extrapolates capacity or overwrites source evidence.
"""

import argparse
import hashlib
import json
import statistics
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402

MIB = 1024**2
ARMS = (
    ("opensandbox-docker", "OpenSandbox + private Docker", "#64748b"),
    ("native", "pVisor default", "#2563eb"),
    ("native-pool", "pVisor physical pool (explicit)", "#0d9488"),
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Figures:
    def __init__(self, args):
        self.args = args
        self.inputs = {}
        self.outputs = {}
        self.data = {}

    def read(self, path):
        self.inputs[str(path)] = sha(path)
        return json.loads(path.read_text())

    def save(self, fig, name, data, scope):
        path = self.args.output_dir / f"opensandbox-{name}.svg"
        path.parent.mkdir(parents=True, exist_ok=True)
        fig.savefig(path, metadata={"Date": None, "Description": scope})
        if self.args.preview_dir:
            self.args.preview_dir.mkdir(parents=True, exist_ok=True)
            fig.savefig(self.args.preview_dir / f"{name}.png", dpi=120)
        self.outputs[str(path)] = sha(path)
        self.data[name] = {"scope": scope, "data": data}
        plt.close(fig)


def clean(ax, axis="y"):
    ax.set_axisbelow(True)
    ax.grid(axis=axis, color="#e2e8f0", linewidth=0.7)
    ax.spines[["top", "right"]].set_visible(False)


def group_bars(ax, categories, series, digits=1):
    """Display every group, its median bar and min–max whisker; not a CI."""
    width = 0.24
    x = np.arange(len(categories))
    for index, (label, color, groups) in enumerate(series):
        centers = x + (index - (len(series) - 1) / 2) * width
        medians = np.array([statistics.median(group) for group in groups])
        lows = np.array([min(group) for group in groups])
        highs = np.array([max(group) for group in groups])
        bars = ax.bar(
            centers,
            medians,
            width,
            color=color,
            alpha=0.85,
            label=label,
            yerr=[medians - lows, highs - medians],
            capsize=3,
            zorder=3,
        )
        ax.bar_label(bars, fmt=f"%.{digits}f", padding=9, fontsize=9)
        for center, group in zip(centers, groups):
            ax.scatter(
                center + np.linspace(-0.06, 0.06, len(group)),
                group,
                s=17,
                color=color,
                edgecolors="white",
                linewidths=0.5,
                zorder=5,
            )
    ax.set_xticks(x, categories)
    ax.set_ylim(0, max(max(group) for _, _, groups in series for group in groups) * 1.28)
    clean(ax)


def startup(figures):
    batch = figures.args.startup_batch
    summary = figures.read(batch / "summary.json")
    assert summary["benchmark"] == "B-OPENSANDBOX-ENG"
    audit = figures.read(batch / "audit.json")
    assert audit["all_82_sandboxes_correct_and_deleted"] and audit["failure"] is None
    fig, axes = plt.subplots(2, 2, figsize=(11, 6.5), constrained_layout=True)
    plotted = {}
    for row, (scenario, file, title) in enumerate(
        (
            ("first-after-api-restart", "first/samples.json", "First after API restart"),
            ("steady", "steady.json", "Steady: fresh sandbox"),
        )
    ):
        raw = figures.read(batch / file)["samples"]
        raw = [sample for sample in raw if sample["pair"] >= 0]
        assert len(raw) == 40 and all(s["success"] and s["delete_confirmed"] for s in raw)
        assert summary["scenarios"][scenario]["failed_samples"] == 0
        for col, (metric, metric_title) in enumerate(
            (
                ("create_seconds", "SDK create"),
                ("first_command_seconds", "Create → first checked command"),
            )
        ):
            ax = axes[row, col]
            stats = summary["scenarios"][scenario]["metrics"][metric]
            values = []
            for arm in ("docker", "native"):
                samples = sorted((s for s in raw if s["arm"] == arm), key=lambda s: s["pair"])
                assert [s["pair"] for s in samples] == list(range(20))
                group = [s[metric] * 1000 for s in samples]
                assert np.allclose(sorted(group), stats[arm]["sorted_ms"])
                values.append(group)
            boxes = ax.boxplot(
                values, positions=[0, 1], widths=0.35, patch_artist=True, showfliers=False
            )
            for box, color in zip(boxes["boxes"], (ARMS[0][2], ARMS[1][2])):
                box.set(facecolor=color, alpha=0.2, edgecolor=color)
            for pair in range(20):
                ax.plot(
                    [0, 1],
                    [values[0][pair], values[1][pair]],
                    color="#cbd5e1",
                    alpha=0.5,
                    linewidth=0.6,
                )
            for pos, group, color in zip([0, 1], values, (ARMS[0][2], ARMS[1][2])):
                ax.scatter(pos + np.linspace(-0.08, 0.08, 20), group, color=color, s=17, zorder=4)
                ax.text(
                    pos,
                    max(group) + 14,
                    f"P50 {statistics.median(group):.1f}",
                    ha="center",
                    fontsize=9,
                )
            ax.set_xticks([0, 1], ["OpenSandbox Docker/runc", "pVisor default"])
            ax.set_ylabel("Elapsed time (ms)")
            ax.set_ylim(0, 700)
            ax.set_title(f"{title}\n{metric_title}", fontsize=11)
            clean(ax)
            plotted[f"{scenario}/{metric}"] = {"docker": values[0], "native": values[1]}
    fig.suptitle(
        "All 20 matched pairs per scenario · prepared images · no warm sandbox reuse", fontsize=12
    )
    figures.save(
        fig,
        "startup-distribution",
        plotted,
        "Startup cohort only; each dot one valid sample; lines join randomized trial pairs; boxes show quartiles, whiskers 1.5 IQR; no tail-latency claim.",
    )


def disk(figures):
    summary = figures.read(figures.args.disk_summary)
    assert summary["benchmark"] == "B-IMAGE-STORAGE-ENG" and summary["status"] == "passed"
    rows = summary["rows"]
    assert len(rows) == 5 and all(row["repeats"] == 3 for row in rows)
    fig, axes = plt.subplots(1, 2, figsize=(11, 4.5), constrained_layout=True)
    ax = axes[0]
    x = np.arange(4)
    for offset, key, label, color in (
        (-0.19, "docker_incremental_allocated", "Docker", ARMS[0][2]),
        (0.19, "pvisor_allocated", "pVisor compressed pool", ARMS[1][2]),
    ):
        values = [row[key] / MIB for row in rows[:4]]
        bars = ax.bar(x + offset, values, 0.38, label=label, color=color)
        ax.bar_label(bars, fmt="%.1f", padding=4, fontsize=9)
    ax.set_xticks(x, ["Alpine\n3.20", "Ubuntu\n24.04", "Python\n3.12 slim", "Python\n3.13 slim"])
    ax.set_ylim(0, 215)
    ax.set_ylabel("Allocated image-store blocks (MiB)")
    ax.set_title("Separate single-image inventories · N=3")
    ax.legend(frameon=False, fontsize=9)
    clean(ax)
    ax = axes[1]
    combined = next(row for row in rows if row["image"] == "combined")
    values = [
        combined["docker_incremental_allocated"] / MIB,
        summary["individual_store_sum"] / MIB,
        summary["combined_store"] / MIB,
    ]
    bars = ax.bar([0, 1, 2], values, color=[ARMS[0][2], "#93c5fd", ARMS[1][2]], width=0.6)
    ax.bar_label(bars, fmt="%.2f", padding=5)
    ax.set_xticks([0, 1, 2], ["Docker\none store", "pVisor\nfour stores", "pVisor\none pool"])
    ax.set_ylim(0, 430)
    ax.set_title("Same four-image set\n66.5% below Docker; 28.1% below four pVisor stores")
    clean(ax)
    figures.save(
        fig,
        "disk-inventory",
        summary,
        "Disk cohort only; full image content and metadata; st_blocks*512, unique inodes; Docker empty baseline subtracted. N=3 fresh imports; no speed inference.",
    )


def deployment(figures, summary):
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.8), constrained_layout=True)
    series = [
        (
            label,
            color,
            [
                summary["phases"][p]["charged"][arm]["samples"]
                for p in ("service", "idle-1", "idle-4")
            ],
        )
        for arm, label, color in ARMS
    ]
    group_bars(axes[0], ["Service only", "Service + 1", "Service + 4"], series)
    axes[0].set_ylabel("Complete product-group charge (MiB)")
    axes[0].set_title("Total deployment: only measured counts 0, 1 and 4")
    series = [
        (
            label,
            color,
            [summary["incremental_charged_mib"][p][arm]["samples"] for p in ("idle-1", "idle-4")],
        )
        for arm, label, color in ARMS
    ]
    group_bars(axes[1], ["1 sandbox", "4 sandboxes"], series)
    axes[1].set_ylabel("(Group total − service baseline) / count (MiB)")
    axes[1].set_title("Amortized deployment increment per sandbox")
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="outside lower center", ncols=3, frameon=False, fontsize=9)
    figures.save(
        fig,
        "memory-deployment",
        {
            "charged": {
                p: summary["phases"][p]["charged"] for p in ("service", "idle-1", "idle-4")
            },
            "incremental": summary["incremental_charged_mib"],
        },
        "Memory cohort only; five groups per arm; all dots, median bars, min-max whiskers (not CIs). Complete cgroup charges, not net host physical memory or marginal next-instance cost.",
    )


def timeline(figures, groups):
    fig, axes = plt.subplots(1, 2, figsize=(11, 4.8), constrained_layout=True, sharey=True)
    plotted = {}
    for arm, label, color in ARMS:
        for group in groups:
            if group["arm"] != arm:
                continue
            path = figures.args.memory_batch / f"{group['repeat']}-{arm}" / "observations.jsonl"
            figures.inputs[str(path)] = sha(path)
            observations = [json.loads(line) for line in path.read_text().splitlines() if line]
            for ax, phase, length, title in zip(
                axes,
                ("shared-4", "unique-4"),
                (30, 10),
                ("After identical data is ready", "After instance-unique rewrite is complete"),
            ):
                start = group["phases"][phase]["start"]
                end = group["phases"][phase]["end"]
                samples = (
                    [start]
                    + [
                        s
                        for s in observations
                        if start["monotonic"] < s["monotonic"] < end["monotonic"]
                    ]
                    + [end]
                )
                points = [
                    [s["monotonic"] - start["monotonic"], s["charged"] / MIB] for s in samples
                ]
                assert len(points) >= 15 and points[-1][0] >= length
                ax.plot(
                    *np.array(points).T,
                    color=color,
                    linewidth=1.1,
                    alpha=0.65,
                    label=label if group["repeat"] == 0 else None,
                )
                ax.scatter(*points[-1], color=color, s=15)
                ax.set_title(title, fontsize=11)
                ax.set_xlim(0, length + 0.1)
                ax.set_xlabel("Seconds since this observation window began")
                clean(ax)
                plotted[f"{phase}/{arm}/{group['repeat']}"] = points
    for ax, length in zip(axes, (30, 10)):
        ax.axvspan(length - 2, length, color="#fef3c7", alpha=0.55, zorder=0)
        ax.set_ylim(0, 850)
    axes[0].set_ylabel("Complete product-group charge (MiB)")
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="outside lower center", ncols=3, frameon=False, fontsize=9)
    fig.suptitle(
        "All five independent traces per configuration · shaded: final 2 s used for phase statistics",
        fontsize=11,
    )
    figures.save(
        fig,
        "memory-timeline",
        plotted,
        "Memory cohort only; every group and every recorded charged-memory point within each phase, with start/end; phase clocks reset separately. No allocation/write interval, smoothing, convergence-time estimate or long-term extrapolation.",
    )


def tradeoffs(figures, summary, groups):
    fig, axes = plt.subplots(1, 2, figsize=(11, 4.8), constrained_layout=True)
    cpu = {arm: summary["phases"]["shared-4"]["cpu_seconds"][arm]["samples"] for arm, _, _ in ARMS}
    readback = {
        arm: [group["readback_seconds"] * 1000 for group in groups if group["arm"] == arm]
        for arm, _, _ in ARMS
    }
    for ax, values, title, ylabel, digits in (
        (
            axes[0],
            cpu,
            "CPU work during the same 30 s window",
            "Whole product-group CPU time (seconds)",
            3,
        ),
        (
            axes[1],
            readback,
            "Full checked readback of all four working sets",
            "SDK completion time (ms)",
            1,
        ),
    ):
        series = [(label, color, [values[arm]]) for arm, label, color in ARMS]
        group_bars(ax, ["4 × identical 64 MiB"], series, digits)
        ax.set_title(title, fontsize=11)
        ax.set_ylabel(ylabel)
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="outside lower center", ncols=3, frameon=False, fontsize=9)
    figures.save(
        fig,
        "memory-tradeoffs",
        {"cpu_seconds": cpu, "readback_ms": readback},
        "Memory cohort only; five groups per arm, all dots, median bars, min-max whiskers (not CIs). Readback includes verification, SDK and stream completion, not isolated page fault cost. Zero-based linear axes.",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--startup-batch", type=Path, required=True)
    parser.add_argument("--disk-summary", type=Path, required=True)
    parser.add_argument("--memory-batch", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--preview-dir", type=Path)
    args = parser.parse_args()
    args.evidence_dir.mkdir(parents=True, exist_ok=False)
    plt.rcParams.update(
        {
            "font.size": 10,
            "svg.hashsalt": "pvisor-opensandbox-comparison",
            "font.family": "DejaVu Sans",
        }
    )
    figures = Figures(args)
    startup(figures)
    disk(figures)
    summary = figures.read(args.memory_batch / "summary.json")
    assert summary["benchmark"] == "B-OPENSANDBOX-MEMORY-ENG"
    assert summary["rounds"] == 5 and summary["failed_groups"] == summary["excluded_groups"] == 0
    audit = figures.read(args.memory_batch / "audit.json")
    assert audit["all_content_private_files_limits_cleanup_verified"]
    assert audit["source_summary_sha256"] == sha(args.memory_batch / "summary.json")
    groups = figures.read(args.memory_batch / "results.json")
    assert sha(args.memory_batch / "results.json") == summary["evidence"]["results.json"]
    assert len(groups) == 15 and all(group["success"] and group["all_deleted"] for group in groups)
    for arm, _, _ in ARMS:
        assert sorted(g["repeat"] for g in groups if g["arm"] == arm) == list(range(5))
        for phase in summary["phases"]:
            values = [g["phases"][phase]["charged"] / MIB for g in groups if g["arm"] == arm]
            assert values == summary["phases"][phase]["charged"][arm]["samples"]
    deployment(figures, summary)
    timeline(figures, groups)
    tradeoffs(figures, summary, groups)
    (args.evidence_dir / "figure-data.json").write_text(json.dumps(figures.data, indent=2) + "\n")
    receipt = {
        "inputs": figures.inputs,
        "outputs": figures.outputs,
        "plotter_sha256": sha(Path(__file__)),
        "matplotlib": matplotlib.__version__,
        "figure_data_sha256": sha(args.evidence_dir / "figure-data.json"),
        "measurement": "No new measurements; three existing independent cohorts; no source evidence modified.",
    }
    (args.evidence_dir / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"Rendered {len(figures.outputs)} figures; provenance: {args.evidence_dir}")


if __name__ == "__main__":
    main()
