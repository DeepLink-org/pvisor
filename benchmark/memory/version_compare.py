#!/usr/bin/env python3
"""B-MEMORY-SCALE: strict, paired UKSM version A/B, not a product-arm comparison.

Uses matched A/B probe+daemon artifacts, identical embedded FW and guest bytes,
the existing four-VM worker and strict host/resource gates. Busy-host and
historical reports are never eligible. Preparation/building happens separately.
"""

import argparse
import json
import math
import random
import shlex
import shutil
import statistics
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "benchmark/support"))

from firecracker_ksm import expected  # noqa: E402
from memory_savings import host_identity  # noqa: E402
from plot_pool import sustained_drop  # noqa: E402
from pool_compare import (  # noqa: E402
    execution,
    harness_paths,
    validate_observation,
    validate_worker,
    values,
    verify_harness,
)
from scanner_compare import digest, save  # noqa: E402

SCHEMA = "pvisor-uksm-version-ab/v1"
PATTERNS = ("repeated", "random-shared", "random-unique")
METRICS = (
    "charged_mib",
    "pss_mib",
    "idle_cpu_seconds",
    "first_read_ms",
    "postwrite_mib",
    "peak_mib",
    "kernel_mib",
    "pool_vmas",
)


def schedule(rounds, preflight, seed):
    """Freeze adjacent randomized AB/BA pairs; never reorder after observing cost."""
    if rounds < (1 if preflight else 5) or (preflight and rounds != 1):
        raise ValueError("one preflight round or at least five paired rounds required")
    rng, result = random.Random(seed), []
    for n in range(rounds):
        pairs = [
            (p, mode)
            for p in PATTERNS
            for mode in (("off", "physical") if preflight else ("physical",))
        ]
        rng.shuffle(pairs)
        for pattern, mode in pairs:
            versions = ["A", "B"]
            rng.shuffle(versions)
            for version in versions:
                result.append(
                    dict(
                        round=n,
                        pattern=pattern,
                        arm="pvisor-" + mode,
                        version=version,
                        pair=f"{n}:{pattern}:{mode}",
                    )
                )
    return result


def interval(samples):
    """Resample paired differences, not arms independently."""
    rng = random.Random(20261114)
    medians = sorted(statistics.median(rng.choices(samples, k=len(samples))) for _ in range(5000))
    return [medians[124], medians[4874]]


def version_index(cases, rounds):
    """Require exactly one physical condition per version, content and round."""
    wanted = {(r, p, v) for r in range(rounds) for p in PATTERNS for v in ("A", "B")}
    keys = [(c["round"], c["pattern"], c["version"]) for c in cases]
    if (
        rounds < 5
        or len(keys) != len(wanted)
        or set(keys) != wanted
        or any(c.get("arm") != "pvisor-physical" for c in cases)
    ):
        raise ValueError("incomplete, duplicated or insufficient version pairs")
    return dict(zip(keys, cases))


def paired_summary(cases, rounds):
    """A positive B-A difference means higher cost; crossing zero is inconclusive."""
    index = version_index(cases, rounds)
    summaries = []
    for pattern in PATTERNS:
        for metric in METRICS:
            a = [index[r, pattern, "A"][metric] for r in range(rounds)]
            b = [index[r, pattern, "B"][metric] for r in range(rounds)]
            if any(not isinstance(x, (int, float)) or not math.isfinite(x) or x < 0 for x in a + b):
                raise ValueError("invalid cost measurement")
            differences = [new - old for old, new in zip(a, b)]
            lo, hi = interval(differences)
            summaries.append(
                dict(
                    pattern=pattern,
                    metric=metric,
                    n=rounds,
                    baseline_samples=a,
                    candidate_samples=b,
                    baseline_median=statistics.median(a),
                    candidate_median=statistics.median(b),
                    paired_differences=differences,
                    median_difference=statistics.median(differences),
                    paired_bootstrap_95=[lo, hi],
                    interpretation="higher-cost"
                    if lo > 0
                    else "lower-cost"
                    if hi < 0
                    else "inconclusive",
                )
            )
    return summaries


def paired_crossings(cases, rounds):
    """Keep misses censored and include both sampled crossing boundaries."""
    index = version_index(cases, rounds)
    result = []
    for pattern in PATTERNS:
        bounds = []
        for n in range(rounds):
            a, b = (index[n, pattern, v]["sustained_drop"] for v in ("A", "B"))
            if not a["reached"] or not b["reached"]:
                bounds.append(None)
                continue
            lo_a, hi_a = a["crossing_interval_seconds"]
            lo_b, hi_b = b["crossing_interval_seconds"]
            if not (0 <= lo_a <= hi_a <= 60 and 0 <= lo_b <= hi_b <= 60):
                raise ValueError("invalid crossing interval")
            bounds.append([lo_b - hi_a, hi_b - lo_a])
        paired = all(b is not None for b in bounds)
        result.append(
            dict(
                pattern=pattern,
                n=rounds,
                paired_interval_differences=bounds,
                censored_pairs=sum(b is None for b in bounds),
                median_difference_bracket=[statistics.median(b[i] for b in bounds) for i in (0, 1)]
                if paired
                else None,
                conservative_bootstrap_95=[
                    interval([b[0] for b in bounds])[0],
                    interval([b[1] for b in bounds])[1],
                ]
                if paired
                else None,
                scope="additional sampled 128MiB drop after ready; positive means later, no imputation",
            )
        )
    return result


