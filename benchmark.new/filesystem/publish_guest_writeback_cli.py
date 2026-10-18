#!/usr/bin/env python3
"""B-FS-ENG/B-FS-DIAG: publish only a complete ordinary-CLI WB0/1 cohort.

--check is read-only. --publish archives four derived CSVs and their provenance
under the input batch's publication directory. Articles and
registry entries remain caller-owned. No subjects, compilation or source-tree
inventory runs here; retained before/after source audits bind the build inputs.
"""

import argparse
import csv
import hashlib
import importlib.util
import io
import json
import random
import re
import statistics
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from publication_archive import archive_targets, preflight_archive, write_archive  # noqa: E402 - import after module-loading setup

PREFIX = "filesystem-guest-writeback-cli"
PRIVATE = re.compile(rb"/(?:home|tmp|sys|proc|run|root)(?:/|\\)")
BUCKETS = (
    "write_size_le_1024",
    "write_size_le_4096",
    "write_size_le_16384",
    "write_size_le_65536",
    "write_size_le_131072",
    "write_size_le_1048576",
    "write_size_gt_1048576",
)
RPC = (
    "LOOKUP",
    "GETATTR",
    "SETATTR",
    "OPEN",
    "READ",
    "WRITE",
    "FLUSH",
    "RELEASE",
    "FSYNC",
    "FSYNCDIR",
    "UNLINK",
    "RENAME",
    "MKDIR",
)


def require(value, message):
    if not value:
        raise ValueError(message)


def load(path):
    return json.loads(Path(path).read_text())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def digest(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def csv_bytes(rows):
    fields = list(dict.fromkeys(key for row in rows for key in row))
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                key: json.dumps(value, sort_keys=True) if isinstance(value, (dict, list)) else value
                for key, value in row.items()
            }
        )
    data = stream.getvalue().encode()
    require(not PRIVATE.search(data), "private host path in public CSV")
    return data


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def entrypoint(batch, phase, rows):
    folder = batch / ("campaign-" + phase)
    template, bench, render = (
        load(folder / name) for name in ("in.json", "in.bench.json", "in.render.json")
    )
    require(set(template) == {"guest-writeback-cli"}, "wrong registered semspec row")
    command = template["guest-writeback-cli"].removeprefix("$ ")
    expected = dict(state="complete", rows=rows, report=str(batch / phase / "report.json"))
    require(
        bench.get("format") == 1
        and len(bench.get("commands", [])) == 1
        and len(bench.get("rows", [])) == 1
        and bench["rows"][0]["row"] == "guest-writeback-cli",
        "authentic semspec command inventory required",
    )
    executed = bench["commands"][0]
    require(
        executed.get("command") == bench["rows"][0].get("command") == command
        and executed.get("kind") == "script-row"
        and executed.get("raw") == executed.get("value") == expected
        and render == {"guest-writeback-cli": expected},
        "semspec result/report join differs",
    )
    logfile = Path(executed["log"])
    require(
        logfile.resolve().parent == folder / "in.bench.logs"
        and json.loads(logfile.read_text()) == expected,
        "semspec stdout evidence differs",
    )
    return {name: sha(folder / name) for name in ("in.json", "in.bench.json", "in.render.json")}


