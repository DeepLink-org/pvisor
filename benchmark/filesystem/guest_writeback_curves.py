#!/usr/bin/env python3
"""B-FS-ENG/B-FS-DIAG: guest writeback coalescing, an independent followup.

Question: does guest-side merging reduce FUSE WRITE requests and completed
file+directory-fsync time, with host write buffering disabled?
Design: off/guest-wb/old-wb/native, N5 fresh environments x three dependent rounds;
independent N1 diagnostics. No auxiliary dup/close before final fsync. No quiet,
interference, inventory or slow-value gate. This file does not change old batches.

--draft only prints the registered plan. --prepare freezes and builds ONLY when
the caller has finalized product sources and firmware; do not use it as a draft.
--measure starts the frozen campaign; --campaign is its budgeted worker entry.
"""

import argparse
import contextlib
import io
import json
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import curves as BASE  # noqa: E402 - import after module-loading setup
import curves_diagnostics as DIAG  # noqa: E402 - import after module-loading setup

MIB = 1 << 20
SUITE = "guest-writeback"
ARMS = ("off", "guest-wb", "old-wb", "native")
FIXED = {
    "PVISOR_VM_FS_HOST_WRITEBACK": "0",
    "PVISOR_VM_GUEST_CACHE": "1",
    "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY": "256",
    "PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE": "0",
    "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE": "0",
    "PVISOR_VM_FS_LOWER_DATA_CACHE": "0",
    "PVISOR_VM_FS_READONLY_FLUSH_ELISION": "0",
    "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE": "0",
    "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION": "0",
}
PHASE_CONTRACT = dict(
    applied_phase_measured=False,
    applied_ns=0,
    applied_barriers=0,
    applied_boundary="unused_no_auxiliary_flush",
    stream_fsync_boundary="fsync_original_open_file_includes_guest_writeback_drain",
    completed="accepted loop + final file fsync/drain + checked final close + directory fsync",
    smallfiles="create/write/close in loop; reopen/fsync/checked-close each file at final sync",
    scope="SDK mechanism only; no RAM offload, cold disk, power-loss or ordinary CLI claim",
)
COUNTERS = dict(
    enabled_component="virtio-fs-overlay",
    enabled_label="guest_writeback_enabled",
    batched_enabled_label="guest_batched_writeback_enabled",
    bytes_label="write_payload_bytes",
    background_label="write_background_requests",
    buckets=[
        dict(label="write_size_le_1024", upper_bytes=1024),
        dict(label="write_size_le_4096", upper_bytes=4096),
        dict(label="write_size_le_16384", upper_bytes=16384),
        dict(label="write_size_le_65536", upper_bytes=65536),
        dict(label="write_size_le_131072", upper_bytes=131072),
        dict(label="write_size_le_1048576", upper_bytes=1048576),
        dict(label="write_size_gt_1048576", upper_bytes=None),
    ],
)


def require(value, message):
    if not value:
        raise ValueError(message)


def validate_counter_contract(contract):
    require(set(contract) == set(COUNTERS), "explicit counter contract fields differ")
    require(
        contract["enabled_component"] in DIAG.COMPONENTS
        and isinstance(contract["enabled_label"], str)
        and contract["enabled_label"]
        and isinstance(contract["batched_enabled_label"], str)
        and contract["batched_enabled_label"]
        and isinstance(contract["background_label"], str)
        and contract["background_label"]
        and isinstance(contract["bytes_label"], str)
        and contract["bytes_label"]
        and isinstance(contract["buckets"], list)
        and contract["buckets"],
        "counter contract is incomplete",
    )
    previous, names = 0, set()
    for index, bucket in enumerate(contract["buckets"]):
        require(
            set(bucket) == {"label", "upper_bytes"}
            and isinstance(bucket["label"], str)
            and bucket["label"]
            and bucket["label"] not in names,
            "invalid/repeated size bucket",
        )
        bound = bucket["upper_bytes"]
        require(
            (bound is None and index == len(contract["buckets"]) - 1)
            or (type(bound) is int and bound > previous),
            "size bounds must increase; overflow last",
        )
        names.add(bucket["label"])
        if bound is not None:
            previous = bound
    require(contract["buckets"][-1]["upper_bytes"] is None, "explicit overflow bucket required")


