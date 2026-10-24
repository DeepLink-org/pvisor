#!/usr/bin/env python3
"""Analyze B-OPENSANDBOX-MEMORY-ENG complete five-round paired cohorts."""

import argparse
import hashlib
import json
import random
import statistics
from pathlib import Path

ARMS = ["native", "native-pool", "opensandbox-docker"]
PHASES = ["service", "idle-1", "idle-4", "shared-4", "unique-4"]


def compare(x, y):
    assert len(x) == len(y) >= 5
    rng = random.Random(20261024)
    differences, percentages = [], []
    for _ in range(10000):
        chosen = rng.choices(range(len(x)), k=len(x))
        a = statistics.median(x[i] for i in chosen)
        b = statistics.median(y[i] for i in chosen)
        differences.append(a - b)
        percentages.append((1 - a / b) * 100)
    differences.sort()
    percentages.sort()
    return {
        "difference": statistics.median(x) - statistics.median(y),
        "difference_95_ci": [differences[249], differences[9749]],
        "reduction_percent": (1 - statistics.median(x) / statistics.median(y)) * 100,
        "reduction_95_ci_percent": [percentages[249], percentages[9749]],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    rows = json.loads((args.batch / "results.json").read_text())
    execution = json.loads((args.batch / "execution-receipt.json").read_text())
    assert execution["preflight"] is False
    assert (
        hashlib.sha256((args.batch / "harness.py").read_bytes()).hexdigest()
        == execution["harness_sha256"]
    )
    assert len(rows) >= 15
    assert all(
        r["success"] and r["all_deleted"] and r["api_inventory_empty"] and not r["observer_errors"]
        for r in rows
    )
    order = json.loads((args.batch / "order.json").read_text())
    assert [(r["repeat"], r["arm"]) for r in rows] == [(r["repeat"], r["arm"]) for r in order]
    for row in rows:
        assert set(row["phases"]) == set(PHASES)
        checks = [c for c in row["checks"] if "bytes" in c]
        assert len(checks) == 16 and all(
            c["bytes"] == 64 * 1024**2 and c["verified"] for c in checks
        )
        assert all(c["seed"] == 123456789 for c in checks[:8])
        assert len({c["sum"] for c in checks[:8]}) == 1
        expected = [987654321 + i * 100000000 for i in range(4)]
        assert [c["seed"] for c in checks[8:12]] == [c["seed"] for c in checks[12:]] == expected
        assert [c["sum"] for c in checks[8:12]] == [c["sum"] for c in checks[12:]]
    arms = {a: sorted([r for r in rows if r["arm"] == a], key=lambda r: r["repeat"]) for a in ARMS}
    repeats = [r["repeat"] for r in arms["native"]]
    assert len(repeats) >= 5 and len(set(repeats)) == len(repeats)
    assert all([r["repeat"] for r in arms[a]] == repeats for a in ARMS)
    ids = [i["id"] for r in rows for i in r["instances"]]
    assert len(ids) == len(set(ids)) == len(rows) * 4
    result = {
        "benchmark": "B-OPENSANDBOX-MEMORY-ENG",
        "rounds": len(repeats),
        "fresh_instances": len(ids),
        "formal_groups": len(rows),
        "failed_groups": 0,
        "excluded_groups": 0,
        "units": "MiB = bytes/1048576; primary is complete product-slice cgroup charged memory; PSS only if every process is readable; not whole-host net physical memory",
        "phases": {},
    }
    for phase in PHASES:
        result["phases"][phase] = {}
        for metric in ["pss", "charged", "cpu_seconds"]:
            scale = 1 if metric == "cpu_seconds" else 1024**2
            values = {
                a: [r["phases"][phase][metric] / scale for r in arms[a]]
                for a in ARMS
                if all(r["phases"][phase][metric] is not None for r in arms[a])
            }
            stats = {
                a: {"median": statistics.median(v), "range": [min(v), max(v)], "samples": v}
                for a, v in values.items()
            }
            for arm in set(ARMS) - set(values):
                stats[arm] = {
                    "available": False,
                    "reason": "non-dumpable product processes deny PSS; full cgroup charged memory is available",
                }
            if metric != "cpu_seconds":
                for key, left, right in [
                    ("native_vs_docker", "native", "opensandbox-docker"),
                    ("pool_vs_docker", "native-pool", "opensandbox-docker"),
                    ("pool_vs_native", "native-pool", "native"),
                ]:
                    if left in values and right in values:
                        stats[key] = compare(values[left], values[right])
            result["phases"][phase][metric] = stats
        print(phase, {a: round(result["phases"][phase]["charged"][a]["median"], 2) for a in ARMS})
    for metric in ["charged", "pss"]:
        key = "incremental_" + metric + "_mib"
        result[key] = {}
        for phase, count in [("idle-1", 1), ("idle-4", 4)]:
            result[key][phase] = {}
            for arm in ARMS:
                if any(
                    r["phases"][p][metric] is None for r in arms[arm] for p in [phase, "service"]
                ):
                    continue
                values = [
                    (r["phases"][phase][metric] - r["phases"]["service"][metric]) / count / 1024**2
                    for r in arms[arm]
                ]
                result[key][phase][arm] = {
                    "median": statistics.median(values),
                    "range": [min(values), max(values)],
                    "samples": values,
                }
    before, after = [
        json.loads((args.batch / name).read_text())
        for name in ["host-before.json", "host-after.json"]
    ]
    assert sorted(before["host_docker_ids"]) == sorted(after["host_docker_ids"])
    assert before["ksm"]["run"] == after["ksm"]["run"]
    result["evidence"] = {
        name: hashlib.sha256((args.batch / name).read_bytes()).hexdigest()
        for name in [
            "results.json",
            "order.json",
            "execution-receipt.json",
            "input-receipt.json",
            "harness.py",
            "host-before.json",
            "host-after.json",
        ]
    }
    (args.batch / "summary.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
