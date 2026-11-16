"""Read-only validation of recorded startup output markers."""

import json
import re


def validate_output(stdout, stderr, guest=False):
    if re.search(r"kernel panic", stdout + "\n" + stderr, re.I):
        raise ValueError("guest kernel panic")
    lines = stdout.splitlines()
    ready = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_READY")]
    result = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_RESULT")]
    exits = [i for i, line in enumerate(lines) if line.startswith("REFERENCE_EXIT")]
    if len(ready) != 1 or lines[ready[0]] != "REFERENCE_READY" or len(result) != 1:
        raise ValueError("missing or duplicate startup markers")
    if json.loads(lines[result[0]].removeprefix("REFERENCE_RESULT ")) != {
        "mode": "ready",
        "correctness": "passed",
    }:
        raise ValueError("incorrect startup result")
    if not ready[0] < result[0]:
        raise ValueError("startup markers out of order")
    if guest:
        if len(exits) != 1 or lines[exits[0]] != "REFERENCE_EXIT 0" or not result[0] < exits[0]:
            raise ValueError("guest did not report a unique ordered successful exit")
    elif exits:
        raise ValueError("unexpected guest exit marker in process output")