def artifacts(directory):
    """Require identical controls and bound source/build receipts before sampling."""
    prepared = json.loads((directory / "preparation.json").read_text())
    if not prepared.get("prepared"):
        raise ValueError("matched artifact preparation has not completed")
    result = {}
    for version in ("A", "B"):
        folder = directory / version
        inputs = folder / "inputs"
        receipt = json.loads((inputs / "receipt.json").read_text())
        if receipt != prepared["variants"][version]:
            raise ValueError("variant receipt changed")
        if (
            not receipt.get("embedded_fw")
            or receipt["target"] != "x86_64-unknown-linux-musl"
            or receipt["profile"] != "release"
        ):
            raise ValueError("wrong build/FW configuration")
        if any(digest(inputs / name) != value for name, value in receipt["hashes"].items()):
            raise ValueError("variant input bytes changed")
        for name, key in (
            ("source.tar.gz", "source_archive_sha256"),
            ("source-files.json", "source_files_sha256"),
            ("guest-supervisor", "guest_supervisor_sha256"),
        ):
            if digest(folder / name) != receipt[key]:
                raise ValueError("variant source/supervisor changed")
        result[version] = dict(inputs=str(inputs.resolve()), receipt=receipt)
    a, b = (result[v]["receipt"] for v in ("A", "B"))
    for key in ("rustc", "zig", "guest_supervisor_sha256", "common_helper_sha256"):
        if a[key] != b[key]:
            raise ValueError("build/helper/guest control differs: " + key)
    if digest(directory / "common/pool_memory_case.rs") != a["common_helper_sha256"]:
        raise ValueError("common helper bytes changed")
    for name in ("worker", "worker.rs", "root/worker", "bundle/kernel.bin", "bundle/kernel.json"):
        if a["hashes"][name] != b["hashes"][name]:
            raise ValueError("shared workload/FW differs: " + name)
    return result


