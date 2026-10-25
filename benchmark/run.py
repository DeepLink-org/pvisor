#!/usr/bin/env python3
"""Run a registered table and print its rendered result; retain raw data on request."""

import argparse
import importlib.util
import json
import os
import shlex
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

from common import ROOT, digest, registered, save, table_template
from update import display_table, validate_batch


def resolve_entry(value):
    """Accept a registered relative path or an unambiguous short entry name."""
    candidate = Path(value)
    candidates = (
        [candidate]
        if candidate.suffix in (".json", ".md")
        else [Path(str(candidate) + suffix) for suffix in (".json", ".md")]
    )
    paths = []
    for candidate in candidates:
        paths.extend([candidate, ROOT / candidate])
        if len(candidate.parts) == 1:
            paths.extend(ROOT.glob("*/" + candidate.name))
    matches = {path.resolve() for path in paths if path.is_file()}
    if len(matches) != 1:
        raise ValueError("unknown or ambiguous benchmark table: " + value)
    template = matches.pop()
    registered(template)
    return template


def load_prepare(topic):
    path = ROOT / topic / ("common.py" if topic == "startup" else "prepare.py")
    sys.path.insert(0, str(path.parent))
    spec = importlib.util.spec_from_file_location("prepare_" + topic, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def resolve_row(template, value):
    """Match a row exactly, or by case and whitespace when unambiguous."""
    table = table_template(template)
    if value in table:
        return value
    normalized = " ".join(value.casefold().split())
    matches = [name for name in table if " ".join(name.casefold().split()) == normalized]
    if len(matches) == 1:
        return matches[0]
    raise ValueError(f"unknown or ambiguous table row: {value}; available: {', '.join(table)}")


def resolve_selection(values, row=None):
    """Keep multiple table arguments, with an optional second positional row."""
    values = values or ["smoke"]
    first = resolve_entry(values[0])
    if len(values) == 2 and row is None:
        try:
            second = resolve_entry(values[1])
        except ValueError:
            return [first], resolve_row(first, values[1])
        return [first, second], None
    templates = [first, *(resolve_entry(value) for value in values[1:])]
    if row is not None:
        if len(templates) != 1:
            raise ValueError("row selection requires exactly one table")
        row = resolve_row(first, row)
    return templates, row


def run_table(
    template,
    semspec,
    output=None,
    binary=None,
    row=None,
    cpu_affinity=None,
    assets=None,
    receipt=None,
):
    template = Path(template).resolve()
    metadata = registered(template)
    topic = template.parent.name
    table = table_template(template)
    row_names = list(table)
    if row is not None:
        row = resolve_row(template, row)
        table = {row: table[row]}
    rendered = template.with_suffix(".render.json")
    previous = json.loads(rendered.read_text()) if row is not None and rendered.exists() else {}
    if not isinstance(previous, dict):
        raise ValueError("existing rendered table must be a JSON object")
    if metadata["status"] == "requires-single-condition-command":
        raise ValueError("table has no prepared single-condition command")
    if metadata["status"] == "explicit-python-paired-dax":
        raise ValueError(
            "paired DAX needs dax_ab.py --prepare followed by semspec bench; see filesystem/README.md"
        )
    if row is not None and topic not in ("startup", "filesystem"):
        raise ValueError("--row is supported only for startup/filesystem tables")
    prepare = load_prepare(topic)
    binary = binary or ROOT.parent / "target/release" / (
        "pvisor-replay" if topic == "replay" else "pvisor"
    )
    # Temporary evidence is audited before removal. Explicit output retains it,
    # including on failure, for CI comparison or a later publication.
    temporary = tempfile.TemporaryDirectory(prefix="pvisor-benchmark-") if output is None else None
    evidence_root = Path(temporary.name) / ".data" if temporary else Path(output).resolve()
    token = uuid.uuid4().hex[:12]
    run_record = None
    try:
        if topic == "startup":
            if ".data" not in evidence_root.parts:
                raise ValueError("raw output must live under an ignored .data directory")
            evidence_root.mkdir(parents=True, exist_ok=False)
            run_record = dict(
                format=1,
                kind="startup-run",
                status="running",
                entry=metadata.get("entry", str(template)),
                specification_sha256=digest(template),
                selected_rows=list(table),
                batches=[
                    dict(row=name, directory=f"row-{index}-{token}", status="pending")
                    for index, name in enumerate(table)
                ],
            )
            save(evidence_root / "run.json", run_record)
            if temporary is None:
                print(f"Evidence: {evidence_root}", file=sys.stderr)
        result = {}
        names = list(table) if topic == "startup" else [None]
        for index, name in enumerate(names):
            if run_record is not None:
                run_record["batches"][index]["status"] = "running"
                save(evidence_root / "run.json", run_record)
            destination = (
                evidence_root / f"row-{index}-{token}" if topic == "startup" else evidence_root
            )
            if topic == "startup":
                batch = prepare.prepare_template(
                    template, destination, assets, binary, receipt, cpu_affinity, row=name
                )
            elif topic == "process":
                batch = prepare.prepare_template(template, binary, destination)
            elif topic == "replay":
                batch = prepare.prepare_batch(template, binary, destination)
            elif topic == "filesystem":
                batch = prepare.prepare(template, output=destination, row=row)
            else:
                raise ValueError("unsupported prepared benchmark topic: " + topic)
            manifest = json.loads((batch / "manifest.json").read_text())
            subprocess.run(
                [str(semspec), "bench", "--cwd", manifest["cwd"], str(batch / "in.json")],
                check=True,
                stdout=subprocess.DEVNULL,
            )
            _, resolved, _ = validate_batch(batch)
            if run_record is not None:
                if set(resolved) != {name}:
                    raise ValueError("audited startup row differs from the selected row")
                run_record["batches"][index].update(
                    status="passed", manifest_sha256=digest(batch / "manifest.json")
                )
                save(evidence_root / "run.json", run_record)
            result.update(resolved)
        if run_record is not None:
            if digest(template) != run_record["specification_sha256"]:
                raise ValueError("startup specification changed during the run")
            run_record["status"] = "passed"
            save(evidence_root / "run.json", run_record)
            if temporary is None:
                print(
                    "Audit: mise exec -- python3 benchmark/run.py --audit "
                    + shlex.quote(str(evidence_root)),
                    file=sys.stderr,
                )
        previous.update(display_table(result))
        save(rendered, {name: previous[name] for name in row_names if name in previous})
        subprocess.run(
            [str(semspec), "render", str(template.with_suffix(".md")), "--stdin"],
            input='{{ table("' + rendered.name + '") }}\n',
            text=True,
            check=True,
        )
        return rendered
    except BaseException as error:
        if run_record is not None and run_record["status"] != "passed":
            run_record.update(status="failed", error=str(error))
            for batch in run_record["batches"]:
                if batch["status"] == "running":
                    batch["status"] = "failed"
            save(evidence_root / "run.json", run_record)
        raise
    finally:
        if temporary:
            temporary.cleanup()


def validate_retained_batch(directory):
    """Use the recorded startup auditor version when rechecking saved evidence."""
    manifest = json.loads((directory / "manifest.json").read_text())
    if not isinstance(manifest, dict):
        raise ValueError("invalid batch manifest")
    if manifest.get("benchmark_id") == "B-STARTUP":
        if not isinstance(manifest.get("harness"), dict):
            raise ValueError("invalid startup harness inventory")
        sources = [
            ROOT / name
            for name in (
                "run.py",
                "update.py",
                "common.py",
                "startup/audit.py",
                "startup/hooks.py",
                "startup/timing.py",
                "startup/subjects.py",
                "support/reference_inputs.py",
                "support/firecracker_kernels.py",
            )
        ] + [ROOT.parent / "tools/semaspec/runtime.py"]
        for source in sources:
            name = source.parent.name + "-" + source.name
            if manifest.get("harness", {}).get(name) != digest(source):
                raise ValueError(
                    "audit code differs from retained harness: "
                    + name
                    + "; use the matching source checkout"
                )
    return validate_batch(directory)


def audit_retained(directory):
    """Recheck a retained batch or complete startup selection without measuring or publishing.

    Audit may refresh the batch's validation status and audit.json. It never runs
    subjects, prepares inputs, changes source projections or publishes documents.
    A startup selection must retain every selected row and its sealed manifest.
    """
    directory = Path(directory).resolve(strict=True)
    index = directory / "run.json"
    if not index.exists():
        manifest, table, _ = validate_retained_batch(directory)
        return [(directory, manifest, table)]
    record = json.loads(index.read_text())
    if not isinstance(record, dict):
        raise ValueError("invalid startup run index")
    rows = record.get("selected_rows")
    batches = record.get("batches")
    if (
        record.get("format") != 1
        or record.get("kind") != "startup-run"
        or record.get("status") != "passed"
        or not isinstance(rows, list)
        or not rows
        or any(not isinstance(row, str) for row in rows)
        or len(set(rows)) != len(rows)
        or not isinstance(batches, list)
        or any(not isinstance(batch, dict) for batch in batches)
        or [batch.get("row") for batch in batches] != rows
    ):
        raise ValueError("incomplete or invalid startup run")
    paths = []
    for batch in batches:
        name = batch.get("directory")
        if not isinstance(name, str) or not name or Path(name).name != name:
            raise ValueError("startup batch directory must be a direct child")
        path = (directory / name).resolve(strict=True)
        if path.parent != directory or path in paths or batch.get("status") != "passed":
            raise ValueError("incomplete or invalid startup batch")
        if digest(path / "manifest.json") != batch.get("manifest_sha256"):
            raise ValueError("startup batch manifest changed: " + name)
        paths.append(path)
    results = []
    for path, row in zip(paths, rows):
        manifest, table, _ = validate_retained_batch(path)
        if (
            manifest["benchmark_id"] != "B-STARTUP"
            or set(table) != {row}
            or manifest["entry"] != record["entry"]
            or manifest["template_sha256"] != record["specification_sha256"]
        ):
            raise ValueError("startup batch differs from its selected row")
        results.append((path, manifest, table))
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tables", nargs="*")
    parser.add_argument(
        "--audit", type=Path, help="recheck retained evidence without measuring or publishing"
    )
    parser.add_argument("--row")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--kernel-receipt", type=Path)
    parser.add_argument("--cpu-affinity")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.audit is not None:
            if args.tables or any(
                (
                    args.row,
                    args.binary,
                    args.assets,
                    args.kernel_receipt,
                    args.cpu_affinity,
                    args.output,
                    args.check,
                )
            ):
                raise ValueError("--audit accepts only a retained evidence directory")
            for directory, manifest, table in audit_retained(args.audit):
                print(
                    f"AUDITED {', '.join(table)}: N={manifest['runs']}, "
                    f"warmup={manifest['warmup']}; {directory}"
                )
            return
        templates, row = resolve_selection(args.tables, args.row)
        if row is not None and templates[0].parent.name not in ("startup", "filesystem"):
            raise ValueError("row selection is supported only for startup/filesystem tables")
        if args.check:
            print(
                "process"
                if args.binary is None
                and any(template.parent.name == "process" for template in templates)
                else "prepared"
            )
            return
        semspec = os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semaspec/debug/semaspec")
        original = os.sched_getaffinity(0) if hasattr(os, "sched_getaffinity") else None
        selected = args.cpu_affinity or (
            ",".join(map(str, sorted(original)[:2])) if original else None
        )
        try:
            if original and selected:
                os.sched_setaffinity(0, {int(cpu) for cpu in selected.split(",")})
            for template in templates:
                output = args.output
                if output is None and os.environ.get("PVISOR_BENCHMARK_OUTPUT"):
                    output = Path(os.environ["PVISOR_BENCHMARK_OUTPUT"]) / template.stem
                if output is not None and args.output is not None and len(templates) > 1:
                    output = output / template.parent.name / template.stem
                run_table(
                    template,
                    semspec,
                    output,
                    args.binary,
                    row,
                    selected,
                    args.assets,
                    args.kernel_receipt,
                )
        finally:
            if original:
                os.sched_setaffinity(0, original)
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