def raw_row(batch, phase, arm, pair, row, config, runner, reference, rounds, sources):
    worker = batch / "raw" / phase / f"{arm}-{pair:03d}"
    root = worker / "trials" / f"filesystem-pvisor-vm-{pair:03d}"
    expected_logs = Path("/tmp/private-out") / root.relative_to(config["raw_root"])
    require(
        row["logs"] == str(expected_logs)
        and load(worker / "row.json") == row
        and load(worker / "worker-exit.json")["exit_code"] == 0
        and row.get("normal_cli") is True
        and row.get("correctness") == "passed"
        and row["condition"] == arm
        and row["pair"] == pair,
        "actual normal-CLI raw row/exit/identity differs",
    )
    require(
        row["actual_environment"] == runner.environment(arm, phase == "profiles")
        and row["factors"]
        == dict(GUEST_WRITEBACK=runner.FACTORS[arm], HOST_WRITEBACK=0, DATA=0, FLUSH=0, NE=0, SQ=0)
        and row["selected_firmware"] == config["artifacts"]["firmware"],
        "actual treatment differs",
    )
    frontend = load(worker / "frontend-environment.json")
    require(
        frontend == row["frontend_guest_writeback_environment"]
        and frontend["controls"] == runner.environment(arm, phase == "profiles")
        and frontend["host_writeback_config_present"] is False,
        "actual frontend environment evidence differs",
    )
    command = load(root / "command.json")
    require(
        command["exit"] == 0 and command["argv"] == frontend["argv"],
        "actual CLI command/exit differs",
    )
    selected = config["inputs"]["profiles" if phase == "profiles" else "timing"]
    sources.validate_cli_source(row, config, "workspace", selected, physical_directory=root)
    argv = command["argv"]
    firmware_dir = str(
        Path("/tmp/private-out") / Path(config["firmware"]).relative_to(config["raw_root"])
    )
    require(
        argv.count("--vm-library-dir") == 1
        and argv[argv.index("--vm-library-dir") + 1] == firmware_dir,
        "CLI did not select the frozen runtime firmware",
    )
    job = load(root / "stage/execution-job.json")
    require(
        job["config"]["vm"]["library_dir"] == firmware_dir,
        "persisted final firmware selector differs",
    )
    bundles = list((root / "runs").glob("*/run-bundle.json")) + list(
        (root / "stage").glob("run-bundle.json")
    )
    require(len(bundles) == 1, "exactly one completed Run Bundle required")
    bundle = load(bundles[0])
    reference.validate_bundle_execution(bundle, "pvisor-vm")
    stdout = (root / "stdout.log").read_text()
    results = [
        json.loads(line.removeprefix("REFERENCE_RESULT "))
        for line in stdout.splitlines()
        if line.startswith("REFERENCE_RESULT ")
    ]
    require(results == [row["result"]], "unique actual guest Result differs from retained row")
    rounds.validate_rounds(row["result"])
    reference.validate_staged_filesystem(root / "workspace", root / "stage", 256 * 64 * 1024)
    rounds.validate_stream_files({**row, "logs": str(root)}, "pvisor-vm")
    return dict(
        phase=phase,
        condition=arm,
        pair=pair,
        row_sha256=sha(worker / "row.json"),
        command_sha256=sha(root / "command.json"),
        frontend_environment_sha256=sha(worker / "frontend-environment.json"),
        stdout_sha256=sha(root / "stdout.log"),
        stderr_sha256=sha(root / "stderr.log"),
        Bundle_sha256=sha(bundles[0]),
        Job_sha256=sha(root / "stage/execution-job.json"),
        run_sha256=sha(root / "stage/run.json"),
        overlay_sha256=sha(root / "stage/overlay.json"),
        workspace_receipt_sha256=sha(root / "stage/workspace-baseline.json"),
        actual_firmware_sha256=config["artifacts"]["firmware"]["sha256"],
    )


