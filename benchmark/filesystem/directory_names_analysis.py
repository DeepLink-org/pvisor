#!/usr/bin/env python3
"""Summarize only complete, successful sequential SDK A/B batches."""

import argparse
import json
import random
import statistics
from pathlib import Path


def analyze(batch):
    manifest = json.loads((batch / "manifest.json").read_text())
    status = json.loads((batch / "status.json").read_text())
    if not status.get("valid") or status.get("failures") != 0:
        raise ValueError("refusing performance conclusions from an invalid batch")
    records = json.loads((batch / "records.json").read_text())
    timed = [r for r in records if not r["diagnostic"]]
    expected = 2 * (manifest["samples"] + manifest["controls_pairs"])
    if len(timed) != expected:
        raise ValueError("incomplete timed environment inventory")
    groups = {}
    for record in timed:
        for phase, value in record["totals_ms"].items():
            pair = groups.setdefault(phase, {}).setdefault(record["pair"], {})
            if record["enabled"] in pair:
                raise ValueError("duplicate paired environment")
            pair[record["enabled"]] = value
    summary = {}
    for phase, pairs in groups.items():
        if any(set(pair) != {0, 1} for pair in pairs.values()):
            raise ValueError("unpaired environment")
        ratios = [pair[1] / pair[0] for pair in pairs.values()]
        rng = random.Random(42)
        bootstrap = sorted(
            statistics.median(rng.choices(ratios, k=len(ratios))) for _ in range(5000)
        )
        summary[phase] = {
            "pairs": len(pairs),
            "off_median_ms": statistics.median(pair[0] for pair in pairs.values()),
            "on_median_ms": statistics.median(pair[1] for pair in pairs.values()),
            "paired_reduction_percent": 100 * (1 - statistics.median(ratios)),
            "bootstrap_95_percent": [
                100 * (1 - bootstrap[4874]),
                100 * (1 - bootstrap[124]),
            ],
            "paired_ratios": ratios,
        }
    (batch / "analysis.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    analyze(parser.parse_args().batch.resolve())
