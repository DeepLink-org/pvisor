#!/usr/bin/env python3
"""Independently audit raw B-PEER-CHAOS-ENG results, without running workloads."""

import argparse
import collections
import hashlib
import json
import statistics
from pathlib import Path


def load(path):
    return json.loads(path.read_text())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    root = args.directory.resolve()
    manifest, summary = load(root / "manifest.json"), load(root / "summary.json")
    assert manifest["benchmark"] == "B-PEER-CHAOS-ENG" and summary["verified"]
    assert summary["failures"] == summary["excluded"] == 0
    assert not (root / "failure.json").exists()
    for name, digest in manifest["harness_sha256"].items():
        assert hashlib.sha256((root / "harness" / name).read_bytes()).hexdigest() == digest
    for name, digest in manifest["config"]["artifact_sha256"].items():
        assert hashlib.sha256(Path(name).read_bytes()).hexdigest() == digest
    assert summary["traffic_seconds"] >= manifest["seconds"]
    assert len(summary["faults"]) == manifest["fault_count"]
    cases = collections.Counter()
    tcp_bytes, tcp_requests, raw_clients = 0, 0, 0
    udp_requests, udp_attempts, udp_timeouts, udp_duplicates, udp_bytes = 0, 0, 0, 0, 0
    for path in root.rglob("result.json"):
        result = load(path)
        if isinstance(result, list):
            assert len(result) == 4
            for node, record in enumerate(result):
                assert load(path.parent / f"node-{node}.stdout") == record
                assert record["verified"] and record["node"] == node and record["requests"] == 540
                assert record["bytes"] == 318240 and record["attempts"] >= record["requests"]
                assert not (path.parent / f"node-{node}.stderr").read_text()
                udp_requests += record["requests"]
                udp_attempts += record["attempts"]
                udp_timeouts += record["timeouts"]
                udp_duplicates += record["duplicates"]
                udp_bytes += record["bytes"]
                raw_clients += 1
        elif "case" in result:
            case = result["case"]
            cases[case] += 1
            expected = {
                "rpc": (384000, 1500),
                "healthy": (128000, 500),
                "bulk-long": (100663296, 0),
                "ring": (51200, 200),
                "slow": (128000, 500),
            }[case]
            assert len(result["nodes"]) == (3 if case == "healthy" else 4)
            total_bytes = 0
            for node, record in result["nodes"].items():
                lines = (path.parent / f"client-{node}.stdout").read_text().splitlines()
                assert len(lines) == 2 and lines[0] == f"READY {node} {case}"
                assert json.loads(lines[1]) == record
                assert not (path.parent / f"client-{node}.stderr").read_text()
                assert record["verified"] and record["scenario"] == case
                assert (record["bytes"], len(record["rtt_us"])) == expected
                total_bytes += record["bytes"]
                tcp_requests += len(record["rtt_us"])
                raw_clients += 1
            assert total_bytes == result["payload_bytes"]
            tcp_bytes += total_bytes
            for state in result["after"]:
                assert state["peak"] <= 512 * 1024**2 and state["swap"] == 0
                assert all(state["events"].get(key, 0) == 0 for key in ("oom", "oom_kill", "max"))
    assert tcp_bytes == summary["tcp_payload_bytes"]
    assert tcp_requests == summary["tcp_echo_requests"]
    assert udp_requests == summary["udp_requests"]
    assert udp_attempts == summary["udp_attempts"]
    assert udp_timeouts == summary["udp_timeouts"]
    assert udp_duplicates == summary["udp_duplicates"]
    assert udp_bytes == summary["udp_payload_bytes"]
    for fault, kind in zip(summary["faults"], manifest["schedule"]):
        assert fault["verified"] and fault["kind"] == kind
        if kind == "freeze":
            assert fault["frozen_seconds"] >= 3 and "frozen 1" in fault["frozen_events"]
        else:
            assert fault["before_generation"] != fault["after_generation"]
            assert (fault["before_instance"] != fault["after_instance"]) == (kind == "recreate")
    dns = list(root.glob("dns-initial/*.txt")) + list(root.glob("fault-*/dns-after/*.txt"))
    assert len(dns) == 12 * (manifest["fault_count"] + 1)
    assert all(path.read_text().startswith("RESOLVED ") for path in dns)
    persistent = []
    for record in summary["persistent_tcp"]:
        lines = (root / f"persistent-tcp-{record['node']}.log").read_text().splitlines()
        assert len(lines) == 2 and json.loads(lines[1]) == record
        assert record["verified"] and record["seconds"] >= manifest["seconds"]
        sizes, count = [32, 64, 256, 512, 1200, 1472], record["requests"]
        assert record["bytes"] == (count // 6) * sum(sizes) + sum(sizes[: count % 6])
        persistent.append(record)
    assert len(persistent) == 3
    samples = [json.loads(line) for line in (root / "resources.jsonl").read_text().splitlines()]
    assert len(samples) >= manifest["seconds"] * 0.8
    resources = {}
    for node in range(4):
        values = [
            (sample["time"], sample["nodes"][str(node)])
            for sample in samples
            if str(node) in sample["nodes"]
        ]
        for _, state in values:
            assert state["peak"] <= 512 * 1024**2 and state["swap"] == 0
            assert all(state["events"].get(key, 0) == 0 for key in ("oom", "oom_kill", "max"))
        resources[node] = {
            "samples": len(values),
            "max_peak_mib": max(v["peak"] for _, v in values) / 2**20,
            "first_60s_memory_median_mib": statistics.median(
                v["memory"] for t, v in values if t < samples[0]["time"] + 60
            )
            / 2**20,
            "last_60s_memory_median_mib": statistics.median(
                v["memory"] for t, v in values if t > samples[-1]["time"] - 60
            )
            / 2**20,
            "max_total_fds": max(sum(p["fds"] for p in v["processes"]) for _, v in values),
            "max_total_threads": max(sum(p["threads"] for p in v["processes"]) for _, v in values),
            "first_60s_total_fds_median": statistics.median(
                sum(p["fds"] for p in v["processes"])
                for t, v in values
                if t < samples[0]["time"] + 60
            ),
            "last_60s_total_fds_median": statistics.median(
                sum(p["fds"] for p in v["processes"])
                for t, v in values
                if t > samples[-1]["time"] - 60
            ),
        }
    for node, before in summary["persistent_processes_before"].items():
        after = summary["persistent_processes_after"][node]
        initial = {(p["pid"], p["starttime"]) for p in before if p["comm"].startswith("pvisor")}
        assert initial <= {(p["pid"], p["starttime"]) for p in after}
        resources[int(node)]["initial_fds"] = sum(p["fds"] for p in before)
        resources[int(node)]["final_fds"] = sum(p["fds"] for p in after)
        resources[int(node)]["initial_threads"] = sum(p["threads"] for p in before)
        resources[int(node)]["final_threads"] = sum(p["threads"] for p in after)
    assert load(root / "group/cleanup.json") == {"failures": []}
    output = {
        "verified": True,
        "raw_client_outputs": raw_clients,
        "tcp_cases": cases,
        "tcp_payload_bytes": tcp_bytes,
        "tcp_echo_requests": tcp_requests,
        "slow_reader_additional_verified_bytes": cases["slow"] * 8388608,
        "udp_requests": udp_requests,
        "udp_attempts": udp_attempts,
        "udp_timeouts": udp_timeouts,
        "udp_duplicates": udp_duplicates,
        "udp_payload_bytes": udp_bytes,
        "dns_checks": len(dns),
        "persistent_tcp": persistent,
        "resources": resources,
        "resource_samples": len(samples),
        "foreign_activity_samples": sum(bool(s.get("foreign_work")) for s in samples),
        "foreign_activity": list(
            {
                json.dumps(a, sort_keys=True): a
                for sample in samples
                for a in sample.get("foreign_work", [])
            }.values()
        ),
        "summary_sha256": hashlib.sha256((root / "summary.json").read_bytes()).hexdigest(),
        "audit_source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }
    (root / "audit.json").write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
