#!/usr/bin/env python3
"""Summarize product-v1 evidence without pooling batches or dropping failures."""

import argparse
import collections
import csv
from pathlib import Path

from bench import percentile


def summarize(paths):
    output = []
    dimensions = (
        "suite",
        "workload",
        "backend",
        "agent",
        "files",
        "concurrency",
        "requested_state",
    )
    for path in paths:
        value = load_evidence(path)
        groups = collections.defaultdict(list)
        for row in value["rows"]:
            if row.get("correctness") != "passed":
                raise ValueError(f"{path}: failed samples must not be in rows")
            groups[tuple(row.get(key, "") for key in dimensions)].append(row)
        for key, rows in sorted(groups.items(), key=lambda item: str(item[0])):
            metrics = collections.defaultdict(list)
            for row in rows:
                for metric in (
                    "wall_ms",
                    "worker_ms",
                    "review_ms",
                    "apply_ms",
                    "drop_ms",
                    "recovery_ms",
                    "child_cpu_ms",
                    "peak_tree_rss_kib",
                ):
                    if metric in row:
                        metrics[metric].append(row[metric])
                metrics["job_wall_ms"].extend(row.get("job_wall_ms", []))
                check = row.get("check", {})
                if row["suite"] == "network":
                    metrics["request_ms"].extend(r["elapsed_ms"] for r in check.get("requests", []))
                    if row["workload"] == "bulk":
                        metrics["bulk_mib_per_s"].append(
                            check["bytes"] / 2**20 * 1000 / check["elapsed_ms"]
                        )
                    if row["workload"] == "stream":
                        metrics["first_body_ms"].append(check["first_byte_ms"])
            for metric, values in sorted(metrics.items()):
                if not values:
                    continue
                output.append(
                    dict(
                        batch=path.parent.name if path.stem == "report" else path.stem,
                        **dict(zip(dimensions, key)),
                        metric=metric,
                        n=len(values),
                        p50=percentile(values, 50),
                        p95=percentile(values, 95),
                        p99=percentile(values, 99),
                        minimum=min(values),
                        maximum=max(values),
                    )
                )
        for capability, details in value.get("capabilities", {}).items():
            if details["state"] == "available":
                continue
            output.append(
                dict(
                    batch=path.parent.name if path.stem == "report" else path.stem,
                    suite="capability",
                    workload=capability,
                    state=details["state"],
                    reason=details.get("reason", ""),
                    attempted=sum(b["attempted"] for b in details.get("batches", [])),
                    completed=sum(b["completed"] for b in details.get("batches", [])),
                )
            )
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--output-csv", type=Path, required=True)
    args = parser.parse_args()
    fields = (
        "batch",
        "suite",
        "workload",
        "backend",
        "agent",
        "files",
        "concurrency",
        "requested_state",
        "metric",
        "n",
        "p50",
        "p95",
        "p99",
        "minimum",
        "maximum",
        "state",
        "reason",
        "attempted",
        "completed",
    )
    with args.output_csv.open("w") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(summarize(args.reports))


if __name__ == "__main__":
    main()
