#!/usr/bin/env python3
"""Offline statistics for the same-host-staged, readonly lower comparison.

Reads one frozen protocol and all 132 warmup/sample results. It never executes a
subject, starts services, mounts filesystems, or combines prior experiment data.
The subject owns file/mount/Bundle assertions; this module checks its retained
result against every coordinator row before calculating paired statistics.
"""

import argparse
import csv
import hashlib
import json
import random
import re
import statistics
import sys
from pathlib import Path

sys.dont_write_bytecode = True
from image_compare_analysis import Evidence, distribution, percentile, positive_ns, require  # noqa: E402 - import after module-loading setup

SCOPE = "same-host-staged-lower-readonly"
GROUPS = ("pvisor", "turbo")
CACHES = ("cold", "warm")
RUNS = 30
WARMUPS = 3
BOOTSTRAPS = 5000
SEED = 20261017
WORKLOAD_BYTES = 2575738
TIME_METRICS = ("ready_ns", "completion_ns")
BYTE_METRICS = ("content_bytes", "metadata_bytes", "response_bytes")
ROW_FIELDS = (
    "phase",
    "round",
    "group",
    "cache",
    "valid",
    *TIME_METRICS,
    *BYTE_METRICS,
    "evidence_dir",
)
STAGE_METRICS = ("stage_ready_ns", "stage_completion_ns")


def byte_distribution(values):
    """Use the same scale-invariant split rule while retaining byte units."""
    result = distribution([value * 1_000_000 for value in values])
    converted = {
        key.replace("_ms", "_bytes"): value for key, value in result.items() if key != "clusters"
    }
    converted["clusters"] = [
        {key.replace("_ms", "_bytes"): value for key, value in cluster.items()}
        for cluster in result["clusters"]
    ]
    converted["unit"] = "bytes"
    return converted


def paired_comparison(cache, metric, values, summary, seed):
    """Resample whole paired rounds; compare marginal medians, not median deltas."""
    left = values["pvisor"][cache][metric]
    right = values["turbo"][cache][metric]
    divisor = 1_000_000 if metric.endswith("_ns") else 1
    unit = "ms" if divisor != 1 else "bytes"
    a, b = statistics.median(left), statistics.median(right)
    rng = random.Random(seed)
    differences, ratios = [], []
    for _ in range(BOOTSTRAPS):
        indices = rng.choices(range(RUNS), k=RUNS)
        x = statistics.median(left[index] for index in indices)
        y = statistics.median(right[index] for index in indices)
        differences.append((x - y) / divisor)
        if y > 0:
            ratios.append(x / y)
    separated = any(summary[group][cache][metric]["clusters"] for group in GROUPS)
    ratio_defined = b > 0 and len(ratios) == BOOTSTRAPS
    ci = [percentile(differences, 0.025), percentile(differences, 0.975)]
    return {
        "cache": cache,
        "metric": metric,
        "unit": unit,
        "left": "pvisor",
        "right": "turbo",
        "n_pairs": RUNS,
        "median_difference": (a - b) / divisor,
        "median_difference_ci95": ci,
        "median_ratio": a / b if ratio_defined else None,
        "median_ratio_ci95": [percentile(ratios, 0.025), percentile(ratios, 0.975)]
        if ratio_defined
        else None,
        "ratio_defined": ratio_defined,
        "ratio_note": None
        if ratio_defined
        else "Zero denominator in observed or resampled median; ratio is undefined.",
        "single_mode_ranking": not separated,
        "interpretation": (
            "separated distributions; report cluster proportions rather than one ranking"
            if separated
            else "median difference interval includes zero"
            if ci[0] <= 0 <= ci[1]
            else "median difference interval excludes zero"
        ),
    }


def file_path(batch, value):
    require(isinstance(value, str) and value, "evidence_dir must be a nonempty path string")
    path = Path(value)
    return (path if path.is_absolute() else batch / path).resolve(strict=True)


