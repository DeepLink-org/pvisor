#!/usr/bin/env python3
"""Publish completed B-FS-ENG guest-WB curves and separate B-FS-DIAG counts.

Only verified SVG bytes enter the public filesystem-guest-writeback namespace.
CSV and provenance stay in the input batch's publication archive. --check reads inputs and destinations
without creating directories, figures, caches or receipts. No subject is run.
"""

import argparse
import csv
import hashlib
import importlib.util
import io
import json
import re
import shlex
import statistics
import sys
from contextlib import contextmanager
from pathlib import Path

# Importing frozen helpers must not alter the retained measurement artifacts.
sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE.parent))
from publication_archive import archive_targets, preflight_archive, write_archive  # noqa: E402 - import after module-loading setup

ARMS = {"off", "guest-wb", "old-wb", "native"}
FAMILIES = {"block", "workers", "barrier", "pressure", "smallfiles"}
PREFIX = "filesystem-guest-writeback-"
ASSETS = Path("docs/overrides/assets/benchmarks/filesystem-guest-writeback")
PRIVATE = re.compile(rb"/(?:home|tmp|sys|proc|run|root|Users|private|var)(?:/|\\)")
PATCH48 = "patches/0048-fuse-pipeline-native-writeback-pressure.patch"


def require(value, message):
    if not value:
        raise ValueError(message)


def load(path):
    return json.loads(Path(path).read_text())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def digest(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    ).hexdigest()


def public(data, name):
    require(not PRIVATE.search(data), "private host path in public output: " + name)
    return data


def relative(name):
    path = Path(name)
    require(
        not path.is_absolute() and ".." not in path.parts and name != "",
        "unsafe relative artifact/output name",
    )
    return path


def csv_bytes(rows):
    require(rows, "empty derived table")
    stream = io.StringIO(newline="")
    fields = list(dict.fromkeys(key for row in rows for key in row))
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                key: json.dumps(value, sort_keys=True) if isinstance(value, (dict, list)) else value
                for key, value in row.items()
            }
        )
    return stream.getvalue().encode()


def projection(path, expected):
    require(
        Path(path).read_bytes() == csv_bytes(expected),
        "derived CSV is not the exact full-precision projection: " + Path(path).name,
    )


def marker_json(path, prefix):
    return [
        json.loads(line[len(prefix) :])
        for line in Path(path).read_text().splitlines()
        if line.startswith(prefix)
    ]


@contextmanager
def frozen(batch):
    """Isolate same-named imports when validating two independent cohorts."""
    names = ("curves", "curves_analysis", "curves_diagnostics", "guest_writeback_curves")
    old_modules = {name: sys.modules.pop(name) for name in names if name in sys.modules}
    old_path = sys.path[:]
    try:
        sys.path.insert(0, str(batch / "harness"))
        path = batch / "harness/guest_writeback_curves.py"
        spec = importlib.util.spec_from_file_location("guest_writeback_curves", path)
        wrapper = importlib.util.module_from_spec(spec)
        sys.modules["guest_writeback_curves"] = wrapper
        spec.loader.exec_module(wrapper)
        import curves_analysis as analysis

        require(
            Path(analysis.__file__).resolve() == batch / "harness/curves_analysis.py",
            "analysis helper escaped frozen cohort",
        )
        yield wrapper, analysis
    finally:
        for name in names:
            sys.modules.pop(name, None)
        sys.modules.update(old_modules)
        sys.path[:] = old_path


def budget(value):
    require(
        value.get("affinity") == [0, 1]
        and value.get("cpu.max") == "200000 100000"
        and value.get("memory.max") == str(2 << 30)
        and value.get("memory.swap.max") == "0",
        "actual cgroup budget differs",
    )


def semspec(batch, expected, label="guest-writeback-curves"):
    raw, rendered = load(batch / "in.bench.json"), load(batch / "in.render.json")
    require(
        raw.get("format") == 1
        and len(raw.get("commands", [])) == 1
        and len(raw.get("rows", [])) == 1,
        "one completed semspec row required",
    )
    command = raw["commands"][0]
    require(
        command.get("kind") == "script-row"
        and command.get("raw") == expected
        and command.get("value") == expected
        and rendered == {raw["rows"][0]["row"]: expected},
        "semspec checked row differs from retained campaign result",
    )
    require(raw["rows"][0]["command"] == command["command"], "semspec command differs")
    template = load(batch / "in.json")
    require(
        raw["rows"][0]["row"] == label and template == {label: "$ " + command["command"]},
        "semspec template changed after execution",
    )
    argv = shlex.split(command["command"])
    script = (
        "guest_writeback_curves.py"
        if label == "guest-writeback-curves"
        else "guest_writeback_pipeline.py"
    )
    require(
        len(argv) == 4
        and Path(argv[1]).resolve() == batch / "harness" / script
        and argv[2] == "--measure"
        and Path(argv[3]).resolve() == batch,
        "registered semspec command does not invoke these frozen measurement inputs",
    )
    log = Path(command["log"])
    require(
        log.resolve().parent == batch / "in.bench.logs" and load(log) == expected,
        "semspec single JSON stdout receipt differs",
    )