def write_sizes(measurements, contract):
    def units(label):
        value = measurements.get(label, {}).get("units")
        require(type(value) is int and value >= 0, f"missing explicit WRITE size counter: {label}")
        return value

    count = measurements.get("WRITE", {}).get("calls")
    require(type(count) is int and count >= 0, "missing explicit WRITE.calls")
    size = units(contract["bytes_label"])
    background = units(contract["background_label"])
    histogram = [{**bucket, "requests": units(bucket["label"])} for bucket in contract["buckets"]]
    require(
        sum(v["requests"] for v in histogram) == count and background <= count,
        "WRITE histogram/background/count differs",
    )
    require((count == 0 and size == 0) or (count > 0 and size > 0), "WRITE bytes/count differs")
    previous, maximum_bounds = 0, None
    for bucket in histogram:
        if bucket["requests"]:
            maximum_bounds = dict(lower_bytes=previous + 1, upper_bytes=bucket["upper_bytes"])
        if bucket["upper_bytes"] is not None:
            previous = bucket["upper_bytes"]
    return dict(
        write_requests=count,
        write_payload_bytes=size,
        write_background_requests=background,
        mean_payload_bytes=None if count == 0 else size / count,
        payload_histogram=histogram,
        interval_max_bytes=None,
        largest_occupied_bucket_bounds=maximum_bounds,
        max_scope="exact per-phase maximum is unavailable; cumulative maxima are not subtracted",
    )


def actual_writeback(finals, condition, contract=COUNTERS):
    require(condition in ("off", "guest-wb", "old-wb"), "native has no negotiated FUSE state")
    values = finals[contract["enabled_component"]]
    actual = values.get(contract["enabled_label"], {}).get("units")
    batched = values.get(contract["batched_enabled_label"], {}).get("units")
    require(
        type(actual) is int
        and type(batched) is int
        and actual == batched == int(condition in ("guest-wb", "old-wb")),
        "actual guest writeback negotiation differs from requested condition",
    )
    return actual, batched


PATCHES = {
    47: "patches/0047-fuse-isolate-immutable-lower-data.patch",
    48: "patches/0048-fuse-pipeline-native-writeback-pressure.patch",
    49: "patches/0049-fuse-coalesce-writeback-kicks.patch",
}


def validate_firmware_record(record, firmware_sha256, *, old=False, baseline_patch=47):
    require(baseline_patch in (47, 48), "unsupported firmware baseline")
    revision = baseline_patch if old else baseline_patch + 1
    patch = PATCHES[revision]
    require(
        record.get("firmware_sha256") == firmware_sha256
        and isinstance(record.get("inputs"), dict)
        and patch in record["inputs"],
        "firmware receipt must match these bytes and required baseline/new patch",
    )
    require(
        not old or PATCHES[revision + 1] not in record["inputs"],
        "old firmware must precede candidate patch",
    )


def cells(suite=SUITE):
    require(suite == SUITE, "wrong guest-writeback suite")
    result = []

    def add(id_, family, x, block=4096, workers=1, size=4 * MIB, sync=0, workload="stream"):
        result.append(
            dict(
                id=id_,
                family=family,
                x=x,
                workload=workload,
                block_bytes=block,
                workers=workers,
                bytes_per_worker=size // workers,
                sync_every_ops=sync,
                x_label={
                    "block": "Write block (KiB)",
                    "workers": "Concurrent files / workers",
                    "barrier": "Writes per fsync (0 = final only)",
                    "pressure": "Bytes per round (MiB)",
                    "smallfiles": "File size (KiB)",
                }[family],
                x_scale="log2" if family in ("block", "pressure") else "linear",
            )
        )

    for kib in (1, 4, 64, 1024):
        add(f"block-{kib}k", "block", kib, block=kib << 10)
    for workers in (1, 2, 4):
        add(f"workers-{workers}", "workers", workers, workers=workers)
    for interval in (1, 16, 256, 0):
        add(f"barrier-{interval}", "barrier", interval, size=MIB, sync=interval)
    add("tiny-1k", "smallfiles", 1, block=1024, size=128 << 10, workload="smallfiles")
    for mib in (8, 64, 128):
        add(f"pressure-{mib}m", "pressure", mib, block=65536, size=mib * MIB)
    return result