def analyze(batch):
    """Validate a complete cohort, then write its summary, report and sample CSV."""
    batch = Path(batch).resolve(strict=True)
    evidence = Evidence()
    protocol = evidence.json(batch / "protocol.json")
    require(
        protocol.get("scope") == SCOPE
        and protocol.get("groups") == list(GROUPS)
        and protocol.get("runs") == RUNS
        and protocol.get("warmups") == WARMUPS,
        "unexpected stage-lower cohort scope/groups/sample counts",
    )
    require(
        re.fullmatch(r"sha256:[a-f0-9]{64}", protocol.get("source_digest", "")) is not None,
        "source must be frozen by OCI SHA256 digest",
    )
    files = protocol.get("source_files")
    require(
        isinstance(files, dict) and len(files) == 4,
        "protocol must declare the four readonly source files",
    )
    for name, item in files.items():
        require(
            isinstance(name, str)
            and not Path(name).is_absolute()
            and ".." not in Path(name).parts
            and isinstance(item, dict)
            and type(item.get("bytes")) is int
            and item["bytes"] > 0
            and isinstance(item.get("sha256"), str)
            and re.fullmatch(r"[a-f0-9]{64}", item["sha256"]),
            "source file receipt must have a relative path, byte count and SHA256",
        )
    require(
        sum(item["bytes"] for item in files.values()) == WORKLOAD_BYTES,
        "source workload byte count changed",
    )
    planned = []
    for phase, count in (("warmup", WARMUPS), ("sample", RUNS)):
        orders = protocol.get("orders", {}).get(phase)
        require(
            isinstance(orders, list) and len(orders) == count, "wrong number of paired round orders"
        )
        for index, order in enumerate(orders):
            require(
                isinstance(order, list) and sorted(order) == sorted(GROUPS),
                "each round must contain both groups exactly once",
            )
            planned.extend((phase, index, group, cache) for group in order for cache in CACHES)
    records = evidence.json(batch / "records.json")
    require(
        isinstance(records, list) and len(records) == 132,
        "exactly 12 warmup and 120 sample records are required; failures cannot be omitted",
    )
    require(all(isinstance(row, dict) for row in records), "each record must be an object")
    stage_metrics = []
    for metric in STAGE_METRICS:
        present = [metric in row for row in records]
        require(
            all(present) or not any(present),
            "each stage subphase metric must be present in all 132 trials or absent",
        )
        if all(present):
            stage_metrics.append(metric)
    stage_metrics = tuple(stage_metrics)
    metrics = TIME_METRICS + BYTE_METRICS + stage_metrics
    values = {
        group: {cache: {metric: [] for metric in metrics} for cache in CACHES} for group in GROUPS
    }
    directories = set()
    for row, expected in zip(records, planned, strict=True):
        require(
            isinstance(row, dict) and all(name in row for name in ROW_FIELDS),
            "record is missing required fields",
        )
        require(
            type(row["round"]) is int
            and (row["phase"], row["round"], row["group"], row["cache"]) == expected,
            "record order differs from the frozen plan; cold must immediately precede warm",
        )
        require(row["valid"] is True, "failed trial invalidates the entire cohort")
        require(
            all(positive_ns(row[name]) for name in TIME_METRICS)
            and row["ready_ns"] <= row["completion_ns"],
            "invalid Ready/completion durations",
        )
        require(
            all(type(row[name]) is int and row[name] >= 0 for name in BYTE_METRICS),
            "byte counts must be nonnegative integers",
        )
        require(
            row["response_bytes"] >= row["content_bytes"] + row["metadata_bytes"],
            "total response bytes cannot be smaller than classified payload",
        )
        require(
            row["content_bytes"] > 0 if row["cache"] == "cold" else row["content_bytes"] == 0,
            "every cold trial must fetch content and every warm trial must reuse it",
        )
        for metric in stage_metrics:
            full_metric = metric.removeprefix("stage_")
            require(
                positive_ns(row[metric]) and row[metric] <= row[full_metric],
                "mounted-stage subphase must fit inside the full trial",
            )
        if set(STAGE_METRICS).issubset(stage_metrics):
            require(
                row["stage_ready_ns"] <= row["stage_completion_ns"],
                "stage Ready must precede stage completion",
            )
        directory = file_path(batch, row["evidence_dir"])
        require(
            directory.is_dir() and str(directory) not in directories,
            "evidence directory is missing or reused",
        )
        directories.add(str(directory))
        result = evidence.json(directory / "result.json")
        require(
            isinstance(result, dict)
            and all(name in result and result[name] == value for name, value in row.items()),
            "retained result.json does not match the complete coordinator row projection",
        )
        if "exit_code" in result:
            require(
                type(result["exit_code"]) is int and result["exit_code"] == 0,
                "trial did not exit normally",
            )
        if row["phase"] == "sample":
            for metric in metrics:
                values[row["group"]][row["cache"]][metric].append(row[metric])
    for group in GROUPS:
        for cache in CACHES:
            require(
                all(len(xs) == RUNS for xs in values[group][cache].values()),
                "incomplete paired cell",
            )
    summary = {
        group: {
            cache: {
                metric: (distribution(xs) if metric.endswith("_ns") else byte_distribution(xs))
                for metric, xs in cell.items()
            }
            for cache, cell in states.items()
        }
        for group, states in values.items()
    }
    comparisons = [
        paired_comparison(cache, metric, values, summary, SEED + index)
        for index, (cache, metric) in enumerate(
            (cache, metric) for cache in CACHES for metric in metrics
        )
    ]
    # Hash only declared inputs; no credentials or raw protocol body are copied
    # into derived reports. The main subject retains its semantic assertions.
    artifacts = protocol.get("artifacts", {})
    require(isinstance(artifacts, dict), "protocol artifacts must map paths to SHA256 hashes")
    for value, expected_digest in artifacts.items():
        require(
            isinstance(expected_digest, str) and re.fullmatch(r"[a-f0-9]{64}", expected_digest),
            "artifact receipt requires a SHA256 hex string",
        )
        path = file_path(batch, value)
        body = evidence.read(path)
        require(
            hashlib.sha256(body).hexdigest() == expected_digest,
            "frozen artifact changed: " + str(path),
        )
    evidence.finish()
    analysis = {
        "status": "passed",
        "scope": SCOPE,
        "source_digest": protocol["source_digest"],
        "groups": list(GROUPS),
        "caches": list(CACHES),
        "runs": RUNS,
        "warmups": WARMUPS,
        "audited_records": 132,
        "sample_records": 120,
        "warmup_records": 12,
        "failed": 0,
        "excluded_slow_samples": 0,
        "workload": {
            "files": 4,
            "bytes": WORKLOAD_BYTES,
            "readonly": True,
            "source_files": files,
            "executor": "same pVisor host staged",
            "executes_oci_program": False,
        },
        "transport": protocol.get(
            "transport",
            "Private Unix proxy to local TCP cache versus lab-local HTTP registry; no WAN.",
        ),
        "summary": summary,
        "comparisons": comparisons,
        "statistics": {
            "bootstrap_replicates": BOOTSTRAPS,
            "bootstrap_seed": SEED,
            "bootstrap_unit": "paired round",
            "estimand": "difference and ratio of marginal medians",
            "split_rule": "both clusters >= max(5,10% N); largest gap >=20% median and >3x median adjacent gap; cluster medians >=1.5x",
            "p95": "descriptive only at N=30",
        },
        "audit": {
            "evidence_sha256": evidence.files,
            "validation": "132 retained result files match complete row projections; timing/byte/cache/order/artifact gates passed",
            "semantic_validation": "File hashes, mount identity, host Bundle, empty changeset and cleanup are asserted by the subject harness.",
        },
        "limitations": [
            "Same host staged executor reads four fixed image files (2575738 bytes); this is not execution of an OCI image program.",
            "Full clocks include client open/rpull, filesystem mount and the same staged CLI; server preparation/conversion/reset are separate.",
            "Local transport paths differ: cache Unix proxy/loopback TCP and lab-local HTTP registry. No WAN is measured.",
            "Host page cache is retained. Cold means cleared private client state, not a cold disk or fully cold node.",
            "Matching workload affinity does not establish equal whole-node CPU/memory budgets or absence of shared-resource interference.",
            "Response accounting follows each application protocol, not wire bytes; no Ready traffic split is inferred.",
            "Separated distributions are shown by clusters; no single median ranking is inferred. No prior batch is merged.",
        ],
    }
    row = {"Pairs": RUNS, "Failures": 0, "Excluded": 0}
    for group in GROUPS:
        for cache in CACHES:
            for metric in metrics:
                unit = "ms" if metric.endswith("_ns") else "B"
                median_key = "median_ms" if unit == "ms" else "median_bytes"
                label = metric.removesuffix("_ns")
                row[f"{group} {cache} {label} median {unit}"] = summary[group][cache][metric][
                    median_key
                ]
    analysis["row"] = row
    (batch / "summary.json").write_text(json.dumps(analysis, ensure_ascii=False, indent=2) + "\n")
    with (batch / "samples.csv").open("w", newline="") as stream:
        fields = list(ROW_FIELDS) + list(stage_metrics)
        writer = csv.DictWriter(
            stream, fieldnames=fields, lineterminator="\n", extrasaction="ignore"
        )
        writer.writeheader()
        writer.writerows(records)
    (batch / "report.md").write_text(render(analysis))
    return analysis


