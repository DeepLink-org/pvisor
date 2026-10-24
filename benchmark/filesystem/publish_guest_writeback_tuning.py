#!/usr/bin/env python3
"""Strict 0048 -> 0049 cache-batching publication in a separate namespace.

Measurement evidence and projection checks remain unchanged. A private loaded
instance of the pinned shared-layout publisher reuses its receipt/projection/
overwrite checks, while this file owns the explicit 48/49 firmware and independent-time contracts. --check
runs no subjects and writes no inputs, destinations, caches or receipts.
"""

import argparse
import csv
import hashlib
import importlib.util
import io
import json
import re
import statistics
import sys
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
BASE_PATH = HERE / "publish_guest_writeback.py"
# Pin the reviewed helper after formatting and unused-local cleanup.
BASE_SHA256 = "9d9c08b61c2e9a1ffcb4f94041407b811272f8e158b78d8c170dc14122d4b817"
if hashlib.sha256(BASE_PATH.read_bytes()).hexdigest() != BASE_SHA256:
    raise ValueError("shared publication validator differs from the pinned layout revision")
spec = importlib.util.spec_from_file_location("_guest_writeback_tuning_shared", BASE_PATH)
BASE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(BASE)

require, load, sha = BASE.require, BASE.load, BASE.sha
relative, digest, projection, frozen = BASE.relative, BASE.digest, BASE.projection, BASE.frozen
PATCH48 = "patches/0048-fuse-pipeline-native-writeback-pressure.patch"
PATCH49 = "patches/0049-fuse-coalesce-writeback-kicks.patch"
BASELINE_FIRMWARE_SHA256 = "f96af96339d40698faaa88d78992d550f8403137cdb58fcce3a4f9350a3d5dbb"
BASELINE_PATCH_SHA256 = "22fd06b6501d2fd055226aad285d0924bcf9d3abc07251827ac2566f9d7a0fb4"
NAMESPACE = "filesystem-guest-writeback-tuning"
ORIGINAL_COHORT = BASE.audit_cohort
ORIGINAL_COLLECT = BASE.collect
ORIGINAL_PIPELINE = BASE.pipeline


def revision(protocol):
    comparison = protocol["firmware_comparison"]
    require(
        comparison.get("baseline_patch") == 48
        and comparison.get("candidate_patch") == 49
        and comparison["contrasts"]
        == {"incremental": ["old-wb", "guest-wb"], "enablement": ["off", "guest-wb"]},
        "tuning publication requires explicit baseline0048/candidate0049 controls",
    )


def firmware(batch, protocol):
    revision(protocol)
    new = load(batch / "firmware-source/SOURCE.json")
    old = load(batch / "firmware-source/OLD-SOURCE.json")
    require(
        new["firmware_sha256"] == sha(batch / "bin/libkrunfw.so")
        and old["firmware_sha256"]
        == sha(batch / "bin/libkrunfw-old.so")
        == BASELINE_FIRMWARE_SHA256,
        "firmware SOURCE/executed baseline association differs",
    )
    require(
        new["kernel_source_sha256"] == old["kernel_source_sha256"]
        and new["actual_config_sha256"] == old["actual_config_sha256"],
        "0049 contrast changed kernel archive or actual configuration",
    )
    before = {key: value for key, value in old["inputs"].items() if key.startswith("patches/")}
    after = {key: value for key, value in new["inputs"].items() if key.startswith("patches/")}
    require(
        before.get(PATCH48) == BASELINE_PATCH_SHA256
        and PATCH49 not in before
        and after.keys() - before.keys() == {PATCH49}
        and all(after.get(key) == value for key, value in before.items()),
        "0049 must be the only added patch; all frozen0048 baseline patches must match",
    )
    for name, expected in after.items():
        require(
            sha(batch / "firmware-source" / relative(name)) == expected,
            "retained firmware patch differs: " + name,
        )
    require(
        protocol["firmware_source"]["sha256"] == sha(batch / "firmware-source/SOURCE.json")
        and protocol["old_firmware_source"]["sha256"]
        == sha(batch / "firmware-source/OLD-SOURCE.json"),
        "protocol firmware receipt differs",
    )
    return dict(
        baseline_patch=48,
        candidate_patch=49,
        old_sha256=old["firmware_sha256"],
        new_sha256=new["firmware_sha256"],
        kernel_source_sha256=new["kernel_source_sha256"],
        actual_config_sha256=new["actual_config_sha256"],
        old_source_receipt_sha256=sha(batch / "firmware-source/OLD-SOURCE.json"),
        new_source_receipt_sha256=sha(batch / "firmware-source/SOURCE.json"),
        added_patch_sha256=after[PATCH49],
        retained0048_patch_sha256=after[PATCH48],
        baseline_patches=len(before),
    )


