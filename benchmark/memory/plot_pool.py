#!/usr/bin/env python3
"""Audit and plot retained B-MEMORY-SCALE preflight curves, without measuring."""

import argparse
import json
import statistics
from pathlib import Path

from pool_compare import (
    ARMS,
    CURRENT_SCHEMA,
    PATTERNS,
    digest,
    validate_observation,
    validate_worker,
    values,
)


def sustained_drop(curve, reference, drop_mib=128):
    """Observed bracket for a drop sustained until 60s; unreached stays censored."""
    window = [(x, y) for x, y in curve if 0 <= x <= 60]
    target = reference - drop_mib
    result = dict(
        reference_mib=reference,
        drop_mib=drop_mib,
        reached=False,
        crossing_interval_seconds=None,
        observation_scope="sampled memory, sustained until 60 seconds",
    )
    if not window or window[-1][1] > target:
        return result
    last_high = max((n for n, (_, y) in enumerate(window) if y > target), default=-1)
    result.update(
        reached=True,
        crossing_interval_seconds=[
            0 if last_high < 0 else window[last_high][0],
            window[last_high + 1][0],
        ],
    )
    return result


def select_arms(report, wanted, arms, *, formal=False):
    """Explicit preflight projection; formal cohorts must keep their full grid."""
    if arms is not None and formal:
        raise ValueError("formal cohorts cannot project conditions")
    selected = list(report["arms"]) if arms is None else list(arms)
    if (
        not selected
        or len(selected) != len(set(selected))
        or not set(selected) <= wanted
        or not set(selected) <= set(report["arms"])
    ):
        raise ValueError("select supported measured arms explicitly for historical preflights")
    result = {**report, "arms": selected}
    if arms is not None:
        result["selection"] = dict(
            included=selected,
            excluded=[arm for arm in report["arms"] if arm not in selected],
            scope="selected preflight conditions audited; original report and raw measurements retained unchanged",
        )
    return result