def conditions(suite=SUITE, *, batch=None, baseline_patch=47):
    require(suite == SUITE, "wrong guest-writeback suite")
    old = (
        str(Path(batch) / "bin/libkrunfw-old.so")
        if batch is not None
        else "@BATCH@/bin/libkrunfw-old.so"
    )
    return {
        name: dict(
            env={
                **FIXED,
                "PVISOR_VM_FS_WRITEBACK": "1" if name in ("guest-wb", "old-wb") else "0",
                "CURVES_FIRMWARE": old if name == "old-wb" else "",
            },
            description=(
                "same static payload on native filesystem, reference only"
                if name == "native"
                else f"old{baseline_patch:04} firmware WB1; same current host/payload; incremental baseline"
                if name == "old-wb"
                else "guest writeback enabled; host write buffer off"
                if name == "guest-wb"
                else "guest writeback off; direct FUSE writes"
            ),
        )
        for name in ARMS
    }


def validate_row(row):
    require(
        row.get("applied_phase_measured") is False
        and row.get("applied_ns") == 0
        and row.get("applied_barriers") == 0
        and row.get("applied_boundary") == "unused_no_auxiliary_flush",
        "auxiliary applied/FLUSH phase must be absent, not treated as a WB1 drain",
    )
    require(
        row["total_ns"]
        == sum(row[k] for k in ("accepted_ns", "applied_ns", "fsync_ns", "close_ns", "dirsync_ns")),
        "completed phase partition differs",
    )
    expected_files = row["workers"] * (
        row["bytes_per_worker"] // row["block_bytes"] if row["workload"] == "smallfiles" else 1
    )
    require(
        row["final_fsyncs"] == expected_files
        and row["final_closes"] == expected_files
        and row["directory_fsyncs"] == row["workers"] + 4,
        "file/close/directory durability boundary was weakened",
    )
    if row["workload"] == "stream":
        require(
            row.get("fsync_boundary") == PHASE_CONTRACT["stream_fsync_boundary"],
            "stream fsync must include final dirty-page drain",
        )


def install_trial_validation():
    original = BASE.one_trial

    def trial(batch, protocol, pair, condition):
        # The parent observer removes successful output after full-byte checks.
        # Delay only this known stage deletion until the new clock contract also
        # passes, so a malformed new payload cannot lose its failed evidence.
        stage = batch / "trials" / f"{pair:03}-{condition}" / "stage"
        remove = BASE.shutil.rmtree
        deferred = []

        def retain_stage(path, *args, **kwargs):
            if Path(path) == stage:
                deferred.append((path, args, kwargs))
            else:
                remove(path, *args, **kwargs)

        BASE.shutil.rmtree = retain_stage
        try:
            result = original(batch, protocol, pair, condition)
        finally:
            BASE.shutil.rmtree = remove
        if result.get("valid"):
            try:
                expected = protocol["conditions"][condition]["env"]
                require(
                    all(result["environment"].get(k) == v for k, v in expected.items()),
                    "actual subject environment differs from treatment",
                )
                for row in result["rows"]:
                    validate_row(row)
                if condition != "native":
                    path = (
                        batch
                        / "bin"
                        / ("libkrunfw-old.so" if condition == "old-wb" else "libkrunfw.so")
                    )
                    receipt_lines = (stage.parent / "stderr.log").read_text().splitlines()
                    receipts = [
                        json.loads(line.split(" ", 1)[1])
                        for line in receipt_lines
                        if line.startswith("CURVES_FIRMWARE_SELECTED ")
                    ]
                    require(
                        receipts
                        == [
                            dict(
                                path=str(path.resolve()),
                                selection="condition_env" if condition == "old-wb" else "argv",
                            )
                        ],
                        "actual subject firmware selection differs from frozen condition",
                    )
                    result["firmware_selection"] = receipts[0]
                    result["firmware_sha256"] = BASE.sha(path)
                    BASE.save(stage.parent / "result.json", result)
            except BaseException as error:
                result.update(
                    valid=False,
                    error=repr(error),
                    boundary="post-observer phase contract; original full-byte receipt retained",
                )
                BASE.save(batch / "trials" / f"{pair:03}-{condition}" / "result.json", result)
        if result.get("valid"):
            for path, args, kwargs in deferred:
                remove(path, *args, **kwargs)
        return result

    BASE.one_trial = trial


