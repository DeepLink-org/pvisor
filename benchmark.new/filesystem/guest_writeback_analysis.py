#!/usr/bin/env python3
"""Independent postprocessor for the four-arm guest-writeback followup.

Never starts a subject, changes frozen sampling inputs, or publishes files. The
three resident rounds are summed inside each independent environment before a
paired ratio is formed. Native is an absolute reference, not a firmware A/B.
"""

import argparse
import hashlib
import importlib.util
import json
import random
import statistics
import sys
from pathlib import Path

CONTRASTS = (
    ("incremental_0048", "old-wb", "guest-wb"),
    ("guest_writeback_enablement", "off", "guest-wb"),
)
METRICS = (("completed", "total_ns"), ("accepted", "accepted_ns"))


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def frozen_helpers(batch):
    """Use only the helpers whose bytes were retained by this batch."""
    sys.path.insert(0, str(batch / "harness"))
    path = batch / "harness/guest_writeback_curves.py"
    spec = importlib.util.spec_from_file_location("frozen_guest_writeback", path)
    wrapper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(wrapper)
    protocol = wrapper.verify_protocol(batch)
    import curves_analysis

    require(
        Path(curves_analysis.__file__).resolve() == batch / "harness/curves_analysis.py",
        "analysis helper must come from the frozen batch",
    )
    return wrapper, curves_analysis, protocol


def paired_estimate(values, labels, seed, analysis):
    require(
        len(values) == len(labels) == 5 and all(v > 0 for v in values),
        "paired estimate requires five positive independent environments",
    )
    # Keep the sampling algorithm explicit; do not bootstrap resident rounds.
    rng = random.Random(seed)
    resamples = sorted(statistics.median(rng.choices(values, k=5)) for _ in range(5000))
    lo, hi = (analysis.percentile(resamples, p) for p in (0.025, 0.975))
    return dict(
        n=5,
        median=statistics.median(values),
        min=min(values),
        max=max(values),
        ci_low=lo,
        ci_high=hi,
        pairs=labels,
        paired_values=values,
        bootstrap_seed=seed,
        bootstrap_resamples=5000,
        evidence="exploratory_N5_pointwise_uncorrected",
        interval_excludes_one=hi < 1 or lo > 1,
    )


def validate_entrypoint(batch, trials, pairs):
    """Require the real registered entrypoint, not manufactured wrapper data."""
    template = json.loads((batch / "in.json").read_text())
    require(set(template) == {"guest-writeback-curves"}, "unexpected semspec row")
    command = template["guest-writeback-curves"].removeprefix("$ ")
    bench = json.loads((batch / "in.bench.json").read_text())
    render = json.loads((batch / "in.render.json").read_text())
    expected = dict(trials=trials, failed=0, cells=15, pairs=pairs)
    require(
        bench.get("format") == 1
        and len(bench.get("commands", [])) == 1
        and len(bench.get("rows", [])) == 1,
        "incomplete semspec execution inventory",
    )
    row, executed = bench["rows"][0], bench["commands"][0]
    require(
        row.get("row") == "guest-writeback-curves"
        and row.get("command") == command
        and executed.get("command") == command
        and executed.get("kind") == "script-row"
        and executed.get("raw") == executed.get("value") == expected
        and render == {"guest-writeback-curves": expected},
        "semspec command/result differs from completed campaign",
    )
    log = Path(executed["log"])
    require(
        log.resolve().parent == batch / "in.bench.logs" and json.loads(log.read_text()) == expected,
        "semspec command log is missing or inconsistent",
    )
    return {name: sha(batch / name) for name in ("in.json", "in.bench.json", "in.render.json")}


