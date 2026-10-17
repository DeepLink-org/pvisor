#!/usr/bin/env python3
"""Publish audited paired startup timing tables and CSV."""

import argparse
import csv
import importlib.util
import json
import sys
from pathlib import Path

from subjects import LABELS, NAMES

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("benchmark_common", ROOT / "common.py")
COMMON = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(COMMON)


def publish_startup(directory, source):
    # Explicit module loading avoids the startup/common.py import name collision.
    spec = importlib.util.spec_from_file_location("benchmark_update", ROOT / "update.py")
    module = importlib.util.module_from_spec(spec)
    sys.path.insert(0, str(ROOT))
    spec.loader.exec_module(module)
    from public_assets import csv_path

    source = Path(source)
    metadata = json.loads((Path(directory) / "manifest.json").read_text())
    scope = metadata["metric_scope"]
    if scope != COMMON.READY_EXIT_SCOPE:
        raise ValueError("startup publication requires paired Ready / Exit data")
    stem = "startup-timing"
    filename = stem + ".csv"
    csv_target = csv_path(source.parent.parent, filename)
    validated = module.validate_batch(directory)
    module._publish_csv(directory, csv_target, *validated, dry_run=True)
    if csv_target.exists():
        with csv_target.open() as stream:
            if any(row["metric_scope"] != scope for row in csv.DictReader(stream)):
                raise ValueError("mixed Ready metric scopes")
    provenance_path = source.parent / "overrides/assets/benchmarks" / (stem + ".provenance.json")
    previous = json.loads(provenance_path.read_text()) if provenance_path.exists() else {}
    # Validate the new configuration before writing CSVs.
    current_configuration = configuration(
        directory, {"batch": metadata["batch"], "condition": metadata["conditions"][0]}, {}
    )
    module._publish_csv(directory, csv_target, *validated)
    with csv_target.open() as stream:
        latest = {row["condition"]: row for row in csv.DictReader(stream)}
    if any(r["metric_scope"] != scope for r in latest.values()):
        raise ValueError("mixed Ready metric scopes")
    for locale in ("zh", "en"):
        path = source / locale / "benchmarks" / filename
        from timing import FIELDS

        fields = FIELDS
        translations = {
            "median": "中位数",
            "mean": "均值",
            "SD": "标准差",
            "min": "最小",
            "max": "最大",
        }
        labels = [
            "N",
            *[
                key.replace(key.split()[1], translations[key.split()[1]]) if locale == "zh" else key
                for key in fields[1:]
            ],
        ]
        table = {}
        for name in NAMES:
            row = latest.get(name)
            values = (
                [int(row["runs"]), *[f"{float(row[key]):.2f}" for key in fields[1:]]]
                if row
                else [None] * len(fields)
            )
            table[LABELS[name]] = dict(zip(labels, values))
        for metric in ("Ready", "Exit"):
            projection = {
                name: {k: v for k, v in columns.items() if k == "N" or k.startswith(metric + " ")}
                for name, columns in table.items()
            }
            COMMON.save(path.with_name(stem + "." + metric.lower() + ".render.json"), projection)
    provenance = {
        LABELS[name]: dict(
            batch=row["batch"],
            metric_scope=row["metric_scope"],
            measurement_engine="python-perf_counter_ns",
            evidence_sha256=row["evidence_sha256"],
            manifest_sha256=row["manifest_sha256"],
            configuration=(
                current_configuration
                if row["batch"] == metadata["batch"]
                else configuration(directory, row, previous.get(LABELS[name], {}))
            ),
        )
        for name, row in latest.items()
    }
    COMMON.save(provenance_path, provenance)
    if source.resolve() == (ROOT.parent / "docs/src").resolve():
        from measurement_coverage import coverage

        coverage()


def configuration(directory, row, previous):
    """Retain the configuration of each independent latest successful cohort."""
    root = Path(directory)
    if root.name != row["batch"]:
        return previous.get("configuration") if previous.get("batch") == row["batch"] else None
    protocol = json.loads((root / "protocol.json").read_text())
    manifest = json.loads((root / "manifest.json").read_text())
    guest = row["condition"] in ("pvisor-vm", "fc-system", "qemu", "qemu-microvm")
    return compact_configuration(
        {
            "cpu_affinity": manifest["cpu_affinity"],
            "guest_cpus": protocol["guest_cpus"] if guest else None,
            "guest_memory_mib": protocol["guest_memory_mib"] if guest else None,
            "device_transport": protocol.get("device_transport"),
            "kernel": protocol["kernel"],
            "cache": protocol["cache"],
            "source_commit_scope": manifest["source_commit_scope"],
            "artifacts": manifest["artifacts"],
            "publisher_sha256": COMMON.digest(Path(__file__)),
        }
    )


def compact_configuration(config):
    """Publish configuration facts and hashes; full receipts remain in the batch."""
    result = {key: value for key, value in config.items() if value is not None}
    result["artifacts"] = COMMON.artifact_summary(config.get("artifacts", {}))
    kernel = config.get("kernel")
    if kernel and "record" in kernel:
        record = kernel["record"]
        result["kernel"] = {
            "receipt_sha256": kernel["receipt_sha256"],
            "variant": record["variant"],
            "provenance": {
                key: value
                for key, value in record["provenance"].items()
                if key not in ("rpm_info", "rpm_file_digest_checks")
            },
            "artifacts": {name: item["sha256"] for name, item in record["artifacts"].items()},
        }
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("directory", type=Path)
    p.add_argument("--docs-source", type=Path, default=ROOT.parent / "docs/src")
    args = p.parse_args()
    try:
        publish_startup(args.directory, args.docs_source)
    except (ValueError, KeyError, OSError) as error:
        p.exit(1, f"{error}\n")
