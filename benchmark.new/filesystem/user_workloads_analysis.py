#!/usr/bin/env python3
"""Derive B-FS-TOOLS user tables and figures from fresh cross-mode observations.

This helper never executes a subject. Three resident rounds belong to one fresh
environment; their sum is formed before computing the across-environment median.
Modes are reported independently. No composite workload score, paired causal
speedup, confidence interval, or operation tail latency is inferred from N=3.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import statistics
import sys
from pathlib import Path

MIB = 1 << 20
TIMES = (
    "setup_ns",
    "work_ns",
    "final_sync_ns",
    "completed_ns",
    "validation_ns",
    "foreground_read_ns",
)
COUNTERS = (
    "bytes_written",
    "bytes_read",
    "write_ops",
    "read_ops",
    "metadata_ops",
    "periodic_fsyncs",
    "final_fsyncs",
    "checked_closes",
    "files_verified",
    "bytes_verified",
)
BACKEND_LABELS = {
    "native": "Native",
    "host": "pVisor host",
    "staged": "pVisor staged",
    "vm": "pVisor VM",
    "vm-writeback": "pVisor VM + WB",
    "docker": "Docker",
    "fc-system": "Firecracker",
    "qemu": "QEMU q35",
    "qemu-microvm": "QEMU microvm",
}


def require(value, message):
    if not value:
        raise ValueError(message)


def number(value, *, positive=False):
    return (
        not isinstance(value, bool)
        and isinstance(value, (int, float))
        and math.isfinite(value)
        and (value > 0 if positive else value >= 0)
    )


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def _backends(protocol, records):
    values = protocol.get("backends", protocol.get("conditions"))
    if isinstance(values, dict):
        values = list(values)
    if isinstance(values, list):
        return [value["id"] if isinstance(value, dict) else value for value in values]
    return list(dict.fromkeys(record.get("backend", record.get("condition")) for record in records))


def collect(protocol, records):
    """Keep failures and raw rows; only complete backend/case cells get medians."""
    require(
        isinstance(records, list) and all(isinstance(r, dict) for r in records),
        "records must be an array of environment objects",
    )
    catalog = protocol.get("catalog")
    require(isinstance(catalog, list) and catalog, "frozen protocol catalog missing")
    cases = {case["id"]: case for case in catalog}
    require(len(cases) == len(catalog), "duplicate case in protocol catalog")
    selected = protocol.get(
        "cases", protocol.get("inventory", {}).get("selected_cell_ids", list(cases))
    )
    if selected and isinstance(selected[0], dict):
        selected = [case["id"] for case in selected]
    require(
        len(set(selected)) == len(selected) and set(selected) <= cases.keys(),
        "selected case inventory differs from catalog",
    )
    backends = _backends(protocol, records)
    require(
        backends
        and len(set(backends)) == len(backends)
        and all(isinstance(b, str) for b in backends),
        "backend inventory missing or duplicated",
    )
    samples = protocol.get("samples", 3)
    rounds = protocol.get("resident_rounds", protocol.get("rounds", 3))
    require(
        type(samples) is int and samples >= 1 and type(rounds) is int and rounds == 3,
        "expected positive sample count and three dependent rounds",
    )
    index_base = protocol.get("sample_index_base", 0)
    require(
        type(index_base) is int and index_base in (0, 1), "sample_index_base must be zero or one"
    )
    sample_ids = list(range(index_base, index_base + samples))
    expected = {
        (case, backend, sample)
        for case in selected
        for backend in backends
        for sample in sample_ids
    }
    seen, usable, raw, environments, failures = set(), {}, [], [], []

    def defect(key, reason):
        failures.append(dict(case_id=key[0], backend=key[1], sample=key[2], reason=reason))

    for index, record in enumerate(records):
        key = (
            record.get("case_id", record.get("case")),
            record.get("backend", record.get("condition")),
            record.get("sample", record.get("pair")),
        )
        require(type(key[2]) is int and key in expected, f"unexpected environment {index}: {key}")
        require(key not in seen, f"duplicate environment {key}")
        seen.add(key)
        start_defects = len(failures)
        verified = record.get("host_verified", record.get("hostverified"))
        if isinstance(verified, dict):
            verified = verified.get("verified")
        if record.get("returncode") != 0 or record.get("valid") is False or verified is not True:
            defect(
                key,
                "nonzero exit, invalid environment, or host verification missing: "
                + str(record.get("error", "unspecified")),
            )
        ready_ns = record.get("ready_ns")
        if ready_ns is None and number(record.get("ready_ms")):
            ready_ns = record["ready_ms"] * 1e6
        if not number(ready_ns, positive=True):
            defect(key, "missing or invalid launch-to-Ready clock")
        rows = record.get("rounds", record.get("rows", []))
        if not isinstance(rows, list):
            rows = []
            defect(key, "rounds are not an array")
        ids = [row.get("round") for row in rows if isinstance(row, dict)]
        if len(rows) != rounds or any(type(r) is not int for r in ids) or sorted(ids) != [1, 2, 3]:
            defect(key, "three dependent round records missing or duplicated")
        for row in rows:
            if not isinstance(row, dict):
                defect(key, "non-object round result")
                continue
            if row.get("id", key[0]) != key[0] or row.get("correctness") is not True:
                defect(key, "case identity or guest correctness failed")
            for field in TIMES:
                if field in ("work_ns", "completed_ns"):
                    valid = number(row.get(field), positive=True)
                else:
                    valid = number(row.get(field, 0))
                if not valid:
                    defect(key, f"invalid clock {field}")
            if all(
                number(row.get(field)) for field in ("work_ns", "final_sync_ns", "completed_ns")
            ):
                if row["completed_ns"] != row["work_ns"] + row["final_sync_ns"]:
                    defect(key, "completed_ns differs from work_ns + final_sync_ns")
            else:
                defect(key, "missing work/final-sync/completed clock")
            for field in COUNTERS:
                if field in row and (type(row[field]) is not int or row[field] < 0):
                    defect(key, f"invalid logical counter {field}")
            raw.append(
                dict(
                    case_id=key[0],
                    backend=key[1],
                    sample=key[2],
                    **{
                        field: value
                        for field, value in row.items()
                        if not isinstance(value, (dict, list))
                    },
                )
            )
        environment = dict(
            case_id=key[0],
            backend=key[1],
            sample=key[2],
            returncode=record.get("returncode"),
            host_verified=verified is True,
            ready_ms=ready_ns / 1e6 if number(ready_ns) else None,
            analysis_eligible=start_defects == len(failures),
            stdout=record.get("stdout", record.get("stdout_path")),
            stderr=record.get("stderr", record.get("stderr_path")),
            error=record.get("error"),
        )
        for field in ("wrapper_prepare_ns", "job_ns", "exit_after_done_ns"):
            if number(record.get(field)):
                environment[field.removesuffix("_ns") + "_ms"] = record[field] / 1e6
        for field, value in record.get("resource", {}).items():
            if not isinstance(value, (dict, list)):
                environment[f"resource_{field}"] = value
        if environment["analysis_eligible"]:
            usable[key] = {row["round"]: row for row in rows}
            for field in TIMES:
                environment[f"sum_{field.removesuffix('_ns')}_ms"] = (
                    sum(row.get(field, 0) for row in rows) / 1e6
                )
            for field in COUNTERS:
                environment[field] = sum(row.get(field, 0) for row in rows)
            elapsed = environment["sum_completed_ms"] / 1000
            work = environment["sum_work_ms"] / 1000
            for kind in ("write", "read"):
                byte_count = environment["bytes_written" if kind == "write" else "bytes_read"]
                environment[f"{kind}_completed_mib_s"] = (
                    byte_count / MIB / elapsed if byte_count else None
                )
                environment[f"{kind}_work_mib_s"] = byte_count / MIB / work if byte_count else None
            environment["final_sync_fraction"] = (
                environment["sum_final_sync_ms"] / environment["sum_completed_ms"]
            )
        environments.append(environment)
    for key in sorted(expected - seen):
        defect(key, "planned fresh environment missing")
    cells = [
        (case, backend)
        for case in selected
        for backend in backends
        if all((case, backend, sample) in usable for sample in sample_ids)
    ]
    return cases, selected, backends, sample_ids, usable, raw, environments, failures, cells


def summarize(protocol, records):
    cases, selected, backends, sample_ids, usable, raw, environments, failures, cells = collect(
        protocol, records
    )
    samples = len(sample_ids)
    levels, ready_levels = [], []
    for case_id, backend in cells:
        for scope in ("sum_three_rounds", "round_1", "round_2", "round_3"):
            rounds = [1, 2, 3] if scope == "sum_three_rounds" else [int(scope[-1])]
            for metric in TIMES:
                points = [
                    sum(usable[case_id, backend, sample][r].get(metric, 0) for r in rounds) / 1e6
                    for sample in sample_ids
                ]
                levels.append(
                    dict(
                        case_id=case_id,
                        name=cases[case_id]["name"],
                        layer=cases[case_id]["layer"],
                        backend=backend,
                        scope=scope,
                        metric=metric,
                        n=samples,
                        dependent_rounds=len(rounds),
                        median_ms=statistics.median(points),
                        min_ms=min(points),
                        max_ms=max(points),
                        points_ms=points,
                    )
                )
    for case_id, backend in cells:
        points = [
            env["ready_ms"]
            for env in environments
            if env["case_id"] == case_id and env["backend"] == backend and env["analysis_eligible"]
        ]
        ready_levels.append(
            dict(
                case_id=case_id,
                backend=backend,
                scope="case_launch_before_setup",
                n=len(points),
                median_ms=statistics.median(points),
                min_ms=min(points),
                max_ms=max(points),
                points_ms=points,
            )
        )
    expected_envs = len(selected) * len(backends) * samples
    return dict(
        schema=1,
        benchmark_id="B-FS-TOOLS",
        role="user-facing",
        status="complete_exploratory"
        if len(cells) == len(selected) * len(backends)
        else "incomplete_cells_retained",
        statistical_scope=dict(
            independent_environments_per_case_backend=samples,
            sample_ids=sample_ids,
            dependent_rounds_per_environment=3,
            estimator="median of independent environment observations; sum three rounds inside each environment before the aggregate median",
            comparison="cross-mode descriptive levels; no paired causal speedup or confidence interval",
            ready="launch to payload Ready before case setup; teardown excluded; per-case cohorts, primary M01 only",
            task="payload work plus declared final synchronization; setup, validation, startup and exit excluded",
            aggregate_score=None,
            tails="not measured; N3 environment means do not establish P95/P99",
            failures="all retained; no medians for an incomplete backend/case cell; no zero filling or replacement",
        ),
        counts=dict(
            cases=len(selected),
            backends=len(backends),
            expected_environments=expected_envs,
            observed_environments=len(records),
            expected_round_rows=expected_envs * 3,
            observed_round_rows=len(raw),
            complete_cells=len(cells),
            defects=len(failures),
        ),
        backends=backends,
        catalog=[cases[case] for case in selected],
        complete_cells=[dict(case_id=case, backend=backend) for case, backend in cells],
        levels=levels,
        ready=ready_levels,
        environments=environments,
        raw_rows=raw,
        failures=failures,
    )


def csv_file(path, rows):
    fields = list(dict.fromkeys(key for row in rows for key in row)) or ["no_records"]
    with path.open("x", newline="") as stream:
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


def plot(summary, output):
    """Show sparse measured anchors; curves do not imply unmeasured interpolation."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "font.size": 9,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "pdf.fonttype": 42,
            "svg.fonttype": "none",
        }
    )
    palette = [
        "#222222",
        "#0072B2",
        "#009E73",
        "#D55E00",
        "#CC79A7",
        "#E69F00",
        "#56B4E9",
        "#7B3294",
        "#999999",
    ]
    backends = summary["backends"]
    expanded = len(backends) > 5
    labels = {backend: BACKEND_LABELS.get(backend, backend) for backend in backends}
    colors = {backend: palette[index % len(palette)] for index, backend in enumerate(backends)}
    levels = {
        (row["case_id"], row["backend"], row["scope"], row["metric"]): row
        for row in summary["levels"]
    }
    paths = []

    def save(fig, stem):
        fig.suptitle(
            "Exploratory fresh environments; 3 dependent rounds; startup and exit excluded from task clocks",
            fontsize=10,
        )
        if expanded:
            handles, names = fig.axes[0].get_legend_handles_labels()
            fig.legend(
                handles,
                names,
                loc="lower center",
                bbox_to_anchor=(0.5, 0.015),
                ncol=5,
                fontsize=8,
                frameon=False,
            )
        fig.tight_layout(rect=(0, 0.14 if expanded else 0, 1, 0.94))
        for extension in ("svg", "pdf"):
            path = output / f"{stem}.{extension}"
            fig.savefig(path)
            paths.append(path.name)
        plt.close(fig)

    scenarios = [case for case in summary["catalog"] if case["layer"] == "scenario"]
    if scenarios:
        fig, axes = plt.subplots(
            1,
            len(scenarios),
            figsize=(4.4 * len(scenarios), 5.3 if expanded else 4.5),
            squeeze=False,
        )
        for ax, case in zip(axes[0], scenarios):
            for backend in backends:
                xs, ys = [], []
                for r in range(1, 4):
                    row = levels.get((case["id"], backend, f"round_{r}", "completed_ns"))
                    if row:
                        xs.append(r)
                        ys.append(row["median_ms"])
                        ax.scatter(
                            [r] * row["n"], row["points_ms"], color=colors[backend], s=18, alpha=0.4
                        )
                ax.plot(
                    xs, ys, marker="o", markersize=4, color=colors[backend], label=labels[backend]
                )
            ax.set_title(case["id"] + " " + case["name"])
            ax.set_xticks([1, 2, 3])
            ax.set_xlabel("Dependent round in the same environment")
            ax.set_ylabel("Task sync-completed time (ms)")
            ax.grid(alpha=0.15)
        if not expanded:
            axes[0][0].legend(fontsize=7)
        save(fig, "workflow-rounds")

    mechanisms = [case for case in summary["catalog"] if case["layer"] == "mechanism"]
    if mechanisms:
        fig, axes = plt.subplots(1, 2, figsize=(13, 8 if expanded else 7))
        reference = "native" if "native" in backends else backends[0]
        for ax, metric in zip(axes, ("work_ns", "completed_ns")):
            for b, backend in enumerate(backends):
                xs, ys = [], []
                for y, case in enumerate(mechanisms):
                    row = levels.get((case["id"], backend, "sum_three_rounds", metric))
                    ref = levels.get((case["id"], reference, "sum_three_rounds", metric))
                    if row and ref and ref["median_ms"] > 0:
                        xs.append(row["median_ms"] / ref["median_ms"])
                        ys.append(
                            y + (b - (len(backends) - 1) / 2) * min(0.11, 0.7 / len(backends))
                        )
                ax.scatter(xs, ys, color=colors[backend], label=labels[backend], s=24)
            ax.axvline(1, color="#777777", linewidth=1, linestyle="--")
            ax.set_xscale("log")
            ax.set_yticks(
                range(len(mechanisms)), [case["id"] + " " + case["name"] for case in mechanisms]
            )
            ax.invert_yaxis()
            ax.set_title("Work-return clock" if metric == "work_ns" else "Sync-completed clock")
            ax.set_xlabel(f"Ratio of three-round median times to {reference} (lower is faster)")
            ax.grid(axis="x", alpha=0.15)
        if not expanded:
            axes[0].legend(fontsize=7)
        save(fig, "mechanism-relative-time")

    resources = [case for case in summary["catalog"] if case["layer"] == "resource"]
    if resources:
        working_sets = sorted({case["parameters"]["working_set_bytes"] for case in resources})
        fig, axes = plt.subplots(
            2,
            len(working_sets),
            figsize=(6 * len(working_sets), 8 if expanded else 7),
            squeeze=False,
        )
        for column, wss in enumerate(working_sets):
            cases = sorted(
                (case for case in resources if case["parameters"]["working_set_bytes"] == wss),
                key=lambda case: case["parameters"]["workers"],
            )
            for backend in backends:
                for axis, field in [
                    (axes[0][column], "write_completed_mib_s"),
                    (axes[1][column], "final_sync_fraction"),
                ]:
                    xs, ys = [], []
                    for case in cases:
                        if not any(
                            cell["case_id"] == case["id"] and cell["backend"] == backend
                            for cell in summary["complete_cells"]
                        ):
                            continue
                        points = [
                            env[field]
                            for env in summary["environments"]
                            if env["case_id"] == case["id"]
                            and env["backend"] == backend
                            and env["analysis_eligible"]
                        ]
                        points = [point for point in points if point is not None]
                        if not points:
                            continue
                        workers = case["parameters"]["workers"]
                        xs.append(workers)
                        ys.append(statistics.median(points))
                        axis.scatter(
                            [workers] * len(points), points, color=colors[backend], s=18, alpha=0.4
                        )
                    axis.plot(
                        xs,
                        ys,
                        marker="o",
                        markersize=4,
                        color=colors[backend],
                        label=labels[backend],
                    )
                    axis.set_xticks([1, 4])
                    axis.set_xlabel("Workers; fixed four files and 1 GiB logical writes per round")
                    axis.grid(alpha=0.15)
            axes[0][column].set_title(
                f"Retained working set {wss / MIB:g} MiB (fixed R=512 MiB reference)"
            )
            axes[0][column].set_ylabel("Logical sync-completed write throughput (MiB/s)")
            axes[1][column].set_ylabel("Final synchronization / completed task time")
        if not expanded:
            axes[0][0].legend(fontsize=7)
        save(fig, "resource-workers")

    grain = [case for case in mechanisms if case["id"] in ("M06", "M07")]
    if len(grain) == 2:
        fig, axes = plt.subplots(1, 2, figsize=(11, 5.3 if expanded else 4.5))
        for ax, metric in zip(axes, ("work_ns", "completed_ns")):
            for backend in backends:
                xs, ys = [], []
                for case in grain:
                    row = levels.get((case["id"], backend, "sum_three_rounds", metric))
                    if not row:
                        continue
                    written = case["parameters"]["logical_write_bytes"] * 3
                    points = [written / MIB / (time / 1000) for time in row["points_ms"]]
                    block = case["parameters"]["block_bytes"] / 1024
                    xs.append(block)
                    ys.append(statistics.median(points))
                    ax.scatter(
                        [block] * len(points), points, color=colors[backend], s=18, alpha=0.4
                    )
                ax.plot(
                    xs, ys, marker="o", markersize=4, color=colors[backend], label=labels[backend]
                )
            ax.set_xscale("log", base=2)
            ax.set_xticks([1, 1024], ["1 KiB", "1 MiB"])
            ax.set_xlabel("Request size; two anchors, fixed 4 MiB sequential write per round")
            ax.set_ylabel("Logical write throughput (MiB/s)")
            ax.set_title(
                "Work-return throughput" if metric == "work_ns" else "Sync-completed throughput"
            )
            ax.grid(alpha=0.15)
        if not expanded:
            axes[0].legend(fontsize=7)
        save(fig, "write-request-size")
    return paths


