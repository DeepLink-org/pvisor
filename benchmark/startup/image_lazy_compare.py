#!/usr/bin/env python3
"""B-LAZY-STARTUP: independent cold-client / warm-client paired campaign.

Preparation freezes inputs and starts an owned pVisor worker. Measurement runs
only through a semspec Python row: three warmup rounds, thirty randomized paired
rounds, with cold immediately followed by warm for each condition. This is not
the warm-prepared cohort, a cold-disk experiment, or a pure-format comparison.
The overlay adapter owns its explicit client reset; no global cleanup is used.
"""

import argparse
import itertools
import json
import os
import platform
import random
import shlex
import shutil
import sys
import time
import traceback
from pathlib import Path

sys.dont_write_bytecode = True
import image_compare as common  # noqa: E402 - import after module-loading setup

HERE = Path(__file__).resolve().parent
GROUPS = common.GROUPS
STATES = ("cold", "warm")
ROW = "cold-warm-client-paired"
SEED = 20261018


def prepare(args):
    batch = args.prepare.resolve()
    if ".data" not in batch.parts:
        raise ValueError("Retain raw evidence in a new .data directory")
    cpus, service_cpus = common.parse_cpus(args.cpus), common.parse_cpus(args.service_cpus)
    if set(cpus) & set(service_cpus):
        raise ValueError("Application and service CPUs must be disjoint")
    warm = args.warm_batch.resolve(strict=True)
    original_receipts = {}
    for group in ("oci", "turbo"):
        path = warm / "prepare" / group / "result.json"
        receipt = json.loads(path.read_text())
        if (
            receipt.get("valid") is not True
            or receipt.get("action") != "prepare"
            or receipt.get("budgets_verified") is not True
            or receipt.get("condition") != group
            or receipt.get("cpus") != cpus
            or receipt.get("workload_argv") != common.WORKLOAD
            or receipt.get("provenance", {}).get("source_digest") != common.SOURCE
        ):
            raise ValueError("Incompatible prior container prepare receipt: " + str(path))
        original_receipts[group] = path
    batch.mkdir(parents=True, exist_ok=False)
    harness = batch / "harness"
    harness.mkdir()
    for name in (
        "image_lazy_compare.py",
        "image_lazy_analysis.py",
        "image_compare.py",
        "image_compare_analysis.py",
        "pvisor_image_perf.py",
    ):
        shutil.copy2(HERE / name, harness / name)
    for name in ("overlaybd_image_perf.py", "overlaybd_lab.py", "overlaybd_smoke.py"):
        shutil.copy2(args.overlay_adapter.resolve().parent / name, harness / name)
    rng = random.Random(SEED)
    orders = list(itertools.permutations(GROUPS)) * 5
    rng.shuffle(orders)
    protocol = {
        "schema": 1,
        "benchmark": "B-LAZY-STARTUP",
        "scope": "cold-warm-client",
        "groups": GROUPS,
        "states": list(STATES),
        "runs": 30,
        "warmups": 3,
        "orders": {"warmup": [rng.sample(GROUPS, len(GROUPS)) for _ in range(3)], "sample": orders},
        "seed": SEED,
        "clock_ticks": os.sysconf("SC_CLK_TCK"),
        "source_digest": common.SOURCE,
        "workload_argv": common.WORKLOAD,
        "cpus": cpus,
        "service_cpus": service_cpus,
        "lab": str(args.lab.resolve()),
        "pvisor_output": str(batch / "pvisor"),
        "preparation": {},
        "prepared_receipts": {},
        "clock": "adapter perf_counter_ns from cold pull+run or warm lookup+run through marker/normal exit",
        "clock_limits": "Exact adapter interval and command are audited; setup, reset, RPC and cleanup excluded.",
        "cache": "new private client state for cold; warm immediately reuses its cold cache with a new task/VM and writable state; host page cache and server inputs retained",
        "transport": "local loopback protocol simulation; HTTP registry and authenticated plain TCP pVisor cache",
        "traffic_scope": "completed application response bodies; no claimed wire bytes or unmeasured Ready cutoff",
        "budget": "2 CPU/2 GiB container tasks vs 2 vCPU/2 GiB guest RAM; helpers on separate CPUs; not equal whole-node budgets",
        "background_policy": "same retained foreign-build/benchmark gate as the prepared cohort; no slow-sample exclusions",
        "host": {
            "kernel": platform.release(),
            "machine": platform.machine(),
            "python": sys.version,
            "allowed_cpus": sorted(os.sched_getaffinity(0)),
        },
        "status": "preparing",
    }
    common.save(batch / "protocol.json", protocol)
    try:
        for group, source in original_receipts.items():
            destination = batch / "prepare" / group / "result.json"
            destination.parent.mkdir(parents=True)
            shutil.copy2(source, destination)
            checksum = common.digest(destination)
            if common.digest(source) != checksum:
                raise ValueError("Prepare receipt changed while freezing")
            protocol["prepared_receipts"][group] = {
                "source": str(source),
                "path": str(destination),
                "sha256": checksum,
                "scope": "reused successful warm budget/lab validation; not a new cold measurement",
            }
        started = time.monotonic_ns()
        common.invoke(
            [
                sys.executable,
                HERE / "pvisor_image_perf.py",
                "prepare",
                "--output",
                batch / "pvisor",
                "--binary-dir",
                args.binary_dir.resolve(),
                "--prepared-store",
                args.prepared_store.resolve(),
                "--cpus",
                args.cpus,
                "--service-cpus",
                args.service_cpus,
                "--memory-mib",
                "2048",
                "--lifetime",
                str(args.lifetime),
            ],
            batch / "requests/prepare-pvisor",
        )
        protocol["preparation"]["pvisor_cached_input_ns"] = time.monotonic_ns() - started
        frozen = {}
        for directory in (harness, batch / "pvisor/frozen", batch / "prepare"):
            for path in directory.rglob("*"):
                if path.is_file() and "__pycache__" not in path.parts:
                    frozen[str(path)] = common.digest(path)
        for path in original_receipts.values():
            frozen[str(path)] = common.digest(path)
        frozen[str(batch / "pvisor/prepared-input.json")] = common.digest(
            batch / "pvisor/prepared-input.json"
        )
        protocol["artifacts"] = frozen
        supervisor = json.loads((batch / "pvisor/supervisor.json").read_text())
        protocol["owned_process_roots"] = [supervisor["pid"]]
        protocol["background_before"] = common.process_snapshot(protocol["owned_process_roots"])
        protocol["status"] = "prepared"
        common.save(batch / "protocol.json", protocol)
        command = "$ " + shlex.join(
            [sys.executable, str(harness / "image_lazy_compare.py"), "--measure", str(batch)]
        )
        common.save(batch / "in.json", {ROW: command})
        print(batch)
    except BaseException:
        protocol["status"] = "prepare-failed"
        common.save(batch / "protocol.json", protocol)
        (batch / "failure.txt").write_text(traceback.format_exc())
        if (batch / "pvisor/worker.json").exists():
            common.invoke(
                [
                    sys.executable,
                    harness / "pvisor_image_perf.py",
                    "stop",
                    "--output",
                    batch / "pvisor",
                ],
                batch / "requests/prepare-failed-stop",
            )
        raise


