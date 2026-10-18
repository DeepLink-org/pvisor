#!/usr/bin/env python3
"""Validate semspec/hyperfine evidence, then append CSV and publish JSON tables."""

import argparse
import csv
import io
import json
import math
import os
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

from common import FILESYSTEM_SCOPE, METRIC_SCOPE, READY_EXIT_SCOPE, ROOT, digest, save

FIELDS = {
    "N": "count",
    "Median ms": "median",
    "Mean ms": "mean",
    "SD ms": "stddev",
    "Min ms": "min",
    "Max ms": "max",
}


def replace(path, content):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", newline="", dir=path.parent, delete=False
    ) as stream:
        temporary = Path(stream.name)
        stream.write(content)
    temporary.replace(path)


def validate_batch(directory):
    directory = Path(directory).resolve()
    manifest_path = directory / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest["status"] not in ("prepared", "passed") or manifest["metric_scope"] not in (
        METRIC_SCOPE,
        READY_EXIT_SCOPE,
        FILESYSTEM_SCOPE,
    ):
        raise ValueError("expected a prepared or successful direct-command semspec batch")
    if manifest["benchmark_id"] == "B-STARTUP" and manifest["metric_scope"] != READY_EXIT_SCOPE:
        raise ValueError("startup validation requires paired Ready / Exit data")
    if (
        manifest["entry"] == "filesystem/tools.json"
        and manifest["metric_scope"] != FILESYSTEM_SCOPE
    ):
        raise ValueError("filesystem validation requires the current Python command timer")
    if digest(directory / "in.json") != manifest["input_sha256"]:
        raise ValueError("benchmark input changed")
    if manifest["status"] == "prepared":
        for path, expected in manifest["artifacts"].items():
            if digest(path) != expected:
                raise ValueError("measured artifact changed: " + path)
    for name, expected in manifest["harness"].items():
        if digest(directory / "harness" / name) != expected:
            raise ValueError("frozen harness changed: " + name)
    exports = {name: digest(directory / name) for name in ("in.bench.json", "in.render.json")}
    if manifest["metric_scope"] == READY_EXIT_SCOPE:
        exports["timing.samples.json"] = digest(directory / "timing.samples.json")
    if manifest["metric_scope"] == FILESYSTEM_SCOPE:
        for condition in manifest["conditions"]:
            if condition not in ("native-metadata", "native-read"):
                raise ValueError("unknown filesystem condition")
            name = condition.removeprefix("native-") + ".samples.json"
            exports[name] = digest(directory / name)
    if manifest["status"] == "passed" and manifest["exports"] != exports:
        raise ValueError("export checksum mismatch")
    evidence = json.loads((directory / "in.bench.json").read_text())
    resolved = json.loads((directory / "in.render.json").read_text())
    template = json.loads((directory / "in.json").read_text())
    if (
        evidence["format"] != 1
        or evidence["cwd"] != manifest["cwd"]
        or evidence["template"] != str(directory / "in.json")
    ):
        raise ValueError("semspec evidence does not match the prepared input/cwd")
    if manifest["metric_scope"] == READY_EXIT_SCOPE:
        sys.path.insert(0, str(ROOT / "startup"))
        from audit import audit_timing

        results = audit_timing(directory, manifest, evidence, resolved, template)
        manifest["exports"], manifest["status"] = exports, "passed"
        save(manifest_path, manifest)
        return manifest, resolved, results
    if manifest["metric_scope"] == FILESYSTEM_SCOPE:
        import importlib.util

        sys.path.insert(0, str(ROOT / "filesystem"))
        spec = importlib.util.spec_from_file_location(
            "filesystem_audit", ROOT / "filesystem/audit.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        results = module.audit_filesystem(directory, manifest, evidence, resolved, template)
        manifest["exports"], manifest["status"] = exports, "passed"
        save(manifest_path, manifest)
        return manifest, resolved, results
    records = {item["command"]: item for item in evidence["commands"]}
    if len(records) != len(evidence["commands"]):
        raise ValueError("duplicate command evidence")
    if evidence["cells"] or set(resolved) != set(template):
        raise ValueError("expected complete row-command evidence without cell commands")
    results = {}
    expected_rows = []
    for row, expression in template.items():
        if not isinstance(expression, str) or not expression.startswith("$ "):
            raise ValueError("invalid row command")
        command = expression[2:]
        matches = [
            item for key, item in records.items() if shlex.split(key) == shlex.split(command)
        ]
        record = matches[0] if len(matches) == 1 else None
        if record is None or record.get("kind") != "hyperfine-row":
            raise ValueError("missing raw hyperfine row evidence")
        result = audit_hyperfine_row(record, manifest, row, resolved[row])
        condition = manifest["rows"][row]
        if condition in results and results[condition] != result:
            raise ValueError("one row refers to different experiments")
        results[condition] = result
        expected_rows.append(
            dict(row=row, command=record["command"], expression=command, columns=list(FIELDS))
        )
    if evidence.get("rows", []) != expected_rows:
        raise ValueError("incomplete row matrix")
    if set(results) != set(manifest["conditions"]) or len(records) != len(results):
        raise ValueError("incomplete condition set")
    if manifest["benchmark_id"] == "B-REPLAY":
        sys.path.insert(0, str(ROOT / "replay"))
        from replay_hooks import audit_replay

        audit_replay(directory, manifest, results)
    manifest["exports"] = exports
    manifest["status"] = "passed"
    save(manifest_path, manifest)
    return manifest, resolved, results


def audit_hyperfine_row(record, manifest, row, resolved):
    """Audit one full row against one direct hyperfine export, preserving its scope."""
    raw = record["raw"]
    if raw.get("schema_version") != 2 or len(raw.get("results", [])) != 1:
        raise ValueError("expected one raw hyperfine 2.x condition per command")
    argv = shlex.split(record["command"])
    if argv[0] != "hyperfine" or "|" in argv:
        raise ValueError("row command must invoke hyperfine directly")
    if (
        int(argv[argv.index("--runs") + 1]) != manifest["runs"]
        or int(argv[argv.index("--warmup") + 1]) != manifest["warmup"]
        or argv[argv.index("--command-name") + 1] != manifest["rows"][row]
    ):
        raise ValueError("row command differs from prepared runs/warmup/condition")
    if raw["metadata"]["hyperfine_version"] != manifest["hyperfine_version"].split()[-1]:
        raise ValueError("hyperfine version differs from preparation")
    result = raw["results"][0]
    if result.get("name") != manifest["rows"][row] or result["command"] != argv[-1]:
        raise ValueError("measured subject/name differs from template")
    samples = result["measurements"]
    summary = result["summary"]["time_wall_clock"]
    if (
        len(samples) != manifest["runs"]
        or any(s["exit_code"] != 0 for s in samples)
        or summary["count"] != manifest["runs"]
        or summary["unit"] != "second"
    ):
        raise ValueError("failed or incomplete hyperfine samples")
    expected = {
        column: summary[field] * (1 if column == "N" else 1000) for column, field in FIELDS.items()
    }
    if list(resolved) != list(FIELDS) or record["value"] != resolved:
        raise ValueError("resolved row schema/value differs from recorded hyperfine row")
    for column, value in expected.items():
        if type(resolved[column]) not in (int, float) or not math.isclose(
            resolved[column], value, rel_tol=1e-12, abs_tol=0
        ):
            raise ValueError("resolved cell differs from the recorded hyperfine selection")
    return result


def display_table(table):
    return {
        name: {
            column: f"{value:.2f}" if column.endswith(" ms") and value is not None else value
            for column, value in row.items()
        }
        for name, row in table.items()
    }


def publish_csv(directory, csv_path):
    return _publish_csv(directory, csv_path, *validate_batch(directory))


def _publish_csv(directory, csv_path, manifest, table, results, dry_run=False):
    """Append already-validated evidence without repeating full artifact audits."""
    directory, csv_path = Path(directory).resolve(), Path(csv_path)
    metadata = dict(
        batch=manifest["batch"],
        benchmark_id=manifest["benchmark_id"],
        role=manifest["role"],
        runs=manifest["runs"],
        warmup=manifest["warmup"],
        hyperfine_version=manifest["hyperfine_version"],
        metric_scope=manifest["metric_scope"],
        evidence_sha256=manifest["exports"]["in.bench.json"],
        manifest_sha256=digest(directory / "manifest.json"),
    )
    if manifest["metric_scope"] in (READY_EXIT_SCOPE, FILESYSTEM_SCOPE):
        metadata.update(
            measurement_engine=manifest["measurement_engine"],
            python_version=manifest["python_version"],
        )
    fields = [*metadata, "condition", "command", *next(iter(table.values()))]
    rows = [
        metadata
        | dict(condition=manifest["rows"][row], command=results[manifest["rows"][row]]["command"])
        | columns
        for row, columns in table.items()
    ]
    existing = []
    if csv_path.exists():
        reader = csv.DictReader(io.StringIO(csv_path.read_text()))
        if reader.fieldnames != fields:
            raise ValueError("CSV schema differs; use a new result file")
        existing = list(reader)
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    writer.writerows(rows)
    new = list(csv.DictReader(io.StringIO(stream.getvalue())))
    previous = [row for row in existing if row["batch"] == manifest["batch"]]
    if previous and previous != new:
        raise ValueError("batch name collision or incomplete published batch")
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    writer.writerows(existing if previous else [*existing, *new])
    protected = {p.resolve() for p in directory.iterdir() if p.is_file()}
    if csv_path.resolve() in protected:
        raise ValueError("do not overwrite raw evidence")
    if not dry_run:
        replace(csv_path, stream.getvalue())
    return manifest, table


def publish(directory, csv_path=None, markdown_path=None):
    manifest, table, results = validate_batch(directory)
    entry = ROOT / manifest["entry"]
    destination = entry.parent / ".data/results"
    csv_path = Path(csv_path or destination / (entry.stem + ".semspec.csv"))
    custom_markdown = markdown_path is not None
    markdown_path = Path(markdown_path or destination / (entry.stem + ".semspec.md"))
    json_path = (
        markdown_path.with_suffix(".render.json")
        if custom_markdown
        else entry.with_suffix(".render.json")
    )
    if len({csv_path.resolve(), markdown_path.resolve(), json_path.resolve()}) != 3 or any(
        p.resolve().is_relative_to(Path(directory).resolve())
        for p in (csv_path, markdown_path, json_path)
    ):
        raise ValueError("result paths must be distinct and outside raw evidence")
    if any(p.exists() and not p.is_file() for p in (csv_path, markdown_path, json_path)):
        raise ValueError("result destinations must be regular files")
    # CSV validation completes before any public table is changed.
    _publish_csv(directory, csv_path, manifest, table, results)
    save(json_path, display_table(table))
    replace(
        markdown_path,
        f'# {manifest["benchmark_id"]}: {"Ready / Exit" if manifest["metric_scope"] == READY_EXIT_SCOPE else "complete commands"}\n\n{manifest["role"]}; N={manifest["runs"]}, warmup={manifest["warmup"]}; separate conditions.\n\nBatch: `{manifest["batch"]}`.\n\n{{{{ table("{os.path.relpath(json_path, markdown_path.parent)}") }}}}\n',
    )
    return json_path


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("directory", type=Path)
    p.add_argument("--csv", type=Path)
    p.add_argument("--markdown", type=Path)
    p.add_argument("--startup-docs", type=Path)
    args = p.parse_args()
    try:
        if args.startup_docs:
            sys.path.insert(0, str(ROOT / "startup"))
            from publish import publish_startup

            publish_startup(args.directory, args.startup_docs)
        else:
            print(publish(args.directory, args.csv, args.markdown))
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        p.exit(1, f"{error}\n")