def prepare(args):
    require(
        args.samples == (1 if args.diagnostic or args.preflight else 5),
        "formal N5 or explicit N1 diagnostic/preflight only",
    )
    require(not (args.diagnostic and args.preflight), "preflight and diagnostic are distinct")
    require(args.firmware_source is not None, "new firmware SOURCE receipt required")
    require(
        args.old_firmware is not None and args.old_firmware_source is not None,
        "explicit old firmware and its SOURCE receipt required",
    )
    firmware_source = args.firmware_source.read_bytes()
    firmware_record = json.loads(firmware_source)
    validate_firmware_record(
        firmware_record, BASE.sha(args.firmware), baseline_patch=args.baseline_patch
    )
    old_source = args.old_firmware_source.read_bytes()
    old_record = json.loads(old_source)
    validate_firmware_record(
        old_record, BASE.sha(args.old_firmware), old=True, baseline_patch=args.baseline_patch
    )
    require(
        old_record["kernel_source_sha256"] == firmware_record["kernel_source_sha256"]
        and old_record["actual_config_sha256"] == firmware_record["actual_config_sha256"],
        "old/new firmware kernel source or actual configuration differs",
    )
    old_patches = {
        key: value for key, value in old_record["inputs"].items() if key.startswith("patches/")
    }
    new_patches = {
        key: value for key, value in firmware_record["inputs"].items() if key.startswith("patches/")
    }
    require(
        all(new_patches.get(key) == value for key, value in old_patches.items())
        and new_patches.keys() - old_patches.keys() == {PATCHES[args.baseline_patch + 1]},
        "firmware A/B must retain all old patches and add only the candidate patch",
    )
    patches = {}
    for name, expected in firmware_record["inputs"].items():
        if name.startswith("patches/"):
            source = BASE.REPO / "fw" / name
            require(
                ".." not in Path(name).parts and BASE.sha(source) == expected,
                "current patch differs from compiled firmware SOURCE: " + name,
            )
            patches[name] = source.read_bytes()
    # Let the unchanged, audited preparer build the new private payload under its
    # expected name. The new payload's source hash is explicit in this protocol.
    with tempfile.TemporaryDirectory(prefix="pvisor-guest-writeback-inputs-") as directory:
        inputs = Path(directory)
        for name in ("curves.py", "curves_analysis.py", "curves_diagnostics.py"):
            shutil.copy2(HERE / name, inputs / name)
        shutil.copy2(HERE / "guest_writeback_driver.rs", inputs / "curves_driver.rs")
        shutil.copy2(HERE / "guest_writeback_guest.rs", inputs / "curves_guest.rs")
        original_here, original_cells, original_conditions = BASE.HERE, BASE.cells, BASE.conditions
        BASE.HERE, BASE.cells, BASE.conditions = inputs, cells, conditions
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                BASE.prepare(args)
        finally:
            BASE.HERE, BASE.cells, BASE.conditions = (
                original_here,
                original_cells,
                original_conditions,
            )
    batch = args.prepare.resolve()
    target = batch / "harness/guest_writeback_curves.py"
    shutil.copy2(__file__, target)
    protocol = json.loads((batch / "protocol.json").read_text())
    require(
        BASE.sha(batch / "bin/libkrunfw.so") == firmware_record["firmware_sha256"],
        "firmware changed while preparing build",
    )
    shutil.copy2(args.old_firmware, batch / "bin/libkrunfw-old.so")
    require(
        BASE.sha(batch / "bin/libkrunfw-old.so") == old_record["firmware_sha256"],
        "old firmware changed while preparing build",
    )
    firmware_dir = batch / "firmware-source"
    firmware_dir.mkdir()
    (firmware_dir / "SOURCE.json").write_bytes(firmware_source)
    (firmware_dir / "OLD-SOURCE.json").write_bytes(old_source)
    for name, data in patches.items():
        destination = firmware_dir / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
    protocol.update(
        question=__doc__.split("Question: ", 1)[1].split("Design:", 1)[0].strip(),
        baseline_condition="old-wb",
        phase_contract=PHASE_CONTRACT,
        conditions=conditions(batch=batch, baseline_patch=args.baseline_patch),
        preflight=args.preflight,
        performance_samples=not args.diagnostic and not args.preflight,
        condition_factors={
            name: dict(
                backend="native" if name == "native" else "vm",
                guest_writeback=name in ("guest-wb", "old-wb"),
                host_writeback=False,
                dax=False,
                firmware=f"old{args.baseline_patch:04}"
                if name == "old-wb"
                else f"new{args.baseline_patch + 1:04}"
                if name != "native"
                else "none",
            )
            for name in protocol["conditions"]
        },
        timing="write loop (including configured periodic fsync); applied_ns reserved0/unmeasured; final file fsync includes dirty drain; checked close; directory fsync; preparation/verification/exit excluded",
        payload_origin=dict(
            file="guest_writeback_guest.rs",
            sha256=BASE.sha(HERE / "guest_writeback_guest.rs"),
            unchanged_parent_sha256=BASE.sha(HERE / "curves_guest.rs"),
        ),
        driver_origin=dict(
            file="guest_writeback_driver.rs",
            sha256=BASE.sha(HERE / "guest_writeback_driver.rs"),
            unchanged_parent_sha256=BASE.sha(HERE / "curves_driver.rs"),
        ),
        firmware_source=dict(
            file="firmware-source/SOURCE.json",
            sha256=BASE.sha(firmware_dir / "SOURCE.json"),
            firmware_sha256=firmware_record["firmware_sha256"],
            kernel_source_sha256=firmware_record.get("kernel_source_sha256"),
            retained_patch_files=len(patches),
        ),
        old_firmware_source=dict(
            file="firmware-source/OLD-SOURCE.json",
            sha256=BASE.sha(firmware_dir / "OLD-SOURCE.json"),
            firmware_sha256=old_record["firmware_sha256"],
        ),
        firmware_comparison=dict(
            baseline_patch=args.baseline_patch,
            candidate_patch=args.baseline_patch + 1,
            kernel_source_sha256=old_record["kernel_source_sha256"],
            actual_config_sha256=old_record["actual_config_sha256"],
            changed_receipt_inputs={
                key: dict(old=old_record["inputs"].get(key), new=firmware_record["inputs"].get(key))
                for key in sorted(old_record["inputs"].keys() | firmware_record["inputs"].keys())
                if old_record["inputs"].get(key) != firmware_record["inputs"].get(key)
            },
            contrasts=dict(incremental=["old-wb", "guest-wb"], enablement=["off", "guest-wb"]),
            scope=f"same host/payload; old{args.baseline_patch:04} versus new{args.baseline_patch + 1:04} kernel with identical kernel-source/config digests",
        ),
        diagnostic_requirements="exact four startup/final owners; all six phase tokens per cell; actual guest WB flag and additive WRITE size histogram; native counters unavailable",
    )
    protocol["artifacts"]["harness/guest_writeback_curves.py"] = BASE.sha(target)
    protocol["artifacts"]["bin/libkrunfw-old.so"] = BASE.sha(batch / "bin/libkrunfw-old.so")
    for source in firmware_dir.rglob("*"):
        if source.is_file():
            protocol["artifacts"][str(source.relative_to(batch))] = BASE.sha(source)
    protocol["write_counter_contract"] = COUNTERS
    BASE.save(batch / "protocol.json", protocol)
    BASE.save(
        batch / "in.json",
        {
            "guest-writeback-curves": "$ "
            + sys.executable
            + " "
            + str(target)
            + " --measure "
            + str(batch)
        },
    )
    print(batch)