def firmware(batch, protocol):
    new = load(batch / "firmware-source/SOURCE.json")
    old = load(batch / "firmware-source/OLD-SOURCE.json")
    require(
        new["firmware_sha256"] == sha(batch / "bin/libkrunfw.so")
        and old["firmware_sha256"] == sha(batch / "bin/libkrunfw-old.so"),
        "firmware SOURCE does not identify the executed bytes",
    )
    require(
        new["kernel_source_sha256"] == old["kernel_source_sha256"]
        and new["actual_config_sha256"] == old["actual_config_sha256"],
        "old/new kernel source or actual config differs",
    )
    a = {key: value for key, value in old["inputs"].items() if key.startswith("patches/")}
    b = {key: value for key, value in new["inputs"].items() if key.startswith("patches/")}
    require(
        PATCH48 not in a
        and b.keys() - a.keys() == {PATCH48}
        and all(b.get(key) == value for key, value in a.items()),
        "firmware contrast must add only 0048 and preserve baseline patches",
    )
    require(
        "patches/0047-fuse-isolate-immutable-lower-data.patch" in a,
        "old firmware does not include 0047 baseline",
    )
    for name, expected in b.items():
        require(
            sha(batch / "firmware-source" / relative(name)) == expected,
            "retained firmware patch differs: " + name,
        )
    require(
        protocol["firmware_source"]["sha256"] == sha(batch / "firmware-source/SOURCE.json")
        and protocol["old_firmware_source"]["sha256"]
        == sha(batch / "firmware-source/OLD-SOURCE.json"),
        "protocol firmware receipt differs",
    )
    return dict(
        old_sha256=old["firmware_sha256"],
        new_sha256=new["firmware_sha256"],
        kernel_source_sha256=new["kernel_source_sha256"],
        actual_config_sha256=new["actual_config_sha256"],
        old_source_receipt_sha256=sha(batch / "firmware-source/OLD-SOURCE.json"),
        new_source_receipt_sha256=sha(batch / "firmware-source/SOURCE.json"),
        added_patch_sha256=b[PATCH48],
        baseline_patches=len(a),
    )


