#!/usr/bin/env python3
"""Replot B-MEMORY-SCALE engineering preflights; never run a measurement.

Compare two complete off/physical static-payload cohorts without smoothing,
pooling or a causal version claim. Historical published seven-arm/library SVGs
remain unchanged; published table levels are reference data, not recovered curves.
"""

import argparse
import json
import shutil
from pathlib import Path

from plot_pool import audit
from pool_compare import PATTERNS, digest

ARMS = ("pvisor-off", "pvisor-physical")
PHASES = ("idle", "startup", "post-write")
METRICS = ("charged_mib", "pss_mib")


def index_rows(rows):
    result = {}
    for row in rows:
        key = row[0]["pattern"], row[0]["arm"]
        if key in result:
            raise ValueError("duplicate condition")
        result[key] = row
    if set(result) != {(p, a) for p in PATTERNS for a in ARMS}:
        raise ValueError("comparison requires all six static conditions")
    if any("post_write_offset_seconds" not in row[0] for row in rows):
        raise ValueError("comparison requires complete post-write observations")
    return result


def points(row, metric, phase):
    if metric not in METRICS:
        raise ValueError("unknown plot metric")
    summary = row[0]
    curve = row[1 if metric == "charged_mib" else 2]
    if phase == "idle":
        return [(x, y) for x, y in curve if 0 <= x <= 60]
    if phase == "post-write":
        origin = summary["post_write_offset_seconds"]
        return [(x - origin, y) for x, y in curve if origin <= x <= origin + 30]
    if phase != "startup":
        raise ValueError("unknown plot phase")
    offset = summary["startup_seconds_until_all_ready"]
    ready = summary["idle-0"]["actual_seconds_after_ready"]
    return [
        (0, summary["before_start"][metric]),
        *((x + offset, y) for x, y in curve if -offset < x < ready),
        (offset + ready, summary["idle-0"][metric]),
    ]


def load_cohort(root):
    report, rows = audit(root)
    if report.get("campaign", "static") != "static":
        raise ValueError("library data cannot populate a static comparison")
    indexed = index_rows(rows)
    qualification = json.loads((root.parent / "audit.json").read_text())
    if not qualification.get("complete") or qualification["report_sha256"] != digest(
        root / "report.json"
    ):
        raise ValueError("independent cohort audit missing or changed")
    for path, checksum in report["harness_files"].items():
        if digest(root / "harness" / Path(path).name) != checksum:
            raise ValueError("retained supporting harness changed")
    inputs = set()
    for index, execution in enumerate(report["executions"]):
        folder = root / str(index)
        expected = execution.get("observation_hashes", {})
        if set(expected) != {"monitor.json", "pss.json", "guard.json"} or any(
            digest(folder / name) != value for name, value in expected.items()
        ):
            raise ValueError("retained observations missing or changed")
        inputs.add(json.loads((folder / "config.json").read_text())["inputs"])
    if len(inputs) != 1:
        raise ValueError("mixed product inputs")
    inputs = Path(inputs.pop())
    receipt = json.loads((inputs / "receipt.json").read_text())
    if receipt != report["receipt"] or any(
        digest(inputs / name) != value for name, value in receipt["hashes"].items()
    ):
        raise ValueError("frozen product inputs changed")
    for name, field in (
        ("source.tar.gz", "source_archive_sha256"),
        ("source-files.json", "source_manifest_sha256"),
    ):
        candidates = [inputs / name, inputs.parent / name]
        path = next((p for p in candidates if p.is_file()), None)
        if path is None or digest(path) != receipt[field]:
            raise ValueError("frozen product source evidence changed")
    return (
        report,
        indexed,
        {
            "directory": str(root),
            "report_sha256": digest(root / "report.json"),
            "independent_audit_sha256": digest(root.parent / "audit.json"),
            "source_manifest_sha256": receipt["source_manifest_sha256"],
            "receipt_sha256": digest(inputs / "receipt.json"),
            "host": report["host"],
        },
    )


def published_levels(markdown):
    result = {}
    in_static_table = False
    for line in markdown.read_text().splitlines():
        if line.startswith("### "):
            in_static_table = "{#pool-static-results}" in line
            continue
        if not in_static_table:
            continue
        cells = [cell.strip() for cell in line.split("|")[1:-1]]
        if cells and cells[0] in ARMS:
            if cells[0] in result or len(cells) != 4:
                raise ValueError("ambiguous published reference table")
            result[cells[0]] = dict(zip(PATTERNS, map(float, cells[1:])))
    if set(result) != set(ARMS):
        raise ValueError("published off/physical table unavailable")
    return result


