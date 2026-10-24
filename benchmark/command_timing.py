"""B-PROCESS/B-REPLAY adapters for the semspec experiment runtime."""

import argparse
import json
import subprocess
from pathlib import Path

from common import METRIC_SCOPE, RUNTIME, digest, save

FIELDS = RUNTIME.FIELDS
summarize = RUNTIME.summarize
audit_command_row = RUNTIME.audit_command_row


def trial(batch, condition, phase, index, config):
    return RUNTIME.command_trial(
        batch / "command-trials" / condition / f"{phase}-{index:04d}.json",
        condition,
        phase,
        index,
        config,
    )


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
    samples = RUNTIME.repeat(
        manifest["warmup"],
        manifest["runs"],
        lambda phase, index: trial(batch, condition, phase, index, config),
    )
    result = summarize(samples)
    save(
        batch / f"{condition}.samples.json",
        dict(engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=result),
    )
    return result


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