def retain_result(batch, records, phase, index, group, state, result, wrapper):
    if (
        result.get("valid") is not True
        or result.get("workload_argv") != common.WORKLOAD
        or type(result.get("ready_ns")) is not int
        or type(result.get("completion_ns")) is not int
        or not 0 < result["ready_ns"] <= result["completion_ns"]
    ):
        raise ValueError(f"Invalid {group}/{state} adapter result; preserve pair evidence")
    common.save(wrapper, result)
    records.append(
        {
            "phase": phase,
            "round": index,
            "condition": group,
            "state": state,
            "result": result,
            "result_path": str(wrapper),
            "result_sha256": common.digest(wrapper),
        }
    )
    common.save(batch / "records.partial.json", records)


def measure(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    if (
        protocol.get("status") != "prepared"
        or protocol.get("scope") != "cold-warm-client"
        or (batch / "records.json").exists()
        or (batch / "records.partial.json").exists()
    ):
        raise ValueError("Expected an unused, prepared cold/warm batch")
    common.check_artifacts(protocol)
    records, pairs, background, flags = [], [], [], []
    cpus = ",".join(map(str, protocol["cpus"]))
    service_cpus = ",".join(map(str, protocol["service_cpus"]))
    try:
        for phase in ("warmup", "sample"):
            for index, order in enumerate(protocol["orders"][phase]):
                before = common.process_snapshot(protocol["owned_process_roots"])
                for group in order:
                    identity = f"{phase}-{index:02d}-{group}"
                    if group == "pvisor":
                        cache_key = "cache-" + identity
                        pointers = {}
                        for state, cache_state in (("cold", "fresh"), ("warm", "reuse")):
                            directory = batch / "requests" / (identity + "-" + state)
                            result = common.invoke(
                                [
                                    sys.executable,
                                    batch / "harness/pvisor_image_perf.py",
                                    "trial",
                                    "--output",
                                    batch / "pvisor",
                                    "--mode",
                                    "lazy",
                                    "--trial-id",
                                    identity + "-" + state,
                                    "--cache-key",
                                    cache_key,
                                    "--cache-state",
                                    cache_state,
                                ],
                                directory,
                            )
                            retain_result(
                                batch,
                                records,
                                phase,
                                index,
                                group,
                                state,
                                result,
                                directory / "result.json",
                            )
                            pointers[state] = str(directory / "result.json")
                        pairs.append(
                            {
                                "phase": phase,
                                "round": index,
                                "condition": group,
                                "cache_key": cache_key,
                                "results": pointers,
                            }
                        )
                    else:
                        directory = batch / "requests" / identity
                        result = common.invoke(
                            [
                                sys.executable,
                                batch / "harness/overlaybd_image_perf.py",
                                "--scenario",
                                "cold-warm-pair",
                                "--action",
                                "trial",
                                "--lab",
                                protocol["lab"],
                                "--condition",
                                group,
                                "--cpus",
                                cpus,
                                "--service-cpus",
                                service_cpus,
                                "--prepare-receipt",
                                batch / "prepare" / group / "result.json",
                                "--output",
                                batch / "trials" / identity,
                            ],
                            directory,
                            timeout=900,
                        )
                        if result.get("valid") is not True:
                            raise ValueError("Cold/warm container pair failed")
                        for state in STATES:
                            retain_result(
                                batch,
                                records,
                                phase,
                                index,
                                group,
                                state,
                                result[state],
                                directory / (state + "-result.json"),
                            )
                        pairs.append(
                            {
                                "phase": phase,
                                "round": index,
                                "condition": group,
                                "result_path": str(directory / "result.json"),
                                "result_sha256": common.digest(directory / "result.json"),
                            }
                        )
                    common.save(batch / "pair_receipts.partial.json", pairs)
                after = common.process_snapshot(protocol["owned_process_roots"])
                detected = common.interference(before, after, protocol["cpus"])
                background.append(
                    {
                        "phase": phase,
                        "round": index,
                        "before": before,
                        "after": after,
                        "flagged": detected,
                    }
                )
                flags.extend(detected)
                common.save(batch / "background.json", background)
                print(
                    f"{phase} cold/warm round {index + 1}/{len(protocol['orders'][phase])} checked",
                    file=sys.stderr,
                    flush=True,
                )
        common.save(batch / "records.json", records)
        common.save(batch / "pair_receipts.json", pairs)
        if flags:
            raise RuntimeError("Background interference invalidates the entire cold/warm cohort")
        common.check_artifacts(protocol)
    except BaseException:
        (batch / "failure.txt").write_text(traceback.format_exc())
        raise
    finally:
        common.invoke(
            [
                sys.executable,
                batch / "harness/pvisor_image_perf.py",
                "stop",
                "--output",
                batch / "pvisor",
            ],
            batch / "requests/stop-pvisor",
        )
    from image_lazy_analysis import analyze

    analysis = analyze(batch)
    row = measured_row(protocol, analysis)
    common.save(batch / "measured-row.json", row)
    print(json.dumps(row))


def measured_row(protocol, analysis):
    row = {"Pairs": protocol["runs"], "Failures": 0, "Excluded": 0}
    for group in protocol["groups"]:
        for state in STATES:
            for metric in ("ready", "completion"):
                row[f"{group} {state} {metric} median ms"] = analysis["summary"][group][state][
                    metric
                ]["median_ms"]
    return row


def audit(batch):
    from image_lazy_analysis import analyze, render

    analysis = analyze(batch)
    protocol = json.loads((batch / "protocol.json").read_text())
    expected = measured_row(protocol, analysis)
    if (batch / "in.render.json").exists():
        if json.loads((batch / "in.render.json").read_text()) != {ROW: expected}:
            raise ValueError("semspec rendered values differ from recomputed cold/warm audit")
        if json.loads((batch / "measured-row.json").read_text()) != expected:
            raise ValueError("retained measured row differs from recomputed cold/warm audit")
    (batch / "report.md").write_text(render(batch, analysis))
    print(batch / "report.md")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--prepare", type=Path)
    action.add_argument("--measure", type=Path)
    action.add_argument("--audit", type=Path)
    parser.add_argument("--lab", type=Path)
    parser.add_argument("--prepared-store", type=Path)
    parser.add_argument(
        "--warm-batch", type=Path, help="Prior validated prepare/{oci,turbo}/result.json receipts"
    )
    parser.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--overlay-adapter", type=Path, default=HERE / "overlaybd_image_perf.py")
    parser.add_argument("--cpus", default="12,13")
    parser.add_argument("--service-cpus", default="14,15")
    parser.add_argument("--lifetime", type=int, default=14400)
    args = parser.parse_args()
    if args.prepare:
        if args.lab is None or args.prepared_store is None or args.warm_batch is None:
            parser.error("--prepare requires --lab, --prepared-store and --warm-batch")
        if args.lifetime <= 0:
            parser.error("--lifetime must be positive")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    else:
        audit(args.audit.resolve())


if __name__ == "__main__":
    main()