def audit_cohort(batch, diagnostic):
    batch = batch.resolve()
    protocol, records, status = (
        load(batch / name) for name in ("protocol.json", "records.json", "status.json")
    )
    n = 1 if diagnostic else 5
    require(
        protocol.get("suite") == "guest-writeback"
        and protocol.get("profiling") is diagnostic
        and protocol.get("preflight") is False
        and protocol.get("samples") == n
        and protocol.get("resident_rounds") == 3
        and set(protocol["conditions"]) == ARMS,
        "complete formal N5 or independent diagnostic N1 four-arm cohort required",
    )
    require(
        protocol.get("benchmark_id") == ("B-FS-DIAG" if diagnostic else "B-FS-ENG")
        and protocol.get("performance_samples") is (not diagnostic),
        "cohort role differs",
    )
    require(
        status == dict(valid=True, failed=0, excluded=0, trials=4 * n) and len(records) == 4 * n,
        "failed, excluded or incomplete environments",
    )
    require(
        len(protocol["cells"]) == 15 and {v["family"] for v in protocol["cells"]} == FAMILIES,
        "registered fifteen-cell/five-family inventory differs",
    )
    require(
        protocol["budget"]
        == dict(
            cpus=[0, 1], quota="200000 100000", memory_bytes=2 << 30, swap_bytes=0, guest_mib=512
        ),
        "frozen budget differs",
    )
    for name, expected in protocol["artifacts"].items():
        require(sha(batch / relative(name)) == expected, "changed artifact: " + name)
    source = protocol["source_hashes"]
    require(
        source
        and all(
            sha(batch / "source" / relative(name)) == expected for name, expected in source.items()
        ),
        "retained product source changed",
    )
    fw = firmware(batch, protocol)
    budget(load(batch / "budget-before.json"))
    budget(load(batch / "budget-after.json"))
    expected_order = [
        (order["pair"], condition)
        for order in protocol["orders"]
        for condition in order["conditions"]
    ]
    require(
        [(r["pair"], r["condition"]) for r in records] == expected_order
        and len(set(expected_order)) == 4 * n,
        "seeded environment order differs",
    )
    cells = {cell["id"]: cell for cell in protocol["cells"]}
    with frozen(batch) as (wrapper, analysis):
        require(wrapper.verify_protocol(batch) == protocol, "frozen protocol validation differs")
        for record in records:
            require(
                record.get("valid") is True
                and record.get("host_verified") is True
                and record.get("returncode") == 0,
                "guest/host correctness or normal exit missing",
            )
            condition, pair = record["condition"], record["pair"]
            folder = batch / "trials" / f"{pair:03}-{condition}"
            require(load(folder / "result.json") == record, "trial receipt differs from records")
            require(
                all(
                    record["environment"].get(k) == v
                    for k, v in protocol["conditions"][condition]["env"].items()
                ),
                "actual environment differs",
            )
            plan, stage = batch / f"plan-{pair:03}.tsv", folder / "stage"
            command = (
                [str(batch / "bin/payload"), str(plan), str(stage / "native"), "3"]
                if condition == "native"
                else [
                    str(batch / "bin/subject"),
                    protocol["rootfs"],
                    str(batch / "bin/libkrunfw.so"),
                    str(batch / "bin/payload"),
                    str(plan),
                    str(stage),
                    "3",
                ]
            )
            require(
                record["command"] == command,
                "actual payload/driver argv differs from frozen artifacts and plan",
            )
            require(
                record["environment"].get("PVISOR_FS_PROFILE") == ("1" if diagnostic else "0")
                and record["environment"].get("CURVES_PROFILE")
                == ("1" if diagnostic and condition != "native" else "0"),
                "actual instrumentation state differs",
            )
            budget(record["resource_before"])
            budget(record["resource"])
            rows = record["rows"]
            require(
                len(rows) == 45
                and {(r["id"], r["round"]) for r in rows}
                == {(key, round_) for key in cells for round_ in (1, 2, 3)},
                "cell/round inventory differs",
            )
            for row in rows:
                wrapper.validate_row(row)
                require(
                    row.get("correctness") is True
                    and row["bytes"]
                    == cells[row["id"]]["workers"] * cells[row["id"]]["bytes_per_worker"],
                    "row bytes/correctness differs",
                )
                require(
                    all(
                        row[key] == cells[row["id"]][key]
                        for key in (
                            "workload",
                            "block_bytes",
                            "workers",
                            "bytes_per_worker",
                            "sync_every_ops",
                        )
                    ),
                    "actual workload factors differ",
                )
            require(
                marker_json(folder / "stdout.log", "CURVES_RESULT ") == rows
                and marker_json(folder / "stdout.log", "CURVES_READY ")
                == [dict(schema=1, cells=15, rounds=3)],
                "retained stdout row/Ready receipts differ",
            )
            done = marker_json(folder / "stdout.log", "CURVES_DONE ")
            require(
                len(done) == 1
                and done[0].get("schema") == 1
                and done[0].get("verified") is True
                and done[0].get("rows") == 45,
                "unique final guest full-byte receipt missing",
            )
            host = marker_json(folder / "host-verification.log", "CURVES_VERIFIED ")
            require(
                len(host) == 1
                and host[0].get("verified") is True
                and host[0].get("bytes") == sum(row["bytes"] for row in rows),
                "independent host full-byte receipt differs",
            )
            if condition != "native":
                path = (
                    batch
                    / "bin"
                    / ("libkrunfw-old.so" if condition == "old-wb" else "libkrunfw.so")
                )
                selected = dict(
                    path=str(path), selection="condition_env" if condition == "old-wb" else "argv"
                )
                require(
                    marker_json(folder / "stderr.log", "CURVES_FIRMWARE_SELECTED ") == [selected]
                    and record.get("firmware_selection") == selected
                    and record.get("firmware_sha256") == sha(path),
                    "actual firmware selection differs",
                )
            if not diagnostic:
                require(
                    "pvisor-fs-profile" not in (folder / "stderr.log").read_text(),
                    "instrumented timings cannot enter performance curves",
                )
        points, _, failures, _ = analysis.collect(protocol, records)
        require(not failures and len(points) == 60 * n, "independent environment analysis differs")
    semspec(batch, dict(trials=4 * n, failed=0, cells=15, pairs=n))
    evidence_files = [
        "protocol.json",
        "records.json",
        "status.json",
        "in.json",
        "in.bench.json",
        "in.render.json",
        "budget-before.json",
        "budget-after.json",
    ]
    evidence_files += [
        name
        for name in (
            "build-reuse.json",
            "campaign-command.json",
            "campaign.stdout.log",
            "campaign.stderr.log",
            "launch.log",
            "prepare.log",
        )
        if (batch / name).is_file()
    ]
    evidence_files += [
        str(path.relative_to(batch))
        for path in sorted((batch / "in.bench.logs").glob("*"))
        if path.is_file()
    ]
    evidence_files += [
        str(path.relative_to(batch))
        for path in sorted((batch / "trials").rglob("*"))
        if path.is_file()
    ]
    raw = {name: sha(batch / name) for name in evidence_files}
    cpu_models = sorted(
        set(re.findall(r"^model name\s*:\s*(.+)$", protocol["host"]["cpuinfo"], re.M))
    )
    mounts = json.loads(protocol["host"]["filesystem"]).get("filesystems", [])
    provenance = dict(
        cohort=batch.name,
        benchmark_id=protocol["benchmark_id"],
        environments=len(records),
        independent_pairs=n,
        dependent_rounds=3,
        cells=15,
        raw_receipt_files=len(raw),
        raw_receipts_sha256=digest(raw),
        core_receipts={
            name: raw[name]
            for name in (
                "protocol.json",
                "records.json",
                "status.json",
                "in.json",
                "in.bench.json",
                "in.render.json",
            )
        },
        executed_artifacts={
            name: protocol["artifacts"][name]
            for name in ("bin/subject", "bin/payload", "bin/libkrunfw.so", "bin/libkrunfw-old.so")
        },
        artifact_files=len(protocol["artifacts"]),
        artifact_inventory_sha256=digest(protocol["artifacts"]),
        raw_inventory_scope="planned/executed campaign and semspec receipts plus all retained trial logs; full inventories remain local",
        product_source_files=len(source),
        product_source_hashes_sha256=digest(source),
        product_source_head=protocol["source_head"],
        firmware=fw,
        source_scope="same frozen current host and identical static payload; old/new firmware isolate 0048 within WB1",
        rootfs_scope="same caller-owned prepared userspace; full rootfs tree digest is not claimed",
        budget=protocol["budget"],
        host=dict(
            platform=protocol["host"]["platform"],
            machine=protocol["host"]["machine"],
            cpu_models=cpu_models,
            backing_filesystems=sorted({mount["fstype"] for mount in mounts}),
        ),
        timing_scope=protocol["timing"],
        cache_scope=protocol["cache"],
        phase_contract=protocol["phase_contract"],
        factors=protocol["condition_factors"],
        failures="zero failed/excluded; all valid slow environments retained",
        resource_scope="whole trial includes fixture and host verification; shared campaign memory.peak is not per trial",
        actual_flags_scope="WB negotiation checked in independent diagnostic representatives; timing factors are observed env and firmware-selection receipts; DAX disabled by frozen SDK driver",
    )
    return batch, protocol, records, points, provenance