def audit(root, *, formal=False, arms=None):
    report = json.loads((root / "report.json").read_text())
    if not report.get("complete"):
        raise ValueError("incomplete cohort")
    library = report.get("campaign") == "library"
    if library:
        from library_protocol import VARIANTS

        wanted_arms, wanted_patterns = set(VARIANTS), {"random-shared"}
    else:
        if report.get("campaign", "static") != "static":
            raise ValueError("unknown campaign")
        wanted_arms, wanted_patterns = set(ARMS), set(PATTERNS)
    if formal:
        if (
            report.get("preflight") is not False
            or report.get("samples", 0) < 5
            or set(report["arms"]) != wanted_arms
            or set(report["patterns"]) != wanted_patterns
        ):
            raise ValueError("formal analysis requires all conditions and at least five rounds")
    elif not report.get("preflight") or report.get("samples") != 1:
        raise ValueError("this renderer only accepts complete, separate N=1 preflights")
    expected = {
        (n, a, p)
        for n in range(report["samples"])
        for a in report["arms"]
        for p in report["patterns"]
    }
    rows = report["attempts"]
    if (
        len(rows) != len(expected)
        or {
            (r.get("round", 0), r.get("library_variant") if library else r["arm"], r["pattern"])
            for r in rows
        }
        != expected
    ):
        raise ValueError("missing or duplicated conditions")
    report = select_arms(report, wanted_arms, arms, formal=formal)
    frozen = root / "harness/pool_compare.py"
    if not frozen.is_file() or digest(frozen) != report["harness_sha256"]:
        raise ValueError("exact measured harness bytes must be retained")
    docker_selected = any(arm.startswith("docker-rootless") for arm in report["arms"])
    if formal or library or docker_selected:
        files = report["harness_files"]
        if len({Path(n).name for n in files}) != len(files):
            raise ValueError("ambiguous harness names")
        if any(
            digest(root / "harness" / Path(n).name) != checksum for n, checksum in files.items()
        ):
            raise ValueError("frozen supporting harness changed")
    result = []
    input_directories = set()
    docker_directories = set()
    for i, row in enumerate(rows):
        folder = root / str(i)
        if row.get("status") != "successful" or not row.get("unit_quiescent"):
            raise ValueError("failed or unreaped condition")
        worker = folder / "worker.json"
        if digest(worker) != row["worker_sha256"]:
            raise ValueError("worker report changed")
        identity = row.get("library_variant") if library else row["arm"]
        if identity not in report["arms"]:
            continue
        raw = json.loads(worker.read_text())
        cfg = json.loads((folder / "config.json").read_text())
        if library:
            from library_protocol import details

            details(cfg)
            if row.get("library_variant") != cfg.get("library_variant") or row["arm"] != cfg["arm"]:
                raise ValueError("library row/condition mismatch")
        elif "library_variant" in cfg:
            raise ValueError("library case cannot populate static worker campaign")
        input_directories.add(cfg["inputs"])
        if cfg.get("docker_inputs"):
            docker_directories.add(cfg["docker_inputs"])
        if formal and cfg.get("schema") != CURRENT_SCHEMA:
            raise ValueError("formal cohort requires current placement/KSM evidence")
        if formal or library or cfg["arm"] == "docker-rootless":
            observations = row.get("observation_hashes", {})
            if set(observations) != {"monitor.json", "pss.json", "guard.json"}:
                raise ValueError("formal observation hashes missing")
            if any(digest(folder / n) != checksum for n, checksum in observations.items()):
                raise ValueError("formal observation changed")
        validate_worker(raw, cfg)
        if cfg["arm"] == "docker-rootless":
            from docker_support import validate_case

            retained = row.get("docker_evidence_hashes", {})
            if not retained or set(retained) != {
                path.name for path in folder.glob("docker-*.json")
            }:
                raise ValueError("Docker runtime evidence inventory missing")
            if any(digest(folder / name) != checksum for name, checksum in retained.items()):
                raise ValueError("Docker runtime evidence changed")
            validate_case(raw, cfg, folder)
        memory = json.loads((folder / "monitor.json").read_text())
        pss = json.loads((folder / "pss.json").read_text())
        validate_observation(
            memory,
            json.loads((folder / "guard.json").read_text()),
            tasks_max=256 if cfg.get("schema") == CURRENT_SCHEMA else 128,
        )
        if not pss or any("error" in p for p in pss):
            raise ValueError("missing PSS curve")
        origin = raw["all_ready_ns"]
        charged = [
            ((p["time_ns"] - origin) / 1e9, p["memory_current"] / 2**20)
            for p in memory
            if "error" not in p
        ]
        resident = [((p["monotonic_ns"] - origin) / 1e9, p["pss_bytes"] / 2**20) for p in pss]
        if any(curve[0][0] > 0 or curve[-1][0] < 60 for curve in (charged, resident)):
            raise ValueError("curve does not cover the full all-ready window")
        if "post_write_origin_ns" in raw:
            offset = (raw["post_write_origin_ns"] - origin) / 1e9
            if any(
                curve[0][0] > offset or curve[-1][0] < offset + 30 for curve in (charged, resident)
            ):
                raise ValueError("curve does not cover the full post-write window")
        phases = {p["name"]: p for p in raw["phases"]}
        summary = dict(
            arm=row["arm"],
            pattern=row["pattern"],
            round=row.get("round", 0),
            n=1,
            first_full_read_ms=raw["first_full_read_ms"],
        )
        summary["startup_seconds_until_all_ready"] = (origin - raw["before"]["monotonic_ns"]) / 1e9
        if summary["startup_seconds_until_all_ready"] < 0:
            raise ValueError("all-ready precedes Instance-group setup")
        summary["before_start"] = dict(
            charged_mib=int(raw["before"]["counters"]["memory.current"]) / 2**20,
            pss_mib=sum(v["pss_bytes"] for v in raw["before"]["processes"]) / 2**20,
        )
        if library:
            from library_protocol import validate_library

            evidence = validate_library(raw, cfg, folder)
            summary.update(
                arm=row["library_variant"],
                library_evidence=evidence,
                library_audit_wait_seconds=raw["library_audit"]["wait_seconds"],
            )
        for p in raw["phases"]:
            name = p["name"]
            memory_types = values(p["counters"]["memory.stat"])
            summary[name] = dict(
                charged_mib=int(p["counters"]["memory.current"]) / 2**20,
                actual_seconds_after_ready=(p["monotonic_ns"] - origin) / 1e9,
                pss_mib=sum(v["pss_bytes"] for v in p["processes"]) / 2**20,
                ksm_mapped_mib=sum(
                    v["mapping_evidence"].get("ksm_bytes", 0) for v in p["processes"]
                )
                / 2**20,
                **{
                    f"{kind}_mib": memory_types[kind] / 2**20
                    for kind in ("anon", "file", "kernel", "pagetables", "slab")
                },
            )
            if "post_write_origin_ns" in raw:
                summary[name]["actual_seconds_after_write"] = (
                    p["monotonic_ns"] - raw["post_write_origin_ns"]
                ) / 1e9
        summary["sustained_drop_128_mib"] = {
            "charged": sustained_drop(charged, summary["idle-0"]["charged_mib"]),
            "pss": sustained_drop(resident, summary["idle-0"]["pss_mib"]),
        }
        summary["whole_condition_peak_mib"] = int(raw["after"]["counters"]["memory.peak"]) / 2**20
        summary["observed_idle_peak_mib"] = max(y for x, y in charged if 0 <= x <= 60)
        if "post_write_origin_ns" in raw:
            summary["post_write_offset_seconds"] = (raw["post_write_origin_ns"] - origin) / 1e9
            summary["post_write_cpu_seconds"] = (
                values(phases["post-write-30"]["counters"]["cpu.stat"])["usage_usec"]
                - values(phases["cow-100"]["counters"]["cpu.stat"])["usage_usec"]
            ) / 1e6
        summary["idle_cpu_seconds"] = (
            values(phases["idle-60"]["counters"]["cpu.stat"])["usage_usec"]
            - values(phases["idle-0"]["counters"]["cpu.stat"])["usage_usec"]
        ) / 1e6
        starts = {p["pid"]: p for p in phases["idle-0"]["processes"]}
        guests = set(phases["idle-0"]["guest_pids"])
        control = {p["pid"] for p in raw["before"]["processes"]}
        summary["idle_process_cpu"] = []
        for process in phases["idle-60"]["processes"]:
            first = starts.get(process["pid"])
            if not first or "cpu_user_seconds" not in first:
                continue
            if first["starttime"] != process["starttime"]:
                raise ValueError("process identity changed")
            summary["idle_process_cpu"].append(
                dict(
                    pid=process["pid"],
                    comm=process["comm"],
                    role="vm"
                    if process["pid"] in guests
                    else "control"
                    if process["pid"] in control
                    else "runtime"
                    if cfg["arm"] == "docker-rootless"
                    else "pool",
                    user_seconds=process["cpu_user_seconds"] - first["cpu_user_seconds"],
                    system_seconds=process["cpu_system_seconds"] - first["cpu_system_seconds"],
                )
            )
        if summary["idle_process_cpu"]:
            for role in ("vm", "control", "pool", "runtime"):
                for kind in ("user", "system"):
                    summary[f"{role}_{kind}_cpu_seconds"] = sum(
                        p[f"{kind}_seconds"]
                        for p in summary["idle_process_cpu"]
                        if p["role"] == role
                    )
        if "ksm" in phases["idle-0"]:
            first, last = phases["idle-0"]["ksm"], phases["idle-60"]["ksm"]
            if (first["pid"], first["starttime"]) != (last["pid"], last["starttime"]):
                raise ValueError("ksmd restarted during the scan window")
            summary["global_ksmd_cpu_seconds"] = last["cpu_seconds"] - first["cpu_seconds"]
        result.append((summary, charged, resident))
    if formal or library or docker_selected:
        if len(input_directories) != 1:
            raise ValueError("mixed input cohorts")
        inputs = Path(next(iter(input_directories)))
        receipt = json.loads((inputs / "receipt.json").read_text())
        if receipt != report["receipt"]:
            raise ValueError("input receipt changed")
        if any(digest(inputs / n) != checksum for n, checksum in receipt["hashes"].items()):
            raise ValueError("frozen input bytes changed")
        if (
            digest(inputs / "source.tar.gz") != receipt["source_archive_sha256"]
            or digest(inputs / "source-files.json") != receipt["source_manifest_sha256"]
        ):
            raise ValueError("product source evidence changed")
        if "firecracker_build" in receipt:
            fc = json.loads((inputs / "firecracker-build-receipt.json").read_text())
            if fc != receipt["firecracker_build"]:
                raise ValueError("Firecracker build receipt changed")
            source = Path(fc["source_archive_directory"])
            if any(digest(source / n) != checksum for n, checksum in fc["hashes"].items()):
                raise ValueError("Firecracker source/build evidence changed")
        if library:
            from library_compare import verify_inputs

            verify_inputs(inputs, receipt)
    if docker_selected:
        from docker_support import verify_inputs as verify_docker_inputs

        if len(docker_directories) != 1:
            raise ValueError("mixed or missing Docker input cohort")
        docker_inputs = Path(next(iter(docker_directories)))
        docker_receipt = json.loads((docker_inputs / "receipt.json").read_text())
        if docker_receipt != report.get("docker_receipt") or docker_receipt[
            "pool_receipt_sha256"
        ] != digest(inputs / "receipt.json"):
            raise ValueError("Docker input receipt/source changed")
        verify_docker_inputs(docker_inputs, docker_receipt)
    return report, result