def audit_cohort(batch, diagnostic):
    revision(load(Path(batch) / "protocol.json"))
    result = ORIGINAL_COHORT(batch, diagnostic)
    result[4]["source_scope"] = (
        "same frozen current host and identical static payload; both WB1; "
        "old0048 versus new0049 compares cheaper admission, larger submission hints and "
        "bounded per-VQ scatterlist scratch replacing the large transient atomic SG allocation"
    )
    return result


def audit_analyses(batch, protocol, records, points, directory, time_directory):
    summary, times = load(directory / "summary.json"), load(time_directory / "summary.json")
    require(
        protocol["firmware_comparison"].get("baseline_patch") == 48
        and protocol["firmware_comparison"].get("candidate_patch") == 49
        and times.get("firmware_comparison") == dict(baseline_patch=48, candidate_patch=49),
        "tuning analysis must explicitly compare baseline0048 with candidate0049",
    )
    require(
        summary.get("status") == "analyzed"
        and summary.get("failures") == []
        and summary["counts"]["valid_environments"] == 20
        and summary["counts"]["failed_environments"] == 0
        and summary["counts"]["environment_points"] == 300,
        "timing analysis is incomplete",
    )
    require(
        summary["source"]["protocol_sha256"] == sha(batch / "protocol.json")
        and summary["source"]["records_sha256"] == sha(batch / "records.json")
        and summary["source"]["analysis_sha256"] == sha(batch / "harness/curves_analysis.py")
        and summary["followup_analyzer_sha256"] == sha(batch / "harness/guest_writeback_curves.py"),
        "timing analysis input/source identity differs",
    )
    with frozen(batch) as (_, analysis):
        curves, comparisons, _ = analysis.summarize(protocol, points)
        _, enablement, _ = analysis.summarize({**protocol, "baseline_condition": "off"}, points)
        enablement = [row for row in enablement if row["condition"] == "guest-wb"]
        require(
            summary["curves"] == curves
            and summary["comparisons"] == comparisons
            and summary["enablement_comparisons"] == enablement,
            "timing summary/statistics differ from raw environments",
        )
        projection(directory / "environment_points.csv", points)
        projection(directory / "curve_summary.csv", analysis.flattened_summary(curves))
        projection(directory / "paired_ratio_summary.csv", comparisons)
        projection(directory / "enablement_comparisons.csv", enablement)
    require(
        times.get("schema") == 1
        and times.get("status") == "verified"
        and times.get("performance_samples") is True
        and times["counts"]
        == dict(
            environments=20,
            resident_rows=900,
            environment_cells=300,
            contrasts=2,
            metrics_per_cell=2,
            comparisons=60,
        ),
        "independent time analysis inventory differs",
    )
    for name in (
        "protocol.json",
        "records.json",
        "status.json",
        "in.json",
        "in.bench.json",
        "in.render.json",
    ):
        require(times["inputs"][name] == sha(batch / name), "time analysis input changed: " + name)
    require(
        times["postprocessor_sha256"] == sha(HERE / "guest_writeback_analysis.py"),
        "time postprocessor source is unavailable or changed",
    )
    for name, expected in times["frozen_helpers"].items():
        require(
            sha(batch / "harness" / relative(name)) == expected,
            "time analysis frozen helper differs",
        )
    time_rows = list(csv.DictReader(io.StringIO((time_directory / "time_levels.csv").read_text())))
    require(len(time_rows) == 300, "time-level projection inventory differs")
    lookup = {(str(record["pair"]), record["condition"]): record for record in records}
    levels, expected_time_rows = {}, []
    cells = {cell["id"]: cell for cell in protocol["cells"]}
    for row in time_rows:
        key = (row["pair"], row["condition"], row["id"])
        require(key not in levels, "duplicate time environment/cell")
        raw = sorted(
            [v for v in lookup[(row["pair"], row["condition"])]["rows"] if v["id"] == row["id"]],
            key=lambda v: v["round"],
        )
        expected = dict(
            rounds=3,
            logical_bytes=sum(v["bytes"] for v in raw),
            completed_ns=sum(v["total_ns"] for v in raw),
            accepted_ns=sum(v["accepted_ns"] for v in raw),
            round_completed_ns=[v["total_ns"] for v in raw],
            round_accepted_ns=[v["accepted_ns"] for v in raw],
        )
        for name, value in expected.items():
            require(
                row[name] == (json.dumps(value) if isinstance(value, list) else str(value)),
                "time level differs from summed dependent rounds: " + name,
            )
        levels[key] = expected
        expected_time_rows.append(
            dict(
                id=row["id"],
                family=cells[row["id"]]["family"],
                x=cells[row["id"]]["x"],
                pair=row["pair"],
                condition=row["condition"],
                **expected,
            )
        )
    require(
        set(levels) == {(p["pair"], p["condition"], p["id"]) for p in points},
        "time-level pairing inventory differs from completed throughput points",
    )
    projection(time_directory / "time_levels.csv", expected_time_rows)
    with frozen(batch) as (_, analysis):
        for row in times["comparisons"]:
            require(
                row["contrast"] in ("incremental_0049", "guest_writeback_enablement")
                and row["condition"] == "guest-wb"
                and row["baseline"]
                == ("old-wb" if row["contrast"] == "incremental_0049" else "off")
                and row["metric"] in ("completed", "accepted")
                and row["n"] == 5
                and row["pairs"] == ["0", "1", "2", "3", "4"],
                "time contrast independent pairing differs",
            )
            before = [
                levels[(pair, row["baseline"], row["id"])][row["metric"] + "_ns"]
                for pair in row["pairs"]
            ]
            after = [
                levels[(pair, "guest-wb", row["id"])][row["metric"] + "_ns"]
                for pair in row["pairs"]
            ]
            values = [a / b for a, b in zip(after, before)]
            cell = cells[row["id"]]
            require(
                row["baseline_ns"] == before
                and row["candidate_ns"] == after
                and row["paired_values"] == values
                and row["median"] == statistics.median(values)
                and row["family"] == cell["family"]
                and row["x"] == cell["x"]
                and row["min"] == min(values)
                and row["max"] == max(values)
                and row["baseline_median_round_ms"] == statistics.median(v / 3e6 for v in before)
                and row["candidate_median_round_ms"] == statistics.median(v / 3e6 for v in after),
                "time ratio is not candidate/baseline per independent environment",
            )
            import random

            rng = random.Random(
                analysis.seed_for(
                    protocol["seed"], row["contrast"], row["id"], row["metric"], "time_ratio"
                )
            )
            draws = sorted(statistics.median(rng.choices(values, k=5)) for _ in range(5000))
            require(
                row["ci_low"] == analysis.percentile(draws, 0.025)
                and row["ci_high"] == analysis.percentile(draws, 0.975)
                and row["bootstrap_resamples"] == 5000
                and row["bootstrap_seed"]
                == analysis.seed_for(
                    protocol["seed"], row["contrast"], row["id"], row["metric"], "time_ratio"
                )
                and row["reduction_percent"] == 100 * (1 - row["median"])
                and row["reduction_ci_low"] == 100 * (1 - row["ci_high"])
                and row["reduction_ci_high"] == 100 * (1 - row["ci_low"])
                and row["interval_excludes_one"] is (row["ci_high"] < 1 or row["ci_low"] > 1),
                "time bootstrap CI or reported reduction differs",
            )
    require(
        len(times["comparisons"]) == 60
        and len({(r["contrast"], r["id"], r["metric"]) for r in times["comparisons"]}) == 60,
        "time contrast inventory differs",
    )
    projection(time_directory / "time_comparisons.csv", times["comparisons"])
    for name, expected in times["outputs"].items():
        require(
            sha(time_directory / relative(name)) == expected,
            "time analysis output changed: " + name,
        )
    return summary, times