def audit_analyses(batch, protocol, records, points, directory, time_directory):
    summary, times = load(directory / "summary.json"), load(time_directory / "summary.json")
    require(
        summary.get("status") == "analyzed"
        and summary.get("failures") == []
        and summary["counts"]["valid_environments"] == 20
        and summary["counts"]["failed_environments"] == 0
        and summary["counts"]["environment_points"] == 300,
        "timing analysis is incomplete",
    )
    require(
        summary["source"]["protocol_sha256"] == sha(batch / "protocol.json")
        and summary["source"]["records_sha256"] == sha(batch / "records.json")
        and summary["source"]["analysis_sha256"] == sha(batch / "harness/curves_analysis.py")
        and summary["followup_analyzer_sha256"] == sha(batch / "harness/guest_writeback_curves.py"),
        "timing analysis input/source identity differs",
    )
    with frozen(batch) as (_, analysis):
        curves, comparisons, _ = analysis.summarize(protocol, points)
        _, enablement, _ = analysis.summarize({**protocol, "baseline_condition": "off"}, points)
        enablement = [row for row in enablement if row["condition"] == "guest-wb"]
        require(
            summary["curves"] == curves
            and summary["comparisons"] == comparisons
            and summary["enablement_comparisons"] == enablement,
            "timing summary/statistics differ from raw environments",
        )
        projection(directory / "environment_points.csv", points)
        projection(directory / "curve_summary.csv", analysis.flattened_summary(curves))
        projection(directory / "paired_ratio_summary.csv", comparisons)
        projection(directory / "enablement_comparisons.csv", enablement)
    require(
        times.get("schema") == 1
        and times.get("status") == "verified"
        and times.get("performance_samples") is True
        and times["counts"]
        == dict(
            environments=20,
            resident_rows=900,
            environment_cells=300,
            contrasts=2,
            metrics_per_cell=2,
            comparisons=60,
        ),
        "independent time analysis inventory differs",
    )
    for name in (
        "protocol.json",
        "records.json",
        "status.json",
        "in.json",
        "in.bench.json",
        "in.render.json",
    ):
        require(times["inputs"][name] == sha(batch / name), "time analysis input changed: " + name)
    require(
        times["postprocessor_sha256"] == sha(HERE / "guest_writeback_analysis.py"),
        "time postprocessor source is unavailable or changed",
    )
    for name, expected in times["frozen_helpers"].items():
        require(
            sha(batch / "harness" / relative(name)) == expected,
            "time analysis frozen helper differs",
        )
    time_rows = list(csv.DictReader(io.StringIO((time_directory / "time_levels.csv").read_text())))
    require(len(time_rows) == 300, "time-level projection inventory differs")
    lookup = {(str(record["pair"]), record["condition"]): record for record in records}
    levels, expected_time_rows = {}, []
    cells = {cell["id"]: cell for cell in protocol["cells"]}
    for row in time_rows:
        key = (row["pair"], row["condition"], row["id"])
        require(key not in levels, "duplicate time environment/cell")
        raw = sorted(
            [v for v in lookup[(row["pair"], row["condition"])]["rows"] if v["id"] == row["id"]],
            key=lambda v: v["round"],
        )
        expected = dict(
            rounds=3,
            logical_bytes=sum(v["bytes"] for v in raw),
            completed_ns=sum(v["total_ns"] for v in raw),
            accepted_ns=sum(v["accepted_ns"] for v in raw),
            round_completed_ns=[v["total_ns"] for v in raw],
            round_accepted_ns=[v["accepted_ns"] for v in raw],
        )
        for name, value in expected.items():
            require(
                row[name] == (json.dumps(value) if isinstance(value, list) else str(value)),
                "time level differs from summed dependent rounds: " + name,
            )
        levels[key] = expected
        expected_time_rows.append(
            dict(
                id=row["id"],
                family=cells[row["id"]]["family"],
                x=cells[row["id"]]["x"],
                pair=row["pair"],
                condition=row["condition"],
                **expected,
            )
        )
    require(
        set(levels) == {(p["pair"], p["condition"], p["id"]) for p in points},
        "time-level pairing inventory differs from completed throughput points",
    )
    projection(time_directory / "time_levels.csv", expected_time_rows)
    with frozen(batch) as (_, analysis):
        for row in times["comparisons"]:
            require(
                row["contrast"] in ("incremental_0048", "guest_writeback_enablement")
                and row["condition"] == "guest-wb"
                and row["baseline"]
                == ("old-wb" if row["contrast"] == "incremental_0048" else "off")
                and row["metric"] in ("completed", "accepted")
                and row["n"] == 5
                and row["pairs"] == ["0", "1", "2", "3", "4"],
                "time contrast independent pairing differs",
            )
            before = [
                levels[(pair, row["baseline"], row["id"])][row["metric"] + "_ns"]
                for pair in row["pairs"]
            ]
            after = [
                levels[(pair, "guest-wb", row["id"])][row["metric"] + "_ns"]
                for pair in row["pairs"]
            ]
            values = [a / b for a, b in zip(after, before)]
            cell = cells[row["id"]]
            require(
                row["baseline_ns"] == before
                and row["candidate_ns"] == after
                and row["paired_values"] == values
                and row["median"] == statistics.median(values)
                and row["family"] == cell["family"]
                and row["x"] == cell["x"]
                and row["min"] == min(values)
                and row["max"] == max(values)
                and row["baseline_median_round_ms"] == statistics.median(v / 3e6 for v in before)
                and row["candidate_median_round_ms"] == statistics.median(v / 3e6 for v in after),
                "time ratio is not candidate/baseline per independent environment",
            )
            import random

            rng = random.Random(
                analysis.seed_for(
                    protocol["seed"], row["contrast"], row["id"], row["metric"], "time_ratio"
                )
            )
            draws = sorted(statistics.median(rng.choices(values, k=5)) for _ in range(5000))
            require(
                row["ci_low"] == analysis.percentile(draws, 0.025)
                and row["ci_high"] == analysis.percentile(draws, 0.975)
                and row["bootstrap_resamples"] == 5000
                and row["bootstrap_seed"]
                == analysis.seed_for(
                    protocol["seed"], row["contrast"], row["id"], row["metric"], "time_ratio"
                )
                and row["reduction_percent"] == 100 * (1 - row["median"])
                and row["reduction_ci_low"] == 100 * (1 - row["ci_high"])
                and row["reduction_ci_high"] == 100 * (1 - row["ci_low"])
                and row["interval_excludes_one"] is (row["ci_high"] < 1 or row["ci_low"] > 1),
                "time bootstrap CI or reported reduction differs",
            )
    require(
        len(times["comparisons"]) == 60
        and len({(r["contrast"], r["id"], r["metric"]) for r in times["comparisons"]}) == 60,
        "time contrast inventory differs",
    )
    projection(time_directory / "time_comparisons.csv", times["comparisons"])
    for name, expected in times["outputs"].items():
        require(
            sha(time_directory / relative(name)) == expected,
            "time analysis output changed: " + name,
        )
    return summary, times


