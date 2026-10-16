#!/usr/bin/env python3
"""Run a registered table and print its rendered result; retain raw data on request."""

import argparse
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

from common import ROOT, registered, save
from update import display_table, validate_batch


def resolve_entry(value):
    """Accept a registered relative path or an unambiguous short entry name."""
    candidate = Path(value)
    if candidate.suffix != ".json":
        candidate = Path(str(candidate) + ".json")
    paths = [candidate, ROOT / candidate]
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
    table = json.loads(Path(template).read_text())
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
    table = json.loads(template.read_text())
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
    try:
        if topic == "startup":
            if ".data" not in evidence_root.parts:
                raise ValueError("raw output must live under an ignored .data directory")
            evidence_root.mkdir(parents=True, exist_ok=False)
        result = {}
        names = list(table) if topic == "startup" else [None]
        for index, name in enumerate(names):
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
            result.update(resolved)
        previous.update(display_table(result))
        save(rendered, {name: previous[name] for name in row_names if name in previous})
        subprocess.run(
            [str(semspec), "render", str(template.with_suffix(".md")), "--stdin"],
            input='{{ table("' + rendered.name + '") }}\n',
            text=True,
            check=True,
        )
        return rendered
    finally:
        if temporary:
            temporary.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tables", nargs="*", default=["smoke"])
    parser.add_argument("--row")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--kernel-receipt", type=Path)
    parser.add_argument("--cpu-affinity")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
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
        semspec = os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec")
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