def render_plots(
    output, levels, comparisons, cells, analysis, diagnostic=None, contrasts=CONTRASTS
):
    """Render derived data only; diagnostic counts never supply timing points."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "font.size": 10,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "pdf.fonttype": 42,
            "svg.fonttype": "none",
        }
    )
    arms = ("off", "old-wb", "guest-wb", "native")
    colors = {"off": "#0072B2", "old-wb": "#D55E00", "guest-wb": "#009E73", "native": "#777777"}
    markers = {"off": "o", "old-wb": "s", "guest-wb": "^", "native": "D"}
    paths = []

    def save(fig, name):
        for suffix in ("svg", "pdf"):
            path = output / (name + "." + suffix)
            fig.savefig(path, bbox_inches="tight")
            paths.append(path.name)
        plt.close(fig)

    def values(ax, x, samples, color, marker, label=None, seed=0):
        estimate = analysis.describe(samples, bootstrap=True, seed=seed)
        # N5 cannot establish stable clusters. Every independent point is shown.
        for index, sample in enumerate(samples):
            ax.scatter(x + (index - 2) * 0.012, sample, c=color, marker=marker, s=16, alpha=0.45)
        ax.errorbar(
            x,
            estimate["median"],
            yerr=[
                [estimate["median"] - estimate["ci_low"]],
                [estimate["ci_high"] - estimate["median"]],
            ],
            fmt=marker,
            color=color,
            capsize=3,
            markersize=5,
            label=label,
        )

    for family in dict.fromkeys(cell["family"] for cell in cells.values()):
        selected = sorted(
            (cell for cell in cells.values() if cell["family"] == family), key=lambda c: c["x"]
        )
        fig, (upper, lower) = plt.subplots(
            2,
            1,
            figsize=(10, 7),
            sharex=True,
            gridspec_kw={"height_ratios": [1.15, 1]},
            layout="constrained",
        )
        for arm_index, arm in enumerate(arms):
            for x, cell in enumerate(selected):
                samples = [
                    level["logical_bytes"] * 1e9 / level["completed_ns"] / (1 << 20)
                    for level in levels
                    if level["id"] == cell["id"] and level["condition"] == arm
                ]
                values(
                    upper,
                    x + (arm_index - 1.5) * 0.14,
                    samples,
                    colors[arm],
                    markers[arm],
                    arm if x == 0 else None,
                    analysis.seed_for(cell["id"], arm, "completed_plot"),
                )
        for contrast_index, (contrast, _, _) in enumerate(contrasts):
            color = ("#CC79A7", "#56B4E9")[contrast_index]
            for x, cell in enumerate(selected):
                comparison = next(
                    row
                    for row in comparisons
                    if row["contrast"] == contrast
                    and row["id"] == cell["id"]
                    and row["metric"] == "completed"
                )
                xpos = x + (contrast_index - 0.5) * 0.20
                for index, point in enumerate(comparison["paired_values"]):
                    lower.scatter(xpos + (index - 2) * 0.012, point, color=color, s=16, alpha=0.45)
                lower.errorbar(
                    xpos,
                    comparison["median"],
                    yerr=[
                        [comparison["median"] - comparison["ci_low"]],
                        [comparison["ci_high"] - comparison["median"]],
                    ],
                    fmt="o" if contrast_index == 0 else "s",
                    color=color,
                    capsize=3,
                    label=("new WB / old WB" if contrast_index == 0 else "new WB / off")
                    if x == 0
                    else None,
                )
        upper.set(
            title=family + ": completed file + directory fsync",
            ylabel="Completed throughput (MiB/s)",
        )
        upper.legend(ncols=4, fontsize=9)
        lower.axhline(1, color="#333333", linewidth=0.8, linestyle="--")
        lower.set(
            ylabel="Paired completed-time ratio\n(lower is faster)", xlabel=selected[0]["x_label"]
        )
        lower.set_xticks(range(len(selected)), [str(cell["x"]) for cell in selected])
        lower.legend(fontsize=9)
        for ax in (upper, lower):
            ax.grid(axis="y", alpha=0.2)
        fig.suptitle(
            "N5 fresh paired environments; three resident rounds summed per environment\n"
            "Points: all independent environments; bars: median and pointwise 95% bootstrap CI",
            fontsize=11,
        )
        save(fig, "family-" + family)
    if diagnostic is not None:
        diag_arms = ("off", "old-wb", "guest-wb")
        ordered = list(cells)
        fig, ax = plt.subplots(figsize=(13, 5), layout="constrained")
        for index, arm in enumerate(diag_arms):
            counts = [
                sum(
                    row["write_requests"]
                    for row in diagnostic["intervals"]
                    if row["condition"] == arm and row["id"] == cell
                )
                for cell in ordered
            ]
            ax.bar(
                [x + (index - 1) * 0.26 for x in range(len(ordered))],
                counts,
                width=0.26,
                color=colors[arm],
                label=arm,
            )
        ax.set(
            ylabel="FUSE WRITE requests (sum of three rounds)",
            title="Independent N1 diagnostic; cell through directory fsync",
        )
        ax.set_xticks(range(len(ordered)), ordered, rotation=45, ha="right")
        ax.legend(ncols=3)
        ax.grid(axis="y", alpha=0.2)
        save(fig, "write-requests")
        fig, ax = plt.subplots(figsize=(10, 5), layout="constrained")
        buckets = diagnostic["intervals"][0]["payload_histogram"]
        for index, arm in enumerate(diag_arms):
            counts = [
                sum(
                    row["payload_histogram"][bucket]["requests"]
                    for row in diagnostic["intervals"]
                    if row["condition"] == arm
                )
                for bucket in range(len(buckets))
            ]
            ax.bar(
                [x + (index - 1) * 0.26 for x in range(len(buckets))],
                counts,
                width=0.26,
                color=colors[arm],
                label=arm,
            )
        ax.set_xticks(
            range(len(buckets)),
            ["<=1KiB", "1-4KiB", "4-16KiB", "16-64KiB", "64-128KiB", "128-1024KiB", ">1MiB"],
        )
        ax.set(
            ylabel="FUSE WRITE requests (all cell intervals)",
            title="Independent N1 diagnostic; disjoint payload-size buckets",
        )
        ax.set_yscale("symlog", linthresh=1)
        ax.legend(ncols=3)
        ax.grid(axis="y", alpha=0.2)
        save(fig, "write-payload-histogram")
    return paths


def analyze(batch, output, diagnostic_batch=None, plots=True):
    wrapper, analysis, protocol = frozen_helpers(batch)
    revision = protocol["firmware_comparison"].get("candidate_patch", 48)
    require(revision in (48, 49), "unsupported candidate firmware revision")
    contrasts = ((f"incremental_{revision:04}", "old-wb", "guest-wb"), CONTRASTS[1])
    require(
        protocol["samples"] == 5
        and protocol["resident_rounds"] == 3
        and protocol["profiling"] is False
        and protocol["preflight"] is False,
        "only a completed formal N5 timing cohort is eligible",
    )
    require(
        set(protocol["conditions"]) == {"native", "off", "old-wb", "guest-wb"}
        and protocol["firmware_comparison"]["contrasts"]
        == {"incremental": ["old-wb", "guest-wb"], "enablement": ["off", "guest-wb"]},
        "explicit four-arm firmware contrasts are required",
    )
    status = json.loads((batch / "status.json").read_text())
    require(
        status.get("valid") is True
        and type(status.get("failed")) is int
        and status["failed"] == 0
        and status.get("excluded") == 0
        and status.get("trials") == 20,
        "all twenty planned environments must have finished without failures or exclusions",
    )
    entrypoint_inputs = validate_entrypoint(batch, 20, 5)
    records = json.loads((batch / "records.json").read_text())
    points, _, failures, _ = analysis.collect(protocol, records)
    require(
        not failures and len(records) == 20 and len(points) == 300,
        "formal environment/cell/round inventory is incomplete",
    )
    cells = {cell["id"]: cell for cell in protocol["cells"]}
    require(len(cells) == 15, "fifteen registered cells are required")
    levels, groups = [], {}
    for record in records:
        require(
            record.get("valid") is True
            and record.get("host_verified") is True
            and record.get("returncode") == 0,
            "full-byte checks and normal exit are required",
        )
        condition, pair = record["condition"], str(record["pair"])
        require(
            all(
                record["environment"].get(key) == value
                for key, value in protocol["conditions"][condition]["env"].items()
            ),
            "observed environment differs from frozen factors",
        )
        grouped = {}
        for row in record["rows"]:
            wrapper.validate_row(row)
            grouped.setdefault(row["id"], []).append(row)
        for cell_id, rows in grouped.items():
            require(
                sorted(row["round"] for row in rows) == [1, 2, 3],
                "resident rounds must be exactly 1, 2, 3",
            )
            amounts = [row["bytes"] for row in rows]
            require(
                len(set(amounts)) == 1 and amounts[0] > 0,
                "rounds must process the same positive logical bytes",
            )
            level = dict(
                id=cell_id,
                family=cells[cell_id]["family"],
                x=cells[cell_id]["x"],
                pair=pair,
                condition=condition,
                rounds=3,
                logical_bytes=sum(amounts),
                completed_ns=sum(row["total_ns"] for row in rows),
                accepted_ns=sum(row["accepted_ns"] for row in rows),
                round_completed_ns=[
                    row["total_ns"] for row in sorted(rows, key=lambda r: r["round"])
                ],
                round_accepted_ns=[
                    row["accepted_ns"] for row in sorted(rows, key=lambda r: r["round"])
                ],
            )
            require(
                level["completed_ns"] > 0 and level["accepted_ns"] > 0,
                "completed and accepted durations must be positive",
            )
            levels.append(level)
            groups.setdefault((cell_id, condition), {})[pair] = level
    comparisons = []
    for contrast, baseline, candidate in contrasts:
        for cell_id, cell in cells.items():
            before, after = groups[(cell_id, baseline)], groups[(cell_id, candidate)]
            labels = sorted(before, key=int)
            require(
                set(before) == set(after) and len(labels) == 5,
                "all five independent pairs must be retained",
            )
            for metric, _ in METRICS:
                field = metric + "_ns"
                require(
                    all(before[p]["logical_bytes"] == after[p]["logical_bytes"] for p in labels),
                    "paired arms must process identical logical bytes",
                )
                values = [after[p][field] / before[p][field] for p in labels]
                estimate = paired_estimate(
                    values,
                    labels,
                    analysis.seed_for(protocol["seed"], contrast, cell_id, metric, "time_ratio"),
                    analysis,
                )
                comparisons.append(
                    dict(
                        contrast=contrast,
                        baseline=baseline,
                        condition=candidate,
                        id=cell_id,
                        family=cell["family"],
                        x=cell["x"],
                        metric=metric,
                        statistic="median of per-environment candidate/baseline three-round time sums; lower is faster",
                        baseline_ns=[before[p][field] for p in labels],
                        candidate_ns=[after[p][field] for p in labels],
                        baseline_median_round_ms=statistics.median(
                            before[p][field] / 3e6 for p in labels
                        ),
                        candidate_median_round_ms=statistics.median(
                            after[p][field] / 3e6 for p in labels
                        ),
                        reduction_percent=100 * (1 - estimate["median"]),
                        reduction_ci_low=100 * (1 - estimate["ci_high"]),
                        reduction_ci_high=100 * (1 - estimate["ci_low"]),
                        **estimate,
                    )
                )
    require(len(levels) == 300 and len(comparisons) == 60, "derived output inventory differs")
    diagnostic = None
    diagnostic_inputs = None
    if diagnostic_batch is not None:
        diagnostic_status = json.loads((diagnostic_batch / "status.json").read_text())
        require(
            diagnostic_status == dict(valid=True, failed=0, excluded=0, trials=4),
            "diagnostic four-arm inventory is incomplete",
        )
        diagnostic_entrypoint = validate_entrypoint(diagnostic_batch, 4, 1)
        diagnostic = json.loads((diagnostic_batch / "guest-writeback-diagnostics.json").read_text())
        dp = json.loads((diagnostic_batch / "protocol.json").read_text())
        require(
            dp["profiling"] is True
            and dp["samples"] == 1
            and dp["cells"] == protocol["cells"]
            and dp["condition_factors"] == protocol["condition_factors"]
            and dp["source_hashes"] == protocol["source_hashes"]
            and dp["firmware_source"] == protocol["firmware_source"]
            and dp["old_firmware_source"] == protocol["old_firmware_source"],
            "diagnostic source/firmware/cell treatment differs from timing",
        )
        require(
            diagnostic["protocol_sha256"] == sha(diagnostic_batch / "protocol.json")
            and diagnostic["records_sha256"] == sha(diagnostic_batch / "records.json")
            and diagnostic["performance_samples"] is False
            and len(diagnostic["intervals"]) == 135,
            "diagnostic hash or interval inventory differs",
        )
        wrapper.BASE.verify_artifacts(diagnostic_batch, dp)
        require(
            diagnostic["full_profile_evidence"] == wrapper.DIAG.summarize_batch(diagnostic_batch),
            "diagnostic report differs from complete registered raw profiles",
        )
        diagnostic_inputs = {
            name: sha(diagnostic_batch / name)
            for name in (
                "protocol.json",
                "records.json",
                "status.json",
                "guest-writeback-diagnostics.json",
            )
        }
        diagnostic_inputs.update(diagnostic_entrypoint)
    output.mkdir(parents=True, exist_ok=False)
    analysis.csv_file(output / "time_levels.csv", levels)
    analysis.csv_file(output / "time_comparisons.csv", comparisons)
    summary = dict(
        schema=1,
        status="verified",
        performance_samples=True,
        inputs={
            **{
                name: sha(batch / name) for name in ("protocol.json", "records.json", "status.json")
            },
            **entrypoint_inputs,
        },
        postprocessor_sha256=sha(Path(__file__)),
        frozen_helpers={
            name: sha(batch / "harness" / name)
            for name in ("guest_writeback_curves.py", "curves_analysis.py")
        },
        counts=dict(
            environments=20,
            resident_rows=900,
            environment_cells=300,
            contrasts=2,
            metrics_per_cell=2,
            comparisons=60,
        ),
        statistical_scope=dict(
            independent_unit="one paired fresh environment block",
            independent_pairs=5,
            dependent_rounds=3,
            bootstrap_resamples=5000,
            time_ratio="candidate/baseline; lower is faster",
            intervals="95% percentile paired bootstrap, pointwise, no multiplicity correction",
            rounds="three durations summed within environment before forming ratios; never N15",
            accepted="write loop includes configured periodic fsync and small-file close; not pure admission latency",
            completed="write loop + final file fsync/drain + checked close + directory fsync; preparation/verification/exit excluded",
            native="absolute reference only; no native/VM causal firmware comparison",
            applied="reserved zero; no auxiliary dup/close and no applied-prefix measurement",
            distribution="all five raw paired values and all three dependent rounds retained; N5 does not establish stable clusters",
        ),
        comparisons=comparisons,
        diagnostic_inputs=diagnostic_inputs,
        firmware_comparison=dict(baseline_patch=revision - 1, candidate_patch=revision),
        plots=[],
        plot_status="disabled" if not plots else "pending",
    )
    if plots:
        try:
            summary["plots"] = render_plots(
                output, levels, comparisons, cells, analysis, diagnostic, contrasts
            )
            summary["plot_status"] = "generated"
        except ImportError as error:
            summary.update(plot_status="dependency_missing", plot_error=str(error))
    summary["outputs"] = {
        name: sha(output / name)
        for name in ("time_levels.csv", "time_comparisons.csv", *summary["plots"])
    }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(dict(status="verified", counts=summary["counts"], output=str(output))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--diagnostic", type=Path)
    parser.add_argument("--no-plots", action="store_true")
    args = parser.parse_args()
    batch = args.batch.resolve()
    analyze(
        batch,
        args.output.resolve() if args.output else batch / "analysis-times",
        args.diagnostic.resolve() if args.diagnostic else None,
        not args.no_plots,
    )


if __name__ == "__main__":
    main()