def audit(root):
    """Reject incomplete/contended reports; audit every native and external witness."""
    report = json.loads((root / "report.json").read_text())
    if report.get("schema") != SCHEMA or report.get("complete") is not True:
        raise ValueError("complete strict version A/B report required")
    order = schedule(report["rounds"], report["preflight"], report["seed"])
    if report["order"] != order or len(report["executions"]) != len(order):
        raise ValueError("missing, duplicated or reordered version conditions")
    if artifacts(Path(report["artifacts"])) != report["versions"]:
        raise ValueError("version source/input identity changed")
    if not verify_harness(report["harness_files"]):
        raise ValueError("analysis must use the exact recorded harness")
    for name, value in report["harness_files"].items():
        if digest(root / "harness" / Path(name).name) != value:
            raise ValueError("frozen harness changed")
    cases = []
    for n, (row, planned) in enumerate(zip(report["executions"], order)):
        if any(row.get(k) != v for k, v in planned.items()):
            raise ValueError("version/order binding mismatch")
        if row.get("status") != "successful" or not row.get("unit_quiescent"):
            raise ValueError("failed or unreaped version condition")
        folder = root / str(n)
        if digest(folder / "worker.json") != row["worker_sha256"]:
            raise ValueError("worker report changed")
        if set(row["observation_hashes"]) != {"monitor.json", "pss.json", "guard.json"}:
            raise ValueError("observation witness missing")
        if any(digest(folder / name) != value for name, value in row["observation_hashes"].items()):
            raise ValueError("observation changed")
        cfg = json.loads((folder / "config.json").read_text())
        if cfg["inputs"] != report["versions"][row["version"]]["inputs"]:
            raise ValueError("wrong version probe/daemon pairing")
        if (cfg["arm"], cfg["pattern"]) != (planned["arm"], planned["pattern"]):
            raise ValueError("wrong version workload/mode")
        raw = json.loads((folder / "worker.json").read_text())
        validate_worker(raw, cfg)
        monitor = json.loads((folder / "monitor.json").read_text())
        pss = json.loads((folder / "pss.json").read_text())
        validate_observation(
            monitor, json.loads((folder / "guard.json").read_text()), tasks_max=256
        )
        if not pss or any("error" in point for point in pss):
            raise ValueError("PSS curve missing or failed")
        origin, end = raw["all_ready_ns"], raw["post_write_origin_ns"] + 30_000_000_000
        for times in ([p["time_ns"] for p in monitor], [p["monotonic_ns"] for p in pss]):
            if times[0] > origin or times[-1] < end:
                raise ValueError("incomplete idle/post-write curve")
        phases = {p["name"]: p for p in raw["phases"]}
        first, last = phases["idle-0"], phases["idle-60"]
        case = dict(planned)
        case.update(
            checks=len(raw["checks"]),
            charged_mib=int(last["counters"]["memory.current"]) / 2**20,
            pss_mib=sum(p["pss_bytes"] for p in last["processes"]) / 2**20,
            idle_cpu_seconds=(
                values(last["counters"]["cpu.stat"])["usage_usec"]
                - values(first["counters"]["cpu.stat"])["usage_usec"]
            )
            / 1e6,
            first_read_ms=raw["first_full_read_ms"],
            postwrite_mib=int(phases["post-write-30"]["counters"]["memory.current"]) / 2**20,
            peak_mib=int(raw["after"]["counters"]["memory.peak"]) / 2**20,
            kernel_mib=values(last["counters"]["memory.stat"])["kernel"] / 2**20,
            pool_vmas=sum(
                p["mapping_evidence"]["pool_vmas"]
                for p in last["processes"]
                if p["pid"] in last["guest_pids"]
            ),
            sustained_drop=sustained_drop(
                [((p["time_ns"] - origin) / 1e9, p["memory_current"] / 2**20) for p in monitor],
                int(first["counters"]["memory.current"]) / 2**20,
            ),
        )
        cases.append(case)
    return report, cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--preflight", action="store_true")
    parser.add_argument("--preflight-report", type=Path)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--seed", type=int, default=20261114)
    parser.add_argument("--analyze", type=Path)
    parser.add_argument(
        "--prepare",
        action="store_true",
        help="write a NEW semspec input without launching measurements",
    )
    args = parser.parse_args()
    if args.analyze:
        report, cases = audit(args.analyze.resolve())
        result = dict(
            schema=SCHEMA,
            preflight=report["preflight"],
            cases=cases,
            non_regression_proven=False,
            interval_scope="pointwise paired intervals, not a global/multiple-metric non-inferiority test",
        )
        if not report["preflight"]:
            result["paired_metrics"] = paired_summary(cases, report["rounds"])
            result["paired_crossings"] = paired_crossings(cases, report["rounds"])
        save(args.analyze.resolve() / "audited-results.json", result)
        print(json.dumps(result, indent=2))
        return 0
    if not args.artifacts or not args.output:
        parser.error("artifacts and a NEW short output are required")
    directory, root = args.artifacts.resolve(), args.output.resolve()
    versions = artifacts(directory)
    rounds = 1 if args.preflight else args.rounds
    order = schedule(rounds, args.preflight, args.seed)
    if len(str(root / "44/pool/pool.sock").encode()) > 100:
        parser.error("use a short output directory for Unix sockets")
    if not args.preflight:
        if not args.preflight_report:
            parser.error("a complete, audited matched-version preflight is required")
        prior, _ = audit(args.preflight_report.resolve())
        if not prior["preflight"] or prior["versions"] != versions:
            raise ValueError("preflight does not cover these exact version artifacts")
    if args.prepare:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "--artifacts",
            str(directory),
            "--output",
            str(root),
            "--rounds",
            str(rounds),
            "--seed",
            str(args.seed),
        ]
        if args.preflight:
            command.append("--preflight")
        else:
            command += ["--preflight-report", str(args.preflight_report.resolve())]
        destination = root.with_suffix(".input.json")
        with destination.open("x") as stream:
            json.dump({"version-pairs": "$ " + shlex.join(command)}, stream, indent=2)
            stream.write("\n")
        print(destination)
        return 0
    root.mkdir()
    paths = [*harness_paths(), Path(__file__), Path(__file__).with_name("plot_pool.py")]
    harness = {str(p.resolve()): digest(p) for p in paths}
    if len({Path(p).name for p in harness}) != len(harness):
        raise ValueError("ambiguous harness names")
    (root / "harness").mkdir()
    for name in harness:
        shutil.copy2(name, root / "harness" / Path(name).name)
    report = dict(
        schema=SCHEMA,
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        artifacts=str(directory),
        versions=versions,
        preflight=args.preflight,
        rounds=rounds,
        seed=args.seed,
        order=order,
        executions=[],
        complete=False,
        harness_files=harness,
        host=host_identity(),
    )
    save(root / "report.json", report)
    hashes = {
        p: {f"{n}:{w}": expected(p, n, w) for n in range(1, 5) for w in (0, 25, 100)}
        for p in PATTERNS
    }
    for planned in order:
        if artifacts(directory) != versions or not verify_harness(harness):
            raise ValueError("version source/input/harness changed")
        row = execution(
            Path(versions[planned["version"]]["inputs"]),
            root / str(len(report["executions"])),
            planned["arm"],
            planned["pattern"],
            hashes[planned["pattern"]],
            harness,
            without_ksm=True,
            embedded_fw=True,
        )
        row.update(planned)
        report["executions"].append(row)
        save(root / "report.json", report)
        print(
            json.dumps({**planned, "status": row["status"], "error": row.get("error")}),
            file=sys.stderr,
            flush=True,
        )
        if row["status"] != "successful":
            return 1
    report["complete"] = artifacts(directory) == versions and verify_harness(harness)
    save(root / "report.json", report)
    audit(root)
    print(
        json.dumps(
            dict(
                complete=report["complete"],
                preflight=args.preflight,
                conditions=len(report["executions"]),
                evidence=str(root),
            )
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
