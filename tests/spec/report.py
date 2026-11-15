"""Fail closed on incomplete native Scrut JSON; never parse Markdown."""

import json
import sys
from pathlib import Path


def validate(rows, paths):
    expected = [str(Path(path).resolve()) for path in paths]
    identifiers = [Path(path).stem for path in paths]
    if not expected or len(identifiers) != len(set(identifiers)):
        raise ValueError("empty or duplicate selected IDs")
    actual = []
    for row in rows:
        location = row.get("location")
        if not isinstance(location, str):
            raise ValueError(f"Scrut result without a source location: {row}")
        actual.append(str(Path(location).resolve()))
        if row.get("result", {}).get("kind") != "success":
            raise ValueError(
                f"scenario did not succeed: {location}; inspect the native report streams"
            )
    if sorted(actual) != sorted(expected):
        raise ValueError(f"Scrut inventory mismatch: expected {expected}, got {actual}")


if __name__ == "__main__":
    try:
        validate(json.loads(Path(sys.argv[1]).read_text()), sys.argv[2:])
    except (ValueError, OSError) as error:
        raise SystemExit(str(error)) from error
