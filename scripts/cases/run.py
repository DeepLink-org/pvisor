#!/usr/bin/env python3
"""Run the documented USE cases and require every selected case to PASS."""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / "docs/src/zh/cases"


def catalog_ids(catalog=CATALOG):
    ids = []
    for path in sorted(catalog.glob("*.md")):
        ids.extend(re.findall(r"^### (S-USE-\d{3})[：:]", path.read_text(), re.MULTILINE))
    if not ids or len(ids) != len(set(ids)):
        raise ValueError("learning-path case IDs are empty or duplicated")
    return ids


def require_pass(report, expected):
    results = report["results"]
    actual = [result["id"] for result in results]
    if len(actual) != len(set(actual)) or set(actual) != set(expected):
        raise ValueError(f"case inventory mismatch: expected {sorted(expected)}, got {actual}")
    failures = [row for row in results if row["verdict"]["verdict"] != "PASS"]
    if failures:
        details = []
        for row in failures:
            verdict = row["verdict"]
            tail = "\n".join(
                verdict.get("output_tail", verdict.get("message", "")).splitlines()[-8:]
            )
            details.append(
                f"{row['id']}: {verdict['verdict']}; workdir={row.get('workdir')}\n{tail}"
            )
        raise ValueError("\n".join(details))
    return len(results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--subject-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "target/pvisor-learning-report.json")
    parser.add_argument("--case", action="append", default=[], help="comma-separated S-USE IDs")
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--require-reviewed", action="store_true")
    args = parser.parse_args()
    ids = catalog_ids()
    selected = [item for group in args.case for item in group.split(",")] if args.case else ids
    if len(set(selected)) != len(selected) or set(selected) - set(ids):
        parser.error("selected case IDs must be unique and present in the learning path")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    # A previous successful report must never hide a failed or aborted new run.
    output.unlink(missing_ok=True)
    command = [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        "tools/semspec/Cargo.toml",
        "--locked",
        "--",
        "--config",
        "semspec-use.toml",
        "run",
        "--domain",
        "USE",
        "--subject-bin",
        str(args.subject_bin.resolve()),
        "--format",
        "json",
        "--output",
        str(output),
    ]
    if args.case:
        command += ["--case", ",".join(selected)]
    if args.keep:
        command.append("--keep")
    if args.require_reviewed:
        command.append("--require-reviewed")
    result = subprocess.run(command, cwd=ROOT, check=False)
    try:
        count = require_pass(json.loads(output.read_text()), selected)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"learning-path gate failed: {error}", file=sys.stderr)
        return result.returncode or 1
    if result.returncode:
        return result.returncode
    print(f"{count} learning-path cases PASS; report: {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