def report(summary):
    lines = [
        "# Filesystem cross-mode workload results",
        "",
        "Three complete workflows are primary results; mechanism and resource anchors explain their boundaries. No overall score is calculated.",
        "",
        json.dumps(summary["counts"], sort_keys=True),
        "",
        "Each number is the median of independent fresh environments. Three rounds are summed inside each environment first. "
        "Round-level results and every retained observation are in CSV. N=3 is exploratory; no confidence intervals or operation tails are claimed.",
        "",
        "| Case | Mode | Three-round work ms | Three-round final-sync ms | Three-round completed ms | R1 completed ms | R2 completed ms | R3 completed ms |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    levels = {
        (row["case_id"], row["backend"], row["scope"], row["metric"]): row
        for row in summary["levels"]
    }
    for case in sorted(
        summary["catalog"], key=lambda case: (case["layer"] != "scenario", case["id"])
    ):
        for backend in summary["backends"]:
            values = [
                levels.get((case["id"], backend, scope, metric))
                for scope, metric in [
                    ("sum_three_rounds", "work_ns"),
                    ("sum_three_rounds", "final_sync_ns"),
                    ("sum_three_rounds", "completed_ns"),
                    ("round_1", "completed_ns"),
                    ("round_2", "completed_ns"),
                    ("round_3", "completed_ns"),
                ]
            ]
            text = [f"{row['median_ms']:.3f}" if row else "incomplete" for row in values]
            lines.append(f"| {case['id']} {case['name']} | {backend} | " + " | ".join(text) + " |")
    lines += [
        "",
        "Medians of work and final synchronization need not add to the median of their sum.",
        "",
        "Ready is launch to the payload Ready line, before case setup, and excludes teardown. Work and final synchronization "
        "are separate payload clocks. Explicit periodic fsync and checked closes remain inside work. "
        "Validation and setup are excluded, but can warm subsequent resident rounds. "
        "Resource retained-byte anchors share a fixed 512 MiB reference, which is not native host RAM.",
        "",
        "Failures and missing observations:",
        "",
    ]
    lines += [json.dumps(failure, sort_keys=True) for failure in summary["failures"]] or ["None."]
    return "\n".join(lines) + "\n"


