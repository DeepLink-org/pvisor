#!/usr/bin/env python3
"""Bind Windows hardware acceptance to the exact candidate inputs."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

INPUTS = (
    "pvisor.exe", "examples/whp_smoke.exe", "libkrunfw.dll", "whp-guest",
    "vm-guest", "pvisor-rootfs.bin.gz",
)
COVERAGE = {
    "smoke": "whp-api-startup-exit-and-files",
    "vm-cli": "whp-common-vm-cli",
    "containers": "whp-containers-network-console-two-rounds",
}


def inputs(root: Path, source_sha: str) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise ValueError("expected a full source commit SHA")
    hashes = {}
    for name in INPUTS:
        path = root / name
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError(f"missing or invalid candidate input: {name}")
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        hashes[name] = digest.hexdigest()
    return {"schema_version": 1, "source_sha": source_sha, "inputs_sha256": hashes}


def read(path: Path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def record(root: Path, evidence: Path, source_sha: str, suite: str) -> dict:
    frozen = inputs(root, source_sha)
    if read(evidence / "inputs.json") != frozen:
        raise ValueError("candidate inputs changed during acceptance")
    if read(evidence / "remaining-processes.json") != []:
        raise ValueError("acceptance cleanup is not confirmed")
    if suite == "smoke":
        log = (evidence / "smoke.log").read_text(encoding="utf-8-sig")
        for cpus, code in [(1, "abort"), (1, "0"), (2, "0"), (2, "7"), (1, "255")]:
            if f"WHP_SMOKE cpus={cpus} exit={code} PASS" not in log.splitlines():
                raise ValueError("missing successful WHP smoke scenario")
    else:
        reports = ["vm-cli/report.json"] if suite == "vm-cli" else [
            "containers-1/report.json", "containers-2/report.json"
        ]
        for name in reports:
            report = read(evidence / name)
            expected_hashes = {
                "binary_sha256": frozen["inputs_sha256"]["pvisor.exe"],
                "firmware_sha256" if suite == "vm-cli" else "libkrunfw.dll_sha256":
                    frozen["inputs_sha256"]["libkrunfw.dll"],
                "guest_sha256" if suite == "vm-cli" else "pvisor-rootfs.bin.gz_sha256":
                    frozen["inputs_sha256"]["vm-guest" if suite == "vm-cli" else "pvisor-rootfs.bin.gz"],
            }
            if any(report.get(key) != digest for key, digest in expected_hashes.items()):
                raise ValueError(f"driver did not exercise these candidate bytes: {name}")
            cases = report.get("cases" if suite == "vm-cli" else "commands")
            if (report.get("passed") is not True or not cases
                    or not all(case.get("passed") is True for case in cases)
                    or report.get("cleanup_errors")):
                raise ValueError(f"acceptance failed: {name}")
    return {**frozen, "status": "PASS", "suite": suite, "coverage": COVERAGE[suite]}


def qualify(root: Path, receipts: Path, source_sha: str) -> dict:
    frozen = inputs(root, source_sha)
    for suite, coverage in COVERAGE.items():
        receipt = read(receipts / f"windows-vm-whp-{suite}" / "acceptance.json")
        expected = {**frozen, "status": "PASS", "suite": suite, "coverage": coverage}
        if receipt != expected:
            raise ValueError(f"{suite}: acceptance does not match candidate and coverage")
    return {**frozen, "status": "PASS", "coverage": COVERAGE}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["snapshot", "record", "qualify"])
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--suite", choices=COVERAGE)
    args = parser.parse_args()
    # Withdraw stale success before validation; failure must leave no PASS receipt.
    args.output.unlink(missing_ok=True)
    if args.mode == "snapshot":
        result = inputs(args.inputs, args.source_sha)
    elif args.mode == "record":
        if args.evidence is None or args.suite is None:
            parser.error("record requires --evidence and --suite")
        result = record(args.inputs, args.evidence, args.source_sha, args.suite)
    else:
        if args.evidence is None:
            parser.error("qualify requires --evidence")
        result = qualify(args.inputs, args.evidence, args.source_sha)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
