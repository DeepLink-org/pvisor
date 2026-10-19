#!/usr/bin/env python3
"""Validate complete Run Bundles outside the measured command."""

import json
import sys
from pathlib import Path


def validate(path):
    run = json.loads(path.read_text())["run"]
    if run["state"] != "completed" or run["exit_code"] != 0:
        raise ValueError("Run Bundle must describe successful completion")


if __name__ == "__main__":
    mode, value = sys.argv[1:]
    path = Path(value)
    if mode == "bundle":
        validate(path)
    elif mode == "new-bundle":
        # A failed command never reaches this hook. Track every completed Run,
        # rather than silently revalidating an old Bundle when no new Run exists.
        current = {str(p) for p in path.glob("run-*/run-bundle.json")}
        previous_path = path / ".checked.json"
        previous = set(json.loads(previous_path.read_text())) if previous_path.exists() else set()
        fresh = current - previous
        if len(fresh) != 1:
            raise ValueError("expected exactly one new completed Run Bundle")
        for name in fresh:
            validate(Path(name))
        previous_path.write_text(json.dumps(sorted(current)))
    else:
        raise ValueError("unknown validation mode")