def counters(batch, runner, inherited):
    rows, receipts = [], []
    for arm in runner.ARMS:
        directory = batch / "raw/profiles" / (arm + "-000")
        trial = directory / "trials/filesystem-pvisor-vm-000"
        saved = load(directory / "profiles.json")
        inherited.FACTORS = dict(off=0, on=0)  # readonly FLUSH remains off in both arms.
        inherited.ARMS = runner.ARMS
        checked = inherited.validate_profiles(
            (trial / "stderr.log").read_text().splitlines(), arm, runner.HISTOGRAM
        )
        require(
            checked["collected"] == saved["collected"] and checked["registered_instances"] == 13,
            "raw profile/token/final report differs",
        )
        collected = checked["collected"]
        all_ids = {(p["pid"], p["component"], p["instance"]) for p in collected["final_profiles"]}
        unscoped_ids = {
            (p["pid"], p["component"], p["instance"]) for p in collected["unscoped_final_profiles"]
        }
        require(
            len(collected["rows"]) == 216
            and len(collected["final_profiles"]) == 13
            and len(all_ids) == 13
            and len(unscoped_ids) == 5
            and unscoped_ids <= all_ids
            and len(all_ids - unscoped_ids) == 8,
            "expected 13/8/5 profile inventory differs",
        )
        finals = {
            (p["pid"], p["component"], p["instance"]): p["measurements"]
            for p in collected["final_profiles"]
        }
        for scope in ("rootfs", "pvisor-workspace"):
            scoped = [p for p in collected["rows"] if p["scope"] == scope]
            require(
                {p[k] for p in scoped for k in ("before_token", "after_token")}
                == set(range(1, 55)),
                "all 54 scoped phase tokens are required",
            )
            owner = saved["actual"][scope]["virtio-fs-overlay"]
            negotiated = runner.validate_guest_writeback_state(
                finals[(owner["pid"], "virtio-fs-overlay", owner["instance"])], arm
            )
            require(owner["guest_writeback"] == negotiated, "actual mounted WB state differs")
            for number in range(1, 4):
                for operation in runner.OPERATIONS:
                    phase = [
                        p for p in scoped if p["round"] == number and p["operation"] == operation
                    ]
                    by = {p["component"]: p["measurements"] for p in phase}
                    require(
                        len(phase) == 4 and len(by) == 4,
                        "complete phase component inventory required",
                    )
                    protocol, core, overlay = (
                        by[name]
                        for name in ("virtio-fs-protocol", "overlay-core", "virtio-fs-overlay")
                    )

                    def value(values, label, field, explicit=False):
                        result = values.get(label, {}).get(field)
                        if result is None and not explicit:
                            return 0
                        require(
                            type(result) is int and result >= 0,
                            "invalid profile count " + label + "/" + field,
                        )
                        return result

                    writes = value(protocol, "WRITE", "calls")
                    payload = value(protocol, "write_payload_bytes", "units", True)
                    histogram = {key: value(protocol, key, "units", True) for key in BUCKETS}
                    background = value(protocol, "write_background_requests", "units", True)
                    require(
                        sum(histogram.values()) == writes
                        and background <= writes
                        and ((writes == 0 and payload == 0) or (writes > 0 and payload > 0)),
                        "WRITE size/background/count differs",
                    )
                    row = dict(
                        condition=arm,
                        scope=scope,
                        round=number,
                        operation=operation,
                        guest_writeback_requested=runner.FACTORS[arm],
                        guest_writeback_enabled=negotiated["actual"]["guest_writeback_enabled"],
                        guest_batched_writeback_enabled=negotiated["actual"][
                            "guest_batched_writeback_enabled"
                        ],
                        **{
                            "protocol_" + name.lower() + "_calls": value(protocol, name, "calls")
                            for name in RPC
                        },
                        write_payload_bytes=payload,
                        write_background_requests=background,
                        write_mean_payload_bytes=payload / writes if writes else None,
                        **histogram,
                        **{
                            "core_" + label + "_units": value(core, label, "units")
                            for label in (
                                "layer_parent_stats",
                                "layer_leaf_stats",
                                "parent_identity_statx_calls",
                                "mount_identity_attempts",
                                "immutable_lower_cache_misses_missing_leaf",
                            )
                        },
                        **{
                            "core_" + label + "_calls": value(core, label, "calls")
                            for label in (
                                "fingerprint",
                                "fingerprint_content",
                                "fingerprint_metadata",
                                "fingerprint_xattrs",
                            )
                        },
                        core_fingerprint_content_reused_units=value(
                            core, "fingerprint_content_reused", "units"
                        ),
                        **{
                            label: value(overlay, label, "units")
                            for label in (
                                "open_qualification_fgetfl_attempts",
                                "open_qualification_fstat_attempts",
                                "open_qualification_statx_attempts",
                                "open_qualification_fstatfs_attempts",
                            )
                        },
                    )
                    rows.append(row)
        selection = load(directory / "firmware-selection.json")
        require(
            len(selection["observed"]) == 1
            and selection["expected_firmware_sha256"]
            == runner.load(batch / "campaign-profiles/config.json")["artifacts"]["firmware"][
                "sha256"
            ],
            "actual native firmware mapping evidence required",
        )
        observed = selection["observed"][0]
        file = Path(load(batch / "campaign-profiles/config.json")["artifacts"]["firmware"]["path"])
        identity = file.stat()
        config = load(batch / "campaign-profiles/config.json")
        expected = Path("/tmp/private-out") / file.relative_to(config["raw_root"])
        require(
            observed.get("mapped_sha256") == config["artifacts"]["firmware"]["sha256"],
            "actual process-root firmware byte hash differs",
        )
        parsed = runner.firmware_mapping_entry(
            observed["maps_line"],
            expected,
            identity,
            mountinfo=observed.get("btrfs_mountinfo"),
            mapped_sha256=observed["mapped_sha256"],
        )
        require(
            parsed is not None
            and all(observed[key] == value for key, value in parsed.items())
            and type(observed.get("pid")) is int
            and observed["pid"] > 0
            and observed["device"] == identity.st_dev
            and observed["inode"] == identity.st_ino
            and load(directory / "row.json")["firmware_selection"] == selection,
            "actual native mapping device/inode differs",
        )
        receipts.append(
            dict(
                condition=arm,
                registered_final_instances=13,
                scoped_instances=8,
                unscoped_instances=5,
                tokens_per_mount=54,
                component_intervals=216,
                native_firmware_mapping_observed=True,
                firmware_selection_sha256=sha(directory / "firmware-selection.json"),
                raw_profile_report_sha256=sha(directory / "profiles.json"),
            )
        )
    require(len(rows) == 108, "108 diagnostic operation/scope rows required")
    return rows, receipts