def layout_figure(figure, axes, arms):
    """Keep long library-condition legends outside the measured curves."""
    if len(axes) == 1 and any(len(arm) > 24 for arm in arms):
        handles, labels = axes[0][0].get_legend_handles_labels()
        indexed = dict(zip(labels, handles))
        for axis in axes.flat:
            legend = axis.get_legend()
            if legend is not None:
                legend.remove()
        figure.legend([indexed[arm] for arm in arms], arms, loc="lower center", ncol=2, fontsize=8)
        width, height = figure.get_size_inches()
        figure.set_size_inches(width, max(height, 5))
        figure.tight_layout(rect=(0, 0.20, 1, 0.94))
    else:
        figure.tight_layout()


def plot_startup(rows, patterns, arms, output, title):
    """Keep startup sharing visible instead of hiding it behind all-ready zero."""
    import matplotlib.pyplot as plt

    colors = {arm: plt.get_cmap("tab10")(n) for n, arm in enumerate(arms)}
    figure, axes = plt.subplots(len(patterns), 2, figsize=(12, 3.8 * len(patterns)), squeeze=False)
    for index, pattern in enumerate(patterns):
        for arm in arms:
            selected = [r for r in rows if (r[0]["pattern"], r[0]["arm"]) == (pattern, arm)]
            for axis, metric, curve_index in zip(axes[index], ("charged_mib", "pss_mib"), (1, 2)):
                for n, row in enumerate(selected):
                    summary, curve = row[0], row[curve_index]
                    offset = summary["startup_seconds_until_all_ready"]
                    ready = summary["idle-0"]["actual_seconds_after_ready"]
                    points = [(0, summary["before_start"][metric])]
                    points += [(x + offset, y) for x, y in curve if -offset < x < ready]
                    points += [(offset + ready, summary["idle-0"][metric])]
                    axis.plot(
                        [x for x, _ in points],
                        [y for _, y in points],
                        color=colors[arm],
                        marker=".",
                        markersize=3,
                        linewidth=1,
                        alpha=0.3 if len(selected) > 1 else 1,
                        label=arm if n == 0 else None,
                    )
                    axis.scatter(
                        [offset + ready],
                        [summary["idle-0"][metric]],
                        facecolors="none",
                        edgecolors=[colors[arm]],
                        s=25,
                    )
        for axis, metric in zip(axes[index], ("Whole-group charged memory", "Sum of process PSS")):
            axis.set(
                title=f"{pattern}: {metric}",
                xlabel="Seconds after initial group snapshot\nDots: recorded samples; open circles: all-ready snapshot",
                ylabel="MiB",
                xlim=(0, None),
            )
            axis.grid(alpha=0.25)
            axis.legend(fontsize=8)
    figure.suptitle(title, fontsize=12)
    layout_figure(figure, axes, arms)
    figure.savefig(output / "startup-memory-curves.svg")
    figure.savefig(output / "startup-memory-curves.png", dpi=140)


