#!/usr/bin/env python3
"""Offline evidence audit for B-NETWORK-PROFILE-ENG; never runs a workload."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path


def load(path):
    return json.loads(path.read_text())


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def profiles(path):
    result = {}
    for line in path.read_text().splitlines():
        record = json.loads(line)
        if record.get("reason") == "compiler-artifact":
            key = (
                record["package_id"],
                record["target"]["name"],
                tuple(record["target"]["kind"]),
                tuple(record["features"]),
            )
            # Host build dependencies and target dependencies can share a
            # package/name. Preserve every instance; Cargo emission order varies.
            result.setdefault(key, []).append(record["profile"])
    for values in result.values():
        values.sort(key=lambda value: json.dumps(value, sort_keys=True))
    return result


def audit(root):
    manifest = load(root / "manifest.json")
    require(not manifest["preflight"], "preflight is not formal evidence")
    require(manifest["rounds"] >= 30 and manifest["warmups"] == 3, "incomplete protocol")
    validation = load(root / "validation.json")
    require(validation["valid"] and not validation["preflight"], "failed cohort")
    frozen = root / "frozen"
    for name, digest in manifest["harness"].items():
        require(sha(frozen / name) == digest, f"frozen harness/artifact changed: {name}")
    for name, record in manifest["artifacts"].items():
        require(sha(frozen / name) == record["sha256"], f"artifact changed: {name}")
    for name, digest in manifest["firmware"].items():
        require(sha(frozen / "firmware" / name) == digest, f"firmware changed: {name}")
    require(
        (frozen / "firmware/libkrunfw.so.5").resolve().parent == (frozen / "firmware").resolve(),
        "firmware SONAME escaped frozen directory",
    )

    # Use the exact sampler/parser that produced the cohort, not a newer source.
    spec = importlib.util.spec_from_file_location(
        "frozen_network_profile", frozen / "profile_ab.py"
    )
    sampler = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sampler)
    receipt = frozen / "build-receipt"
    baseline, candidate = (
        profiles(receipt / f"{arm}-cargo.jsonl") for arm in ("baseline", "candidate")
    )
    require(baseline.keys() == candidate.keys(), "Cargo artifact/feature graph changed")
    changed = []
    build_script_levels = set()
    for key in baseline:
        left, right = baseline[key], candidate[key]
        require(len(left) == len(right), f"artifact instance count changed: {key}")
        if left != right:
            runtime = key[1] in ("pvisor_overlaynet", "smoltcp") and key[2] == ("lib",)
            build_script = "#smoltcp@0.13.1" in key[0] and key[1:3] == (
                "build-script-build",
                ("custom-build",),
            )
            require(runtime or build_script, f"unrelated profile changed: {key}")
            for lp, rp in zip(left, right):
                lp, rp = dict(lp), dict(rp)
                # Cargo package overrides also affect that package's host build
                # script. Historical release used 0; reproducing the baseline
                # with an explicit package override also compiles it at z.
                baseline_level, candidate_level = lp.pop("opt_level"), rp.pop("opt_level")
                require(
                    baseline_level in (("0", "z") if build_script else ("z",))
                    and candidate_level == "3",
                    "unexpected profile",
                )
                require(lp == rp, "more than opt-level changed")
                if build_script:
                    build_script_levels.add((baseline_level, candidate_level))
            if runtime:
                changed.append(key[1])
    require(
        sorted(changed) == ["pvisor_overlaynet", "smoltcp"],
        f"unexpected profile changes: {changed}",
    )
    for line in (receipt / "sha256.txt").read_text().splitlines():
        digest, original = line.split(maxsplit=1)
        arm = {"release-pvisor": "release", "net-o3-pvisor": "net_o3"}.get(Path(original).name)
        if arm:
            require(digest == manifest["artifacts"][arm]["sha256"], f"build receipt differs: {arm}")
    sources = load(receipt / "source-sha256.txt")
    require(
        sources["benchmark/network/worker.rs"] == sha(frozen / "worker.rs"),
        "worker source differs from build receipt",
    )

    port = int(load(root / "server.json")["address"].rsplit(":", 1)[1])
    trials, identities = [], set()
    for item in load(root / "schedule.json"):
        require(sorted(item["arms"]) == sorted(sampler.ARMS), "missing/duplicate scheduled arm")
        require(sorted(item["scenarios"]) == sorted(sampler.SCENARIOS), "invalid scenario schedule")
        for arm in item["arms"]:
            identity = item["phase"], item["round"], arm
            require(identity not in identities, "duplicate scheduled trial")
            identities.add(identity)
            folder = root / f"{item['phase']}-{item['round']:02d}-{arm}"
            require(load(folder / "interference.json") == [], "build interference")
            records = sampler.parse_results((folder / "stdout.log").read_text(), arm)
            expected = [name for name in item["scenarios"] if name != "dns" or arm != "native"]
            require([r["scenario"] for r in records] == expected, "scenario order changed")
            trial = load(folder / "result.json")
            require(trial["results"] == records, "parsed output differs from retained result")
            require((trial["phase"], trial["round"], trial["arm"]) == identity, "identity changed")
            command = load(folder / "command.json")
            require(
                command[:3] == ["taskset", "-c", ",".join(map(str, manifest["cpus"]))],
                "affinity changed",
            )
            if arm != "native":
                counters = sampler.validate_bundle(load(folder / "stage/run-bundle.json"), port)
                require(counters == trial["network_accounting"], "network accounting changed")
            trials.append(trial)
    require(
        len(trials) == (manifest["rounds"] + manifest["warmups"]) * 3 == validation["trials"],
        "missing scheduled trials",
    )
    require(trials == load(root / "completed.json"), "completion journal differs")
    require(
        sampler.summarize(trials, manifest["rounds"]) == load(root / "summary.json"),
        "statistics differ",
    )
    require((root / "server.stderr").read_bytes() == b"", "origin reported failure")
    return dict(
        valid=True,
        trials=len(trials),
        formal=manifest["rounds"] * 3,
        summary_sha256=sha(root / "summary.json"),
        profile_changes=sorted(changed),
        smoltcp_build_script_opt_levels=sorted(build_script_levels),
        vm_boundary_and_exact_bytes=True,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    print(json.dumps(audit(args.batch.resolve()), indent=2))