def diagnostic_tables(batch, protocol):
    retained = load(batch / "guest-writeback-diagnostics.json")
    require(
        retained.get("schema") == 1
        and retained.get("performance_samples") is False
        and retained["protocol_sha256"] == sha(batch / "protocol.json")
        and retained["records_sha256"] == sha(batch / "records.json")
        and retained["source_sha256"] == sha(batch / "harness/guest_writeback_curves.py"),
        "diagnostic receipt input/source differs",
    )
    intervals, flags = [], []
    with frozen(batch) as (wrapper, _):
        report = wrapper.DIAG.summarize_batch(batch)
        require(
            report == retained["full_profile_evidence"],
            "startup/final/phase diagnostic evidence differs",
        )
        for trial in report["trials"]:
            condition = trial["condition"]
            if condition == "native":
                flags.append(
                    dict(
                        condition=condition,
                        actual_guest_writeback=None,
                        actual_guest_batched_writeback=None,
                        counters="unavailable; native has no FUSE connection",
                    )
                )
                continue
            finals = {p["component"]: p["measurements"] for p in trial["final_profiles"]}
            actual, batched = wrapper.actual_writeback(
                finals, condition, protocol["write_counter_contract"]
            )
            require(
                trial["host_writeback"].get("host_writeback_enabled", 0) == 0,
                "host write buffering unexpectedly enabled",
            )
            flags.append(
                dict(
                    condition=condition,
                    actual_guest_writeback=actual,
                    actual_guest_batched_writeback=batched,
                    counters="independent N1 final profiles",
                )
            )
            for phase in trial["phase_rows"]:
                if phase["component"] == "virtio-fs-protocol":
                    intervals.append(
                        dict(
                            condition=condition,
                            pair=trial["pair"],
                            id=phase["id"],
                            round=phase["round"],
                            **wrapper.write_sizes(
                                phase["measurements"], protocol["write_counter_contract"]
                            ),
                        )
                    )
    require(
        intervals == retained["intervals"] and len(intervals) == 135,
        "diagnostic per-round interval inventory differs",
    )
    counts, histogram = [], []
    for condition in ("off", "old-wb", "guest-wb"):
        for cell in protocol["cells"]:
            rows = sorted(
                [r for r in intervals if r["condition"] == condition and r["id"] == cell["id"]],
                key=lambda r: r["round"],
            )
            require([r["round"] for r in rows] == [1, 2, 3], "diagnostic resident rounds differ")
            requests = sum(r["write_requests"] for r in rows)
            payload = sum(r["write_payload_bytes"] for r in rows)
            counts.append(
                dict(
                    condition=condition,
                    id=cell["id"],
                    family=cell["family"],
                    x=cell["x"],
                    independent_environments=1,
                    dependent_rounds=3,
                    write_requests_total=requests,
                    write_requests_per_round=requests / 3,
                    write_requests_by_round=[r["write_requests"] for r in rows],
                    write_payload_bytes_total=payload,
                    write_background_requests_total=sum(
                        r["write_background_requests"] for r in rows
                    ),
                    mean_payload_bytes=None if requests == 0 else payload / requests,
                    interval_max_bytes=None,
                    max_scope="unavailable; cumulative maxima are not subtracted",
                    scope="independent diagnostic counts, no timing or tail inference",
                )
            )
            for index, bucket in enumerate(protocol["write_counter_contract"]["buckets"]):
                values = [r["payload_histogram"][index]["requests"] for r in rows]
                require(
                    all(r["payload_histogram"][index]["label"] == bucket["label"] for r in rows),
                    "diagnostic histogram bucket differs",
                )
                histogram.append(
                    dict(
                        condition=condition,
                        id=cell["id"],
                        family=cell["family"],
                        bucket=bucket["label"],
                        upper_bytes=bucket["upper_bytes"],
                        requests=sum(values),
                        requests_by_round=values,
                        fraction=None if requests == 0 else sum(values) / requests,
                        independent_environments=1,
                        dependent_rounds=3,
                    )
                )
    require(
        len(counts) == 45 and len(histogram) == 315 and len(flags) == 4,
        "diagnostic derived inventory differs",
    )
    return retained, intervals, counts, histogram, flags


