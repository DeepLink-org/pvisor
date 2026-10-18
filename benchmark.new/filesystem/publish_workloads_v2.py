#!/usr/bin/env python3
"""Publish a verified, completed N3 B-FS-ENG V2 guest-writeback analysis.

This postprocessor publishes only SVGs; CSV/provenance are archived in the batch.
It never starts a subject and never edits Markdown or registries.
Full-report publication requires 19 complete cases; incomplete cases can still be
analyzed locally. Use --destination-root /tmp/NEW for a reviewable staging tree.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import shlex
import sys
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import workloads_v2_analysis as ANALYSIS  # noqa: E402 - import after module-loading setup
from publication_archive import archive_targets, preflight_archive, write_archive  # noqa: E402 - import after module-loading setup

CASES = (
    [f"M{i:02}" for i in range(1, 13)] + [f"R{i:02}" for i in range(1, 5)] + ["S01", "S02", "S03"]
)
STEMS = (
    "mechanism-normalized-time",
    "resource-throughput",
    "resource-environment-cost",
    "workflow-times",
)
FIGURES = [f"{stem}.{extension}" for stem in STEMS for extension in ("svg", "pdf")]
TABLES = ("comparisons", "levels", "environments")
PREFIX = "filesystem-workloads-v2"


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load(path):
    return json.loads(Path(path).read_text())


def inside(root, relative):
    path = Path(relative)
    require(
        not path.is_absolute() and ".." not in path.parts and str(path) not in ("", "."),
        "unsafe relative evidence path",
    )
    result = (root / path).resolve()
    require(result.is_relative_to(root), "evidence path escapes batch")
    return result


def csv_bytes(rows):
    fields = list(dict.fromkeys(key for row in rows for key in row)) or ["no_records"]
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                key: json.dumps(value, separators=(",", ":"))
                if isinstance(value, (list, dict))
                else value
                for key, value in row.items()
            }
        )
    return stream.getvalue().encode()


def tagged_csv(rows, batch, protocol_sha, records_sha):
    return csv_bytes(
        [
            dict(batch=batch, protocol_sha256=protocol_sha, records_sha256=records_sha, **row)
            for row in rows
        ]
    )


def entrypoint(batch, status):
    template = load(batch / "in.json")
    require(set(template) == {"filesystem-v2-shallow"}, "unexpected semspec template row")
    expression = template["filesystem-v2-shallow"]
    require(
        isinstance(expression, str) and expression.startswith("$ "), "missing executed row command"
    )
    command = expression[2:]
    argv = shlex.split(command)
    require(
        len(argv) == 4
        and Path(argv[1]).resolve() == batch / "harness/workloads_v2_run.py"
        and argv[2] == "--measure"
        and Path(argv[3]).resolve() == batch,
        "semspec command does not invoke this frozen batch",
    )
    bench = load(batch / "in.bench.json")
    rendered = load(batch / "in.render.json")
    expected = dict(trials=status["trials"], failed=0)
    require(
        bench.get("format") == 1
        and len(bench.get("commands", [])) == 1
        and len(bench.get("rows", [])) == 1,
        "actual semspec execution missing",
    )
    executed, row = bench["commands"][0], bench["rows"][0]
    require(
        Path(bench.get("template", "")).resolve() == batch / "in.json"
        and Path(bench.get("cwd", "")).resolve() == batch
        and executed.get("command") == row.get("command") == command
        and executed.get("kind") == "script-row"
        and executed.get("raw") == executed.get("value") == expected
        and row.get("row") == "filesystem-v2-shallow"
        and rendered == {"filesystem-v2-shallow": expected},
        "semspec command/results do not match complete records",
    )
    log = Path(executed["log"]).resolve()
    require(
        log.parent == batch / "in.bench.logs" and load(log) == expected,
        "actual semspec command log missing or changed",
    )


def validate(batch, directory):
    """Bind completed derived values to actual execution and frozen inputs."""
    protocol = load(batch / "protocol.json")
    records = load(batch / "records.json")
    status = load(batch / "status.json")
    summary = load(directory / "summary.json")
    require(
        protocol.get("benchmark_id") == "B-FS-ENG"
        and protocol.get("role") == "engineering A/B"
        and protocol.get("comparison") == "guest-writeback"
        and protocol.get("diagnostic") is False
        and protocol.get("samples") == 3
        and protocol.get("resident_rounds") == 3,
        "publisher supports formal N3 guest-writeback timing only",
    )
    require(
        protocol.get("inventory", {}).get("selected_cell_ids") == CASES
        and [case["id"] for case in protocol.get("catalog", [])] == CASES,
        "full report requires the fixed 19-case catalog",
    )
    require(
        status == dict(trials=114, failed=0, excluded=0, complete=True, measurements=342),
        "full publication requires 114 valid environments and 342 rows without exclusions",
    )
    require(isinstance(records, list) and len(records) == 114, "incomplete environment inventory")
    require(
        protocol.get("budget")
        == dict(
            cpus=[0, 1],
            quota="200000 100000",
            memory_bytes=2 << 30,
            swap_bytes=0,
            guest_mib=512,
            guest_vcpus=2,
        ),
        "unexpected runtime resource profile",
    )
    conditions = protocol.get("conditions", {})
    require(set(conditions) == {"baseline", "candidate"}, "unexpected treatment arms")
    baseline, candidate = [conditions[arm]["env"] for arm in ("baseline", "candidate")]
    require(
        baseline.keys() == candidate.keys()
        and {key for key in baseline if baseline[key] != candidate[key]}
        == {"PVISOR_VM_FS_WRITEBACK"}
        and baseline["PVISOR_VM_FS_WRITEBACK"] == "0"
        and candidate["PVISOR_VM_FS_WRITEBACK"] == "1",
        "treatment must change only guest WB",
    )
    for env in (baseline, candidate):
        require(
            all(
                env.get(key) == "0"
                for key in (
                    "PVISOR_VM_FS_HOST_WRITEBACK",
                    "PVISOR_VM_FS_LOWER_DATA_CACHE",
                    "PVISOR_FS_PROFILE",
                )
            ),
            "formal arm unexpectedly enables host WB, DATA or profiling",
        )
    artifacts = protocol.get("artifacts", {})
    require(
        all(
            name in artifacts
            for name in (
                "bin/subject",
                "bin/payload",
                "bin/libkrunfw.so",
                "harness/workloads_v2_run.py",
                "harness/workloads_v2_guest.rs",
                "harness/workloads_v2_scenarios.rs",
                "harness/workloads_v2_driver.rs",
                "harness/workloads_v2_analysis.py",
                "firmware-source/SOURCE.json",
            )
        ),
        "frozen artifact inventory incomplete",
    )
    for relative, expected in artifacts.items():
        require(sha(inside(batch, relative)) == expected, "frozen artifact changed: " + relative)
    manifest = batch / "fixture-manifest.json"
    require(sha(manifest) == protocol["fixture_manifest_sha256"], "fixture manifest changed")
    for relative, expected in load(manifest).items():
        require(
            sha(inside(batch / "fixture-layer", relative)) == expected,
            "frozen fixture changed: " + relative,
        )
    require(
        load(batch / "firmware-source/SOURCE.json")["firmware_sha256"]
        == sha(batch / "bin/libkrunfw.so"),
        "firmware does not match its SOURCE receipt",
    )
    require(
        sha(HERE / "workloads_v2_analysis.py") == artifacts["harness/workloads_v2_analysis.py"],
        "available analyzer differs from frozen analyzer",
    )
    provenance = dict(
        protocol_sha256=sha(batch / "protocol.json"),
        records_sha256=sha(batch / "records.json"),
        analyzer_sha256=artifacts["harness/workloads_v2_analysis.py"],
    )
    require(summary.get("provenance") == provenance, "analysis is not bound to these raw inputs")
    fresh = ANALYSIS.summarize(protocol, records)
    require(
        all(summary.get(key) == value for key, value in fresh.items()),
        "analysis values differ from recomputation of actual records",
    )
    require(
        summary.get("status") == "complete_exploratory"
        and summary.get("failures") == []
        and summary.get("complete_case_ids") == CASES
        and summary.get("counts")
        == dict(
            planned_cases=19,
            complete_cases=19,
            expected_environments=114,
            observed_environments=114,
            expected_round_rows=342,
            observed_round_rows=342,
            defects=0,
        ),
        "analysis completeness differs",
    )
    expected_order = [
        (order["case_id"], condition, order["pair"])
        for order in protocol["orders"]
        for condition in order["conditions"]
    ]
    require(
        [(r["case_id"], r["condition"], r["pair"]) for r in records] == expected_order,
        "observed environments differ from predeclared randomized schedule",
    )
    for record in records:
        require(
            record.get("valid") is True
            and record.get("host_verified") is True
            and record.get("returncode") == 0
            and record.get("timed_out") is False,
            "failed or unverified environment",
        )
        folder = batch / "trials" / f"{record['case_id']}-{record['pair']:03}-{record['condition']}"
        require(
            load(folder / "result.json") == record, "raw trial record differs from campaign records"
        )
        stdout = (folder / "stdout.log").read_text()
        observed_rows = [
            json.loads(line.split(" ", 1)[1])
            for line in stdout.splitlines()
            if line.startswith("WORKLOADS_V2_RESULT ")
        ]
        require(
            observed_rows == record["rows"]
            and stdout.count("WORKLOADS_V2_READY ") == 1
            and stdout.count("WORKLOADS_V2_DONE ") == 1,
            "actual guest output does not match accepted round rows",
        )
        require(
            all(
                row["completed_ns"] == row["work_ns"] + row["final_sync_ns"]
                for row in record["rows"]
            ),
            "task clocks do not partition work and synchronization",
        )
        verified = [
            json.loads(line.split(" ", 1)[1])
            for line in (folder / "host-verification.log").read_text().splitlines()
            if line.startswith("WORKLOADS_V2_VERIFIED ")
        ]
        require(
            len(verified) == 1
            and verified[0].get("verified") is True
            and record.get("host_verification") == verified[0],
            "actual host final-state verification missing",
        )
    entrypoint(batch, status)
    for table in TABLES:
        require(
            (directory / f"{table}.csv").read_bytes() == csv_bytes(fresh[table]),
            "analysis CSV projection changed: " + table,
        )
    require(summary.get("plots") == FIGURES, "complete SVG/PDF figure inventory required")
    for name in FIGURES:
        payload = (directory / name).read_bytes()
        require(
            payload.startswith(b"%PDF-") if name.endswith(".pdf") else b"<svg" in payload[:3000],
            "invalid derived figure: " + name,
        )
    return protocol, summary, provenance


def publish(batch, directory=None, destination_root=REPO, *, check=False):
    batch = Path(batch).resolve()
    directory = Path(directory).resolve() if directory else batch / "analysis"
    destination_root = Path(destination_root).resolve()
    require(
        ".data" in batch.parts and ".data" in directory.parts,
        "raw batch and analysis must remain under .data",
    )
    protocol, summary, bound = validate(batch, directory)
    outputs, archived = {}, {}
    for table in TABLES:
        data = tagged_csv(
            summary[table], batch.name, bound["protocol_sha256"], bound["records_sha256"]
        )
        archived[f"{PREFIX}-{table}.csv"] = data
    asset_directory = "docs/overrides/assets/benchmarks/" + PREFIX
    for name in FIGURES:
        if name.endswith(".svg"):
            outputs[asset_directory + "/" + name] = (directory / name).read_bytes()
        archived[name] = (directory / name).read_bytes()
    provenance = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="engineering A/B",
        batch=batch.name,
        comparison="guest writeback off/on; same frozen host, firmware and workload",
        scope="SDK VM mechanisms, resource anchors and three separate local workflows; no aggregate score or IOCP offload claim",
        source_head=protocol.get("source_head"),
        counts=summary["counts"],
        budget=protocol["budget"],
        sampling=summary["statistical_scope"],
        timing=protocol["timing"],
        cache=protocol["cache"],
        conditions=protocol["conditions"],
        qualification="Mutable prepared inputs; host writeback, DATA and DAX off; no Immutable lower-cache claim",
        validation="guest per-round oracle and normal exit; independent host final retained-state oracle; validated physical overlay markers excluded",
        limitations=protocol["limitations"]
        + [
            "Resource rounds 2/3 follow previous-round full readback verification.",
            "Ready precedes untimed setup, including the initial C fixture build.",
            "CPU and sampled memory.current cover the whole environment including setup, validation and exit.",
            "N3 pointwise intervals are exploratory, uncorrected, and cannot establish stable confirmation or operation tail latency.",
        ],
        inputs={
            name: sha(batch / name)
            for name in (
                "protocol.json",
                "records.json",
                "status.json",
                "in.json",
                "in.bench.json",
                "in.render.json",
                "fixture-manifest.json",
                "firmware-source/SOURCE.json",
            )
        },
        analysis=dict(summary_sha256=sha(directory / "summary.json"), **bound),
        artifacts={
            name: protocol["artifacts"][name]
            for name in (
                "bin/subject",
                "bin/payload",
                "bin/libkrunfw.so",
                "harness/workloads_v2_run.py",
                "harness/workloads_v2_driver.rs",
                "harness/workloads_v2_guest.rs",
                "harness/workloads_v2_scenarios.rs",
                "harness/workloads_v2_analysis.py",
            )
        },
        product_source_manifest_sha256=hashlib.sha256(
            json.dumps(protocol["source_hashes"], sort_keys=True).encode()
        ).hexdigest(),
        publisher_sha256=sha(__file__),
        archive_outputs={name: hashlib.sha256(data).hexdigest() for name, data in archived.items()},
        outputs={name: hashlib.sha256(data).hexdigest() for name, data in outputs.items()},
    )
    encoded = (json.dumps(provenance, indent=2, sort_keys=True) + "\n").encode()
    archives = archive_targets(batch, PREFIX, archived, encoded)
    preflight_archive(archives)
    for relative in outputs:
        path = destination_root / relative
        require(
            not path.is_symlink() and not any(parent.is_symlink() for parent in path.parents),
            "publication destination traverses a symlink",
        )
        require(not path.exists(), "refusing to overwrite published result: " + str(path))
    if not check:
        write_archive(archives)
        for relative, data in outputs.items():
            path = destination_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as stream:
                stream.write(data)
    return dict(
        status="checked" if check else "published",
        batch=batch.name,
        destination_root=str(destination_root),
        files=len(outputs),
        archive_directory=str(batch / "publication" / PREFIX),
        archive_files=len(archives),
        outputs={name: hashlib.sha256(data).hexdigest() for name, data in outputs.items()},
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--batch", required=True, type=Path)
    parser.add_argument("--analysis", type=Path)
    parser.add_argument("--destination-root", type=Path, default=REPO)
    parser.add_argument("--check", action="store_true", help="validate without writing any output")
    args = parser.parse_args(argv)
    try:
        print(
            json.dumps(publish(args.batch, args.analysis, args.destination_root, check=args.check))
        )
    except (ValueError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
