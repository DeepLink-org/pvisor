#!/usr/bin/env python3
"""Derived B-FS-ENG curve analysis; never runs subjects or publishes user rankings.

One record is one independent environment. Its dependent resident rounds are
summed before deriving throughput, and averaged for phase durations. Bootstrap
resamples independent paired environments, never individual resident rounds.
Matplotlib is an analysis-only dependency; missing it does not erase CSV/JSON.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import itertools
import json
import math
import random
import re
import statistics
import sys
from collections import Counter
from pathlib import Path
from typing import Any

MIB = 1024 * 1024
NS_PER_SECOND = 1_000_000_000
BOOTSTRAPS = 5000
PHASES = ("accepted", "applied", "fsync", "close", "dirsync", "total")
COLORS = ("#0072B2", "#D55E00", "#009E73", "#CC79A7", "#E69F00", "#56B4E9", "#333333")
MARKERS = ("o", "s", "^", "D", "v", "P", "X")
SEPARATION_RULE = {"minimum_per_cluster": 3, "adjacent_ratio": 1.5, "median_ratio": 1.7}


def finite_number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value) if math.isfinite(value) else None


def flatten_numbers(value: dict, prefix: str = "") -> dict[str, float]:
    result = {}
    for key, item in value.items():
        name = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(item, dict):
            result.update(flatten_numbers(item, name))
        elif (number := finite_number(item)) is not None:
            result[name] = number
    return result


def correct(value: Any) -> bool:
    if value is True:
        return True
    if isinstance(value, str):
        return value.lower() in ("ok", "pass", "passed", "true")
    if isinstance(value, dict):
        return any(value.get(key) is True for key in ("ok", "passed", "valid"))
    return False


def percentile(sorted_values: list[float], fraction: float) -> float:
    position = (len(sorted_values) - 1) * fraction
    lo = math.floor(position)
    hi = math.ceil(position)
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * (position - lo)


def seed_for(*parts: Any) -> int:
    data = json.dumps(parts, sort_keys=True, default=str).encode()
    return int.from_bytes(hashlib.sha256(data).digest()[:8], "little")


def separated_distribution(values: list[float], labels: list[str] | None = None) -> dict:
    """Apply the registered positive-valued two-cluster rule without exclusions.

    If several cuts qualify, select the largest adjacent multiplicative gap;
    ties use the lower cut. The rule cannot detect separation with N < 6.
    """
    minimum = SEPARATION_RULE["minimum_per_cluster"]
    if labels is not None and len(labels) != len(values):
        raise ValueError("distribution labels do not match values")
    result = {
        "separated": False,
        "rule": SEPARATION_RULE.copy(),
        "testable": len(values) >= 2 * minimum,
        "reason": "no_qualifying_split"
        if len(values) >= 2 * minimum
        else "N_lt_6_rule_not_testable",
        "clusters": [],
    }
    if not result["testable"]:
        return result
    ordered = sorted(enumerate(values), key=lambda item: item[1])
    candidates = []
    for cut in range(minimum, len(values) - minimum + 1):
        left = [value for _, value in ordered[:cut]]
        right = [value for _, value in ordered[cut:]]
        if left[-1] <= 0 or statistics.median(left) <= 0:
            continue
        gap = right[0] / left[-1]
        ratio = statistics.median(right) / statistics.median(left)
        if gap >= SEPARATION_RULE["adjacent_ratio"] and ratio >= SEPARATION_RULE["median_ratio"]:
            candidates.append((gap, -cut, cut, ratio))
    if not candidates:
        return result
    gap, _, cut, ratio = max(candidates)
    clusters = []
    for members in (ordered[:cut], ordered[cut:]):
        cluster_values = [value for _, value in members]
        cluster = {
            "n": len(members),
            "fraction": len(members) / len(values),
            "median": statistics.median(cluster_values),
            "min": min(cluster_values),
            "max": max(cluster_values),
            "values": cluster_values,
            "member_indices": [index for index, _ in members],
        }
        if labels is not None:
            cluster["pairs"] = [labels[index] for index, _ in members]
        clusters.append(cluster)
    result.update(
        separated=True,
        reason="registered_split_detected",
        clusters=clusters,
        adjacent_ratio=gap,
        median_ratio=ratio,
    )
    return result


def describe(
    values: list[float],
    *,
    bootstrap: bool = False,
    seed: int = 0,
    labels: list[str] | None = None,
) -> dict:
    distribution = separated_distribution(values, labels)
    if not values:
        return {
            "n": 0,
            "median": None,
            "ci_low": None,
            "ci_high": None,
            "separated": False,
            "distribution": distribution,
        }
    result = {
        "n": len(values),
        "median": statistics.median(values),
        "mean": statistics.mean(values),
        "min": min(values),
        "max": max(values),
        "ci_low": None,
        "ci_high": None,
        "evidence": "exploratory_N_lt_30" if len(values) < 30 else "N_ge_30",
        "separated": distribution["separated"],
        "distribution": distribution,
    }
    if distribution["separated"]:
        result.update(median=None, mean=None, aggregate_withheld="separated_distribution")
        return result
    if bootstrap and len(values) > 1:
        rng = random.Random(seed)
        samples = sorted(
            statistics.median(rng.choices(values, k=len(values))) for _ in range(BOOTSTRAPS)
        )
        result["ci_low"] = percentile(samples, 0.025)
        result["ci_high"] = percentile(samples, 0.975)
    return result


def csv_file(path: Path, rows: list[dict]) -> None:
    fields = list(dict.fromkeys(key for row in rows for key in row))
    with path.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields or ["no_records"])
        writer.writeheader()
        for row in rows:
            writer.writerow(
                {
                    key: json.dumps(value, sort_keys=True)
                    if isinstance(value, (dict, list))
                    else value
                    for key, value in row.items()
                }
            )


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cell_metadata(protocol: dict) -> dict[str, dict]:
    cells = protocol.get("cells", protocol.get("plan", []))
    if not isinstance(cells, list) or not cells:
        raise ValueError("protocol.json must contain a nonempty cells list")
    result = {}
    for index, cell in enumerate(cells):
        if not isinstance(cell, dict) or "id" not in cell:
            raise ValueError(f"protocol cell {index} is missing id")
        key = str(cell["id"])
        if key in result:
            raise ValueError(f"duplicate protocol cell id: {key}")
        result[key] = cell
    return result


def condition_factors(protocol: dict) -> tuple[dict, str]:
    """Use explicit factors, or reconstruct registered knobs for old batches."""
    explicit = protocol.get("condition_factors")
    if isinstance(explicit, dict) and explicit:
        return explicit, "protocol"
    inferred = {}
    for name, condition in protocol.get("conditions", {}).items():
        env = condition.get("env", {})
        if name == "native":
            inferred[name] = {"backend": "native"}
            continue
        enabled = env.get("PVISOR_VM_FS_HOST_WRITEBACK", "0" if name == "sync" else "1") == "1"
        if not enabled:
            inferred[name] = {"backend": "vm", "host_writeback": False}
            continue
        factors = {
            "backend": "vm",
            "host_writeback": True,
            "capacity_mib": 64,
            "batch_kib": 1024,
            "chunks": 64,
            "delay_us": 10000,
            "qualification_lock": name == "async-old",
            "synchronous": name == "owned-sync",
        }
        for pattern, key in (
            (r"capacity-(\d+)m", "capacity_mib"),
            (r"batch-(\d+)k", "batch_kib"),
            (r"delay-(\d+)us", "delay_us"),
        ):
            match = re.fullmatch(pattern, name)
            if match:
                factors[key] = int(match[1])
        if name == "unmerged":
            factors.update(chunks=1, delay_us=0)
        config = env.get("PVISOR_VM_FS_HOST_WRITEBACK_CONFIG")
        if config:
            parsed = json.loads(config) if isinstance(config, str) else config
            if not isinstance(parsed, dict):
                raise ValueError(f"condition {name}: write-buffer config is not an object")
            factors.update(parsed)
        inferred[name] = factors
    return inferred, "inferred_registered_defaults_names_and_env"


def collect(protocol: dict, records: list) -> tuple[list, list, list, list]:
    cells = cell_metadata(protocol)
    expected_rounds = protocol.get("resident_rounds", protocol.get("rounds", 3))
    if not isinstance(expected_rounds, int) or expected_rounds < 1:
        raise ValueError("protocol rounds must be a positive integer")
    conditions = protocol.get("conditions", {})
    points, raw, failures, environments = [], [], [], []
    seen = set()
    identities = [
        (str(record.get("pair")), str(record.get("condition", "")))
        for record in records
        if isinstance(record, dict)
        and record.get("warmup") is not True
        and record.get("status") != "warmup"
    ]
    duplicates = {identity for identity, count in Counter(identities).items() if count > 1}
    for index, record in enumerate(records):
        if not isinstance(record, dict):
            failures.append({"record": index, "reasons": ["record is not an object"]})
            continue
        pair = str(record.get("pair", f"missing-{index}"))
        condition = str(record.get("condition", ""))
        identity = {"record": index, "pair": pair, "condition": condition}
        reasons = []
        warmup = record.get("warmup") is True or record.get("status") == "warmup"
        key = (pair, condition)
        if key in duplicates and not warmup:
            reasons.append("duplicate pair/condition environment")
        if not warmup:
            seen.add(key)
        if not condition or (conditions and condition not in conditions):
            reasons.append("condition is absent from protocol")
        if "pair" not in record:
            reasons.append("missing independent pair identifier")
        if record.get("error"):
            reasons.append(f"runner error: {record['error']}")
        if (
            record.get("returncode", 0) != 0
            or record.get("success") is False
            or record.get("valid") is False
            or record.get("status") in ("failed", "error", "timeout", "cancelled", "incomplete")
        ):
            reasons.append("runner reports an unsuccessful environment")
        rows = record.get("rows", [])
        if not isinstance(rows, list):
            reasons.append("rows is not a list")
            rows = []
        groups = {}
        for ordinal, row in enumerate(rows):
            raw.append(
                {
                    **identity,
                    "warmup": warmup,
                    "row_index": ordinal,
                    **(row if isinstance(row, dict) else {}),
                    "raw_row_json": json.dumps(row, sort_keys=True),
                }
            )
            if not isinstance(row, dict):
                reasons.append(f"row {ordinal} is not an object")
                continue
            cell_id = str(row.get("id", ""))
            if cell_id not in cells:
                reasons.append(f"unknown cell id: {cell_id}")
                continue
            if not correct(row.get("correctness")):
                reasons.append(f"cell {cell_id} round {row.get('round')} failed correctness")
            for name in ("bytes", *(phase + "_ns" for phase in PHASES)):
                number = finite_number(row.get(name))
                if number is None or number < 0:
                    reasons.append(f"cell {cell_id}: invalid or missing {name}")
            if finite_number(row.get("total_ns")) == 0:
                reasons.append(f"cell {cell_id}: zero total_ns")
            durations = [finite_number(row.get(phase + "_ns")) for phase in PHASES[:-1]]
            if all(value is not None for value in durations) and sum(durations) != row.get(
                "total_ns"
            ):
                reasons.append(f"cell {cell_id}: phase durations do not sum to total_ns")
            if "round" not in row:
                reasons.append(f"cell {cell_id}: missing resident round")
            for name in (
                "workload",
                "block_bytes",
                "workers",
                "bytes_per_worker",
                "sync_every_ops",
            ):
                if name in cells[cell_id] and row.get(name) != cells[cell_id][name]:
                    reasons.append(f"cell {cell_id}: payload/protocol {name} mismatch")
            groups.setdefault(cell_id, []).append(row)
        for cell_id in cells:
            group = groups.get(cell_id, [])
            rounds = [row.get("round") for row in group]
            if len(group) != expected_rounds or len({str(r) for r in rounds}) != expected_rounds:
                reasons.append(
                    f"cell {cell_id}: expected {expected_rounds} distinct resident rounds"
                )
        if warmup:
            continue
        if reasons:
            failures.append({**identity, "reasons": list(dict.fromkeys(reasons))})
            continue
        resource = record.get("resource", {})
        resources = flatten_numbers(resource)
        environments.append(
            {
                **identity,
                "ready_ms": finite_number(record.get("ready_ms")),
                "job_ms": finite_number(record.get("job_ms")),
                "resource_json": resource,
                "resource_before_json": record.get("resource_before", {}),
                "sampled_memory_current_peak_bytes": finite_number(
                    record.get("sampled_memory_current_peak")
                ),
                "sampled_memory_byte_seconds": finite_number(
                    record.get("sampled_memory_byte_seconds")
                ),
                **{"resource." + name: value for name, value in resources.items()},
            }
        )
        for cell_id, rows in groups.items():
            cell = cells[cell_id]
            byte_sum = sum(row["bytes"] for row in rows)
            times = {phase: sum(row[phase + "_ns"] for row in rows) for phase in PHASES}
            accepted = times["accepted"]
            points.append(
                {
                    **identity,
                    "id": cell_id,
                    "family": cell.get("family", cell.get("workload", "other")),
                    "workload": cell.get("workload", rows[0].get("workload", "")),
                    "x": cell.get("x", cell.get("block_bytes", cell_id)),
                    "rounds": expected_rounds,
                    "bytes_mean": byte_sum / expected_rounds,
                    "throughput_mib_s": byte_sum * NS_PER_SECOND / times["total"] / MIB,
                    "accepted_mib_s": byte_sum * NS_PER_SECOND / accepted / MIB
                    if accepted
                    else None,
                    **{
                        phase + "_ms": value / expected_rounds / 1_000_000
                        for phase, value in times.items()
                    },
                }
            )
    expected = {
        (str(order["pair"]), str(condition))
        for order in protocol.get("orders", [])
        for condition in order["conditions"]
    }
    if not expected and isinstance(protocol.get("samples"), int):
        expected = {
            (str(pair), condition)
            for pair in range(protocol["samples"])
            for condition in conditions
        }
    for pair, condition in sorted(expected - seen):
        failures.append(
            {
                "record": None,
                "pair": pair,
                "condition": condition,
                "reasons": ["planned independent environment is missing"],
            }
        )
    return points, raw, failures, environments


def summarize(protocol: dict, points: list[dict]) -> tuple[list, list, list]:
    baseline = protocol.get("baseline_condition")
    if not isinstance(baseline, str) or not baseline:
        raise ValueError("protocol baseline_condition must explicitly name the paired baseline")
    groups = {}
    for point in points:
        groups.setdefault((point["id"], point["condition"]), []).append(point)
    summaries, comparisons, ratios = [], [], []
    cells = cell_metadata(protocol)
    conditions = list(protocol.get("conditions", {})) or list(
        dict.fromkeys(p["condition"] for p in points)
    )
    if baseline not in conditions:
        raise ValueError("baseline_condition is absent from protocol conditions")
    for cell_id, condition in itertools.product(cells, conditions):
        group = groups.get((cell_id, condition), [])
        first = (
            group[0]
            if group
            else {
                "family": cells[cell_id].get("family", cells[cell_id].get("workload", "other")),
                "workload": cells[cell_id].get("workload", ""),
                "x": cells[cell_id].get("x", cell_id),
            }
        )
        values = [point["throughput_mib_s"] for point in group]
        accepted = [
            point["accepted_mib_s"] for point in group if point["accepted_mib_s"] is not None
        ]
        labels = [point["pair"] for point in group]
        summaries.append(
            {
                "id": cell_id,
                "family": first["family"],
                "workload": first["workload"],
                "x": first["x"],
                "condition": condition,
                "throughput_mib_s": describe(
                    values,
                    bootstrap=True,
                    labels=labels,
                    seed=seed_for(cell_id, condition, "total"),
                ),
                "accepted_mib_s": describe(
                    accepted,
                    bootstrap=True,
                    labels=[
                        point["pair"] for point in group if point["accepted_mib_s"] is not None
                    ],
                    seed=seed_for(cell_id, condition, "accepted"),
                ),
                **{
                    phase + "_ms": describe(
                        [point[phase + "_ms"] for point in group], labels=labels
                    )
                    for phase in PHASES
                },
            }
        )
    summary_by = {(row["id"], row["condition"]): row for row in summaries}
    for cell_id, condition in itertools.product(cells, conditions):
        if condition == baseline:
            continue
        base = {p["pair"]: p for p in groups.get((cell_id, baseline), [])}
        candidate = {p["pair"]: p for p in groups.get((cell_id, condition), [])}
        shared = sorted(base.keys() & candidate.keys())
        values = []
        for pair in shared:
            if base[pair]["throughput_mib_s"] <= 0:
                continue
            value = candidate[pair]["throughput_mib_s"] / base[pair]["throughput_mib_s"]
            values.append(value)
            ratios.append(
                {
                    "id": cell_id,
                    "condition": condition,
                    "baseline": baseline,
                    "pair": pair,
                    "throughput_ratio": value,
                    "baseline_mib_s": base[pair]["throughput_mib_s"],
                    "candidate_mib_s": candidate[pair]["throughput_mib_s"],
                }
            )
        paired_labels = [pair for pair in shared if base[pair]["throughput_mib_s"] > 0]
        estimate = describe(
            values,
            bootstrap=True,
            labels=paired_labels,
            seed=seed_for(cell_id, condition, "paired"),
        )
        separated_arms = [
            arm
            for arm in (baseline, condition)
            if summary_by[(cell_id, arm)]["throughput_mib_s"]["separated"]
        ]
        if separated_arms:
            estimate.update(
                median=None,
                mean=None,
                ci_low=None,
                ci_high=None,
                aggregate_withheld="separated_input_arm",
            )
        comparisons.append(
            {
                "id": cell_id,
                "condition": condition,
                "baseline": baseline,
                "statistic": "median of per-environment paired throughput ratios; higher is faster",
                **estimate,
                "ranking_withheld": bool(separated_arms or estimate["separated"]),
                "separated_arms": separated_arms,
                "pairs": paired_labels,
                "zero_byte_baseline_pairs": [
                    pair for pair in shared if base[pair]["throughput_mib_s"] <= 0
                ],
                "baseline_only_pairs": sorted(base.keys() - candidate.keys()),
                "candidate_only_pairs": sorted(candidate.keys() - base.keys()),
            }
        )
    return summaries, comparisons, ratios


def slug(value: Any) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "-", str(value)).strip("-") or "curve"


def flattened_summary(rows: list[dict]) -> list[dict]:
    result = []
    for row in rows:
        flat = {}
        for key, value in row.items():
            if isinstance(value, dict):
                flat.update({key + "_" + name: item for name, item in value.items()})
            else:
                flat[key] = value
        result.append(flat)
    return result


def curve_axes(cells: dict[str, dict], cell_ids) -> dict:
    """Prepare stable numeric/categorical axes without importing matplotlib."""
    ordered = sorted(cell_ids, key=lambda key: x_sort(cells[key].get("x", key)))
    numeric = all(finite_number(cells[key].get("x", key)) is not None for key in ordered)
    family = cells[ordered[0]].get("family", "parameter") if ordered else "parameter"
    label = next((cells[key]["x_label"] for key in ordered if cells[key].get("x_label")), None)
    scale = next((cells[key]["x_scale"] for key in ordered if cells[key].get("x_scale")), None)
    if label is None:
        label = {
            "workers": "Concurrent workers",
            "barrier": "Write operations per fsync (0 = final only)",
        }.get(family, family)
        if (
            family in ("block", "smallfiles")
            and numeric
            and all(cells[key].get("block_bytes") == cells[key]["x"] * 1024 for key in ordered)
        ):
            label = "Write block size (KiB)" if family == "block" else "Small-file size (KiB)"
        if (
            family in ("pressure", "readset")
            and numeric
            and all(
                cells[key].get("bytes_per_worker", 0) * cells[key].get("workers", 0)
                == cells[key]["x"] * MIB
                for key in ordered
            )
        ):
            label = "Total write volume (MiB)" if family == "pressure" else "Read working set (MiB)"
    if scale is None and numeric:
        positive = all(cells[key]["x"] > 0 for key in ordered)
        if family in ("block", "pressure", "readset") and positive:
            scale = "log2"
        elif family == "barrier":
            scale = "symlog"
    return {
        "ordered_cells": ordered,
        "numeric": numeric,
        "xvalue": {
            key: float(cells[key]["x"]) if numeric else index for index, key in enumerate(ordered)
        },
        "label": label,
        "scale": scale,
        "tick_labels": [str(cells[key].get("x", key)) for key in ordered],
    }


def phase_breakdown(points: list[dict]) -> list[dict]:
    """Use additive means, rather than adding phase medians from different runs.

    These selected block cells are fixed independently of their measured values.
    Each input phase is already the mean of dependent rounds in one environment.
    """
    rows = []
    for cell_id in ("block-1k", "block-64k", "block-1024k"):
        arms = dict.fromkeys(point["condition"] for point in points if point["id"] == cell_id)
        for arm in arms:
            selected = [
                point for point in points if point["id"] == cell_id and point["condition"] == arm
            ]
            rows.append(
                {
                    "id": cell_id,
                    "condition": arm,
                    "n": len(selected),
                    **{
                        phase + "_mean_ms": statistics.mean(
                            point[phase + "_ms"] for point in selected
                        )
                        for phase in PHASES
                    },
                    "pairs": [point["pair"] for point in selected],
                    "individual_total_ms": [point["total_ms"] for point in selected],
                    "statistic": "arithmetic mean of environment phase means; phases add to mean total",
                }
            )
    return rows


def render_plots(
    output: Path,
    protocol: dict,
    points: list[dict],
    summaries: list[dict],
    comparisons: list[dict],
    ratios: list[dict],
    environments: list[dict],
    failure_count: int,
) -> list[str]:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "font.size": 10,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "axes.grid": True,
            "grid.alpha": 0.25,
            "pdf.fonttype": 42,
            "svg.fonttype": "none",
        }
    )
    cells = cell_metadata(protocol)
    conditions = list(protocol.get("conditions", {})) or list(
        dict.fromkeys(p["condition"] for p in points)
    )
    style = {
        name: (COLORS[i % len(COLORS)], MARKERS[i % len(MARKERS)])
        for i, name in enumerate(conditions)
    }
    baseline = protocol["baseline_condition"]
    lookup = {(row["id"], row["condition"]): row for row in summaries}
    ratios_by = {(row["id"], row["condition"]): row for row in comparisons}
    files = []

    def save(fig, name: str) -> None:
        for extension in ("svg", "pdf"):
            path = output / f"{name}.{extension}"
            fig.savefig(path, bbox_inches="tight")
            files.append(path.name)
        plt.close(fig)

    def series(ax, xy, color, marker, label):
        ordered = sorted(xy, key=lambda item: item[0])
        if not ordered:
            return
        segments, segment = [], []
        used_label = False
        for x, estimate in ordered:
            if estimate.get("median") is None:
                if segment:
                    segments.append(segment)
                    segment = []
                clusters = estimate.get("distribution", {}).get("clusters", [])
                for cluster in clusters:
                    ax.scatter(
                        x,
                        cluster["median"],
                        marker=marker,
                        facecolors="none",
                        edgecolors=color,
                        s=45,
                        label=label if not used_label else None,
                    )
                    used_label = True
                if clusters:
                    ax.annotate(
                        "split",
                        (x, clusters[-1]["median"]),
                        xytext=(3, 5),
                        textcoords="offset points",
                        fontsize=7,
                        color=color,
                    )
                continue
            segment.append((x, estimate))
        if segment:
            segments.append(segment)
        for segment in segments:
            ax.plot(
                [p[0] for p in segment],
                [p[1]["median"] for p in segment],
                color=color,
                marker=marker,
                linewidth=1.6,
                markersize=4,
                label=label if not used_label else None,
            )
            used_label = True
            band = [p for p in segment if p[1].get("ci_low") is not None]
            if band:
                ax.fill_between(
                    [p[0] for p in band],
                    [p[1]["ci_low"] for p in band],
                    [p[1]["ci_high"] for p in band],
                    color=color,
                    alpha=0.12,
                )

    grouped = {}
    for row in summaries:
        grouped.setdefault((row["family"], row["workload"]), set()).add(row["id"])
    for (family, workload), cell_ids in grouped.items():
        axes_spec = curve_axes(cells, cell_ids)
        ordered_cells = axes_spec["ordered_cells"]
        numeric, xvalue = axes_spec["numeric"], axes_spec["xvalue"]
        fig, axes = plt.subplots(1, 3, figsize=(13.2, 3.5), layout="constrained")
        for condition in conditions:
            color, marker = style[condition]
            for ax, metric in zip(axes[:2], ("throughput_mib_s", "accepted_mib_s")):
                data = [
                    (xvalue[key], lookup[(key, condition)][metric])
                    for key in ordered_cells
                    if (key, condition) in lookup and lookup[(key, condition)][metric]["n"]
                ]
                series(ax, data, color, marker, condition)
                for point in points:
                    if (
                        point["id"] in cell_ids
                        and point["condition"] == condition
                        and point[metric] is not None
                    ):
                        ax.scatter(
                            xvalue[point["id"]], point[metric], color=color, s=11, alpha=0.35
                        )
            # Native is an absolute reference, not a VM optimization. Its much
            # larger ratio can hide every VM comparison on this panel.
            native = (
                condition == "native"
                or protocol.get("condition_factors", {}).get(condition, {}).get("backend")
                == "native"
            )
            if condition != baseline and not native:
                data = [
                    (xvalue[key], ratios_by[(key, condition)])
                    for key in ordered_cells
                    if (key, condition) in ratios_by and ratios_by[(key, condition)]["n"]
                ]
                series(axes[2], data, color, marker, condition)
                for point in ratios:
                    if point["id"] in cell_ids and point["condition"] == condition:
                        axes[2].scatter(
                            xvalue[point["id"]],
                            point["throughput_ratio"],
                            color=color,
                            s=11,
                            alpha=0.35,
                        )
        axes[0].set_ylabel("Completed throughput (MiB/s)")
        axes[1].set_ylabel("Accepted / loop throughput (MiB/s)")
        axes[2].set_ylabel(f"VM paired throughput / {baseline}")
        axes[2].axhline(1, color="#666666", linestyle="--", linewidth=0.8)
        axis_label, scale = axes_spec["label"], axes_spec["scale"]
        xs = list(xvalue.values())
        for ax in axes:
            ax.set_xlabel(axis_label)
            if numeric and scale in ("log", "log2") and all(value > 0 for value in xs):
                ax.set_xscale("log", base=2)
            elif numeric and scale == "symlog":
                ax.set_xscale("symlog", base=2, linthresh=1)
            if len(set(xs)) <= 12:
                ax.set_xticks(xs, axes_spec["tick_labels"])
            ax.set_ylim(bottom=0)
        axes[0].legend(fontsize=8, frameon=False)
        nvalues = sorted(
            {row["throughput_mib_s"]["n"] for row in summaries if row["id"] in cell_ids}
        )
        fig.suptitle(
            f"{family}: {workload} | N={','.join(map(str, nvalues))} independent environments/point\n"
            f"95% bootstrap; N<30 exploratory; split points withhold overall median/ranking; N5 cannot test splits\n"
            f"{failure_count} failed environments retained; dependent rounds aggregated",
            fontsize=10,
        )
        save(fig, slug(family) + "-" + slug(workload))

    phases = phase_breakdown(points)
    phase_cells = list(dict.fromkeys(row["id"] for row in phases))
    if phase_cells:
        fig, axes = plt.subplots(
            1,
            len(phase_cells),
            figsize=(5 * len(phase_cells), 4.7),
            layout="constrained",
            squeeze=False,
        )
        phase_labels = (
            "Accepted / operation loop",
            "Applied / flush",
            "File fsync",
            "Close",
            "Directory fsync",
        )
        for ax, cell_id in zip(axes[0], phase_cells):
            selected = {row["condition"]: row for row in phases if row["id"] == cell_id}
            arms = [arm for arm in conditions if arm in selected]
            positions = list(range(len(arms)))
            bottom = [0.0] * len(arms)
            for index, (phase, label) in enumerate(zip(PHASES[:-1], phase_labels)):
                heights = [selected[arm][phase + "_mean_ms"] for arm in arms]
                ax.bar(positions, heights, bottom=bottom, color=COLORS[index], label=label)
                bottom = [base + height for base, height in zip(bottom, heights)]
            for x, arm in enumerate(arms):
                values = selected[arm]["individual_total_ms"]
                offsets = [(index - (len(values) - 1) / 2) * 0.035 for index in range(len(values))]
                ax.scatter(
                    [x + offset for offset in offsets],
                    values,
                    color="#222222",
                    s=15,
                    facecolors="none",
                    linewidths=0.8,
                    zorder=4,
                    label="Individual environment total" if x == 0 else None,
                )
            ax.set_xticks(positions, arms, rotation=35, ha="right")
            ax.set(ylabel="Wall time per resident round (ms)", title=cell_id, ylim=(0, None))
            ax.grid(axis="x", visible=False)
        axes[0][-1].legend(fontsize=8, frameon=False, loc="upper left", bbox_to_anchor=(1.01, 1))
        fig.suptitle(
            "Phase arithmetic means across independent environments; additive to mean total\n"
            "Dependent rounds averaged first; all wall-time interference and team coordination retained",
            fontsize=10,
        )
        save(fig, "phase-means-selected-blocks")

    # A resource record covers the entire VM/environment, not an individual cell.
    # Only condition-level knobs may therefore be used for memory-parameter plots.
    factors = protocol.get("condition_factors", {})
    numeric_factors = sorted(
        {
            key
            for values in factors.values()
            if isinstance(values, dict)
            for key, value in values.items()
            if finite_number(value) is not None
        }
    )
    memory_keys = sorted(
        {
            key
            for env in environments
            for key in env
            if (key.startswith("resource.") or key == "sampled_memory_current_peak_bytes")
            and any(word in key.lower() for word in ("memory", "rss"))
            and key.endswith("_bytes")
            and finite_number(env[key]) is not None
            and not any(word in key.lower() for word in ("integral", "event", "time"))
        }
    )
    for factor in numeric_factors:
        facets = {}
        for condition, values in factors.items():
            if not isinstance(values, dict) or finite_number(values.get(factor)) is None:
                continue
            others = json.dumps({k: v for k, v in values.items() if k != factor}, sort_keys=True)
            facets.setdefault(others, []).append(condition)
        for facet_index, (others, arms) in enumerate(sorted(facets.items())):
            if len({factors[arm][factor] for arm in arms}) < 2:
                continue
            selected = protocol.get("parameter_curve_cells")
            if selected is None:
                selected = [
                    max(ids, key=lambda key: x_sort(cells[key].get("x", key)))
                    for ids in grouped.values()
                ]
            fig, ax = plt.subplots(figsize=(7, 3.8), layout="constrained")
            plotted = False
            for cell_id in selected:
                cell_id = str(cell_id)
                data = [
                    (float(factors[arm][factor]), lookup[(cell_id, arm)]["throughput_mib_s"])
                    for arm in arms
                    if (cell_id, arm) in lookup
                ]
                if len(data) >= 2:
                    color = COLORS[len(ax.lines) % len(COLORS)]
                    series(ax, data, color, "o", cell_id)
                    for point in points:
                        if point["id"] == cell_id and point["condition"] in arms:
                            ax.scatter(
                                factors[point["condition"]][factor],
                                point["throughput_mib_s"],
                                color=color,
                                alpha=0.35,
                                s=11,
                            )
                    plotted = True
            if plotted:
                ax.set(
                    xlabel=factor,
                    ylabel="Completed throughput (MiB/s)",
                    title=f"Condition parameter: {factor}\nOther factors: {others}",
                )
                ax.legend(fontsize=8, frameon=False)
                save(fig, "parameter-" + slug(factor) + f"-{facet_index}")
            else:
                plt.close(fig)
            for memory_key in memory_keys:
                data = []
                for arm in arms:
                    values = [
                        env[memory_key] / MIB
                        for env in environments
                        if env["condition"] == arm
                        and finite_number(env.get(memory_key)) is not None
                    ]
                    if values:
                        data.append(
                            (
                                float(factors[arm][factor]),
                                describe(
                                    values, bootstrap=True, seed=seed_for(factor, arm, memory_key)
                                ),
                            )
                        )
                if len(data) < 2:
                    continue
                fig, ax = plt.subplots(figsize=(6.3, 3.6), layout="constrained")
                series(ax, data, COLORS[0], "o", memory_key.removeprefix("resource."))
                for arm in arms:
                    values = [
                        env[memory_key] / MIB
                        for env in environments
                        if env["condition"] == arm
                        and finite_number(env.get(memory_key)) is not None
                    ]
                    ax.scatter(
                        [factors[arm][factor]] * len(values),
                        values,
                        color=COLORS[0],
                        alpha=0.35,
                        s=15,
                    )
                ax.set(
                    xlabel=factor,
                    ylabel="Whole-environment memory (MiB)",
                    title=f"{memory_key}; scope: all cells / entire VM\nOther factors: {others}",
                )
                save(fig, "memory-" + slug(factor) + "-" + slug(memory_key) + f"-{facet_index}")
    return files


def x_sort(value: Any) -> tuple[int, Any]:
    number = finite_number(value)
    return (0, number) if number is not None else (1, str(value))


def analyze(batch: Path, output: Path, *, plots: bool = True) -> dict:
    protocol_path, records_path = batch / "protocol.json", batch / "records.json"
    protocol = json.loads(protocol_path.read_text())
    factors, factors_source = condition_factors(protocol)
    protocol = {**protocol, "condition_factors": factors}
    loaded = json.loads(records_path.read_text())
    records = loaded.get("records") if isinstance(loaded, dict) else loaded
    if not isinstance(records, list):
        raise ValueError("records.json must contain a list of environment records")
    points, raw, failures, environments = collect(protocol, records)
    summaries, comparisons, ratios = summarize(protocol, points)
    diagnostic = protocol.get("profiling") is True or protocol.get("benchmark_id") == "B-FS-DIAG"
    if diagnostic:
        summaries, comparisons, ratios = [], [], []
    output.mkdir(parents=True, exist_ok=True)
    csv_file(output / "raw_rows.csv", raw)
    csv_file(output / "environment_points.csv", points)
    csv_file(output / "environment_resources.csv", environments)
    csv_file(output / "curve_summary.csv", flattened_summary(summaries))
    csv_file(output / "paired_ratios.csv", ratios)
    csv_file(output / "paired_ratio_summary.csv", comparisons)
    csv_file(output / "phase_means.csv", [] if diagnostic else phase_breakdown(points))
    csv_file(output / "failures.csv", failures)
    resources = {}
    for condition in protocol.get("conditions", {}):
        selected = [env for env in environments if env["condition"] == condition]
        keys = sorted(
            {
                key
                for env in selected
                for key in env
                if key not in ("record", "pair", "condition")
                and finite_number(env[key]) is not None
            }
        )
        resources[condition] = {
            key: describe([env[key] for env in selected if finite_number(env.get(key)) is not None])
            for key in keys
        }
    summary = {
        "analysis_schema": 3,
        "analysis_version": "post_collection_v2_separated_distributions",
        "benchmark": protocol.get("benchmark_id", "B-FS-ENG"),
        "role": protocol.get("role", "engineering A/B"),
        "status": "incomplete_or_failed" if failures else "analyzed",
        "source": {
            "batch": str(batch.resolve()),
            "protocol_sha256": sha256(protocol_path),
            "records_sha256": sha256(records_path),
            "analysis_sha256": sha256(Path(__file__)),
        },
        "baseline_condition": protocol["baseline_condition"],
        "bootstrap_resamples": BOOTSTRAPS,
        "confidence": 0.95,
        "condition_factors": factors,
        "condition_factors_source": factors_source,
        "statistical_scope": {
            "unit": "one independent pair/condition environment",
            "rounds": protocol.get("resident_rounds", protocol.get("rounds", 3)),
            "durations": "sum resident-round durations / round count",
            "throughput": "sum bytes / sum total_ns; includes the payload's specified synchronization",
            "accepted_throughput": "sum bytes / sum accepted_ns; separate from completed throughput",
            "accepted_scope": "payload operation loop, including any configured periodic synchronization",
            "paired_ratio": "median of candidate/baseline per-environment throughput ratios",
            "small_samples": "N<30 exploratory; five environments are not fifteen independent samples",
            "separated_distributions": {
                **SEPARATION_RULE,
                "selection": "largest qualifying adjacent gap; ties choose lower cut",
                "action": "retain all points and cluster counts/fractions/medians; withhold overall median/CI and unified ranking",
                "N5_limit": "minimum three per side requires N>=6; N5 cannot test this registered rule, so an unflagged N5 point is not evidence of unimodality",
            },
            "tails": "No P99 or operation-level tail inference",
            "failure_policy": "failed/incomplete environments retained and excluded explicitly; no speed/interference filtering",
            "resource_scope": "whole environment across every cell; not workload-local memory",
            "kernel_memory_peak": "shared-campaign cgroup memory.peak is cumulative; do not interpret as per-trial peak",
            "parameter_default_cells": "largest protocol x per family/workload; selection does not use measured performance",
        },
        "counts": {
            "input_records": len(records),
            "valid_environments": len(environments),
            "failed_environments": len(failures),
            "environment_points": len(points),
            "separated_primary_points": sum(
                row["throughput_mib_s"]["separated"] for row in summaries
            ),
            "withheld_comparisons": sum(row["ranking_withheld"] for row in comparisons),
        },
        "failures": failures,
        "curves": summaries,
        "comparisons": comparisons,
        "resources": resources,
        "plots": [],
        "plot_status": "diagnostic_only"
        if diagnostic
        else ("disabled" if not plots else "pending"),
    }
    if plots and not diagnostic:
        try:
            summary["plots"] = render_plots(
                output,
                protocol,
                points,
                summaries,
                comparisons,
                ratios,
                environments,
                len(failures),
            )
            summary["plot_status"] = "generated"
        except ImportError as error:
            summary["plot_status"] = "dependency_missing"
            summary["plot_error"] = str(error)
    (output / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True, allow_nan=False) + "\n"
    )
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "batch", type=Path, help="directory containing protocol.json and records.json"
    )
    parser.add_argument(
        "--output", type=Path, help="derived output directory (default: BATCH/analysis)"
    )
    parser.add_argument(
        "--no-plots", action="store_true", help="CSV/JSON only; no matplotlib import"
    )
    args = parser.parse_args()
    try:
        summary = analyze(
            args.batch, args.output or args.batch / "analysis", plots=not args.no_plots
        )
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(2, f"analysis error: {error}\n")
    print(
        json.dumps(
            {
                "status": summary["status"],
                "counts": summary["counts"],
                "plot_status": summary["plot_status"],
            },
            sort_keys=True,
        )
    )
    if summary["plot_status"] == "dependency_missing":
        print(
            "matplotlib is required for SVG/PDF; CSV and summary.json were preserved.",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