def pipeline(batch, formal_protocol):
    batch = batch.resolve()
    protocol, result = load(batch / "protocol.json"), load(batch / "result.json")
    require(
        protocol.get("role") == "B-FS-DIAG"
        and protocol.get("delay_ns") == 1500000000
        and set(result) == {"old", "new"},
        "controlled pipeline diagnostic contract differs",
    )
    for name, expected in protocol["artifacts"].items():
        require(sha(Path(name)) == expected, "controlled pipeline artifact changed")
    require(
        sha(batch / "subject") == formal_protocol["artifacts"]["bin/subject"]
        and sha(protocol["firmware"]["old"]) == formal_protocol["artifacts"]["bin/libkrunfw-old.so"]
        and sha(protocol["firmware"]["new"]) == formal_protocol["artifacts"]["bin/libkrunfw.so"],
        "controlled pipeline uses different host or firmware",
    )
    require(
        sha(protocol["subject_source_receipt"]) == protocol["subject_source_receipt_sha256"],
        "controlled pipeline host build receipt changed",
    )
    host_source = load(protocol["subject_source_receipt"])
    require(
        host_source["artifacts"]["bin/subject"] == sha(batch / "subject"),
        "controlled pipeline host build receipt does not identify these executable bytes",
    )
    before, after = host_source["source_hashes"], formal_protocol["source_hashes"]
    changed = {
        name: dict(pipeline=before.get(name), formal=after.get(name))
        for name in sorted(before.keys() | after.keys())
        if before.get(name) != after.get(name)
    }
    require(
        all(
            name.endswith("/README.md") and before.get(name) and after.get(name) for name in changed
        ),
        "controlled pipeline non-README product source association differs",
    )
    rows, evidence = [], {}
    for arm in ("old", "new"):
        trial = batch / arm
        invocation = load(trial / "invocation.json")
        require(
            invocation["returncode"] == 0
            and invocation["environment"]["PVISOR_VM_FS_WRITEBACK"] == "1"
            and invocation["environment"]["PVISOR_VM_FS_HOST_WRITEBACK"] == "0"
            and invocation["environment"]["PVISOR_FS_PROFILE"] == "0"
            and invocation["environment"]["LD_PRELOAD"] == str(batch / "delay.so")
            and invocation["environment"]["CURVE_PIPELINE_EVENTS"] == str(trial / "events.jsonl"),
            "controlled injection flags/exit differ",
        )
        require(
            invocation["command"]
            == [
                str(batch / "subject"),
                protocol["rootfs"],
                protocol["firmware"][arm],
                str(batch / "payload"),
                str(batch / "plan"),
                str(trial / "stage"),
                "1",
            ],
            "controlled actual argv differs from frozen inputs",
        )
        selected = dict(path=protocol["firmware"][arm], selection="argv")
        require(
            marker_json(trial / "stderr.log", "CURVES_FIRMWARE_SELECTED ") == [selected]
            and invocation["command"][2] == protocol["firmware"][arm],
            "controlled actual firmware selection differs from explicit argv",
        )
        events = [json.loads(line) for line in (trial / "events.jsonl").read_text().splitlines()]

        def first(file, phase):
            return min(e["ns"] for e in events if e["file"] == file and e["phase"] == phase)

        a, end_a, b, end_b = [
            first(*key) for key in [("a", "begin"), ("a", "end"), ("b", "begin"), ("b", "end")]
        ]
        expected = dict(
            a_begin_ns=a,
            a_end_ns=end_a,
            b_begin_ns=b,
            b_end_ns=end_b,
            b_completed_while_a_pending=a < end_b < end_a,
            b_begin_after_a_begin_ms=(b - a) / 1e6,
            verified_bytes=2 << 20,
        )
        require(
            result[arm] == expected and end_a - a >= protocol["delay_ns"] and end_b >= b >= a,
            "controlled event summary differs",
        )
        stdout = (trial / "stdout.log").read_text()
        require(
            stdout.splitlines().count("PIPELINE_BYTES_OK") == 1,
            "controlled guest full-byte marker missing",
        )
        closed = re.findall(r"^CLOSED pipeline-([ab]) elapsed_ns=(\d+)$", stdout, re.M)
        require(
            [name for name, _ in closed] == ["a", "b"], "controlled guest close landmarks missing"
        )
        for name, byte in [("pipeline-a", 0x41), ("pipeline-b", 0x42)]:
            path = trial / "stage/upper/curve-work" / name
            require(
                path.read_bytes() == bytes([byte]) * (1 << 20), "controlled backing bytes differ"
            )
            evidence[str(path.relative_to(batch))] = sha(path)
        rows.append(
            dict(
                condition=arm,
                independent_environments=1,
                injected_a_delay_ns=protocol["delay_ns"],
                a_backing_elapsed_ns=end_a - a,
                b_begin_after_a_begin_ns=b - a,
                b_complete_after_a_begin_ns=end_b - a,
                b_completed_while_a_pending=a < end_b < end_a,
                guest_a_close_landmark_ns=int(closed[0][1]),
                guest_b_close_landmark_ns=int(closed[1][1]),
                verified_bytes=2 << 20,
                scope="controlled delayed backing diagnostic; landmarks are not latency samples",
            )
        )
        for name in ("events.jsonl", "stdout.log", "stderr.log", "invocation.json"):
            evidence[str((trial / name).relative_to(batch))] = sha(trial / name)
    flattened = {
        arm + "_" + key: value for arm, values in result.items() for key, value in values.items()
    }
    semspec(batch, flattened, "controlled-pipeline")
    for name in ("protocol.json", "result.json", "in.bench.json", "in.render.json"):
        evidence[name] = sha(batch / name)
    return rows, dict(
        cohort=batch.name,
        benchmark_id="B-FS-DIAG",
        performance_samples=False,
        injected_delay_ns=protocol["delay_ns"],
        raw_receipt_files=len(evidence),
        raw_receipts_sha256=digest(evidence),
        core_receipts={
            name: evidence[name]
            for name in ("protocol.json", "result.json", "in.bench.json", "in.render.json")
        },
        artifacts_sha256=digest(sorted(protocol["artifacts"].values())),
        subject_sha256=sha(batch / "subject"),
        subject_build_receipt_sha256=protocol["subject_source_receipt_sha256"],
        product_source_hashes_sha256=digest(before),
        documentation_inventory_differences=changed,
        source_association="identical executed host binary and all non-README product sources; host build receipt does not select runtime firmware",
        firmware_association="explicit argv and unique driver selection marker; exact old/new bytes matched to formal cohort SOURCE receipts",
        frozen_harness_inventory_sha256=digest(
            {
                name: sha(batch / "harness" / name)
                for name in (
                    "guest_writeback_pipeline.py",
                    "guest_writeback_pipeline_guest.rs",
                    "guest_writeback_pipeline_delay.c",
                )
            }
        ),
        scope="one controlled old/new execution; no throughput, device latency or independent performance CI",
    )


