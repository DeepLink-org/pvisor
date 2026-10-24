#!/usr/bin/env python3
"""Audit a complete randomized B-MEMORY-SCALE cohort and retain paired estimates."""

import argparse
import json
import math
import random
import statistics
from pathlib import Path

from plot_pool import audit, layout_figure, plot_post_write, plot_startup
from pool_compare import ARMS, BOUNDARIES, PATTERNS, digest


def interval(samples):
    """Paired-round median difference interval; never resample arms separately."""
    rng = random.Random(20261018)
    estimates = sorted(statistics.median(rng.choices(samples, k=len(samples))) for _ in range(5000))
    return [estimates[124], estimates[4874]]


def summarize(cases, rounds, *, campaign="static"):
    if campaign == "library":
        from library_protocol import VARIANTS

        arms, patterns = tuple(VARIANTS), ("random-shared",)
        controls = (
            "firecracker-block",
            "docker-rootless-library",
            "pvisor-rootfs-ksm",
            "pvisor-rootfs-physical-no-hints",
        )
        treatments = ("pvisor-rootfs-physical-hints", "pvisor-dax")
    elif campaign == "static":
        arms, patterns = ARMS, PATTERNS
        controls = ("firecracker", "docker-rootless", "pvisor-ksm")
        treatments = ("pvisor-physical", "pvisor-compressed", "pvisor-local")
    else:
        raise ValueError("unknown campaign")
    identities = [(r["round"], r["pattern"], r["arm"]) for r in cases]
    expected = {(n, p, a) for n in range(rounds) for p in patterns for a in arms}
    if rounds < 5 or len(identities) != len(expected) or set(identities) != expected:
        raise ValueError("missing or duplicated paired rounds")
    indexed = {(r["round"], r["pattern"], r["arm"]): r for r in cases}
    metrics = [
        ("idle_cpu_seconds", None),
        ("first_full_read_ms", None),
        ("startup_seconds_until_all_ready", None),
    ]
    metrics += [("whole_condition_peak_mib", None), ("observed_idle_peak_mib", None)]
    metrics += [("global_ksmd_cpu_seconds", None)]
    metrics += [("post_write_cpu_seconds", None)]
    metrics += [
        (f"{role}_{kind}_cpu_seconds", None)
        for role in ("vm", "control", "pool", "runtime")
        for kind in ("user", "system")
    ]
    metrics += [(metric, f"idle-{n}") for n in BOUNDARIES for metric in ("charged_mib", "pss_mib")]
    metrics += [
        (f"{kind}_mib", "idle-60") for kind in ("anon", "file", "kernel", "pagetables", "slab")
    ]
    metrics += [
        (metric, phase)
        for phase in ("readback", "cow-25", "cow-100", "post-write-30")
        for metric in ("charged_mib", "pss_mib")
    ]
    distributions, comparisons = [], []

    def value(row, metric, phase):
        result = row[phase][metric] if phase else row[metric]
        if not isinstance(result, (int, float)) or not math.isfinite(result) or result < 0:
            raise ValueError("invalid cost observation")
        return result

    for pattern in patterns:
        for metric, phase in metrics:
            for arm in arms:
                samples = [value(indexed[n, pattern, arm], metric, phase) for n in range(rounds)]
                distributions.append(
                    dict(
                        pattern=pattern,
                        arm=arm,
                        metric=metric,
                        phase=phase,
                        n=rounds,
                        median=statistics.median(samples),
                        range=[min(samples), max(samples)],
                        samples=samples,
                    )
                )
            for control in controls:
                for arm in treatments:
                    differences = [
                        value(indexed[n, pattern, arm], metric, phase)
                        - value(indexed[n, pattern, control], metric, phase)
                        for n in range(rounds)
                    ]
                    comparisons.append(
                        dict(
                            pattern=pattern,
                            arm=arm,
                            control=control,
                            metric=metric,
                            phase=phase,
                            n=rounds,
                            paired_differences=differences,
                            median_difference=statistics.median(differences),
                            paired_bootstrap_95=interval(differences),
                            direction="negative means lower cost",
                        )
                    )
    return distributions, comparisons


