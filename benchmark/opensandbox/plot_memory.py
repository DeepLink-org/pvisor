#!/usr/bin/env python3
"""Render the audited B-OPENSANDBOX-MEMORY-ENG summary as a standalone SVG."""

import argparse
import hashlib
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("summary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--preview", type=Path)
    args = parser.parse_args()
    data = json.loads(args.summary.read_text())
    assert data["benchmark"] == "B-OPENSANDBOX-MEMORY-ENG" and data["rounds"] >= 5
    assert data["failed_groups"] == data["excluded_groups"] == 0
    phases = ["idle-4", "shared-4", "unique-4"]
    arms = [
        ("opensandbox-docker", "OpenSandbox + private rootless Docker", "#64748b"),
        ("native", "pVisor default", "#2563eb"),
        ("native-pool", "pVisor physical pool (explicit)", "#0d9488"),
    ]
    plt.rcParams.update({"font.size": 10, "svg.hashsalt": "pvisor-opensandbox-memory"})
    fig, ax = plt.subplots(figsize=(10, 4.6), constrained_layout=True)
    x = np.arange(len(phases))
    width = 0.24
    for index, (arm, label, color) in enumerate(arms):
        stats = [data["phases"][phase]["charged"][arm] for phase in phases]
        median = np.array([s["median"] for s in stats])
        low = np.array([s["range"][0] for s in stats])
        high = np.array([s["range"][1] for s in stats])
        bars = ax.bar(
            x + (index - 1) * width,
            median,
            width,
            label=label,
            color=color,
            yerr=np.array([median - low, high - median]),
            capsize=3,
            zorder=3,
        )
        ax.bar_label(bars, labels=[f"{v:.1f}" for v in median], padding=7, fontsize=9)
    ax.set_xticks(
        x, ["4 ready / idle sandboxes", "4 × 64 MiB identical data", "4 × 64 MiB unique data"]
    )
    ax.set_ylabel("Charged product-group memory (MiB)")
    ax.set_ylim(
        0,
        max(data["phases"][p]["charged"][a]["range"][1] for p in phases for a, _, _ in arms) * 1.22,
    )
    ax.grid(axis="y", color="#e2e8f0", zorder=0)
    ax.spines[["top", "right"]].set_visible(False)
    ax.legend(loc="upper left", fontsize=9, frameon=False)
    ax.set_title("Complete service + sandboxes; median and min–max of five fresh groups", pad=12)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(
        args.output,
        format="svg",
        metadata={
            "Date": None,
            "Description": "B-OPENSANDBOX-MEMORY-ENG; complete cgroup accounting; prepared caches may be charged outside the group; source summary SHA-256 "
            + hashlib.sha256(args.summary.read_bytes()).hexdigest(),
        },
    )
    if args.preview:
        fig.savefig(args.preview, format="png", dpi=140)
    receipt = {
        "summary_sha256": hashlib.sha256(args.summary.read_bytes()).hexdigest(),
        "svg_sha256": hashlib.sha256(args.output.read_bytes()).hexdigest(),
        "plotter_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "matplotlib": matplotlib.__version__,
        "scope": "five fresh groups per arm; median bars and min-max whiskers; no PSS total inferred",
    }
    (args.summary.parent / "figure-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    plt.close(fig)


if __name__ == "__main__":
    main()
