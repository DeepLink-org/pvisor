"""Confirm failed VM guest cells with the balanced paired replay.

The sequential Criterion cohorts can drift on a shared runner. A cell that
exceeds the threshold is replayed as 16 balanced AB/BA blocks before the gate
fails; the threshold itself is unchanged.
"""

import argparse
import json
import math
import os
import re
import sys
from pathlib import Path


def load(path):
    with Path(path).open(encoding="utf-8") as stream:
        return json.load(stream)


def number(value):
    if type(value) not in (float, int) or not math.isfinite(value):
        raise ValueError("invalid comparison number")
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", required=True, choices=("guest", "startup"))
    parser.add_argument("--status-file", required=True)
    parser.add_argument("--comparison", required=True)
    parser.add_argument("--paired", required=True)
    args = parser.parse_args()

    status = int(Path(args.status_file).read_text(encoding="utf-8").strip())
    if status == 0:
        print("Sequential comparison passed.")
        return 0

    original = load(args.comparison)
    raw = Path(args.comparison).with_suffix(".sequential.json")
    if not raw.exists():
        raw.write_text(json.dumps(original, indent=2) + "\n", encoding="utf-8")
    threshold = number(original["threshold_pct"])
    if threshold < 0:
        raise ValueError("negative threshold")
    failed = [row for row in original["rows"] if row.get("passed") is False]
    if not failed:
        print("Sequential comparison failed without a failing pVisor cell.")
        return 1
    if args.kind != "guest":
        print(f"{len(failed)} startup cell(s) exceed the {threshold:g}% threshold.")
        return 1

    medians = {}
    lowers = {}
    changes = {}
    blocks = 0
    rejected = []
    for row in failed:
        condition = row["condition"]
        if not isinstance(condition, str) or not re.fullmatch(r"[a-z0-9-]{1,64}", condition):
            raise ValueError("invalid condition")
        summary = load(Path(args.paired) / condition / "summary.json")
        if summary.get("condition") != condition:
            raise ValueError(f"paired summary condition mismatch: {condition}")
        count = summary.get("blocks")
        if type(count) is not int or count <= 0:
            raise ValueError(f"invalid paired block count: {condition}")
        blocks = max(blocks, count)
        median = number(summary["median_paired_change_pct"])
        lower = number(summary.get("lower_paired_change_pct", median))
        passed = summary.get("paired_passed")
        if type(passed) is not bool:
            passed = lower <= threshold
        medians[condition] = median
        lowers[condition] = lower
        changes[condition] = number(row["change_pct"])
        if not passed:
            rejected.append((condition, changes[condition], median, lower))

    if rejected:
        print(
            "| Condition | Sequential | Paired median | Lower bound | Gate |\n"
            "| --- | ---: | ---: | ---: | --- |"
        )
        for condition, sequential, paired, lower in rejected:
            print(
                f"| `{condition}` | {sequential:+.2f}% | {paired:+.2f}% | "
                f"{lower:+.2f}% | REGRESSION |"
            )
        print(f"Paired replay also exceeds the {threshold:g}% threshold.")
        return 1

    original["gate_passed"] = True
    original["paired_confirmation"] = {
        "blocks": blocks,
        "threshold_pct": threshold,
        "medians": medians,
        "lower_bounds": lowers,
    }
    for row in original["rows"]:
        if row["condition"] in medians:
            row["paired_confirmed"] = True

    temporary = Path(args.comparison).with_suffix(".tmp")
    temporary.write_text(json.dumps(original, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, args.comparison)

    print("| Condition | Sequential | Paired median | Lower bound | Gate |")
    print("| --- | ---: | ---: | ---: | --- |")
    for condition, median in medians.items():
        print(
            f"| `{condition}` | {changes[condition]:+.2f}% | {median:+.2f}% | "
            f"{lowers[condition]:+.2f}% | PASS |"
        )
    print("Failed sequential cells are within the threshold on paired replay.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"invalid paired confirmation: {error}", file=sys.stderr)
        sys.exit(1)