def crossing_distributions(cases, rounds):
    """Keep missed thresholds censored and propagate sampled crossing brackets."""
    result = []
    for pattern, arm in sorted({(c["pattern"], c["arm"]) for c in cases}):
        selected = sorted(
            (c for c in cases if (c["pattern"], c["arm"]) == (pattern, arm)),
            key=lambda case: case["round"],
        )
        if [c["round"] for c in selected] != list(range(rounds)):
            raise ValueError("missing or duplicated crossing rounds")
        for kind in ("charged", "pss"):
            observations = [c["sustained_drop_128_mib"][kind] for c in selected]
            intervals = []
            for observation in observations:
                bounds = observation["crossing_interval_seconds"]
                if observation["reached"]:
                    if (
                        not isinstance(bounds, list)
                        or len(bounds) != 2
                        or not all(isinstance(n, (int, float)) and math.isfinite(n) for n in bounds)
                        or not 0 <= bounds[0] <= bounds[1] <= 60
                    ):
                        raise ValueError("invalid crossing bracket")
                    intervals.append(bounds)
                elif bounds is not None:
                    raise ValueError("censored crossing must have no imputed time")
            complete = len(intervals) == rounds
            result.append(
                dict(
                    pattern=pattern,
                    arm=arm,
                    metric=kind,
                    n=rounds,
                    reached=len(intervals),
                    censored=rounds - len(intervals),
                    observations=observations,
                    median_crossing_bracket_seconds=[
                        statistics.median(x[i] for x in intervals) for i in (0, 1)
                    ]
                    if complete
                    else None,
                    conservative_bootstrap_95_seconds=[
                        interval([x[0] for x in intervals])[0],
                        interval([x[1] for x in intervals])[1],
                    ]
                    if complete
                    else None,
                    scope="observed sustained whole-group 128MiB drop relative to each ready footprint; no imputation or exact convergence claim",
                )
            )
    return result


