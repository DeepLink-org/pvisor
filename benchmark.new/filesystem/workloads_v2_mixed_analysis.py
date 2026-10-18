#!/usr/bin/env python3
"""B-FS-ENG M11: distinguish foreground reading from whole-task completion.

Postprocess completed formal V2 samples only. Each dependent round is contrasted
within its three fresh environment pairs, never pooled as nine observations.
The frozen batch analyzer supplies the same exploratory bootstrap estimator.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import importlib.util
import json
import statistics
import sys
from pathlib import Path

ARMS = ("baseline", "candidate")
METRICS = ("foreground_read_ns", "completed_ns")


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def collect(records):
    """Require all six M11 environments and their actual positive read clocks."""
    environments = {}
    for record in records:
        if record.get("case_id") != "M11":
            continue
        arm, pair = record.get("condition"), record.get("pair")
        require(
            arm in ARMS and type(pair) is int and pair in range(3), "unexpected M11 environment"
        )
        key = (arm, pair)
        require(key not in environments, "duplicate M11 environment")
        require(
            record.get("valid") is True and record.get("host_verified") is True,
            f"invalid or unverified M11 environment {key}",
        )
        rounds = {}
        for row in record.get("rows", []):
            round_number = row.get("round")
            require(
                row.get("id") == "M11"
                and type(round_number) is int
                and round_number in (1, 2, 3)
                and round_number not in rounds,
                f"duplicate or mixed M11 round {key}",
            )
            require(
                row.get("correctness") is True
                and all(type(row.get(metric)) is int and row[metric] > 0 for metric in METRICS),
                f"missing positive foreground/completion clock {key}, round {round_number}",
            )
            require(
                row["foreground_read_ns"] <= row["completed_ns"],
                "foreground completion exceeds whole-task completion",
            )
            rounds[round_number] = row
        require(set(rounds) == {1, 2, 3}, f"incomplete M11 rounds {key}")
        environments[key] = rounds
    require(
        set(environments) == {(arm, pair) for arm in ARMS for pair in range(3)},
        "incomplete M11 paired environment inventory",
    )
    return environments


def summarize(environments, estimate):
    rows = []
    for round_number in (1, 2, 3):
        for metric in METRICS:
            baseline = [environments["baseline", pair][round_number][metric] for pair in range(3)]
            candidate = [environments["candidate", pair][round_number][metric] for pair in range(3)]
            ratios = [candidate[pair] / baseline[pair] for pair in range(3)]
            seed = int.from_bytes(
                hashlib.sha256(f"M11:{round_number}:{metric}".encode()).digest()[:8], "big"
            )
            interval = estimate(ratios, seed)
            rows.append(
                dict(
                    case_id="M11",
                    round=round_number,
                    metric=metric,
                    baseline_median_ms=statistics.median(baseline) / 1e6,
                    candidate_median_ms=statistics.median(candidate) / 1e6,
                    baseline_ms=[value / 1e6 for value in baseline],
                    candidate_ms=[value / 1e6 for value in candidate],
                    **interval,
                )
            )
    return rows


def plot(summary, output):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.lines import Line2D

    plt.rcParams.update({"font.size": 11, "pdf.fonttype": 42, "svg.fonttype": "none"})
    fig, axes = plt.subplots(1, 2, figsize=(11, 4.8), sharex=True)
    titles = {
        "foreground_read_ns": "Foreground 4 MiB read completion",
        "completed_ns": "Whole mixed task + final synchronization",
    }
    colors, markers = ("#267ca8", "#b34a35", "#8160a1"), ("o", "s", "^")
    for ax, metric in zip(axes, METRICS):
        ax.axhline(1, color="#555555", linewidth=1, linestyle="--")
        for row in summary:
            if row["metric"] != metric:
                continue
            x = row["round"]
            for pair, ratio in enumerate(row["paired_ratios"]):
                ax.scatter(
                    x + (pair - 1) * 0.08,
                    ratio,
                    color=colors[pair],
                    marker=markers[pair],
                    s=43,
                    zorder=3,
                )
            ax.vlines(x, row["ratio_ci_low"], row["ratio_ci_high"], color="black", linewidth=1.4)
            ax.scatter(
                x, row["median_ratio"], color="black", marker="_", s=210, linewidths=2, zorder=4
            )
        ax.set_title(titles[metric])
        ax.set_xticks([1, 2, 3], ["R1: fresh guest", "R2: resident", "R3: resident"])
        ax.set_xlim(0.55, 3.45)
        ax.set_ylim(bottom=0)
        ax.set_ylabel("Candidate / baseline time (lower is faster)")
        ax.grid(axis="y", alpha=0.2)
    handles = [
        Line2D(
            [0],
            [0],
            marker=markers[pair],
            color=colors[pair],
            linestyle="",
            label=f"Environment pair {pair + 1}",
        )
        for pair in range(3)
    ]
    handles.append(
        Line2D(
            [0],
            [0],
            marker="_",
            markersize=13,
            color="black",
            label="Median + exploratory 95% interval",
        )
    )
    fig.legend(handles=handles, loc="lower center", ncol=4, bbox_to_anchor=(0.5, 0.07), fontsize=9)
    fig.suptitle("M11: foreground protection and total task speed are separate results")
    fig.text(
        0.5,
        0.015,
        "Guest WB off/on; 3 independent pairs per round. Rounds remain dependent. "
        "Pointwise bootstrap intervals; N3 is exploratory.",
        ha="center",
        fontsize=9,
    )
    fig.tight_layout(rect=(0, 0.14, 1, 0.93))
    files = []
    for extension in ("svg", "pdf"):
        path = output / f"mixed-foreground-vs-completion.{extension}"
        fig.savefig(path)
        files.append(path)
    plt.close(fig)
    return files, matplotlib.__version__


def analyze(batch, output):
    batch = batch.resolve()
    records_path = batch / "records.json"
    require(records_path.is_file(), "records.json missing; wait until campaign completes")
    protocol_path = batch / "protocol.json"
    protocol = json.loads(protocol_path.read_text())
    require(
        protocol.get("diagnostic") is False
        and protocol.get("samples") == 3
        and protocol.get("resident_rounds") == 3,
        "expected unprofiled N3 x three-round formal batch",
    )
    frozen_analyzer = batch / "harness/workloads_v2_analysis.py"
    require(frozen_analyzer.is_file(), "frozen analyzer missing")
    spec = importlib.util.spec_from_file_location("v2_frozen_mixed_estimator", frozen_analyzer)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    environments = collect(json.loads(records_path.read_text()))
    summary = summarize(environments, module.estimate)
    output.mkdir(parents=True, exist_ok=False)
    csv_path = output / "mixed-paired-summary.csv"
    fields = (
        "case_id",
        "round",
        "metric",
        "n",
        "baseline_median_ms",
        "candidate_median_ms",
        "baseline_ms",
        "candidate_ms",
        "paired_ratios",
        "median_ratio",
        "ratio_ci_low",
        "ratio_ci_high",
        "median_reduction_percent",
        "reduction_ci_low",
        "reduction_ci_high",
        "bootstrap_resamples",
        "bootstrap_seed",
        "evidence",
        "interval_multiplicity",
    )
    with csv_path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for row in summary:
            writer.writerow(
                {
                    field: json.dumps(row[field]) if isinstance(row[field], list) else row[field]
                    for field in fields
                }
            )
    raw_path = output / "mixed-environments.csv"
    with raw_path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=("condition", "pair", "round", *METRICS))
        writer.writeheader()
        for (arm, pair), rounds in sorted(environments.items()):
            for round_number, row in sorted(rounds.items()):
                writer.writerow(
                    dict(
                        condition=arm,
                        pair=pair,
                        round=round_number,
                        **{metric: row[metric] for metric in METRICS},
                    )
                )
    files, matplotlib_version = plot(summary, output)
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    files += [csv_path, raw_path, output / "summary.json"]
    manifest = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        case_id="M11",
        batch=str(batch),
        role="derived foreground/whole-task comparison; no subject execution",
        command=[sys.executable, *sys.argv],
        helper_sha256=sha(__file__),
        independent_pairs_per_round=3,
        dependent_rounds=3,
        primary_distinction="whole-task improvement does not imply foreground read protection",
        metrics={
            "foreground_read_ns": "reader start through 4 MiB verified read and checked close",
            "completed_ns": "both read/write workers complete and final file/directory sync acknowledged",
        },
        interval="frozen analyzer estimator, each round/metric independently, pointwise exploratory N3",
        limitations=[
            "no pooling dependent rounds",
            "no operation-level P99",
            "no inference of host applied bytes, queue occupancy or read latency mechanism",
        ],
        inputs={
            str(path.relative_to(batch)): sha(path)
            for path in (records_path, protocol_path, frozen_analyzer)
        },
        matplotlib_version=matplotlib_version,
        outputs={path.name: sha(path) for path in files},
    )
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        output = args.output or args.batch / "mixed-analysis"
        summary = analyze(args.batch, output)
        print(json.dumps(dict(output=str(output), contrasts=len(summary))))
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