def correctness(batch, formal_firmware):
    batch = batch.resolve()
    receipt = load(batch / "receipt.json")
    require(
        receipt.get("schema") == 1
        and receipt.get("role") == "correctness, not performance"
        and receipt["firmware"]["sha256"] == formal_firmware["new_sha256"],
        "correctness receipt role/firmware differs",
    )
    for name, expected in receipt["files"].items():
        require(sha(batch / relative(name)) == expected, "correctness evidence changed: " + name)
    fw = load(batch / relative(receipt["firmware"]["source_receipt"]))
    require(
        fw["firmware_sha256"] == formal_firmware["new_sha256"]
        and fw["inputs"][PATCH48] == formal_firmware["added_patch_sha256"]
        and receipt["files"]["0048.patch"] == formal_firmware["added_patch_sha256"],
        "correctness firmware source/patch association differs",
    )
    invocations = load(batch / "final-v3-invocations.json")
    require(
        invocations["environment"].get("PVISOR_VM_FS_WRITEBACK") == "1"
        and invocations["environment"].get("PVISOR_VM_FS_HOST_WRITEBACK") == "0"
        and invocations["environment"].get("PVISOR_FS_PROFILE") == "0",
        "correctness actual invocation flags differ",
    )
    require(
        {record["mode"] for record in invocations["records"]} == {"full", "error"}
        and len(invocations["records"]) == 2,
        "final correctness probe inventory differs",
    )
    for record in invocations["records"]:
        require(
            record["returncode"] == 0
            and sha(batch / relative(record["log"])) == record["log_sha256"]
            and receipt["firmware"]["path"] in record["command"],
            "correctness final invocation/log differs",
        )
    full = (batch / "full-final-v3.log").read_text()
    errors = (batch / "error-final-v3.log").read_text()
    require(
        "GUEST_WRITEBACK_DETACHED_PASS" in full
        and "GUEST_WRITEBACK_PAYLOAD_PASS" in full
        and "GUEST_WRITEBACK_PASS" in full
        and "GUEST_WRITEBACK_ERROR_ARMED" in errors
        and "GUEST_WRITEBACK_ERROR_PASS" in errors
        and "guest writeback failed:" in errors,
        "correctness final pass/error propagation markers missing",
    )
    pressure = re.findall(r"^WB_PRESSURE_TARGET_VERIFIED bytes=(\d+)$", full, re.M)
    reservations = re.findall(r"^WB_BDI_VERIFIED .* strict=1 max=100 min_bytes=(\d+)$", full, re.M)
    require(
        len(pressure) == 1
        and (16 << 20) <= int(pressure[0]) <= (256 << 20)
        and len(reservations) == 2
        and all(int(value) > (8 << 20) for value in reservations),
        "actual native dirty-pressure/reservation proof missing",
    )
    require(
        "511 tests run: 511 passed, 8 skipped" in (batch / "rust-tests.log").read_text()
        and "Ran 5 tests" in (batch / "firmware-tests.log").read_text()
        and "\nOK\n" in (batch / "firmware-tests.log").read_text()
        and "Ran 44 tests" in (batch / "python-tests.log").read_text()
        and "\nOK\n" in (batch / "python-tests.log").read_text()
        and receipt["validation"]["rustdoc"]["exit_code"] == 0,
        "retained correctness test results differ",
    )
    return dict(
        cohort=batch.name,
        role="correctness, not performance",
        receipt_sha256=sha(batch / "receipt.json"),
        raw_file_count=len(receipt["files"]),
        raw_files_sha256=digest(receipt["files"]),
        firmware_sha256=receipt["firmware"]["sha256"],
        final_logs={
            name: receipt["files"][name]
            for name in (
                "full-final-v3.log",
                "error-final-v3.log",
                "rust-tests.log",
                "firmware-tests.log",
                "python-tests.log",
                "rustdoc.log",
            )
        },
        validation={
            name: {key: value for key, value in result.items() if key != "command"}
            for name, result in receipt["validation"].items()
        },
        final_probe_modes=["full", "error"],
        parent_exit_codes=[0, 0],
        actual_dirty_pressure_target_bytes=int(pressure[0]),
        actual_bdi_minimum_reservations_bytes=[int(value) for value in reservations],
        final_invocations_sha256=sha(batch / "final-v3-invocations.json"),
        limitations=[
            "historical baseline executable invocation was not retained; no binary-level attribution",
            "correctness regressions are separate direct executions, not formal curve samples",
            "Linux x86_64/KVM SDK; no macOS or ordinary CLI performance claim",
            "concurrent reclaim, exhaustive disconnect and crash durability remain unmeasured",
        ],
    )


