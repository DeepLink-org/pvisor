#!/usr/bin/env python3
"""B-FS-ENG: plot recorded resource-worker submission progress; no subjects run.

The x clock is guest worker 0's elapsed submission time. The y value is bytes
whose pwrite returned to that worker. Neither coordinate is host-applied work,
queue occupancy, dirty memory or whole-environment completed throughput.
Four-worker points remain worker 0's own bytes, never multiplied by four.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from pathlib import Path

MIB = 1 << 20
CASES = ("R01", "R02", "R03", "R04")
ARMS = ("baseline", "candidate")


def require(value, message):
    if not value:
        raise ValueError(message)


def positive_integer(value):
    return type(value) is int and value > 0


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def validate_progress(points, *, case_id, round_number, expected_final_bytes):
    """Validate a single worker/round stream without sorting or repairing it.

    Input order is observation order. Reject mixed identity/scope, non-increasing
    guest time/bytes and incomplete final submission. Observer times are retained
    separately and must also increase; no guest/observer clock alignment follows.
    """
    require(points, f"{case_id}: no progress points for round {round_number}")
    previous_guest = previous_bytes = previous_observer = -1
    validated = []
    for index, point in enumerate(points):
        require(
            isinstance(point, dict) and isinstance(point.get("progress"), dict),
            f"{case_id}: invalid observation {index}",
        )
        progress = point["progress"]
        require(
            progress.get("id") == case_id and progress.get("round") == round_number,
            f"{case_id}: mixed case or round",
        )
        require(
            type(progress.get("worker")) is int
            and progress["worker"] == 0
            and progress.get("scope") == "worker_submission",
            f"{case_id}: mixed worker or progress scope",
        )
        guest_ns, count, observer_ns = (
            progress.get("elapsed_ns"),
            progress.get("bytes_written"),
            point.get("elapsed_ns"),
        )
        require(
            all(positive_integer(value) for value in (guest_ns, count, observer_ns)),
            f"{case_id}: clocks and bytes must be positive integers",
        )
        require(
            guest_ns > previous_guest
            and count > previous_bytes
            and observer_ns > previous_observer,
            f"{case_id}: non-monotonic guest clock, submission or observer clock",
        )
        require(count <= expected_final_bytes, f"{case_id}: worker submitted beyond target")
        validated.append(
            dict(
                guest_elapsed_ns=guest_ns,
                observer_elapsed_ns=observer_ns,
                worker_submitted_bytes=count,
            )
        )
        previous_guest, previous_bytes, previous_observer = guest_ns, count, observer_ns
    require(
        previous_bytes == expected_final_bytes, f"{case_id}: final worker submission is incomplete"
    )
    return validated


def collect(batch):
    protocol_path, records_path = batch / "protocol.json", batch / "records.json"
    require(records_path.is_file(), "records.json missing; wait until campaign completes")
    protocol = json.loads(protocol_path.read_text())
    records = json.loads(records_path.read_text())
    require(protocol.get("diagnostic") is False, "expected unprofiled formal batch")
    require(
        protocol.get("samples") == 3 and protocol.get("resident_rounds") == 3,
        "this first-round figure requires N3 environments and three resident rounds",
    )
    catalog = {case["id"]: case for case in protocol["catalog"]}
    selected = [case for case in CASES if case in protocol["inventory"]["selected_cell_ids"]]
    require(selected, "no resource cases selected")
    index = {}
    for record in records:
        case_id = record.get("case_id")
        if case_id not in selected:
            continue
        key = (case_id, record.get("condition"), record.get("pair"))
        require(
            key[1] in ARMS and type(key[2]) is int and key[2] in range(3),
            f"unexpected resource environment {key}",
        )
        require(key not in index, f"duplicate resource environment {key}")
        index[key] = record
    sources = {
        str(protocol_path.relative_to(batch)): sha(protocol_path),
        str(records_path.relative_to(batch)): sha(records_path),
    }
    rows, failures, complete_cases = [], [], []
    for case_id in selected:
        parameters = catalog[case_id]["parameters"]
        workers, target = parameters["workers"], parameters["logical_write_bytes"]
        require(workers in (1, 4) and target % workers == 0, "invalid resource work contract")
        case_rows, case_failures = [], []
        for arm in ARMS:
            for pair in range(3):
                key = (case_id, arm, pair)
                record = index.get(key)
                if (
                    not record
                    or record.get("valid") is not True
                    or record.get("host_verified") is not True
                ):
                    case_failures.append(
                        dict(
                            case_id=case_id,
                            condition=arm,
                            pair=pair,
                            reason="missing or invalid environment",
                        )
                    )
                    continue
                folder = batch / "trials" / f"{case_id}-{pair:03}-{arm}"
                observations_path, result_path = (
                    folder / "observations.json",
                    folder / "result.json",
                )
                observations = json.loads(observations_path.read_text())
                sources[str(observations_path.relative_to(batch))] = sha(observations_path)
                sources[str(result_path.relative_to(batch))] = sha(result_path)
                all_progress = [point for point in observations if "progress" in point]
                # Check every recorded resource progress identity/scope before
                # selecting round one; never hide a mixed stream by filtering it.
                for point in all_progress:
                    progress = point["progress"]
                    require(
                        isinstance(progress, dict)
                        and progress.get("id") == case_id
                        and type(progress.get("round")) is int
                        and progress["round"] in (1, 2, 3)
                        and type(progress.get("worker")) is int
                        and progress["worker"] == 0
                        and progress.get("scope") == "worker_submission",
                        f"{key}: mixed progress identity or scope",
                    )
                points = [point for point in all_progress if point["progress"]["round"] == 1]
                stream = validate_progress(
                    points, case_id=case_id, round_number=1, expected_final_bytes=target // workers
                )
                for point in stream:
                    case_rows.append(
                        dict(
                            case_id=case_id,
                            condition=arm,
                            pair=pair,
                            round=1,
                            worker=0,
                            scope="worker_submission",
                            workers=workers,
                            working_set_bytes=parameters["working_set_bytes"],
                            worker_target_bytes=target // workers,
                            source=str(observations_path.relative_to(batch)),
                            **point,
                        )
                    )
        failures.extend(case_failures)
        rows.extend(case_rows)
        if not case_failures:
            complete_cases.append(case_id)
    return rows, complete_cases, failures, sources, catalog


def plot(rows, complete_cases, catalog, output):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.lines import Line2D

    plt.rcParams.update({"font.size": 10, "pdf.fonttype": 42, "svg.fonttype": "none"})
    fig, axes = plt.subplots(2, 2, figsize=(12, 8), squeeze=False)
    colors = {"baseline": "#b34a35", "candidate": "#267ca8"}
    labels = {"baseline": "Guest WB off", "candidate": "Guest WB on"}
    styles = ("-", "--", ":")
    for case_id, ax in zip(CASES, axes.flat):
        parameters = catalog.get(case_id, {}).get("parameters", {})
        if case_id not in complete_cases:
            ax.text(
                0.5,
                0.5,
                "Complete paired case unavailable",
                ha="center",
                va="center",
                transform=ax.transAxes,
            )
            ax.set_title(case_id)
            continue
        for arm in ARMS:
            for pair in range(3):
                stream = [
                    row
                    for row in rows
                    if row["case_id"] == case_id and row["condition"] == arm and row["pair"] == pair
                ]
                ax.plot(
                    [point["guest_elapsed_ns"] / 1e9 for point in stream],
                    [point["worker_submitted_bytes"] / MIB for point in stream],
                    color=colors[arm],
                    linestyle=styles[pair],
                    linewidth=1.6,
                    marker=".",
                    markersize=3,
                )
        workers = parameters["workers"]
        ax.set_title(
            f"{case_id}: {workers} worker(s), WSS {parameters['working_set_bytes'] / MIB:g} MiB"
        )
        ax.set_xlim(left=0)
        ax.set_ylim(bottom=0)
        ax.set_xlabel("Guest worker 0 submission elapsed time (s)")
        ax.set_ylabel("Worker 0 submitted MiB")
        ax.grid(alpha=0.22)
    handles = [Line2D([0], [0], color=colors[arm], label=labels[arm]) for arm in ARMS]
    handles += [
        Line2D(
            [0], [0], color="#555555", linestyle=styles[pair], label=f"Environment pair {pair + 1}"
        )
        for pair in range(3)
    ]
    fig.legend(handles=handles, loc="lower center", ncol=5, bbox_to_anchor=(0.5, 0.035))
    fig.suptitle("First-round resource progress: all three independent environments per arm")
    fig.text(
        0.5,
        0.006,
        "pwrite returns to worker 0; NOT host-applied bytes or aggregate four-worker progress. "
        "No synthetic zero point; first recorded point is 64 MiB.",
        ha="center",
        fontsize=9,
    )
    fig.tight_layout(rect=(0, 0.08, 1, 0.95))
    files = []
    for extension in ("svg", "pdf"):
        path = output / f"resource-worker-submission.{extension}"
        fig.savefig(path)
        files.append(path)
    plt.close(fig)
    return files, matplotlib.__version__


def analyze(batch, output):
    batch = batch.resolve()
    rows, complete_cases, failures, sources, catalog = collect(batch)
    require(complete_cases, "no complete resource case available for plotting")
    output.mkdir(parents=True, exist_ok=False)
    fields = (
        "case_id",
        "condition",
        "pair",
        "round",
        "worker",
        "scope",
        "workers",
        "working_set_bytes",
        "worker_target_bytes",
        "guest_elapsed_ns",
        "observer_elapsed_ns",
        "worker_submitted_bytes",
        "source",
    )
    csv_path = output / "resource-worker-submission.csv"
    with csv_path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)
    files, matplotlib_version = plot(rows, complete_cases, catalog, output)
    manifest = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        batch=str(batch),
        role="derived first-round guest worker submission progress; no subject execution",
        command=[sys.executable, *sys.argv],
        helper_sha256=sha(__file__),
        complete_cases=complete_cases,
        failures=failures,
        points=len(rows),
        sampled_environments_per_case_per_arm=3,
        dependent_round=1,
        guest_x_origin="worker 0 submission start, guest Instant elapsed clock",
        observer_time="retained in CSV; launch-relative clock, not aligned to guest x axis",
        y_scope="worker 0 pwrite-returned bytes; never multiplied by worker count",
        limitations=[
            "not host-applied bytes or sync completion",
            "not aggregate worker progress",
            "no queue/dirty-memory/backpressure inference from submission alone",
            "64 MiB sampling can hide shorter stalls; line segments interpolate samples",
            "no cross-case summed score or stable N3 confirmation",
        ],
        inputs=sources,
        matplotlib_version=matplotlib_version,
        outputs={path.name: sha(path) for path in [csv_path, *files]},
    )
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def self_test():
    """Exercise stream integrity rejection without running a benchmark."""

    def point(ns, count, observer, **changes):
        progress = dict(
            id="R01",
            round=1,
            worker=0,
            scope="worker_submission",
            elapsed_ns=ns,
            bytes_written=count,
        )
        progress.update(changes)
        return dict(elapsed_ns=observer, progress=progress)

    valid = [point(10, 64, 100), point(20, 128, 200)]
    assert (
        len(validate_progress(valid, case_id="R01", round_number=1, expected_final_bytes=128)) == 2
    )
    failures = [
        [],
        [point(20, 64, 100), point(10, 128, 200)],
        [point(10, 128, 100), point(20, 64, 200)],
        [point(10, 64, 200), point(20, 128, 100)],
        [point(10, 128, 100, scope="host_applied")],
        [point(10, 128, 100, worker=1)],
        [point(10, 128, 100, round=2)],
        [point(10, 128, 100, id="R02")],
        [point(True, 128, 100)],
        [point(10, 64, 100)],
        [point(10, 256, 100)],
    ]
    for invalid in failures:
        try:
            validate_progress(invalid, case_id="R01", round_number=1, expected_final_bytes=128)
        except ValueError:
            continue
        raise AssertionError(f"invalid progress accepted: {invalid}")
    return dict(valid_streams=1, rejected_invalid_streams=len(failures))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", nargs="?", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.self_test:
            require(args.batch is None and args.output is None, "--self-test takes no batch/output")
            print(json.dumps(self_test()))
        else:
            require(args.batch is not None, "batch is required")
            output = args.output or args.batch / "progress-analysis"
            manifest = analyze(args.batch, output)
            print(
                json.dumps(
                    dict(
                        output=str(output),
                        complete_cases=manifest["complete_cases"],
                        points=manifest["points"],
                        failures=manifest["failures"],
                    )
                )
            )
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