def render(before, after, output, metadata):
    import matplotlib

    matplotlib.use("Agg")
    matplotlib.rcParams.update(
        {
            "svg.fonttype": "none",
            "font.family": "DejaVu Sans",
            "font.size": 10,
            "figure.facecolor": "#0d1b30",
            "axes.facecolor": "#142840",
            "axes.edgecolor": "#a8cefa",
            "axes.labelcolor": "#e9eef8",
            "text.color": "#e9eef8",
            "xtick.color": "#e9eef8",
            "ytick.color": "#e9eef8",
            "grid.color": "#a8cefa",
            "legend.facecolor": "#142840",
            "legend.edgecolor": "#a8cefa",
            "savefig.facecolor": "#0d1b30",
        }
    )
    import matplotlib.pyplot as plt

    colors = {"pvisor-off": "#77cbbb", "pvisor-physical": "#ffc26d"}
    descriptions = {
        "idle": ("Idle working set", "Seconds after all four VMs ready", 60),
        "startup": ("Startup through readiness", "Seconds after initial group snapshot", None),
        "post-write": (
            "After 100% private writes",
            "Seconds after private writes and peer checks",
            30,
        ),
    }
    inventory = {}
    for phase in PHASES:
        title, xlabel, end = descriptions[phase]
        figure, axes = plt.subplots(3, 2, figsize=(12, 10.5), squeeze=False)
        handles = {}
        point_counts = {}
        for i, pattern in enumerate(PATTERNS):
            for j, metric in enumerate(METRICS):
                axis = axes[i, j]
                for label, dataset, style in (
                    ("Before cleanup", before, "--"),
                    ("After cleanup", after, "-"),
                ):
                    for arm in ARMS:
                        observed = points(dataset[pattern, arm], metric, phase)
                        if not observed:
                            raise ValueError("empty plot window")
                        name = f"{label} / {arm.removeprefix('pvisor-')}"
                        (line,) = axis.plot(
                            [x for x, _ in observed],
                            [y for _, y in observed],
                            color=colors[arm],
                            linestyle=style,
                            linewidth=1.6,
                            alpha=0.75 if style == "--" else 1,
                            marker="." if phase == "startup" else None,
                            markersize=2.5,
                        )
                        handles[name] = line
                        point_counts[f"{label}/{pattern}/{arm}/{metric}"] = len(observed)
                        if phase == "startup":
                            axis.scatter(
                                *observed[-1],
                                s=24,
                                facecolors="none",
                                edgecolors=colors[arm],
                                marker="s" if style == "--" else "o",
                            )
                metric_name = "Group charged memory" if j == 0 else "Sum of process PSS"
                axis.set(title=f"{pattern} · {metric_name}", xlabel=xlabel, ylabel="MiB")
                axis.set_xlim(0, end)
                axis.grid(alpha=0.15)
                if phase == "startup":
                    axis.set_ylim(bottom=0)
        figure.suptitle(f"{title} · four VMs · N=1 per condition", y=0.985, fontsize=14)
        figure.legend(
            handles.values(),
            handles.keys(),
            loc="upper center",
            bbox_to_anchor=(0.5, 0.96),
            ncol=2,
            fontsize=10,
        )
        footer = "Independent cohorts, not causal A/B · raw samples, no smoothing · shared axes within each panel"
        if phase == "startup":
            footer += "\nEndpoint squares/circles: before/after all-ready snapshots"
        figure.text(0.5, 0.013, footer, ha="center", fontsize=9)
        figure.tight_layout(rect=(0, 0.055, 1, 0.90))
        stem = f"memory-pool-cleanup-compare-{phase}"
        figure.savefig(
            output / f"{stem}.svg",
            metadata={
                "Description": json.dumps(metadata, sort_keys=True),
                "Creator": f"compare_pool_plots.py / matplotlib {matplotlib.__version__}",
            },
        )
        figure.savefig(output / f"{stem}.png", dpi=140)
        inventory[phase] = point_counts
        plt.close(figure)
    return {"python_matplotlib": matplotlib.__version__, "point_counts": inventory}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--publish", type=Path)
    args = parser.parse_args()
    old_report, before, old_proof = load_cohort(args.before)
    new_report, after, new_proof = load_cohort(args.after)
    for name in ("bundle/kernel.bin", "bundle/kernel.json", "fw.packed", "worker.rs"):
        if old_report["receipt"]["hashes"][name] != new_report["receipt"]["hashes"][name]:
            raise ValueError(
                "worker source or firmware differs; use a separately scoped comparison"
            )
    repo = Path(__file__).resolve().parents[2]
    old_assets = {
        name: digest(repo / "docs/overrides/assets/benchmarks" / name)
        for name in (
            "memory-pool-static-idle.svg",
            "memory-pool-library-idle.svg",
            "memory-pool-library-early.svg",
        )
    }
    legacy_levels = published_levels(
        repo / "docs/src/zh/lab-notes/vm-memory-performance-analysis.md"
    )
    args.output.mkdir()
    metadata = {
        "benchmark_id": "B-MEMORY-SCALE",
        "role": "engineering diagnostic",
        "before_report_sha256": old_proof["report_sha256"],
        "after_report_sha256": new_proof["report_sha256"],
        "scope": "Independent N=1 static off/physical cohorts; no causal speedup or DAX result",
    }
    rendering = render(before, after, args.output, metadata)
    manifest = {
        **metadata,
        **rendering,
        "generator_sha256": digest(Path(__file__)),
        "before": old_proof,
        "after": new_proof,
        "published_reference": {
            "raw_archives_available": {
                name: (repo / "benchmark/memory/.data" / name / "report.json").is_file()
                for name in ("q8", "l4")
            },
            "original_svg_sha256": old_assets,
            "charged_60_mib": legacy_levels,
            "scope": "Existing published table only; no recovery or digitization of historical curves",
        },
        "cases": {
            label: [v[0] for v in dataset.values()]
            for label, dataset in (("before", before), ("after", after))
        },
        "figures": {p.name: digest(p) for p in sorted(args.output.iterdir())},
    }
    if args.publish:
        sources = sorted(args.output.glob("*.svg"))
        if any((args.publish / p.name).exists() for p in sources):
            raise ValueError("refusing to overwrite an existing published figure")
        for source in sources:
            shutil.copy2(source, args.publish / source.name)
    if any(
        digest(repo / "docs/overrides/assets/benchmarks" / n) != h for n, h in old_assets.items()
    ):
        raise ValueError("historical published assets changed")
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
