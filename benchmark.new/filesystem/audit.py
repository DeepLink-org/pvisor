"""Recompute current native filesystem rows from retained trials and check inputs."""

import json
import shlex
from pathlib import Path

from common import ROOT, save
from fs_timing import (
    FIELDS,
    FIXTURE_SCOPE,
    ROWS,
    RUNS,
    SCOPE,
    TIMEOUT,
    WARMUP,
    command,
    summarize,
    validate_result,
)
from workload import validate_fixture


def _same_trial_command(actual, expected):
    if not isinstance(actual, list) or len(actual) != len(expected) or actual[1:] != expected[1:]:
        return False
    try:
        actual_python, expected_python = Path(actual[0]), Path(expected[0])
        # uv can launch preparation as bin/python and the fixed script row as
        # bin/python3. Preserve the environment's bin directory and executable
        # identity; do not accept another virtual environment or altered argv.
        return (
            actual_python.is_absolute()
            and actual_python.parent.resolve(strict=True)
            == expected_python.parent.resolve(strict=True)
            and actual_python.resolve(strict=True) == expected_python.resolve(strict=True)
        )
    except (OSError, TypeError, ValueError):
        return False


def audit_filesystem(batch, manifest, evidence, resolved, template):
    batch = Path(batch)
    if (
        manifest["entry"] != "filesystem/tools.json"
        or manifest["benchmark_id"] != "B-FS-TOOLS"
        or manifest["metric_scope"] != SCOPE
        or manifest["fixture_scope"] != FIXTURE_SCOPE
        or manifest["runs"] != RUNS
        or manifest["warmup"] != WARMUP
        or manifest.get("measurement_engine") != "python-perf_counter_ns"
        or not template
        or not set(template) <= set(ROWS)
        or manifest["rows"] != {row: row for row in template}
        or manifest["conditions"] != list(template)
        or set(resolved) != set(template)
    ):
        raise ValueError("filesystem timing/fixture configuration differs")
    fixture = validate_fixture(batch / "fixture")
    if evidence["cells"] or len(evidence["commands"]) != len(template):
        raise ValueError("filesystem evidence requires one script command per row")
    phases = (
        [("preflight", 0)]
        + [("warmup", i) for i in range(WARMUP)]
        + [("sample", i) for i in range(RUNS)]
    )
    rows, results = [], {}
    for (label, expression), record in zip(template.items(), evidence["commands"]):
        mode = ROWS[label]
        expected_argv = ["python3", str(ROOT / "filesystem" / f"{mode}_timing.py"), str(batch)]
        if (
            not isinstance(expression, str)
            or not expression.startswith("$ ")
            or shlex.split(expression[2:]) != expected_argv
            or record.get("kind") != "script-row"
            or record["command"] != expression[2:]
        ):
            raise ValueError("filesystem row command differs from the fixed probe")
        rows.append(
            dict(row=label, command=expression[2:], expression=expression[2:], columns=list(FIELDS))
        )
        expected_paths = [
            batch / "trials" / mode / f"{phase}-{index:03d}.json" for phase, index in phases
        ]
        if set((batch / "trials" / mode).glob("*.json")) != set(expected_paths):
            raise ValueError("missing filesystem preflight/warmup/sample evidence")
        previous_end = 0
        samples = []
        for path, (phase, index) in zip(expected_paths, phases):
            trial = json.loads(path.read_text())
            if (
                not _same_trial_command(trial["command"], command(batch, mode))
                or (trial["phase"], trial["index"]) != (phase, index)
                or "error" in trial
            ):
                raise ValueError("filesystem trial command, order or outcome differs")
            validate_result(trial, mode)
            start, end, duration = (trial[key] for key in ("start_ns", "end_ns", "duration_ns"))
            if (
                any(type(value) is not int for value in (start, end, duration))
                or not previous_end < start < end
                or duration != end - start
                or duration > TIMEOUT * 1_000_000_000
            ):
                raise ValueError("invalid filesystem monotonic duration/timeout")
            previous_end = end
            if phase == "sample":
                samples.append(duration)
        result = summarize(samples)
        raw = json.loads((batch / f"{mode}.samples.json").read_text())
        if raw != dict(
            engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result
        ):
            raise ValueError("filesystem raw samples or statistics changed")
        if (
            list(resolved[label]) != list(FIELDS)
            or resolved[label] != result
            or record["raw"] != result
            or record["value"] != result
        ):
            raise ValueError("filesystem resolved row differs from recorded samples")
        results[label] = dict(command=shlex.join(command(batch, mode)))
    if evidence.get("rows") != rows:
        raise ValueError("incomplete filesystem row mapping")
    save(
        batch / "filesystem.audit.json", dict(status="passed", fixture=fixture, metric_scope=SCOPE)
    )
    return results