def correctness(batch, formal_firmware):
    batch = batch.resolve()
    receipt = load(batch / "receipt.json")
    require(
        receipt.get("schema") == 1
        and receipt.get("role") == "correctness, not performance"
        and receipt["firmware"]["sha256"]
        == formal_firmware["new_sha256"]
        == sha(Path(receipt["firmware"]["path"])),
        "new tuning correctness receipt role/firmware differs",
    )
    for name, expected in receipt["files"].items():
        require(sha(batch / relative(name)) == expected, "correctness evidence changed: " + name)
    require(
        receipt["files"].get("test_virtio_sg_scratch_contract.py")
        == sha(HERE.parents[1] / "fw/tests/test_virtio_sg_scratch_contract.py"),
        "0049v2 correctness requires the retained atomic-SG allocation regression contract",
    )
    fw = load(batch / relative(receipt["firmware"]["source_receipt"]))
    require(
        fw["firmware_sha256"] == formal_firmware["new_sha256"]
        and fw["inputs"][PATCH49] == formal_firmware["added_patch_sha256"]
        and fw["inputs"][PATCH48] == BASELINE_PATCH_SHA256
        and receipt["files"]["0049.patch"] == formal_firmware["added_patch_sha256"]
        and receipt["files"]["0048.patch"] == BASELINE_PATCH_SHA256,
        "correctness firmware source/0048+0049 patch closure differs",
    )
    invocation_file = relative(receipt["final_invocations_file"])
    invocations = load(batch / invocation_file)
    require(
        invocations["environment"].get("PVISOR_VM_FS_WRITEBACK") == "1"
        and invocations["environment"].get("PVISOR_VM_FS_HOST_WRITEBACK") == "0"
        and invocations["environment"].get("PVISOR_VM_GUEST_CACHE") == "1"
        and invocations["environment"].get("PVISOR_FS_PROFILE") == "0",
        "correctness actual invocation flags differ",
    )
    require(
        len(invocations["records"]) == 2
        and {record["mode"] for record in invocations["records"]} == {"full", "error"},
        "full/error final correctness invocation inventory differs",
    )
    for record in invocations["records"]:
        command = record["command"]
        require(
            record["returncode"] == 0
            and record["log"] == record["mode"] + "-final.log"
            and sha(batch / relative(record["log"])) == record["log_sha256"]
            and len(command) == 8
            and command[:3] == ["taskset", "-c", "0,1"]
            and command[-1] == record["mode"]
            and command[5] == receipt["firmware"]["path"]
            and sha(Path(command[3])) == receipt["files"]["host-regression"]
            and sha(Path(command[6])) == receipt["files"]["guest-regression"],
            "new correctness final command/log/firmware differs",
        )
    full = (batch / "full-final.log").read_text()
    error = (batch / "error-final.log").read_text()
    concurrent = receipt["probes"]["concurrency"]
    require(
        {
            name: concurrent.get(name)
            for name in ("threads", "passes", "reopens", "bytes", "pwrite_operations")
        }
        == dict(threads=4, passes=3, reopens=114, bytes=2097408, pwrite_operations=6144)
        and concurrent.get("token")
        == "GUEST_WRITEBACK_MULTI_FD_PASS threads=4 passes=3 reopens=114 bytes=2097408",
        "concurrent-redirty declared workload differs",
    )
    required = [
        "GUEST_WRITEBACK_DETACHED_PASS",
        "GUEST_WRITEBACK_PAYLOAD_PASS",
        "GUEST_WRITEBACK_PASS",
        concurrent["token"],
    ]
    require(
        all(marker in full for marker in required)
        and "GUEST_WRITEBACK_ERROR_ARMED" in error
        and "GUEST_WRITEBACK_ERROR_PASS" in error
        and "guest writeback failed:" in error,
        "new concurrent-redirty/full/error propagation proof missing",
    )
    pressure = re.findall(r"^WB_PRESSURE_TARGET_VERIFIED bytes=(\d+)$", full, re.M)
    reservations = re.findall(r"^WB_BDI_VERIFIED .* strict=1 max=100 min_bytes=(\d+)$", full, re.M)
    require(
        len(pressure) == 1
        and (16 << 20) <= int(pressure[0]) <= (256 << 20)
        and len(reservations) == 2
        and all(int(value) > (8 << 20) for value in reservations),
        "actual native dirty-pressure/reservation proof missing",
    )
    validation = receipt["validation"]
    require(
        validation["rust"]["passed"] == 511
        and validation["rust"]["skipped"] == 8
        and "511 tests run: 511 passed, 8 skipped" in (batch / "rust-tests.log").read_text()
        and validation["fw"]["passed"] == 7
        and validation["rustdoc"]["exit_code"] == 0,
        "new correctness validation scope differs",
    )
    for name, log in [("fw", "firmware-tests.log"), ("python", "python-tests.log")]:
        text = (batch / log).read_text()
        require(
            f"Ran {validation[name]['passed']} tests" in text and "\nOK\n" in text,
            "retained test count/success differs: " + name,
        )
        if name == "fw":
            require(
                "test_virtio_sg_scratch_contract." in text,
                "firmware validation omitted the atomic-allocation regression contract",
            )
    return dict(
        cohort=batch.name,
        role="correctness, not performance",
        receipt_sha256=sha(batch / "receipt.json"),
        raw_file_count=len(receipt["files"]),
        raw_files_sha256=digest(receipt["files"]),
        firmware_sha256=receipt["firmware"]["sha256"],
        final_logs={
            name: receipt["files"][name]
            for name in (
                "full-final.log",
                "error-final.log",
                "rust-tests.log",
                "firmware-tests.log",
                "python-tests.log",
                "rustdoc.log",
            )
        },
        validation={
            name: {key: value for key, value in result.items() if key != "command"}
            for name, result in validation.items()
        },
        final_probe_modes=["full", "error"],
        parent_exit_codes=[0, 0],
        actual_flags={
            name: invocations["environment"][name]
            for name in (
                "PVISOR_VM_FS_WRITEBACK",
                "PVISOR_VM_FS_HOST_WRITEBACK",
                "PVISOR_VM_GUEST_CACHE",
                "PVISOR_FS_PROFILE",
            )
        },
        concurrent_redirty=dict(
            independent_opens=4,
            generations=3,
            reopen_count=114,
            verified_file_bytes=2097408,
            written_bytes_per_generation=2097152,
            pwrite_operations=6144,
            scope="final byte correctness under disjoint concurrent writes; exact interleavings unobserved",
        ),
        actual_dirty_pressure_target_bytes=int(pressure[0]),
        actual_bdi_minimum_reservations_bytes=[int(value) for value in reservations],
        final_invocations_sha256=sha(batch / invocation_file),
        sg_scratch_contract_sha256=receipt["files"]["test_virtio_sg_scratch_contract.py"],
        limitations=[
            "correctness regressions are direct executions, separate from formal curves",
            "Linux x86_64/KVM SDK; no macOS or ordinary CLI result",
            "concurrent reclaim, exhaustive disconnect and crash durability remain unmeasured",
        ],
    )