def plot_post_write(rows, patterns, arms, output, title):
    present = ["post_write_offset_seconds" in row[0] for row in rows]
    if not any(present):
        return
    if not all(present):
        raise ValueError("mixed post-write observation profiles")
    import matplotlib.pyplot as plt

    figure, axes = plt.subplots(len(patterns), 2, figsize=(12, 3.8 * len(patterns)), squeeze=False)
    names = ["cow-100"] + [f"post-write-{n}" for n in (1, 2, 5, 10, 20, 30)]
    for index, pattern in enumerate(patterns):
        for arm in arms:
            selected = [r for r in rows if r[0]["pattern"] == pattern and r[0]["arm"] == arm]
            for axis, metric, curve_index in zip(axes[index], ("charged_mib", "pss_mib"), (1, 2)):
                color = None
                for row in selected:
                    offset = row[0]["post_write_offset_seconds"]
                    points = [
                        (x - offset, y) for x, y in row[curve_index] if offset <= x <= offset + 30
                    ]
                    (line,) = axis.plot(
                        [x for x, _ in points],
                        [y for _, y in points],
                        color=color,
                        alpha=0.2 if len(selected) > 1 else 1,
                        label=arm if len(selected) == 1 else None,
                    )
                    color = line.get_color()
                if len(selected) > 1:
                    axis.plot(
                        [
                            statistics.median(
                                r[0][name]["actual_seconds_after_write"] for r in selected
                            )
                            for name in names
                        ],
                        [statistics.median(r[0][name][metric] for r in selected) for name in names],
                        color=color,
                        linewidth=2,
                        label=arm,
                    )
        for axis, metric in zip(axes[index], ("Whole-group charged memory", "Sum of process PSS")):
            axis.set(
                title=f"{pattern}: {metric}",
                xlabel="Seconds after full private writes and peer checks",
                ylabel="MiB",
                xlim=(0, 30),
            )
            axis.grid(alpha=0.25)
            axis.legend(fontsize=8)
    figure.suptitle(title, fontsize=12)
    layout_figure(figure, axes, arms)
    figure.savefig(output / "post-write-curves.svg")
    figure.savefig(output / "post-write-curves.png", dpi=140)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("report_directory", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument(
        "--arms",
        help="explicit comma-separated supported arms for a historical preflight projection",
    )
    a = p.parse_args()
    report, rows = audit(a.report_directory, arms=None if a.arms is None else a.arms.split(","))
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    a.output.mkdir()
    patterns = report["patterns"]
    colors = {arm: plt.get_cmap("tab10")(n) for n, arm in enumerate(report["arms"])}
    figure, axes = plt.subplots(len(patterns), 2, figsize=(12, 3.8 * len(patterns)), squeeze=False)
    for index, pattern in enumerate(patterns):
        for summary, charged, resident in rows:
            if summary["pattern"] != pattern:
                continue
            for axis, curve in zip(axes[index], (charged, resident)):
                observed = [(x, y) for x, y in curve if -0.5 <= x <= 60]
                axis.plot(
                    [p[0] for p in observed],
                    [p[1] for p in observed],
                    label=summary["arm"],
                    color=colors[summary["arm"]],
                    linewidth=1.7,
                )
        for axis, metric in zip(axes[index], ("Whole-group charged memory", "Sum of process PSS")):
            axis.set(
                title=f"{pattern}: {metric}",
                xlabel="Seconds after all instances ready",
                ylabel="MiB",
                xlim=(0, 60),
            )
            axis.grid(alpha=0.25)
            axis.legend(fontsize=8)
    workload = "dlopen library" if report.get("campaign") == "library" else "static worker"
    figure.suptitle(
        f"Four instances, identical {workload} — diagnostic preflight (N=1 per condition)",
        fontsize=12,
    )
    layout_figure(figure, axes, report["arms"])
    figure.savefig(a.output / "memory-curves.svg")
    figure.savefig(a.output / "memory-curves.png", dpi=140)
    if report.get("campaign") == "library":
        for axis in axes.flat:
            axis.set_xlim(0, 5)
        figure.suptitle(
            "First five seconds after all library instances ready — diagnostic preflight (N=1)",
            fontsize=12,
        )
        figure.savefig(a.output / "early-memory-curves.svg")
        figure.savefig(a.output / "early-memory-curves.png", dpi=140)
    plot_startup(
        rows,
        patterns,
        report["arms"],
        a.output,
        "Instance-group setup through all-ready; scanners may work during sequential startup (N=1)",
    )
    plot_post_write(
        rows,
        patterns,
        report["arms"],
        a.output,
        "After 100% private writes — diagnostic preflight (N=1 per condition)",
    )
    summary = dict(
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        preflight=True,
        report_sha256=digest(a.report_directory / "report.json"),
        audit_sha256=digest(Path(__file__)),
        measured_harness_sha256=report["harness_sha256"],
        caution="N=1 observations; no confidence intervals, tails, pure-codec or pure-VMM speedup claim",
        cases=[r[0] for r in rows],
        caveats=report["caveats"],
    )
    if "selection" in report:
        summary["selection"] = report["selection"]
    (a.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(a.output / "memory-curves.svg")


if __name__ == "__main__":
    main()
