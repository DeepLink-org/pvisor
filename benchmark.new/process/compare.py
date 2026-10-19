#!/usr/bin/env python3
"""CI threshold gate using validated semspec cells and the Python timer median."""

import argparse
import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
_spec = importlib.util.spec_from_file_location("benchmark_update", ROOT / "update.py")
UPDATE = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(UPDATE)


def read(path):
    path = Path(path).resolve()
    directory = path.parent
    if path != directory / "in.bench.json":
        raise ValueError("expected the batch in.bench.json evidence file")
    manifest, table, results = UPDATE.validate_batch(directory)
    if manifest["benchmark_id"] != "B-PROCESS" or set(results) != {
        "native",
        "host-run",
        "status",
        "review",
    }:
        raise ValueError("expected a successful complete B-PROCESS semspec batch")
    return {
        name: result["statistics"]["Median ms"] / 1000 for name, result in results.items()
    }, manifest


def compare(candidate, baseline, threshold):
    rows, manifest = read(candidate)
    old = None
    if baseline:
        old, previous = read(baseline)
        for key in (
            "runs",
            "warmup",
            "measurement_engine",
            "python_version",
            "platform",
            "cpu_affinity",
            "conditions",
            "metric_scope",
        ):
            if manifest[key] != previous[key]:
                raise ValueError("incompatible measurement configuration: " + key)
        # Both cohorts must have used the exact same candidate harness/templates.
        if manifest["harness"] != previous["harness"]:
            raise ValueError("candidate and baseline use different benchmark harnesses")
    lines = [
        "# Process benchmark threshold gate",
        "",
        "perf_counter_ns median direct-command times in seconds. Separate cohorts; this threshold is not a paired improvement claim.",
        "",
        "| Condition | Candidate (s) | Baseline (s) | Gate |",
        "| --- | ---: | ---: | --- |",
    ]
    regressions = 0
    for name, value in rows.items():
        status = "candidate-only"
        previous_value = ""
        if old is not None:
            previous_value = f"{old[name]:.9f}"
            status = (
                "regression" if value > old[name] * (1 + threshold / 100) else "within threshold"
            )
            regressions += status == "regression"
        lines.append(f"| {name} | {value:.9f} | {previous_value} | {status} |")
    return "\n".join(lines) + "\n", regressions


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--candidate", type=Path, required=True)
    p.add_argument("--baseline", default="")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--threshold", type=float, default=15)
    args = p.parse_args()
    if args.threshold < 0:
        p.error("threshold must be nonnegative")
    try:
        text, regressions = compare(
            args.candidate, Path(args.baseline) if args.baseline else None, args.threshold
        )
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / "report.md").write_text(text)
    except (ValueError, KeyError, OSError) as error:
        p.exit(1, f"{error}\n")
    raise SystemExit(1 if regressions else 0)