def verify_protocol(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    comparison = protocol["firmware_comparison"]
    baseline = comparison.get("baseline_patch", 47)
    require(
        baseline in (47, 48) and comparison.get("candidate_patch", 48) == baseline + 1,
        "firmware revision comparison differs",
    )
    require(
        protocol["suite"] == SUITE
        and protocol["cells"] == cells()
        and protocol["conditions"] == conditions(batch=batch, baseline_patch=baseline)
        and protocol["baseline_condition"] == "old-wb"
        and protocol["resident_rounds"] == 3
        and protocol["phase_contract"] == PHASE_CONTRACT
        and protocol["write_counter_contract"] == COUNTERS,
        "frozen guest-writeback protocol differs",
    )
    require(
        protocol["samples"] == (1 if protocol["profiling"] or protocol["preflight"] else 5),
        "independent sample count differs",
    )
    BASE.verify_artifacts(batch, protocol)
    return protocol


def measure(batch):
    verify_protocol(batch)
    require(
        not (batch / "records.json").exists() and not (batch / "records.partial.json").exists(),
        "refusing to reuse a measured batch",
    )
    argv = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit=pvisor-guest-writeback-" + uuid.uuid4().hex[:12],
        "-p",
        "CPUQuota=200%",
        "-p",
        "AllowedCPUs=0,1",
        "-p",
        "MemoryMax=2G",
        "-p",
        "MemorySwapMax=0",
        "-p",
        "LimitNOFILE=65536",
        "taskset",
        "-c",
        "0,1",
        sys.executable,
        str(batch / "harness/guest_writeback_curves.py"),
        "--campaign",
        str(batch),
    ]
    BASE.save(batch / "campaign-command.json", argv)
    with (
        (batch / "campaign.stdout.log").open("w") as out,
        (batch / "campaign.stderr.log").open("w") as err,
    ):
        result = subprocess.run(argv, stdout=out, stderr=err, timeout=7200)
    require(result.returncode == 0, "campaign failed; full evidence retained")
    print((batch / "campaign.stdout.log").read_text())


