#!/usr/bin/env python3
"""Render standalone figures from the shared-filesystem-service A/B reports."""

import argparse
from pathlib import Path

from evidence_tsv import load as load_evidence

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt


def changes(report, before, after, metrics):
    summary = report["summary"]
    return [
        100
        * (summary[after]["timings_ms"][key]["p50"] / summary[before]["timings_ms"][key]["p50"] - 1)
        for key in metrics
    ]


def plot(path, reports, cells, metrics, labels, title):
    fig, axes = plt.subplots(1, 2, figsize=(12, 5), sharey=True)
    for ax, report, (before, after, heading) in zip(axes, reports, cells, strict=True):
        values = changes(report, before, after, metrics)
        bars = ax.barh(
            range(len(values)),
            values,
            color=["#b45309" if value > 0 else "#0f766e" for value in values],
        )
        ax.bar_label(bars, labels=[f"{v:+.1f}%" for v in values], padding=4, fontsize=9)
        ax.axvline(0, color="#64748b", linewidth=1)
        ax.set_yticks(range(len(labels)), labels)
        ax.set_title(heading)
        ax.set_xlabel("P50 elapsed-time change (%) · negative is faster")
        low, high = min(min(values), 0), max(max(values), 0)
        padding = max((high - low) * 0.25, 3)
        ax.set_xlim(low - padding, high + padding)
        ax.grid(axis="x", alpha=0.15)
        ax.set_axisbelow(True)
        ax.spines[["top", "right"]].set_visible(False)
    axes[0].invert_yaxis()
    fig.suptitle(title, fontsize=15)
    fig.text(
        0.5,
        0.015,
        "30 samples per cell · archived optimized v3 → new integrated version · version comparison, not per-change attribution",
        ha="center",
        fontsize=9,
        color="#475569",
    )
    fig.tight_layout(rect=(0, 0.045, 1, 0.94))
    fig.savefig(path, bbox_inches="tight", facecolor="white")
    preview = Path(__file__).resolve().parents[2] / "target/fs-service-figures"
    preview.mkdir(parents=True, exist_ok=True)
    fig.savefig(preview / (path.stem + ".png"), dpi=140, bbox_inches="tight", facecolor="white")
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--assets",
        type=Path,
        default=Path(__file__).resolve().parents[2]
        / "docs/src/assets/benchmarks/.data/filesystem-service-20261005",
    )
    args = parser.parse_args()
    plt.rcParams.update({"font.family": "DejaVu Sans", "svg.fonttype": "none"})
    release, performance, lazy = [
        load_evidence(args.assets / name)
        for name in ("local-release.tsv", "local-performance.tsv", "lazy-performance.tsv")
    ]
    plot(
        args.assets / "local-vm.svg",
        [release, performance],
        [
            ("baseline/pvisor-vm", "candidate/pvisor-vm", "Default release"),
            ("baseline/pvisor-vm", "candidate/pvisor-vm", "Performance profile"),
        ],
        ["metadata", "read", "write", "git", "rg", "cargo", "npm", "completion_ms"],
        [
            "Traverse 2,048 files",
            "Read + SHA256 64 MiB",
            "Write 256 × 64 KiB",
            "Git status",
            "Ripgrep",
            "Cargo build",
            "Offline npm",
            "Whole-job completion",
        ],
        "Local-rootfs VM: full seven-workload A/B",
    )
    plot(
        args.assets / "lazy-vm.svg",
        [lazy, lazy],
        [
            ("baseline/cold", "candidate/cold", "Cold client cache"),
            ("baseline/warm", "candidate/warm", "Warm client disk cache; new guest"),
        ],
        [
            "metadata",
            "metadata_guest_hot",
            "open_read",
            "open_read_guest_hot",
            "read_64m",
            "copy_up_32",
            "completion_ms",
        ],
        [
            "First traversal",
            "Immediate repeat traversal",
            "Open/read/close 2,048 files",
            "Immediate repeat open/read",
            "Read + SHA256 64 MiB",
            "Copy up 32 small files",
            "Whole job (includes 1.2 s wait)",
        ],
        "Lazy-image VM: host FUSE → direct shared backend",
    )


if __name__ == "__main__":
    main()
