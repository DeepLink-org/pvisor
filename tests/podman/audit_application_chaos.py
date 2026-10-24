#!/usr/bin/env python3
"""Audit saved application chaos evidence without starting an application."""

import argparse
import collections
import hashlib
import json
from pathlib import Path


def load(path):
    return json.loads(path.read_text())


def audit(root):
    manifest = load(root / "manifest.json")
    summary = load(root / "summary.json")
    assert summary["passed"] and not summary["monitor_errors"]
    assert not (root / "failure.json").exists()
    for field in ("binary", "http_binary"):
        assert (
            hashlib.sha256(Path(manifest[field]).read_bytes()).hexdigest()
            == manifest[field + "_sha256"]
        )
    rounds = sorted((root / "rounds").glob("*.json"))
    assert len(rounds) == len(manifest["schedule"])
    counts = collections.Counter()
    hashes = {hashlib.sha256(b"initial\n").hexdigest()}
    for index, (case, path) in enumerate(zip(manifest["schedule"], rounds)):
        assert path.stem == f"{index:03d}-{case}"
        record = load(path)
        assert record["verified"]
        hashes.add(record["state_sha256"])
        counts[case] += 1
    assert counts == summary["cases"]
    background = [
        json.loads(line) for line in (root / "background-http.jsonl").read_text().splitlines()
    ]
    assert background
    for record in background:
        if record.get("verified"):
            assert record["sha256"] in hashes
        else:
            assert record["expected_fault"] is True
    http = [json.loads(line) for line in (root / "http.jsonl").read_text().splitlines()]
    assert all(r["verified"] and r["status"] == 200 and r["sha256"] in hashes for r in http)
    scopes = list((root / "commands").glob("*/scope.json"))
    for path in scopes:
        limits = load(path)["limits"]
        assert limits["memory.max"] == str(512 * 1024**2) and limits["memory.swap.max"] == "0"
        assert limits["pids.max"] == "256"
        quota, period = map(int, limits["cpu.max"].split())
        assert quota == period
    command_count = 0
    for path in (root / "commands").glob("*/command.json"):
        command_count += 1
        command = load(path)
        status = load(path.parent / "exit.json")
        assert not status["timed_out"]
        code, arguments = status["code"], command["arguments"]
        stderr = (path.parent / "stderr").read_bytes()
        stdout = (path.parent / "stdout").read_bytes()
        if code == 1:
            assert (arguments[0] == "build" and b"RUN exited with status 7" in stderr) or (
                arguments[0] == "rm" and b"no such container" in stderr.lower()
            )
        elif code == 125:
            assert arguments[0] == "run" and (
                b"IPv4 address already allocated" in stderr
                or b"address already in use" in stderr.lower()
            )
        elif code == 127:
            assert "/missing-application-chaos" in arguments
        else:
            assert code in {0, 17, 23, 29}, (path, code)
        if command["stdin_bytes"] and any("cat;" in value for value in arguments):
            assert len(stdout) == command["stdin_bytes"]
            assert hashlib.sha256(stdout).hexdigest() == command["stdin_sha256"]
    assert command_count == summary["commands"]
    samples = [json.loads(line) for line in (root / "resources.jsonl").read_text().splitlines()]
    peak = 0
    maximum_live = 0
    for sample in samples:
        maximum_live = max(maximum_live, len(sample["instances"]))
        assert len(sample["instances"]) <= 4
        for state in sample["instances"].values():
            peak = max(peak, state["peak"])
            assert state["peak"] <= 512 * 1024**2 and state["swap"] == 0
            assert all(state["events"].get(k, 0) == 0 for k in ("oom", "oom_kill", "max"))
    assert load(root / "cleanup.json") == {"errors": []}
    half_closed = [load(p) for p in root.glob("half-close-*.json")]
    assert len(half_closed) == counts["abort_clients"] and all(r["verified"] for r in half_closed)
    result = {
        "passed": True,
        "seed": manifest["seed"],
        "rounds": len(rounds),
        "cases": counts,
        "commands": command_count,
        "scope_receipts": len(scopes),
        "resource_samples": len(samples),
        "sampled_max_live_vms": maximum_live,
        "max_sampled_cgroup_peak_mib": peak / 2**20,
        "http_responses_verified": len(http),
        "background_responses_verified": sum(bool(r.get("verified")) for r in background),
        "expected_fault_disconnects": sum(bool(r.get("expected_fault")) for r in background),
        "unexpected_faults": 0,
        "half_close_checks": len(half_closed),
        "harness_sha256": hashlib.sha256((root / "application_chaos.py").read_bytes()).hexdigest(),
        "summary_sha256": hashlib.sha256((root / "summary.json").read_bytes()).hexdigest(),
    }
    (root / "audit.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directories", type=Path, nargs="+")
    args = parser.parse_args()
    print(json.dumps([audit(p.resolve()) for p in args.directories], indent=2))


if __name__ == "__main__":
    main()