def analyze(batch, plots=True):
    protocol = verify_protocol(batch)
    require(
        not protocol["profiling"] and not protocol["preflight"],
        "only complete formal N5 timing supplies performance analysis",
    )
    for record in json.loads((batch / "records.json").read_text()):
        require(
            record.get("valid") is True and record.get("host_verified") is True,
            "failed/incomplete environment cannot supply timing",
        )
        for row in record["rows"]:
            validate_row(row)
    import curves_analysis

    output = batch / "analysis"
    summary = curves_analysis.analyze(batch, output, plots=plots)
    points, _, failures, _ = curves_analysis.collect(
        protocol, json.loads((batch / "records.json").read_text())
    )
    require(not failures, "formal collection inventory is incomplete")
    _, enablement, _ = curves_analysis.summarize({**protocol, "baseline_condition": "off"}, points)
    enablement = [row for row in enablement if row["condition"] == "guest-wb"]
    curves_analysis.csv_file(output / "enablement_comparisons.csv", enablement)
    summary["enablement_comparisons"] = enablement
    summary["guest_writeback_phase_contract"] = PHASE_CONTRACT
    summary["followup_analyzer_sha256"] = BASE.sha(__file__)
    summary["statistical_scope"]["native_reference"] = (
        "native is an absolute reference; native/off ratios do not isolate guest writeback"
    )
    BASE.save(output / "summary.json", summary)
    print(json.dumps(dict(status=summary["status"], counts=summary["counts"])))