def pipeline(batch, protocol):
    revision(protocol)
    rows, evidence = ORIGINAL_PIPELINE(batch, protocol)
    require(
        all(row["b_completed_while_a_pending"] is True for row in rows),
        "both0048 and0049 must preserve progress of B while delayed A is pending",
    )
    evidence["firmware_comparison"] = dict(baseline_patch=48, candidate_patch=49)
    evidence["scope"] = (
        "one controlled0048/0049 execution; both preserve independent-file progress; "
        "no throughput, device latency or independent performance CI"
    )
    return rows, evidence


def collect(args):
    require(
        args.pipeline is not None and args.correctness is not None,
        "0049v2 publication requires both completed pipeline and correctness evidence",
    )
    # This module instance is private to the new CLI. Its globals and the
    # frozen reproduction copies remain separate from this publication layout.
    BASE.PREFIX = NAMESPACE + "-"
    BASE.ASSETS = Path("docs/overrides/assets/benchmarks") / NAMESPACE
    BASE.firmware, BASE.audit_cohort = firmware, audit_cohort
    BASE.audit_analyses, BASE.correctness, BASE.pipeline = audit_analyses, correctness, pipeline
    files, manifest, _ = ORIGINAL_COLLECT(args)
    remapped = {}
    for name, item in files.items():
        if name.endswith(".pdf"):
            continue  # Local analysis PDFs are never public manifest entries.
        target = (
            name
            if name.endswith(".csv")
            else name.replace("guest-writeback-", "guest-writeback-tuning-", 1)
        )
        require(target not in remapped, "tuning public filename collision")
        remapped[target] = item
    manifest.update(
        namespace=NAMESPACE,
        publisher_sha256=sha(Path(__file__)),
        common_validator_sha256=BASE_SHA256,
        firmware_comparison=dict(baseline_patch=48, candidate_patch=49),
        public_outputs={
            str(BASE.ASSETS / name): item["sha256"]
            for name, item in sorted(remapped.items())
            if name.endswith(".svg")
        },
        files={
            name: {key: value for key, value in item.items() if key != "data"}
            for name, item in sorted(remapped.items())
            if name.endswith((".csv", ".svg"))
        },
    )
    encoded = BASE.public(
        (json.dumps(manifest, indent=2, sort_keys=True, allow_nan=False) + "\n").encode(),
        "tuning provenance.json",
    )
    return remapped, manifest, encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timing", type=Path, required=True)
    parser.add_argument("--diagnostic", type=Path, required=True)
    parser.add_argument("--analysis", type=Path)
    parser.add_argument("--time-analysis", type=Path)
    parser.add_argument("--pipeline", type=Path, required=True)
    parser.add_argument("--correctness", type=Path, required=True)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    files, manifest, encoded = collect(args)
    targets = BASE.destinations(files, encoded)
    archives = BASE.archive_targets(
        args.timing, NAMESPACE, {name: item["data"] for name, item in files.items()}, encoded
    )
    BASE.preflight_archive(archives)
    BASE.preflight_targets(targets, BASE.REPO)
    if args.publish:
        BASE.write_archive(archives)
        for name, data in targets.items():
            target = BASE.REPO / name
            target.parent.mkdir(parents=True, exist_ok=True)
            try:
                with target.open("xb") as stream:
                    stream.write(data)
            except FileExistsError:
                require(
                    not target.is_symlink() and target.is_file() and target.read_bytes() == data,
                    "concurrent tuning publication differs",
                )
    print(
        json.dumps(
            dict(
                state="published" if args.publish else "checked-not-published",
                csv_files=sum(name.endswith(".csv") for name in files),
                figures=sum(not name.endswith(".csv") for name in files),
                provenance_sha256=hashlib.sha256(encoded).hexdigest(),
                archive_directory=str(Path(args.timing).resolve() / "publication" / NAMESPACE),
                archive_files=len(archives),
                outputs=[str(path) for path in targets],
                manifest=manifest,
            ),
            indent=2,
            allow_nan=False,
        )
    )


if __name__ == "__main__":
    main()
