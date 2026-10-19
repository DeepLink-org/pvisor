"""B-PROCESS/B-REPLAY: direct argv timing; hooks excluded, every trial retained."""

import argparse
import json
import os
import shlex
import statistics
import subprocess
import time
from pathlib import Path

from common import METRIC_SCOPE, digest, save

FIELDS = ("N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms")


def summarize(samples):
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


def trial(batch, condition, phase, index, config):
    record = dict(
        condition=condition,
        phase=phase,
        index=index,
        command=config["argv"],
        environment=config.get("env", {}),
        hooks=[],
        exit_code=None,
    )
    env = os.environ.copy() | config.get("env", {})
    try:
        for action in ("prepare", "subject", "conclude"):
            argv = config["argv"] if action == "subject" else config.get(action)
            if not argv:
                continue
            start = time.perf_counter_ns()
            result = subprocess.run(
                argv,
                cwd=config["cwd"],
                env=env,
                capture_output=True,
                text=True,
                timeout=config["timeout"],
            )
            end = time.perf_counter_ns()
            outcome = dict(
                command=argv,
                exit_code=result.returncode,
                stdout=result.stdout,
                stderr=result.stderr,
            )
            if action == "subject":
                record.update(outcome, start_ns=start, end_ns=end, duration_ns=end - start)
            else:
                record["hooks"].append(dict(action=action, **outcome))
            if result.returncode:
                raise ValueError(f"{action} failed with exit {result.returncode}")
        return record["duration_ns"]
    except BaseException as error:
        record["error"] = str(error)
        if isinstance(error, subprocess.TimeoutExpired):
            record["stdout"] = (error.stdout or b"").decode(errors="replace")
            record["stderr"] = (error.stderr or b"").decode(errors="replace")
        raise
    finally:
        save(batch / "command-trials" / condition / f"{phase}-{index:04d}.json", record)


def measure(batch, condition):
    batch = Path(batch)
    manifest = json.loads((batch / "manifest.json").read_text())
    config = json.loads((batch / "commands.json").read_text())[condition]
    if manifest["status"] != "prepared" or manifest["metric_scope"] != METRIC_SCOPE:
        raise ValueError("expected a freshly prepared command batch")
    if (batch / "command-trials" / condition).exists():
        raise ValueError("do not reuse a measured command row")
    for path, expected in manifest["artifacts"].items():
        if digest(path) != expected:
            raise ValueError("prepared artifact changed: " + path)
    for i in range(manifest["warmup"]):
        trial(batch, condition, "warmup", i, config)
    samples = [trial(batch, condition, "sample", i, config) for i in range(manifest["runs"])]
    result = summarize(samples)
    save(
        batch / f"{condition}.samples.json",
        dict(engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result),
    )
    return result


def audit_command_row(batch, manifest, condition, record, resolved):
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
            (action, config[action]) for action in ("prepare", "conclude") if config.get(action)
        ]
        if [(h["action"], h["command"]) for h in item["hooks"]] != hooks or any(
            h["exit_code"] != 0 for h in item["hooks"]
        ):
            raise ValueError("missing or failed correctness hook")
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


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("condition")
    args = parser.parse_args()
    try:
        print(json.dumps(measure(args.batch.resolve(), args.condition)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        manifest = json.loads((args.batch / "manifest.json").read_text())
        manifest.update(status="failed", error=str(error))
        save(args.batch / "manifest.json", manifest)
        parser.exit(1, f"{error}\n")