def diagnose(batch):
    protocol = verify_protocol(batch)
    require(
        protocol["profiling"] is True and protocol["samples"] == 1,
        "requires separate N1 diagnostic cohort",
    )
    contract = protocol["write_counter_contract"]
    validate_counter_contract(contract)
    report = DIAG.summarize_batch(batch)
    rows, lifetimes = [], []
    for trial in report["trials"]:
        condition = trial["condition"]
        if condition == "native":
            lifetimes.append(
                dict(condition=condition, counters="unavailable; native has no FUSE connection")
            )
            continue
        finals = {p["component"]: p["measurements"] for p in trial["final_profiles"]}
        actual, batched = actual_writeback(finals, condition, contract)
        lifetimes.append(
            dict(
                condition=condition,
                actual_guest_writeback=actual,
                actual_guest_batched_writeback=batched,
                **write_sizes(finals["virtio-fs-protocol"], contract),
            )
        )
        for phase in trial["phase_rows"]:
            if phase["component"] == "virtio-fs-protocol":
                rows.append(
                    dict(
                        condition=condition,
                        pair=trial["pair"],
                        id=phase["id"],
                        round=phase["round"],
                        **write_sizes(phase["measurements"], contract),
                    )
                )
    output = dict(
        schema=1,
        benchmark_id="B-FS-DIAG",
        performance_samples=False,
        protocol_sha256=BASE.sha(batch / "protocol.json"),
        records_sha256=BASE.sha(batch / "records.json"),
        source_sha256=BASE.sha(__file__),
        phase_contract=PHASE_CONTRACT,
        intervals=rows,
        lifetime=lifetimes,
        full_profile_evidence=report,
        scope="cell through directory fsync, excludes fixture and full-byte verification; N1 counts never timings; no RAM-offload inference",
    )
    BASE.save(batch / "guest-writeback-diagnostics.json", output)
    print(json.dumps(dict(state="verified", phase_rows=len(rows), diagnostic_environments=4)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_mutually_exclusive_group(required=True)
    actions.add_argument("--draft", action="store_true")
    for name in ("prepare", "measure", "campaign", "analyze", "diagnose"):
        actions.add_argument("--" + name, type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--firmware-source", type=Path)
    parser.add_argument("--old-firmware", type=Path)
    parser.add_argument("--old-firmware-source", type=Path)
    parser.add_argument("--baseline-patch", type=int, choices=(47, 48), default=47)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--seed", type=int, default=20261017)
    parser.add_argument("--diagnostic", action="store_true")
    parser.add_argument("--preflight", action="store_true")
    parser.add_argument("--reuse-build", type=Path)
    parser.add_argument("--no-plots", action="store_true")
    args = parser.parse_args()
    args.suite, args.conditions = SUITE, None
    if args.draft:
        print(
            json.dumps(
                dict(
                    suite=SUITE,
                    cells=cells(),
                    conditions=conditions(),
                    phase_contract=PHASE_CONTRACT,
                    samples=5,
                    rounds=3,
                    expected_environments=20,
                    expected_rows=900,
                    diagnostic_environments=4,
                    diagnostic_rows=180,
                    profile_inventory_per_VM=4,
                    diagnostic_tokens_per_VM=90,
                    diagnostic_component_intervals_per_VM=180,
                    diagnostic_total_final_records=12,
                    diagnostic_total_tokens=270,
                    diagnostic_total_component_intervals=540,
                    native_protocol_counters=None,
                    readiness="draft only; product source firmware and WRITE counters not yet frozen",
                ),
                indent=2,
            )
        )
    elif args.prepare:
        require(args.rootfs is not None and args.firmware is not None, "rootfs/firmware required")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    elif args.campaign:
        batch = args.campaign.resolve()
        verify_protocol(batch)
        install_trial_validation()
        BASE.campaign(batch)
    elif args.analyze:
        analyze(args.analyze.resolve(), not args.no_plots)
    else:
        diagnose(args.diagnose.resolve())


if __name__ == "__main__":
    main()