def collect(args):
    timing, protocol, records, points, formal = audit_cohort(args.timing, False)
    diagnostic, diag_protocol, _, _, diag = audit_cohort(args.diagnostic, True)
    require(
        protocol["source_hashes"] == diag_protocol["source_hashes"]
        and protocol["condition_factors"] == diag_protocol["condition_factors"]
        and protocol["cells"] == diag_protocol["cells"],
        "timing/diagnostic treatments differ",
    )
    for name in ("bin/subject", "bin/payload", "bin/libkrunfw.so", "bin/libkrunfw-old.so"):
        require(
            protocol["artifacts"][name] == diag_protocol["artifacts"][name],
            "timing/diagnostic executed artifacts differ: " + name,
        )
    directory = (args.analysis or timing / "analysis").resolve()
    time_directory = (args.time_analysis or timing / "analysis-times").resolve()
    summary, times = audit_analyses(timing, protocol, records, points, directory, time_directory)
    report, intervals, counts, histogram, flags = diagnostic_tables(diagnostic, diag_protocol)
    files = {}

    def add(name, data, role, rows=None):
        require(name not in files and Path(name).name == name, "duplicate/unsafe public filename")
        data = public(data, name)
        files[name] = dict(data=data, sha256=hashlib.sha256(data).hexdigest(), role=role, rows=rows)

    def table(name, rows, role="engineering A/B"):
        add(PREFIX + name + ".csv", csv_bytes(rows), role, len(rows))

    with frozen(timing) as (_, analysis):
        table("levels", analysis.flattened_summary(summary["curves"]))
    table("environments", points)
    table(
        "throughput-comparisons",
        [r for r in summary["comparisons"] if r["condition"] == "guest-wb"],
    )
    table("enablement-comparisons", summary["enablement_comparisons"])
    add(
        PREFIX + "time-levels.csv",
        (time_directory / "time_levels.csv").read_bytes(),
        "engineering A/B",
        300,
    )
    table("time-comparisons", times["comparisons"])
    table("write-requests", counts, "diagnostic")
    table("write-request-rounds", intervals, "diagnostic")
    table("write-histogram", histogram, "diagnostic")
    table("diagnostic-flags", flags, "diagnostic")
    expected_plots = {
        f"family-{family}.{extension}" for family in FAMILIES for extension in ("svg", "pdf")
    }
    expected_plots |= {
        f"{name}.{extension}"
        for name in ("write-requests", "write-payload-histogram")
        for extension in ("svg", "pdf")
    }
    plots = times.get("plots", [])
    require(
        expected_plots == set(plots) and len(plots) == len(expected_plots),
        "five completed-throughput/time-ratio and two diagnostic figures are required",
    )
    require(
        times.get("diagnostic_inputs")
        == {
            name: sha(diagnostic / name)
            for name in (
                "protocol.json",
                "records.json",
                "status.json",
                "guest-writeback-diagnostics.json",
                "in.json",
                "in.bench.json",
                "in.render.json",
            )
        },
        "diagnostic figure inputs differ from audited independent counts",
    )
    for name in plots:
        require(
            name.endswith((".svg", ".pdf"))
            and Path(name).name == name
            and name in times["outputs"],
            "plot is not a hashed direct SVG/PDF child",
        )
        target = "guest-writeback-" + name.removeprefix("family-")
        data = (time_directory / name).read_bytes()
        if name.endswith(".svg"):
            add(target, data, "diagnostic" if name.startswith("write-") else "engineering A/B")
    formal["analysis"] = dict(
        summary_sha256=sha(directory / "summary.json"),
        analysis_sha256=summary["source"]["analysis_sha256"],
        time_summary_sha256=sha(time_directory / "summary.json"),
        time_postprocessor_sha256=times["postprocessor_sha256"],
        statistics=times["statistical_scope"],
    )
    diag["analysis"] = dict(
        summary_sha256=sha(diagnostic / "guest-writeback-diagnostics.json"),
        source_sha256=report["source_sha256"],
        actual_flags=flags,
        scope="independent N1 per arm; three dependent rounds; exact startup/final and phase counters; never performance samples",
    )
    cohorts = [formal, diag]
    if args.pipeline:
        rows, evidence = pipeline(args.pipeline, protocol)
        table("pipeline", rows, "diagnostic")
        cohorts.append(evidence)
    manifest = dict(
        schema=1,
        namespace="filesystem-guest-writeback",
        benchmark_ids=["B-FS-ENG", "B-FS-DIAG"],
        role="engineering A/B with separate diagnostics",
        publisher_sha256=sha(Path(__file__)),
        cohorts=cohorts,
        statistical_scope="five fresh paired environments; three dependent rounds summed within each environment; exploratory 95% pointwise paired bootstrap; no P99 or native causal ratio",
        boundaries="SDK mechanism; warm uncontrolled host backing; no cold-disk, crash-durability, offload, CLI or cross-hardware conclusion",
        public_outputs={
            str(ASSETS / name): spec["sha256"]
            for name, spec in sorted(files.items())
            if name.endswith(".svg")
        },
        files={
            name: {key: value for key, value in spec.items() if key != "data"}
            for name, spec in sorted(files.items())
            if name.endswith((".csv", ".svg"))
        },
    )
    if args.correctness:
        manifest["correctness"] = correctness(args.correctness, formal["firmware"])
    encoded = public(
        (json.dumps(manifest, indent=2, sort_keys=True, allow_nan=False) + "\n").encode(),
        "provenance.json",
    )
    return files, manifest, encoded


def destinations(files, provenance):
    """Return only public SVG targets; tables and provenance belong to the batch."""
    result = {}
    for name, spec in files.items():
        if not name.endswith(".svg"):
            continue
        result[ASSETS / name] = spec["data"]
    return result


def preflight_targets(targets, root):
    for relative_path, data in targets.items():
        target = root / relative_path
        require(
            not any(path.is_symlink() for path in [target, *target.parents] if path != root.parent),
            "publication refuses symlink destination: " + str(relative_path),
        )
        require(
            not target.exists() or (target.is_file() and target.read_bytes() == data),
            "publication would replace different bytes: " + str(relative_path),
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timing", type=Path, required=True)
    parser.add_argument("--diagnostic", type=Path, required=True)
    parser.add_argument("--analysis", type=Path)
    parser.add_argument("--time-analysis", type=Path)
    parser.add_argument("--pipeline", type=Path)
    parser.add_argument("--correctness", type=Path)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    files, manifest, encoded = collect(args)
    targets = destinations(files, encoded)
    archives = archive_targets(
        args.timing,
        manifest["namespace"],
        {name: spec["data"] for name, spec in files.items()},
        encoded,
    )
    preflight_archive(archives)
    preflight_targets(targets, REPO)
    if args.publish:
        write_archive(archives)
        # Check every collision before the first write. Existing equal bytes
        # remain untouched, and an exclusive create closes overwrite races.
        for relative_path, data in targets.items():
            target = REPO / relative_path
            if target.exists():
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            try:
                with target.open("xb") as stream:
                    stream.write(data)
            except FileExistsError:
                require(
                    target.read_bytes() == data, "concurrent publication contains different bytes"
                )
    print(
        json.dumps(
            dict(
                state="published" if args.publish else "checked-not-published",
                csv_files=sum(name.endswith(".csv") for name in files),
                figures=sum(not name.endswith(".csv") for name in files),
                provenance_sha256=hashlib.sha256(encoded).hexdigest(),
                archive_directory=str(
                    Path(args.timing).resolve() / "publication" / manifest["namespace"]
                ),
                archive_files=len(archives),
                outputs=[str(path) for path in targets],
                manifest=manifest,
            ),
            indent=2,
            allow_nan=False,
        )
    )


if __name__ == "__main__":
    main()