def analyze(batch, output=None, *, plots=True):
    batch = Path(batch).resolve()
    protocol_path, records_path = batch / "protocol.json", batch / "records.json"
    protocol, loaded = json.loads(protocol_path.read_text()), json.loads(records_path.read_text())
    result = summarize(protocol, loaded.get("records") if isinstance(loaded, dict) else loaded)
    result["provenance"] = dict(
        protocol_sha256=digest(protocol_path),
        records_sha256=digest(records_path),
        analyzer_sha256=digest(__file__),
    )
    output = Path(output).resolve() if output else batch / "analysis"
    require(".data" in output.parts, "analysis artifacts must remain under .data")
    output.mkdir(parents=True, exist_ok=False)
    for key in ("levels", "ready", "environments", "raw_rows", "failures"):
        csv_file(output / f"{key}.csv", result[key])
    result["plots"] = plot(result, output) if plots else []
    (output / "report.md").write_text(report(result))
    (output / "summary.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--no-plots", action="store_true")
    args = parser.parse_args(argv)
    try:
        summary = analyze(args.batch, args.output, plots=not args.no_plots)
    except (ValueError, OSError, ImportError) as error:
        parser.exit(1, str(error) + "\n")
    print(
        json.dumps(dict(status=summary["status"], counts=summary["counts"], plots=summary["plots"]))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
