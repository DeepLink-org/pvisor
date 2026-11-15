"""Read-only verification/statistics for retained Python benchmark evidence.

This is not a measurement runtime: it never starts subjects, reads a clock or
samples a workload. The historical clock scope and evidence schema are preserved.
"""

import json
import shlex
import statistics
import tempfile
from pathlib import Path

FIELDS = ("N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms")
READY_FIELDS = (
    "N",
    "Ready median ms",
    "Ready mean ms",
    "Ready SD ms",
    "Ready min ms",
    "Ready max ms",
)
EXIT_FIELDS = tuple(field.replace("Ready", "Exit") for field in READY_FIELDS)


def save(path, value):
    """Atomically save JSON metadata without changing the archived JSON format."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    temporary.replace(path)


def summarize(samples):
    """Recompute the original millisecond statistics from integer nanoseconds."""
    if len(samples) < 2 or any(type(n) is not int or n <= 0 for n in samples):
        raise ValueError("at least two positive integer durations required")
    values = [n / 1_000_000 for n in samples]
    return dict(
        zip(
            FIELDS,
            (
                len(values),
                statistics.median(values),
                statistics.mean(values),
                statistics.stdev(values),
                min(values),
                max(values),
            ),
        )
    )


def summarize_ready(samples, label="Ready"):
    """Recompute the original event-labelled statistics from retained durations."""
    values = summarize(samples)
    return dict(
        zip(tuple(field.replace("Ready", label) for field in READY_FIELDS), values.values())
    )


def summarize_pairs(samples):
    """Verify paired Ready/Exit durations and recompute their historical fields."""
    if any(
        set(s) != {"ready_ns", "exit_ns"}
        or type(s["ready_ns"]) is not int
        or type(s["exit_ns"]) is not int
        or not 0 < s["ready_ns"] <= s["exit_ns"]
        for s in samples
    ):
        raise ValueError("Ready and Exit must share one valid trial")
    return summarize_ready([s["ready_ns"] for s in samples]) | summarize_ready(
        [s["exit_ns"] for s in samples], "Exit"
    )


def audit_command_row(batch, manifest, condition, record, resolved):
    """Verify every retained trial, correctness hook and derived statistic.

    Reads recorded commands but never executes them or any recorded hook.
    """
    config = json.loads((batch / "commands.json").read_text())[condition]
    phases = [("warmup", i) for i in range(manifest["warmup"])] + [
        ("sample", i) for i in range(manifest["runs"])
    ]
    expected_paths = [batch / "command-trials" / condition / f"{p}-{i:04d}.json" for p, i in phases]
    if set((batch / "command-trials" / condition).glob("*.json")) != set(expected_paths):
        raise ValueError("missing or extra command trials")
    samples = []
    for path, (phase, index) in zip(expected_paths, phases):
        item = json.loads(path.read_text())
        if (
            item.get("error")
            or item.get("cleanup_error")
            or item["condition"] != condition
            or item["phase"] != phase
            or item["index"] != index
            or item["command"] != config["argv"]
            or item["environment"] != config.get("env", {})
            or item["exit_code"] != 0
            or type(item["start_ns"]) is not int
            or type(item["end_ns"]) is not int
            or item["end_ns"] - item["start_ns"] != item["duration_ns"]
            or item["duration_ns"] <= 0
        ):
            raise ValueError("failed or altered command trial")
        hooks = [
            (action, config[action])
            for action in ("prepare", "conclude", "cleanup")
            if config.get(action)
        ]
        if [(h["action"], h["command"]) for h in item["hooks"]] != hooks or any(
            h["exit_code"] != 0 for h in item["hooks"]
        ):
            raise ValueError("missing or failed correctness hook")
        if "expected_stdout" in config and item["stdout"] != config["expected_stdout"]:
            raise ValueError("stdout differs from expected text")
        if phase == "sample":
            samples.append(item["duration_ns"])
    expected = summarize(samples)
    raw = json.loads((batch / f"{condition}.samples.json").read_text())
    if (
        raw
        != dict(
            engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=expected
        )
        or list(resolved) != list(FIELDS)
        or resolved != expected
        or record["raw"] != expected
        or record["value"] != expected
    ):
        raise ValueError("resolved row or samples differ from command trials")
    return dict(command=shlex.join(config["argv"]), statistics=expected)