def render(analysis, maybe_analysis=None):
    """Render the full latency and byte comparison without hiding separated modes."""
    if maybe_analysis is not None:  # Accept the older render(batch, analysis) convention too.
        analysis = maybe_analysis
    require(
        analysis.get("status") == "passed" and analysis.get("scope") == SCOPE,
        "cannot render failed or unrelated analysis",
    )

    def cell(row, unit):
        suffix = "_ms" if unit == "ms" else "_bytes"
        if row["clusters"]:
            return "; ".join(
                f"{cluster['median' + suffix]:.2f}（{cluster['n']}/30）"
                for cluster in row["clusters"]
            )
        return f"{row['median' + suffix]:.2f}" if unit == "ms" else f"{row['median' + suffix]:.0f}"

    lines = [
        "# 同一 host staged 执行器的 lower 文件读取对照",
        "",
        "两种 lower 均由同一 pVisor host staged 执行器只读校验 4 个文件，共 2,575,738 B；"
        "这里测量文件加载路径，不代表 OCI 镜像内程序启动。",
        "本批次 12 次预热、120 次正式执行的记录与原始 result 一致；各格 30 个样本，无失败或慢样本剔除。",
        "",
        "| lower | 缓存 | 完整 Ready（ms） | 完整 Completion（ms） | 内容响应（B） | 元数据响应（B） | 总响应（B） |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    labels = {"pvisor": "cache host FUSE", "turbo": "TurboOCI / OverlayBD ext4"}
    for group in GROUPS:
        for cache in CACHES:
            row = analysis["summary"][group][cache]
            cells = [
                cell(row[metric], "ms" if metric.endswith("_ns") else "bytes")
                for metric in TIME_METRICS + BYTE_METRICS
            ]
            lines.append(f"| {labels[group]} | {cache} | " + " | ".join(cells) + " |")
    lines += [
        "",
        "表内非分离分布展示中位数；分离分布展示各簇中位数及样本数，不用一个 P50 排名。",
        "",
        "| 缓存 | 指标 | 中位数差 pVisor − Turbo（ms）及配对 95% CI | 中位数比值及 95% CI |",
        "| --- | --- | ---: | ---: |",
    ]
    for comparison in analysis["comparisons"]:
        if comparison["metric"] not in TIME_METRICS:
            continue
        low, high = comparison["median_difference_ci95"]
        ratio = comparison["median_ratio"]
        lo, hi = comparison["median_ratio_ci95"]
        caveat = "；分离分布，不作整体排名" if not comparison["single_mode_ranking"] else ""
        lines.append(
            f"| {comparison['cache']}{caveat} | {comparison['metric'][:-3]} | "
            f"{comparison['median_difference']:.2f} [{low:.2f}, {high:.2f}] | {ratio:.3f} [{lo:.3f}, {hi:.3f}] |"
        )
    stage_metrics = [
        metric for metric in STAGE_METRICS if metric in analysis["summary"]["pvisor"]["cold"]
    ]
    if stage_metrics:
        headings = [
            "stage Ready（ms）" if metric == "stage_ready_ns" else "stage Completion（ms）"
            for metric in stage_metrics
        ]
        lines += [
            "",
            "| lower | 缓存 | " + " | ".join(headings) + " |",
            "| --- | --- | " + " | ".join("---:" for _ in stage_metrics) + " |",
        ]
        for group in GROUPS:
            for cache in CACHES:
                row = analysis["summary"][group][cache]
                lines.append(
                    f"| {labels[group]} | {cache} | "
                    + " | ".join(cell(row[metric], "ms") for metric in stage_metrics)
                    + " |"
                )
    lines += [
        "",
        "完整时钟包含客户端 open/rpull、挂载和同一 staged CLI；镜像服务准备、转换和冷状态重置另计。"
        "四个文件的内容摘要、真实挂载、host Bundle、空 changeset 和清理由 subject harness 校验。",
        "pVisor cache 经 Unix socket 代理到本机 loopback TCP，OverlayBD 使用 lab 内部 loopback HTTP；没有 WAN。"
        "宿主页缓存保留，冷客户端不等于冷磁盘。相同执行器和亲和性不代表等整机预算，也不隔绝共享资源干扰。",
        "响应字节按各自应用协议记录，不等于线上流量；不推算 Ready 截点。"
        "配对 bootstrap 为 5,000 次，单位是同轮两组；差值与比值针对边际中位数，P95 仅作参考。"
        "本批次不复用或合并原 VM/容器批次。",
        "",
        "数据：summary.json、samples.csv；逐项证据 SHA256 保存在 summary.json 的 audit 中。",
        "",
    ]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    try:
        analysis = analyze(args.batch)
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(1, f"Analysis refused: {error}\n")
    print(
        json.dumps(
            {
                "status": analysis["status"],
                "audited_records": analysis["audited_records"],
                "summary": str(args.batch.resolve() / "summary.json"),
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    sys.exit(main())
