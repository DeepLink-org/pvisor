"""Read-only comparison of retained process observations.

Current regression gates read Criterion estimates in Rust. This reader only
accepts the original complete evidence schema; it never starts an experiment.
"""

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
        raise ValueError("expected a successful complete B-PROCESS archive")
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
            "harness",
        ):
            if manifest[key] != previous[key]:
                raise ValueError("incompatible measurement configuration: " + key)
    lines = [
        "# Retained process comparison",
        "",
        "Original direct-command medians in seconds. Separate cohorts; not a paired improvement claim.",
        "",
        "| Condition | Candidate (s) | Baseline (s) | Gate |",
        "| --- | ---: | ---: | --- |",
    ]
    regressions = 0
    for name, value in rows.items():
        status, previous_value = "candidate-only", ""
        if old is not None:
            previous_value = f"{old[name]:.9f}"
            status = (
                "regression" if value > old[name] * (1 + threshold / 100) else "within threshold"
            )
            regressions += status == "regression"
        lines.append(f"| {name} | {value:.9f} | {previous_value} | {status} |")
    return "\n".join(lines) + "\n", regressions