def crossing_comparisons(cases, rounds, *, campaign="static"):
    """Paired bracket differences; any missed target prevents a time estimate."""
    if campaign == "static":
        treatments = ("pvisor-physical",)
        controls = ("firecracker", "docker-rootless", "pvisor-ksm")
    elif campaign == "library":
        treatments = ("pvisor-rootfs-physical-hints", "pvisor-dax")
        controls = (
            "firecracker-block",
            "docker-rootless-library",
            "pvisor-rootfs-ksm",
            "pvisor-rootfs-physical-no-hints",
        )
    else:
        raise ValueError("unknown campaign")
    indexed = {(c["round"], c["pattern"], c["arm"]): c for c in cases}
    patterns = sorted({c["pattern"] for c in cases})
    result = []
    for pattern in patterns:
        for treatment in treatments:
            for control in controls:
                for kind in ("charged", "pss"):
                    differences = []
                    for n in range(rounds):
                        a = indexed[n, pattern, treatment]["sustained_drop_128_mib"][kind]
                        b = indexed[n, pattern, control]["sustained_drop_128_mib"][kind]
                        if a["reached"] and b["reached"]:
                            lo, hi = a["crossing_interval_seconds"]
                            blo, bhi = b["crossing_interval_seconds"]
                            differences.append([lo - bhi, hi - blo])
                        else:
                            differences.append(None)
                    complete = all(d is not None for d in differences)
                    result.append(
                        dict(
                            pattern=pattern,
                            arm=treatment,
                            control=control,
                            metric=kind,
                            n=rounds,
                            censored_pairs=sum(d is None for d in differences),
                            paired_difference_brackets_seconds=differences,
                            median_difference_bracket_seconds=[
                                statistics.median(d[i] for d in differences) for i in (0, 1)
                            ]
                            if complete
                            else None,
                            conservative_paired_bootstrap_95_seconds=[
                                interval([d[0] for d in differences])[0],
                                interval([d[1] for d in differences])[1],
                            ]
                            if complete
                            else None,
                            direction="negative means earlier observed sustained drop; different ready references, not pure algorithm convergence",
                        )
                    )
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("report_directory", type=Path)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args()
    report, rows = audit(a.report_directory, formal=True)
    cases = [row[0] for row in rows]
    distributions, comparisons = summarize(
        cases, report["samples"], campaign=report.get("campaign", "static")
    )
    a.output.mkdir()
    result = dict(
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        preflight=False,
        n=report["samples"],
        report_sha256=digest(a.report_directory / "report.json"),
        analyzer_sha256=digest(Path(__file__)),
        audit_sha256=digest(Path(__file__).with_name("plot_pool.py")),
        measured_harness_sha256=report["harness_sha256"],
        cases=cases,
        distributions=distributions,
        comparisons=comparisons,
        caveats=report["caveats"],
        crossing_distributions=crossing_distributions(cases, report["samples"]),
        crossing_comparisons=crossing_comparisons(
            cases, report["samples"], campaign=report.get("campaign", "static")
        ),
        statistical_scope="paired by randomized round; no small-N tail claims; inspect every sample and curve for clusters",
    )
    (a.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    figure, axes = plt.subplots(
        len(report["patterns"]), 2, figsize=(13, 4 * len(report["patterns"])), squeeze=False
    )
    for index, pattern in enumerate(report["patterns"]):
        for arm in report["arms"]:
            for axis, metric in zip(axes[index], ("charged_mib", "pss_mib")):
                selected = [
                    d
                    for d in distributions
                    if d["pattern"] == pattern
                    and d["arm"] == arm
                    and d["metric"] == metric
                    and d["phase"] in {f"idle-{n}" for n in BOUNDARIES}
                ]
                # Faint individual rounds retain shape and separated distributions.
                color = None
                for n in range(report["samples"]):
                    case = next(
                        c
                        for c in cases
                        if (c["round"], c["pattern"], c["arm"]) == (n, pattern, arm)
                    )
                    (line,) = axis.plot(
                        [case[f"idle-{s}"]["actual_seconds_after_ready"] for s in BOUNDARIES],
                        [case[f"idle-{s}"][metric] for s in BOUNDARIES],
                        alpha=0.18,
                        color=color,
                    )
                    color = line.get_color()
                axis.plot(
                    BOUNDARIES, [d["median"] for d in selected], color=color, label=arm, linewidth=2
                )
        for axis, metric in zip(axes[index], ("charged memory", "sum of process PSS")):
            axis.set(
                title=f"{pattern}: {metric}",
                xlabel="Seconds after all instances ready",
                ylabel="MiB",
                xlim=(0, 61),
            )
            axis.grid(alpha=0.25)
            axis.legend(fontsize=7)
    figure.suptitle(
        f"Four instances per condition; {report['samples']} randomized rounds; individual curves and medians"
    )
    layout_figure(figure, axes, report["arms"])
    figure.savefig(a.output / "memory-curves.svg")
    if report.get("campaign") == "library":
        for axis in axes.flat:
            axis.set_xlim(0, 5)
        figure.suptitle(
            f"First five seconds after all library instances ready; {report['samples']} randomized rounds"
        )
        figure.savefig(a.output / "early-memory-curves.svg")
    plot_startup(
        rows,
        report["patterns"],
        report["arms"],
        a.output,
        f"Instance-group setup through all-ready; {report['samples']} randomized rounds, individual observations",
    )
    plot_post_write(
        rows,
        report["patterns"],
        report["arms"],
        a.output,
        f"After 100% private writes; {report['samples']} randomized rounds, individual curves and medians",
    )
    print(a.output / "summary.json")


if __name__ == "__main__":
    main()
