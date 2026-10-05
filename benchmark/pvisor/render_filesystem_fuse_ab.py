#!/usr/bin/env python3
"""Publish verified host FUSE/no-stage comparisons as TSV evidence."""

import argparse
import csv
import json
import shutil
from pathlib import Path

from evidence_tsv import load, write
from filesystem_stage_ab import WORKLOADS, summarize
from reference_baselines import digest, validate_bundle_execution, validate_passthrough_output
from render_reference_baselines import export_evidence


def publish(source, output):
    report = load(source / "report.json")
    backends = ("native", "pvisor-host", "pvisor-fuse", "pvisor-staged")
    samples = report["arguments"]["samples"]
    if report.get("failure") or any(
        report["preflights"].get(backend, {}).get("state") != "passed"
        for backend in backends
    ):
        raise ValueError("incomplete or failed preflight")
    if summarize(report["rows"], backends, samples) != report["summary"]:
        raise ValueError("summary differs from measured rows")
    for name, expected in report["binary_sha256"].items():
        if digest(source / "bin" / name) != expected:
            raise ValueError(f"binary changed: {name}")
    harness = Path(__file__).parent
    for name, expected in report["harness_sha256"].items():
        if digest(harness / name) != expected:
            raise ValueError(f"harness changed: {name}")
    for row in report["rows"]:
        trial = source / "trials" / Path(row["logs"]).name
        command = json.loads((trial / "command.json").read_text())
        if row["correctness"] != "passed" or command["exit"] != 0:
            raise ValueError("failed workload cannot enter the distribution")
        if row["backend"] != "native":
            bundles = list(trial.rglob("run-bundle.json"))
            if len(bundles) != 1:
                raise ValueError("missing or ambiguous Run Bundle")
            validate_bundle_execution(
                json.loads(bundles[0].read_text()), row["backend"],
                "rootless_process", "rootless_process",
            )
        if row["backend"] == "pvisor-fuse":
            if "--stage" in command["argv"]:
                raise ValueError("passthrough control enables staging")
            stats = validate_passthrough_output((trial / "stdout.log").read_text())
            if stats != row["fuse_requests"]:
                raise ValueError("FUSE counters differ from raw logs")
    provenance = load(source / "build-provenance.json")
    output.mkdir(parents=True, exist_ok=False)
    write(output / "report.tsv", report)
    write(output / "build-provenance.tsv", provenance)
    shutil.copy2(source / "summary.tsv", output / "summary.tsv")
    with (output / "samples.tsv").open("w", newline="") as stream:
        writer = csv.writer(stream, delimiter="\t", lineterminator="\n")
        writer.writerow(("backend", "trial", "operation", "worker_ms", "correctness"))
        for row in report["rows"]:
            for op in WORKLOADS:
                writer.writerow((row["backend"], row["trial"], op,
                                 row["result"]["filesystem"][op]["worker_ms"],
                                 row["correctness"]))
    frozen = source / "harness"
    frozen.mkdir(exist_ok=True)
    for name in report["harness_sha256"]:
        shutil.copy2(harness / name, frozen / name)
    export_evidence(source, output)
    write(output / "manifest.tsv", {
        "source_report_sha256": digest(source / "report.json"),
        "verification": "summary recomputed; binary/harness hashes matched; "
                        "all Run Bundles checked; FUSE mounts and full read/write counters checked",
        "files": {p.name: digest(p) for p in sorted(output.iterdir())},
    })
    print(f"Published {len(report['rows'])} verified samples to {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    public = args.output.resolve()
    public.mkdir(parents=True, exist_ok=False)
    publish(args.source.resolve(), public / ".data")
    from publication import write_derived_summary
    from evidence_tsv import load
    write_derived_summary(public / "summary.csv", load(public / ".data/report.tsv")["summary"])


if __name__ == "__main__":
    main()