def project(batch):
    batch = batch.resolve()
    sys.path[:0] = [str(batch / "harness"), str(batch)]
    runner = module("frozen_guest_wb_cli", batch / "entry.py")
    inherited = module("inherited_guest_wb_cli", batch / "af_entry.py")
    import filesystem_rounds as rounds
    import reference_baselines as reference
    import workspace_sources as sources

    audits, reports, configs, raw = {}, {}, {}, []
    for phase, count in (("pre", 2), ("profiles", 2), ("timing", 10)):
        config = load(batch / ("campaign-" + phase) / "config.json")
        config["_config_path"] = str(batch / ("campaign-" + phase) / "config.json")
        runner.verify(config)
        report = load(batch / phase / "report.json")
        require(
            config["benchmark_id"] == ("B-FS-DIAG" if phase == "profiles" else "B-FS-ENG")
            and config["role"] == ("diagnostic" if phase == "profiles" else "engineering A/B")
            and report.get("config") == config,
            "phase role/config identity differs",
        )
        require(
            report.get("state") == "complete"
            and report.get("failures") == []
            and len(report["rows"]) == count
            and report["protocol"]["performance_sample"] is (phase == "timing"),
            "only a complete independent phase is eligible: " + phase,
        )
        samples = 5 if phase == "timing" else 1
        rng = random.Random(20261017)
        expected_order = []
        for pair in range(samples):
            order = list(runner.ARMS)
            rng.shuffle(order)
            expected_order.extend(dict(pair=pair, condition=arm) for arm in order)
        require(
            report["order"] == expected_order
            and len({(r["condition"], r["pair"]) for r in report["rows"]}) == count,
            "independent environment order/inventory differs",
        )
        audits[phase] = dict(
            report_sha256=sha(batch / phase / "report.json"),
            entrypoint=entrypoint(batch, phase, count),
        )
        for row in report["rows"]:
            raw.append(
                raw_row(
                    batch,
                    phase,
                    row["condition"],
                    row["pair"],
                    row,
                    config,
                    runner,
                    reference,
                    rounds,
                    sources,
                )
            )
        reports[phase], configs[phase] = report, config
    phase_specific = {"phase", "samples", "benchmark_id", "role", "output", "_config_path"}
    common = [
        {key: value for key, value in configs[phase].items() if key not in phase_specific}
        for phase in ("pre", "profiles", "timing")
    ]
    require(
        common[0] == common[1] == common[2],
        "pre/diagnostic/timing build, firmware, origins, prepared inputs or controls differ",
    )
    verified = load(batch / "timing-analysis.json")
    require(
        verified["state"] == "verified"
        and verified["n_paired_environments"] == 5
        and verified["dependent_rounds_per_environment"] == 3
        and verified["report"]["sha256"] == audits["timing"]["report_sha256"],
        "timing analysis/raw join differs",
    )
    extracted = {(r["condition"], r["pair"]): runner.metrics(r) for r in reports["timing"]["rows"]}
    expected_summary, expected_comparisons, envrows = [], [], []
    for arm in runner.ARMS:
        for pair in range(5):
            envrows.append(dict(condition=arm, pair=pair, **extracted[(arm, pair)]))
    for metric in next(iter(extracted.values())):
        samples = {a: [extracted[(a, p)][metric] for p in range(5)] for a in runner.ARMS}
        for arm, values in samples.items():
            expected_summary.append(
                dict(
                    condition=arm,
                    metric=metric,
                    n=5,
                    median_ms=statistics.median(values),
                    min_ms=min(values),
                    max_ms=max(values),
                    samples_by_pair_ms=values,
                )
            )
        rng = random.Random(20261017)
        boot = []
        for _ in range(5000):
            indexes = [rng.randrange(5) for _ in range(5)]
            boot.append(
                100
                * (
                    1
                    - statistics.median(samples["on"][i] for i in indexes)
                    / statistics.median(samples["off"][i] for i in indexes)
                )
            )
        expected_comparisons.append(
            dict(
                metric=metric,
                primary=metric == "three_rounds.seven_tools_ms",
                reduction_percent=100
                * (1 - statistics.median(samples["on"]) / statistics.median(samples["off"])),
                ci95_low=runner.percentile(boot, 0.025),
                ci95_high=runner.percentile(boot, 0.975),
                off_samples_ms=samples["off"],
                on_samples_ms=samples["on"],
                bootstrap_replicates=5000,
                seed=20261017,
            )
        )
    require(
        verified["summary"] == expected_summary
        and verified["comparisons"] == expected_comparisons
        and len(expected_summary) == 64
        and len(expected_comparisons) == 32,
        "all 32 independently recomputed timing/paired intervals must match",
    )
    counter_rows, profile_receipts = counters(batch, runner, inherited)
    before, after = load(batch / "source-before.json"), load(batch / "source-after.json")
    require(
        before["state"] == after["state"] == "verified"
        and before["source_hashes_sha256"] == after["source_hashes_sha256"]
        and before["build_receipt"] == after["build_receipt"],
        "before/after source audit association differs",
    )
    build = load(configs["timing"]["build_receipt"]["path"])
    require(
        before["files"] == after["files"] == len(build["source_hashes"])
        and before["source_hashes_sha256"]
        == hashlib.sha256(json.dumps(build["source_hashes"], sort_keys=True).encode()).hexdigest(),
        "retained source audit/build source set differs",
    )
    files = {
        PREFIX + ".csv": csv_bytes(expected_summary),
        PREFIX + "-comparisons.csv": csv_bytes(expected_comparisons),
        PREFIX + "-environments.csv": csv_bytes(envrows),
        PREFIX + "-counters.csv": csv_bytes(counter_rows),
    }
    provenance = dict(
        schema=1,
        benchmark_ids=["B-FS-ENG", "B-FS-DIAG"],
        cohort=batch.name,
        role="ordinary CLI engineering A/B with independent diagnostics",
        publisher_sha256=sha(Path(__file__)),
        analysis_sha256=sha(batch / "entry.py"),
        frozen_harness_sha256=sha(batch / "harness-manifest.json"),
        frozen_files=len(load(batch / "harness-manifest.json")["files"]),
        current_GNU_CLI_build_receipt_sha256=configs["timing"]["build_receipt"]["sha256"],
        product_source_files=len(build["source_hashes"]),
        product_source_hashes_sha256=digest(build["source_hashes"]),
        source_head=build["head"],
        artifact_sha256={k: v["sha256"] for k, v in configs["timing"]["artifacts"].items()},
        firmware_SOURCE_sha256=configs["timing"]["firmware_source"]["sha256"],
        origins_sha256=sha(batch / "origins.json"),
        source_before_sha256=sha(batch / "source-before.json"),
        source_after_sha256=sha(batch / "source-after.json"),
        raw_environment_chain=raw,
        phases=audits,
        diagnostic_receipts=profile_receipts,
        treatment="only guest writeback0/1; hostWB/DAX/DATA/readonlyFLUSH/NE/SQ/Core absence0; guestcache1/directory capacity256",
        input_scope="same existing owned rootfs/workspace generations and frozen baseline; independent live apply target. Generation import/seal not repeated or charged; trial targets/caches and CLI copies still prepared.",
        clock="Ready includes fresh private Host+VM and excludes exit. Worker clocks retain original workload synchronization/close, three resident rounds per environment. Small-file write uses write/close/stat without explicit fsync; stream_write includes file fsync+close. Seven-tool sum is tool responsiveness, not a completed-durability clock; full host bytes verified after normal exit.",
        statistical_scope="N5 fresh paired environment blocks, not N15; 32 pointwise uncorrected percentile-bootstrap intervals, one preregistered sum-of-three-round-seven-tools primary; all five values retained.",
        diagnostic_scope="Independent N1 per arm; counts include checked subprocess startup/validation. WRITE bytes/buckets describe protocol attempts, not physical disk I/O. No exact maximum inferred from cumulative maxima.",
        fingerprint_scope="generic fingerprint.calls and fingerprint_content.calls are distinct; compact content fingerprints must not be inferred from generic0.",
        checkpoint_scope="Bundle supports_checkpoint=false is a declared plan; actual native checkpoint binding unobserved. Native GNU firmware file mapping observed independently in diagnostics.",
        exclusion_scope="no quiet/interference/full-inventory gates; all valid slow samples retained; no historical CLI/SDK timing pooled",
        timing_analysis_sha256=sha(batch / "timing-analysis.json"),
        public_outputs={},
        archived_files={
            name: dict(
                rows=len(list(csv.DictReader(io.StringIO(data.decode())))),
                sha256=hashlib.sha256(data).hexdigest(),
            )
            for name, data in files.items()
        },
    )
    encoded = (json.dumps(provenance, indent=2, sort_keys=True) + "\n").encode()
    require(not PRIVATE.search(encoded), "private path in public provenance")
    return files, encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--batch", type=Path, required=True)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    files, provenance = project(args.batch)
    targets = archive_targets(args.batch, PREFIX, files, provenance)
    preflight_archive(targets)
    if args.publish:
        write_archive(targets)
    print(
        json.dumps(
            dict(
                state="published" if args.publish else "verified-read-only",
                files={
                    name: dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
                    for name, data in files.items()
                },
                provenance_sha256=hashlib.sha256(provenance).hexdigest(),
                public_outputs=[],
                archive_directory=str(args.batch.resolve() / "publication" / PREFIX),
                archive_files=len(targets),
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
