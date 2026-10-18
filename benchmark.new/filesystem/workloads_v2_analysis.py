#!/usr/bin/env python3
"""Postprocess B-FS-ENG V2 case-local paired samples; never execute subjects.

An environment owns three dependent rounds. Comparisons use either a specified
round or the sum within each environment, then pair candidate with baseline.
N=3 exploratory intervals resample the three environment pairs, never nine
rounds. Incomplete cases retain observations but receive no performance ratio.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import random
import statistics
import sys
from pathlib import Path

ARMS = ("baseline", "candidate")
METRICS = ("work_ns", "completed_ns")
BOOTSTRAPS = 5000
MIB = 1 << 20


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


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def percentile(values, p):
    position = (len(values) - 1) * p
    lo, hi = math.floor(position), math.ceil(position)
    return values[lo] + (values[hi] - values[lo]) * (position - lo)


def estimate(ratios, seed):
    """Median paired ratio and exploratory percentile interval of that median."""
    require(
        len(ratios) > 0 and all(number(x, positive=True) for x in ratios),
        "paired ratios must be finite and positive",
    )
    rng = random.Random(seed)
    boot = sorted(statistics.median(rng.choices(ratios, k=len(ratios))) for _ in range(BOOTSTRAPS))
    median = statistics.median(ratios)
    low, high = percentile(boot, 0.025), percentile(boot, 0.975)
    return dict(
        n=len(ratios),
        median_ratio=median,
        min_ratio=min(ratios),
        max_ratio=max(ratios),
        paired_ratios=ratios,
        ratio_ci_low=low,
        ratio_ci_high=high,
        median_reduction_percent=100 * (1 - median),
        reduction_ci_low=100 * (1 - high),
        reduction_ci_high=100 * (1 - low),
        bootstrap_resamples=BOOTSTRAPS,
        bootstrap_seed=seed,
        evidence="exploratory; N3 cannot establish stable confirmation",
        interval_multiplicity="pointwise; uncorrected across cases and metrics",
        distribution_test="N3 insufficient to assess distribution modality",
    )


def collect(protocol, records):
    """Retain raw observations and explicit defects against the frozen inventory."""
    require(isinstance(protocol.get("catalog"), list), "protocol catalog missing")
    cases = {case["id"]: case for case in protocol["catalog"]}
    selected = protocol.get("inventory", {}).get("selected_cell_ids", list(cases))
    require(
        len(set(selected)) == len(selected) and set(selected) <= cases.keys(),
        "selected case inventory differs from catalog",
    )
    require(set(protocol["conditions"]) == set(ARMS), "expected baseline/candidate arms")
    samples = protocol.get("samples", protocol.get("sampling", {}).get("paired_blocks_per_case"))
    rounds = protocol.get(
        "resident_rounds", protocol.get("sampling", {}).get("dependent_rounds_per_environment")
    )
    require(
        type(samples) is int and samples == 3 and rounds == 3,
        "this exploratory analysis requires N3 pairs and three dependent rounds",
    )
    require(isinstance(records, list), "records must be an array")
    expected = {(case, arm, pair) for case in selected for arm in ARMS for pair in range(samples)}
    seen, usable, failures, raw, environments = set(), {}, [], [], []

    def defect(key, reason):
        failures.append(dict(case_id=key[0], condition=key[1], pair=key[2], reason=reason))

    for index, record in enumerate(records):
        require(isinstance(record, dict), f"record {index} must be an object")
        key = (record.get("case_id"), record.get("condition"), record.get("pair"))
        require(type(key[2]) is int and key in expected, f"unexpected environment {key}")
        require(key not in seen, f"duplicate environment {key}")
        seen.add(key)
        failures_before = len(failures)
        if record.get("valid") is not True or record.get("host_verified") is not True:
            defect(
                key,
                "environment failed or host verification missing: "
                + str(record.get("error", "unspecified")),
            )
        rows = record.get("rows", [])
        if not isinstance(rows, list):
            rows = []
            defect(key, "rows are not an array")
        ids = [row.get("round") for row in rows if isinstance(row, dict)]
        if (
            len(rows) != rounds
            or any(type(r) is not int for r in ids)
            or sorted(ids) != list(range(1, rounds + 1))
        ):
            defect(key, "dependent round inventory incomplete or duplicated")
        for row in rows:
            if not isinstance(row, dict):
                defect(key, "non-object payload row")
                continue
            if row.get("id", key[0]) != key[0] or row.get("correctness") is not True:
                defect(key, "row identity or correctness failed")
            if not all(number(row.get(metric), positive=True) for metric in METRICS):
                defect(key, "missing or invalid positive work/completed clock")
            elif row["completed_ns"] < row["work_ns"]:
                defect(key, "completed clock precedes work clock")
            if "final_sync_ns" in row and (
                not number(row["final_sync_ns"])
                or number(row.get("completed_ns"))
                and row["final_sync_ns"] > row["completed_ns"]
            ):
                defect(key, "invalid final synchronization clock")
            for field in ("bytes_written", "bytes_read", "write_ops", "read_ops", "metadata_ops"):
                if field in row and (type(row[field]) is not int or row[field] < 0):
                    defect(key, f"invalid logical work counter {field}")
            raw.append(
                {
                    **{k: v for k, v in row.items() if not isinstance(v, (dict, list))},
                    "case_id": key[0],
                    "condition": key[1],
                    "pair": key[2],
                    "environment_valid": record.get("valid") is True,
                    "host_verified": record.get("host_verified") is True,
                }
            )
        environment = dict(
            case_id=key[0],
            condition=key[1],
            pair=key[2],
            valid=record.get("valid") is True,
            host_verified=record.get("host_verified") is True,
            analysis_eligible=failures_before == len(failures),
        )
        for field in ("ready_ms", "job_ms", "exit_after_done_ms"):
            environment[field] = record.get(field) if number(record.get(field)) else None
        resources = record.get("resource", {})
        for field in ("cpu_usage_usec_delta", "sampled_memory_current_peak"):
            value = resources.get(field) if isinstance(resources, dict) else None
            environment[field] = value if number(value) else None
        environments.append(environment)
        if failures_before == len(failures):
            usable[key] = {row["round"]: row for row in rows}
    for key in sorted(expected - seen):
        defect(key, "expected fresh environment missing")
    # Defective observations remain in raw, never enter any ratios for that case.
    complete = [
        case
        for case in selected
        if all((case, arm, pair) in usable for arm in ARMS for pair in range(samples))
    ]
    return cases, selected, usable, raw, environments, failures, complete


def summarize(protocol, records):
    cases, selected, usable, raw, environments, failures, complete = collect(protocol, records)
    comparisons, levels, observations = [], [], []
    for case_id in selected:
        case = cases[case_id]
        if case_id not in complete:
            continue
        for scope in ("sum_three_rounds", "round_1", "round_2", "round_3"):
            round_ids = [1, 2, 3] if scope == "sum_three_rounds" else [int(scope[-1])]
            for metric in METRICS:
                values = {
                    arm: [
                        sum(usable[case_id, arm, pair][r][metric] for r in round_ids)
                        for pair in range(3)
                    ]
                    for arm in ARMS
                }
                ratios = [
                    candidate / baseline
                    for baseline, candidate in zip(values["baseline"], values["candidate"])
                ]
                seed = int.from_bytes(
                    hashlib.sha256(f"v2:{case_id}:{scope}:{metric}".encode()).digest()[:8], "big"
                )
                comparison = dict(
                    case_id=case_id,
                    name=case["name"],
                    layer=case["layer"],
                    scope=scope,
                    metric=metric,
                    independent_pairs=3,
                    dependent_rounds=len(round_ids),
                    pair_ids=[0, 1, 2],
                    baseline_median_ms=statistics.median(values["baseline"]) / 1e6,
                    candidate_median_ms=statistics.median(values["candidate"]) / 1e6,
                    **estimate(ratios, seed),
                )
                comparisons.append(comparison)
                for arm in ARMS:
                    points_ms = [value / 1e6 for value in values[arm]]
                    levels.append(
                        dict(
                            case_id=case_id,
                            name=case["name"],
                            layer=case["layer"],
                            condition=arm,
                            scope=scope,
                            metric=metric,
                            n=3,
                            median_ms=statistics.median(points_ms),
                            min_ms=min(points_ms),
                            max_ms=max(points_ms),
                            points_ms=points_ms,
                        )
                    )
        for arm in ARMS:
            for pair in range(3):
                for r in range(1, 4):
                    row = usable[case_id, arm, pair][r]
                    entry = dict(
                        case_id=case_id,
                        layer=case["layer"],
                        condition=arm,
                        pair=pair,
                        round=r,
                        work_ms=row["work_ns"] / 1e6,
                        completed_ms=row["completed_ns"] / 1e6,
                    )
                    for kind, field in (("write", "bytes_written"), ("read", "bytes_read")):
                        byte_count = row.get(field)
                        for metric in METRICS:
                            entry[f"{kind}_{metric.removesuffix('_ns')}_mib_s"] = (
                                byte_count / MIB / (row[metric] / 1e9)
                                if number(byte_count, positive=True)
                                else None
                            )
                    observations.append(entry)
    primary = [
        c
        for c in comparisons
        if c["layer"] == "scenario"
        and c["scope"] == "sum_three_rounds"
        and c["metric"] == "completed_ns"
    ]
    return dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="engineering A/B",
        comparison=protocol.get("comparison"),
        status="complete_exploratory"
        if len(complete) == len(selected)
        else "incomplete_cases_retained",
        statistical_scope=dict(
            independent_pairs_per_case=3,
            dependent_rounds_per_environment=3,
            estimator="median of three paired candidate/baseline time ratios; aggregate sums three dependent rounds inside each environment first",
            intervals="5000 percentile bootstrap resamples of the three paired ratios; N3 exploratory and pointwise, not stable confirmation",
            modality="N3 insufficient to assess distribution modality",
            tail_latency="not measured; no P95/P99 from environment means",
            aggregate_score=None,
            failures="no contrasts for incomplete cases; no zero filling or silent sample replacement",
        ),
        counts=dict(
            planned_cases=len(selected),
            complete_cases=len(complete),
            expected_environments=6 * len(selected),
            observed_environments=len(records),
            expected_round_rows=18 * len(selected),
            observed_round_rows=len(raw),
            defects=len(failures),
        ),
        complete_case_ids=complete,
        incomplete_case_ids=[case for case in selected if case not in complete],
        catalog=[cases[case] for case in selected],
        primary_workflows=primary,
        comparisons=comparisons,
        levels=levels,
        observations=observations,
        environments=environments,
        failures=failures,
        raw_rows=raw,
        missing_instrumentation="per-operation latency, guest dirty/temp peak, host applied prefix, precise cache hit rate and time-share remain unmeasured",
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
    """Standalone SVG/PDF from retained points, using no synthesized measurements."""
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
    colors = {"baseline": "#0072B2", "candidate": "#D55E00"}
    paths = []

    def save(fig, stem):
        fig.suptitle(
            "Exploratory N=3 fresh pairs; three dependent rounds; all valid points retained",
            fontsize=10,
        )
        fig.tight_layout(rect=(0, 0, 1, 0.96))
        for extension in ("svg", "pdf"):
            path = output / f"{stem}.{extension}"
            fig.savefig(path)
            paths.append(path.name)
        plt.close(fig)

    mechanism = [case for case in summary["catalog"] if case["layer"] == "mechanism"]
    if mechanism:
        fig, axes = plt.subplots(2, 3, figsize=(14, max(6, len(mechanism) * 0.6)), squeeze=False)
        for m, metric in enumerate(METRICS):
            for r in range(1, 4):
                ax = axes[m][r - 1]
                for y, case in enumerate(mechanism):
                    found = [
                        c
                        for c in summary["comparisons"]
                        if c["case_id"] == case["id"]
                        and c["scope"] == f"round_{r}"
                        and c["metric"] == metric
                    ]
                    if found:
                        entry = found[0]
                        for pair, ratio in enumerate(entry["paired_ratios"]):
                            ax.scatter(
                                ratio,
                                y + (pair - 1) * 0.12,
                                color=colors["candidate"],
                                s=18,
                                alpha=0.65,
                            )
                        ax.scatter(
                            entry["median_ratio"], y, marker="D", s=30, color="black", zorder=3
                        )
                    else:
                        ax.text(1, y, "incomplete", ha="center", color="#777777")
                ax.axvline(1, color="#666666", linestyle="--", linewidth=1)
                ax.set_xscale("log")
                ax.set_yticks(
                    range(len(mechanism)),
                    [c["id"] + " " + c["name"] for c in mechanism] if r == 1 else [],
                )
                ax.invert_yaxis()
                ax.set_title(
                    f"{'API-return' if metric == 'work_ns' else 'Sync-completed'} time, round {r}"
                )
                ax.set_xlabel("Candidate / paired baseline (lower is faster)")
                ax.grid(axis="x", alpha=0.15)
        save(fig, "mechanism-normalized-time")

    resources = [case for case in summary["catalog"] if case["layer"] == "resource"]
    if resources:
        fig, axes = plt.subplots(2, 3, figsize=(13, 7), squeeze=False)
        for m, metric in enumerate(METRICS):
            for r in range(1, 4):
                ax = axes[m][r - 1]
                ratios = sorted(
                    {
                        c["parameters"]["working_set_bytes"]
                        / c["parameters"]["configured_guest_memory_bytes"]
                        for c in resources
                    }
                )
                for wss in ratios:
                    selected = sorted(
                        [
                            c
                            for c in resources
                            if c["parameters"]["working_set_bytes"]
                            / c["parameters"]["configured_guest_memory_bytes"]
                            == wss
                        ],
                        key=lambda c: c["parameters"]["workers"],
                    )
                    for arm in ARMS:
                        xs, ys = [], []
                        for case in selected:
                            field = "write_" + metric.removesuffix("_ns") + "_mib_s"
                            points = [
                                p[field]
                                for p in summary["observations"]
                                if p["case_id"] == case["id"]
                                and p["condition"] == arm
                                and p["round"] == r
                                and p[field] is not None
                            ]
                            if not points:
                                continue
                            workers = case["parameters"]["workers"]
                            xs.append(workers)
                            ys.append(statistics.median(points))
                            ax.scatter(
                                [workers] * len(points),
                                points,
                                s=18,
                                color=colors[arm],
                                alpha=0.6,
                                marker="o" if wss == ratios[0] else "s",
                            )
                        ax.plot(
                            xs,
                            ys,
                            color=colors[arm],
                            linestyle="-" if wss == ratios[0] else "--",
                            label=f"{arm}, WSS/R={wss:g}",
                        )
                ax.set_title(
                    f"{'API-return' if metric == 'work_ns' else 'Sync-completed'}, round {r}"
                )
                ax.set_xlabel("Workers (fixed total work, four files)")
                ax.set_ylabel("Logical write MiB/s")
                ax.set_xticks([1, 4])
                ax.grid(alpha=0.15)
                if m == 0 and r == 1:
                    ax.legend(fontsize=7)
        save(fig, "resource-throughput")

        fig, axes = plt.subplots(1, 2, figsize=(12, 4))
        for ax, field, divisor, label in [
            (axes[0], "cpu_usage_usec_delta", 1e6, "Whole environment CPU seconds"),
            (
                axes[1],
                "sampled_memory_current_peak",
                MIB,
                "Sampled enclosing memory.current peak (MiB)",
            ),
        ]:
            for arm in ARMS:
                for x, case in enumerate(resources):
                    points = [
                        env[field] / divisor
                        for env in summary["environments"]
                        if env["case_id"] == case["id"]
                        and env["condition"] == arm
                        and env[field] is not None
                        and env["analysis_eligible"]
                        and case["id"] in summary["complete_case_ids"]
                    ]
                    offset = -0.12 if arm == "baseline" else 0.12
                    ax.scatter(
                        [x + offset] * len(points),
                        points,
                        s=22,
                        color=colors[arm],
                        alpha=0.7,
                        label=arm if x == 0 else None,
                    )
                    if points:
                        ax.scatter(
                            x + offset,
                            statistics.median(points),
                            color=colors[arm],
                            marker="_",
                            s=180,
                        )
            ax.set_xticks(
                range(len(resources)),
                [c["id"] + f"\nw={c['parameters']['workers']}" for c in resources],
            )
            ax.set_ylabel(label)
            ax.grid(axis="y", alpha=0.15)
            ax.legend()
        save(fig, "resource-environment-cost")

    scenarios = [case for case in summary["catalog"] if case["layer"] == "scenario"]
    if scenarios:
        fig, axes = plt.subplots(
            2, len(scenarios), figsize=(max(5, 4 * len(scenarios)), 7), squeeze=False
        )
        for column, case in enumerate(scenarios):
            for m, metric in enumerate(METRICS):
                ax = axes[m][column]
                for arm in ARMS:
                    xs, ys = [], []
                    for r in range(1, 4):
                        found = [
                            level
                            for level in summary["levels"]
                            if level["case_id"] == case["id"]
                            and level["condition"] == arm
                            and level["scope"] == f"round_{r}"
                            and level["metric"] == metric
                        ]
                        if not found:
                            continue
                        level = found[0]
                        xs.append(r)
                        ys.append(level["median_ms"])
                        ax.scatter([r] * 3, level["points_ms"], color=colors[arm], s=24, alpha=0.65)
                    ax.plot(xs, ys, color=colors[arm], label=arm)
                ax.set_title(case["id"] + " " + case["name"])
                ax.set_xticks([1, 2, 3])
                ax.set_xlabel("Dependent round")
                ax.set_ylabel(
                    ("API-return" if metric == "work_ns" else "Task sync-completed") + " time (ms)"
                )
                ax.grid(alpha=0.15)
                if column == 0 and m == 0:
                    ax.legend()
        save(fig, "workflow-times")
    return paths


def report(summary):
    count = summary["counts"]
    lines = [
        "# 文件系统分层负载快速对照",
        "",
        "**三个完整任务分别判断产品收益；机制与资源结果解释收益边界，不合成系统总分。**",
        "",
        f"完整 case：{count['complete_cases']}/{count['planned_cases']}；观测环境：{count['observed_environments']}/{count['expected_environments']}。"
        "每项是 3 对 fresh 环境，内部 3 轮；独立样本量为 N3。",
        "",
        "统计量为三个配对耗时比 candidate/baseline 的中位数。三轮合计先在各自环境内求和，再配对；"
        "逐轮另报。95% 区间按环境对 bootstrap 5000 次，仅为低样本探索，未校正多重比较，不能当作稳定确认。",
        "",
        "## 完整任务",
        "",
        "| Case | 三轮基线中位数 ms | 三轮候选中位数 ms | 配对耗时比 | 比值范围 | 95% 探索区间 |",
        "| --- | ---: | ---: | ---: | --- | --- |",
    ]

    def table_row(c):
        return (
            f"| {c['case_id']} {c['name']} | {c['baseline_median_ms']:.3f} | {c['candidate_median_ms']:.3f} "
            f"| {c['median_ratio']:.4f} | [{c['min_ratio']:.4f}, {c['max_ratio']:.4f}] "
            f"| [{c['ratio_ci_low']:.4f}, {c['ratio_ci_high']:.4f}] |"
        )

    lines += [table_row(c) for c in summary["primary_workflows"]]
    if not summary["primary_workflows"]:
        lines += ["| 无完整任务结果 | — | — | — | — | — |"]
    lines += [
        "",
        "## 机制与资源：同步完成时间",
        "",
        "| Case | 三轮基线中位数 ms | 三轮候选中位数 ms | 配对耗时比 | 比值范围 | 95% 探索区间 |",
        "| --- | ---: | ---: | ---: | --- | --- |",
    ]
    lines += [
        table_row(c)
        for c in summary["comparisons"]
        if c["scope"] == "sum_three_rounds"
        and c["metric"] == "completed_ns"
        and c["layer"] != "scenario"
    ]
    lines += [
        "",
        "## 解释范围与失败",
        "",
        "- API-return 与同步完成分别报告；退出不计入 Ready 或任务时钟。",
        "- 资源 CPU/内存是整个环境范围，不是逐 case 操作区间精确成本；采样峰值不是内核 memory.peak。",
        "- 没有独立计数时，不从耗时变化推导消除了多少 OPEN/READ/LOOKUP/WRITE。",
        "- N3 无法可靠检验双峰分布，不报告操作 P95/P99，不推导掉电耐久或网络 offload。",
        "- 不完整 case 保留原始观察和错误，不计算其提升，不填零、不补样。",
        "",
    ]
    if summary["failures"]:
        lines += [
            f"- {failure['case_id']}/{failure['condition']}/pair-{failure['pair']}: {failure['reason']}"
            for failure in summary["failures"]
        ]
    else:
        lines += ["没有缺失或失败环境。"]
    lines += [
        "",
        "完整精度与全部配对点见 `comparisons.csv`、`levels.csv`、`raw_rows.csv` 和 `summary.json`。",
        "",
    ]
    return "\n".join(lines)


def analyze(batch, output=None, *, plots=True):
    batch = Path(batch).resolve()
    protocol_path, records_path = batch / "protocol.json", batch / "records.json"
    protocol = json.loads(protocol_path.read_text())
    loaded = json.loads(records_path.read_text())
    records = loaded.get("records") if isinstance(loaded, dict) else loaded
    result = summarize(protocol, records)
    result["provenance"] = dict(
        protocol_sha256=sha(protocol_path),
        records_sha256=sha(records_path),
        analyzer_sha256=sha(__file__),
    )
    output = Path(output).resolve() if output else batch / "analysis"
    require(".data" in output.parts, "analysis artifacts must remain under .data")
    output.mkdir(parents=True, exist_ok=False)
    for key in ("comparisons", "levels", "observations", "environments", "failures", "raw_rows"):
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
        result = analyze(args.batch, args.output, plots=not args.no_plots)
    except (ValueError, OSError, ImportError) as error:
        parser.exit(1, str(error) + "\n")
    print(json.dumps(dict(status=result["status"], counts=result["counts"], plots=result["plots"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
