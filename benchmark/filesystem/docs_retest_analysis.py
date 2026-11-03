#!/usr/bin/env python3
"""Analyze complete B-FS-ENG documentation-workload retests, never partial runs."""

import argparse
import json
import random
import statistics
from pathlib import Path


def clusters(values):
    """Apply the published local-read gap rule; retain all valid slow values."""
    ordered = sorted(values)
    threshold = max(5_000_000, 0.1 * statistics.median(ordered))
    cuts = [0]
    for index in range(3, len(ordered) - 2):
        if ordered[index] - ordered[index - 1] > threshold and index - cuts[-1] >= 3:
            cuts.append(index)
    cuts.append(len(ordered))
    return [
        {"n": right - left, "median_ms": statistics.median(ordered[left:right]) / 1e6}
        for left, right in zip(cuts, cuts[1:])
    ]


def comparison(pairs, key, baseline, candidate):
    if not pairs:
        raise ValueError("no pairs")
    for pair in pairs:
        if any(
            type(pair[arm][key]) is not int or pair[arm][key] <= 0 for arm in (baseline, candidate)
        ):
            raise ValueError("positive integer clocks required")
    ratios = [p[candidate][key] / p[baseline][key] for p in pairs]
    rng = random.Random(42)
    samples = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) for _ in range(5000))
    ci = [100 * (1 - samples[4874]), 100 * (1 - samples[124])]
    return {
        "n": len(pairs),
        "reduction_percent": 100 * (1 - statistics.median(ratios)),
        "bootstrap_95_percent": ci,
        "detected": ci[0] > 0 or ci[1] < 0,
        "baseline_clusters": clusters([p[baseline][key] for p in pairs]),
        "candidate_clusters": clusters([p[candidate][key] for p in pairs]),
        "baseline_ms": [p[baseline][key] / 1e6 for p in pairs],
        "candidate_ms": [p[candidate][key] / 1e6 for p in pairs],
    }


def analyze(root):
    manifest = json.loads((root / "observations/manifest.json").read_text())
    plan = json.loads((root / "plan.json").read_text())
    if manifest.get("status") != "complete" or (root / "interruption.json").exists():
        raise ValueError("incomplete or interrupted batch")
    records = json.loads((root / "observations/pairs.json").read_text())
    if set(records) != {"python", "mmap-scan"}:
        raise ValueError("incomplete workload inventory")
    output = {}
    for workload, pairs in records.items():
        if len(pairs) != plan["runs"] or any(
            set(p) != {"main", "default", "opt-in"} for p in pairs
        ):
            raise ValueError("incomplete triplets")
        output[workload] = {}
        for key in pairs[0]["main"]:
            output[workload][key] = {
                "main_to_default": comparison(pairs, key, "main", "default"),
                "default_to_opt_in": comparison(pairs, key, "default", "opt-in"),
            }
    (root / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    return output


def rootfs_levels(root):
    status = json.loads((root / "status.json").read_text())
    plan = json.loads((root / "plan.json").read_text())
    if not status.get("valid") or status.get("failed_cells"):
        raise ValueError("invalid rootfs cohort")
    records = json.loads((root / "records.json").read_text())
    groups = {}
    inventory = set()
    for row in records:
        key = (row["case_id"], row["backend"], row["revision"])
        unit = (*key, row["pair"])
        if unit in inventory or not row["valid"] or not row["host_verified"]:
            raise ValueError("duplicate or unverified environment")
        inventory.add(unit)
        if len(row["rows"]) != 3 or {r["round"] for r in row["rows"]} != {1, 2, 3}:
            raise ValueError("exact three resident rounds required")
        groups.setdefault(key, []).append(
            {
                "ready_ms": row["ready_ns"] / 1e6,
                "completed_ms": sum(r["completed_ns"] for r in row["rows"]) / 1e6,
            }
        )
    expected = set()
    for revision, batch in plan["batches"].items():
        protocol = json.loads((Path(batch) / "protocol.json").read_text())
        for case in protocol["cases"]:
            for backend in protocol["backends"]:
                for pair in range(1, plan["samples"] + 1):
                    expected.add((case, backend["id"], revision, pair))
    if inventory != expected:
        raise ValueError("incomplete rootfs inventory")
    output = {}
    for key, units in sorted(groups.items()):
        output["/".join(key)] = {
            metric: {
                "n": len(units),
                "median": statistics.median(u[metric] for u in units),
                "min": min(u[metric] for u in units),
                "max": max(u[metric] for u in units),
            }
            for metric in ("ready_ms", "completed_ms")
        }
    (root / "analysis.json").write_text(json.dumps(output, indent=2) + "\n")
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--rootfs", action="store_true")
    args = parser.parse_args()
    print(json.dumps((rootfs_levels if args.rootfs else analyze)(args.batch.resolve()), indent=2))
