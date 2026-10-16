#!/usr/bin/env python3
"""Measure explicit host cache treatments in owned-input Jobs.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: determine whether host caches, read-only FLUSH elision or independent
immutable-lower data mappings improve tools while preserving object semantics.
Conclusion sought: paired Ready and per-round tool differences for explicit
capacities, physical-absence, FLUSH, lower-data or negative-entry variants,
with independent diagnostics.
Design: one new host executable and firmware, both inputs sealed/Immutable,
WB=0/DAX=0, five shuffled fresh environments per arm and three resident
rounds. Keep output/cache/Bundle/lower/full-byte checks and all valid slow rows.
No quiet wait, interference monitor or full input/source inventory gate.

``run(config_path, mode)`` runs within the retained private namespace. Timing
and profile modes use separate outputs and separately prepared rootfs inputs;
the existing instrumented rootfs is only used by profile mode. Diagnostic mode
serves B-FS-DIAG, defaults to one fresh environment per capacity, requires all
registered finals and mounted checkpoints, and publishes no performance rows.
"""

import argparse
from collections import Counter
from datetime import datetime, timezone
import json
from pathlib import Path
import random
import re
import shutil
from types import SimpleNamespace

import filesystem_cache_matrix as matrix


CAPACITY_ENV = "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY"
CAPACITY_COUNTER = "directory_cache_capacity"
NEGATIVE_ENV = "PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE"
NEGATIVE_COUNTER = "immutable_lower_negative_cache_enabled"
UPPER_ENV = "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE"
UPPER_COUNTER = "exclusive_upper_absence_cache_enabled"
FLUSH_ENV = "PVISOR_VM_FS_READONLY_FLUSH_ELISION"
FLUSH_COUNTER = "readonly_flush_elision_enabled"
DATA_ENV = "PVISOR_VM_FS_LOWER_DATA_CACHE"
DATA_COUNTER = "lower_data_cache_enabled"
FACTORIAL_DESIGN = "lower-data-local-flush-factorial"
NEGATIVE_ENTRY_ENV = "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE"
NEGATIVE_ENTRY_COUNTER = "negative_entry_cache_enabled"
NEGATIVE_ENTRY_REPLIES = "negative_entry_cache_replies"
NEGATIVE_ENTRY_DESIGN = "negative-entry-with-data-flush"
SHARED_OPEN_ENV = "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION"
SHARED_OPEN_COUNTER = "shared_open_qualification_enabled"
SHARED_OPEN_DESIGN = "shared-open-qualification"
QUALIFICATION_QUERIES = tuple("open_qualification_" + kind + "_attempts"
    for kind in ("fgetfl", "fstat", "statx", "fstatfs"))
QUALIFICATION_COUNTERS = QUALIFICATION_QUERIES + tuple("open_qualification_" + name
    for name in ("shared_attempts", "legacy_flush_attempts", "legacy_data_attempts",
                 "receipt_hits", "receipt_misses"))
# Owned SDK entry: compact initialization, sealed reopen, native mount owners,
# and preparation/seal/Bundle readers. README records the constructor ledger.
OWNED_SDK_PROFILE_COMPONENTS = {"overlay-core": 4, "preimage-log": 3,
    "preimage-log-read": 4, "virtio-fs-overlay": 2,
    "virtio-fs-protocol": 2, "virtio-fs-dispatch": 2}
MODES = ("timing", "profiles")


def configured_variants(config):
    """Validate optional named variants; fixed guest controls stay outside them."""
    factorial = config.get("treatment_design") == FACTORIAL_DESIGN
    negative_entry = config.get("treatment_design") == NEGATIVE_ENTRY_DESIGN
    shared_open = config.get("treatment_design") == SHARED_OPEN_DESIGN
    if "treatment_design" in config and not (factorial or negative_entry or shared_open):
        raise ValueError("unknown explicit treatment_design")
    if "variants" not in config:
        if factorial or negative_entry or shared_open:
            raise ValueError("explicit treatment design requires named variants")
        return None
    variants = config["variants"]
    if not isinstance(variants, list) or len(variants) < 2:
        raise ValueError("variants must contain at least two explicit arms")
    if factorial and len(variants) != 4:
        raise ValueError("factorial design requires exactly four variants")
    if negative_entry and len(variants) != 2:
        raise ValueError("negative-entry design requires exactly two variants")
    if shared_open and len(variants) != 2:
        raise ValueError("shared-OPEN design requires exactly two variants")
    names, combinations, result = set(), set(), []
    for variant in variants:
        if not isinstance(variant, dict) or set(variant) != {"name", "capacity", "env"}:
            raise ValueError("each variant requires only name, capacity and env")
        name, capacity, environment = variant["name"], variant["capacity"], variant["env"]
        if not isinstance(name, str) or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", name) is None or name in names:
            raise ValueError("variant names must be unique path-safe identifiers of 1..64 characters")
        if type(capacity) is not int or not 1 <= capacity <= 4096:
            raise ValueError("variant capacity must be an integer in 1..4096")
        if factorial:
            if capacity != 256:
                raise ValueError("factorial design fixes every directory capacity at 256")
            if (not isinstance(environment, dict) or set(environment) != {DATA_ENV, FLUSH_ENV}
                    or any(value not in ("0", "1") for value in environment.values())):
                raise ValueError("factorial env requires only lower-data and local-FLUSH switches as 0 or 1")
            combination = (environment[DATA_ENV], environment[FLUSH_ENV])
            if combination in combinations:
                raise ValueError("factorial variants must contain each 0/1 combination exactly once")
            combinations.add(combination)
        elif negative_entry or shared_open:
            if capacity != 256:
                raise ValueError("negative-entry design fixes every directory capacity at 256")
            switch = SHARED_OPEN_ENV if shared_open else NEGATIVE_ENTRY_ENV
            if (not isinstance(environment, dict) or set(environment) != {switch}
                    or environment[switch] not in ("0", "1")):
                raise ValueError("negative-entry env requires only its explicit 0/1 switch")
            setting = environment[switch]
            if setting in combinations:
                raise ValueError("negative-entry variants must cover disabled and enabled once each")
            combinations.add(setting)
        else:
            if (not isinstance(environment, dict)
                    or set(environment) not in ({NEGATIVE_ENV}, {UPPER_ENV}, {FLUSH_ENV}, {DATA_ENV})
                    or next(iter(environment.values())) not in ("0", "1")):
                raise ValueError("variant env must select exactly one allowed cache switch as 0 or 1")
            if result and set(environment) != set(result[0]["env"]):
                raise ValueError("all variants must select the same single treatment")
        names.add(name)
        result.append({"name": name, "capacity": capacity, "env": dict(environment)})
    return result


def capacity_conditions(config):
    """Require explicit, unique, bounded integer capacities for every arm."""
    variants = configured_variants(config)
    if variants is not None:
        return [(variant["name"], variant["capacity"]) for variant in variants]
    capacities = config["capacities"]
    if (not isinstance(capacities, list) or len(capacities) < 2
            or any(type(value) is not int or not 1 <= value <= 4096 for value in capacities)
            or len(set(capacities)) != len(capacities)):
        raise ValueError("capacities must contain at least two unique integers in 1..4096")
    return [(f"capacity-{capacity}", capacity) for capacity in capacities]


def capacity_environment(config, capacity, mode, condition=None):
    """Pin the same ownership and guest controls while selecting capacity."""
    if mode not in MODES:
        raise ValueError("mode must be timing or profiles")
    environment = {
        **matrix.CONTROL_ENVIRONMENT,
        "PVISOR_FS_OWNED_INPUTS_MANIFEST": str(config["owned_inputs_manifest"]),
        "PVISOR_FS_OWNED_INPUTS_CONDITION": "both",
        CAPACITY_ENV: str(capacity),
        "PVISOR_FS_PROFILE": "1" if mode == "profiles" else "0",
        NEGATIVE_ENV: "0",
        UPPER_ENV: "0",
        FLUSH_ENV: "0",
        DATA_ENV: "0",
        NEGATIVE_ENTRY_ENV: "0",
        SHARED_OPEN_ENV: "0",
    }
    if config.get("treatment_design") in (NEGATIVE_ENTRY_DESIGN, SHARED_OPEN_DESIGN):
        environment.update({DATA_ENV: "1", FLUSH_ENV: "1"})
    variants = configured_variants(config)
    if variants is not None:
        selected = next((variant for variant in variants if variant["name"] == condition), None)
        if selected is None or selected["capacity"] != capacity:
            raise ValueError("named variant and requested capacity do not match")
        environment.update(selected["env"])
    if config.get("treatment_design") == FACTORIAL_DESIGN:
        _validate_factorial_environment(environment, mode)
    elif config.get("treatment_design") == NEGATIVE_ENTRY_DESIGN:
        _validate_negative_entry_environment(environment, mode)
    elif config.get("treatment_design") == SHARED_OPEN_DESIGN:
        environment["RUST_LOG"] = "off"
        _validate_shared_open_environment(environment, mode)
    return environment


def _validate_owned_cache_environment(environment, mode):
    """Reject missing or changed shared controls before launching an arm."""
    expected = {
        **matrix.CONTROL_ENVIRONMENT,
        "PVISOR_FS_PROFILE": "1" if mode == "profiles" else "0",
        "PVISOR_FS_OWNED_INPUTS_CONDITION": "both",
        CAPACITY_ENV: "256", NEGATIVE_ENV: "0", UPPER_ENV: "0",
    }
    if any(environment.get(key) != value for key, value in expected.items()):
        raise ValueError("owned-cache environment has missing or changed fixed controls")


def _validate_factorial_environment(environment, mode):
    """Keep negative-entry caching off while varying the two original factors."""
    _validate_owned_cache_environment(environment, mode)
    if environment.get(NEGATIVE_ENTRY_ENV) != "0":
        raise ValueError("factorial design requires negative-entry caching disabled")
    if any(environment.get(key) not in ("0", "1") for key in (DATA_ENV, FLUSH_ENV)):
        raise ValueError("factorial environment needs both explicit 0/1 factor settings")


def _validate_negative_entry_environment(environment, mode):
    """Vary only negative entries over the fixed combined data/FLUSH path."""
    _validate_owned_cache_environment(environment, mode)
    if any(environment.get(key) != "1" for key in (DATA_ENV, FLUSH_ENV)):
        raise ValueError("negative-entry design requires both data and FLUSH enabled")
    if environment.get(NEGATIVE_ENTRY_ENV) not in ("0", "1"):
        raise ValueError("negative-entry design needs an explicit 0/1 setting")


def _validate_profile_setting(profiles, component, counter, expected, field):
    """Match each owner's integer; expected=None accepts any nonnegative count."""
    from phase_profiles import SCOPES

    final = {(profile["pid"], profile["component"], profile["instance"]): profile
             for profile in profiles["final_profiles"]}
    scoped = {}
    for row in profiles["rows"]:
        if row["component"] != component:
            continue
        identity = (row["pid"], row["component"], row["instance"])
        if row["scope"] in scoped and scoped[row["scope"]] != identity:
            raise ValueError("mounted cache owner changed during diagnostics")
        scoped[row["scope"]] = identity
    if set(scoped) != set(SCOPES):
        raise ValueError(f"missing mounted {counter} evidence")
    evidence = {}
    for scope, identity in scoped.items():
        profile = final.get(identity)
        if profile is None or profile.get("final_record") is not True:
            raise ValueError("cache owner has no final profile")
        units = profile["measurements"].get(counter, {}).get("units")
        if type(units) is not int or units < 0 or (expected is not None and units != expected):
            raise ValueError(f"{scope} actual {counter} {units!r} differs from {expected}")
        evidence[scope] = {"pid": identity[0], "instance": identity[2], field: units}
    return evidence


def _validate_shared_open_environment(environment, mode):
    """Keep DATA/FLUSH enabled while only same-OPEN evidence reuse varies."""
    _validate_owned_cache_environment(environment, mode)
    if any(environment.get(key) != "1" for key in (DATA_ENV, FLUSH_ENV)):
        raise ValueError("shared-OPEN requires DATA/FLUSH enabled")
    if environment.get(NEGATIVE_ENTRY_ENV) != "0" or environment.get(SHARED_OPEN_ENV) not in ("0", "1"):
        raise ValueError("shared-OPEN requires negative entries off and explicit reuse 0/1")
    if environment.get("RUST_LOG") != "off":
        raise ValueError("shared-OPEN requires Rust debug logging off")


def validate_profile_shared_open_settings(profiles, environment):
    """Require every mounted actual setting and query label, including zero."""
    _validate_shared_open_environment(environment, "profiles")
    evidence = {
        "capacity_verification": validate_profile_capacity(profiles, 256),
        "negative_cache_verification": validate_profile_negative_cache(profiles, False),
        "upper_absence_cache_verification": validate_profile_upper_absence_cache(profiles, False),
        "readonly_flush_elision_verification": validate_profile_readonly_flush_elision(profiles, True),
        "lower_data_cache_verification": validate_profile_lower_data_cache(profiles, True),
        "negative_entry_cache_verification": validate_profile_negative_entry_cache(profiles, False),
        "shared_open_qualification_verification": _validate_profile_setting(profiles,
            "virtio-fs-overlay", SHARED_OPEN_COUNTER, int(environment[SHARED_OPEN_ENV]), "enabled"),
        "qualification_attempts_verification": {},
    }
    for label in QUALIFICATION_COUNTERS:
        evidence["qualification_attempts_verification"][label] = _validate_profile_setting(
            profiles, "virtio-fs-overlay", label, None, "attempts")
    # Initialization emits explicit zero labels, so checkpoint records must
    # retain them too. A missing event field cannot masquerade as zero attempts.
    for phase in profiles["rows"]:
        if phase["component"] != "virtio-fs-overlay" or "raw_measurements" not in phase:
            continue
        for label in QUALIFICATION_COUNTERS:
            value = phase["raw_measurements"].get(label, {}).get("units")
            if type(value) is not int or value < 0:
                raise ValueError(f"missing/noninteger qualification phase label {phase['scope']}/{label}")
    return evidence


def validate_shared_open_inventory(profiles, lines):
    """Check the source-derived owned SDK inventory, not only emitted owners."""
    registered, finals = [], []
    for line in lines:
        if line.startswith("pvisor-fs-profile-start "):
            profile = json.loads(line.removeprefix("pvisor-fs-profile-start "))
            registered.append((profile["pid"], profile["component"], profile["instance"]))
        elif line.startswith("pvisor-fs-profile "):
            profile = json.loads(line.removeprefix("pvisor-fs-profile "))
            if profile.get("final_record") is True:
                finals.append((profile["pid"], profile["component"], profile["instance"]))
    counts = Counter(registered)
    if (not counts or counts != Counter(finals) or any(value != 1 for value in counts.values())
            or Counter(key[1] for key in registered) != OWNED_SDK_PROFILE_COMPONENTS):
        raise ValueError("owned SDK expected 17-owner inventory or unique registration/final coverage differs")
    saved = {(p["pid"], p["component"], p["instance"]) for p in profiles["final_profiles"]}
    if saved != set(registered):
        raise ValueError("retained final profiles differ from source-derived inventory")
    return dict(expected_component_histogram=OWNED_SDK_PROFILE_COMPONENTS,
        registered_instances=len(registered), final_instances=len(finals), unique_final_per_registered=True,
        basis="owned SDK compact initialization + sealed runtime reopen + native devices + seal/Bundle readers")


def validate_profile_capacity(profiles, capacity):
    """Match each mounted overlay's actual setting without summing instances."""
    return _validate_profile_setting(profiles, "virtio-fs-overlay", CAPACITY_COUNTER, capacity, "capacity")


def validate_profile_negative_cache(profiles, enabled):
    """Require an explicit enabled/disabled counter from both mounted Cores."""
    if type(enabled) is not bool:
        raise ValueError("negative-cache setting must be boolean")
    return _validate_profile_setting(profiles, "overlay-core", NEGATIVE_COUNTER, int(enabled), "enabled")


def validate_profile_upper_absence_cache(profiles, enabled):
    """Require the actual upper-absence-cache switch on each mounted overlay."""
    if type(enabled) is not bool:
        raise ValueError("upper-absence-cache setting must be boolean")
    return _validate_profile_setting(profiles, "overlay-core", UPPER_COUNTER, int(enabled), "enabled")


def validate_profile_readonly_flush_elision(profiles, enabled):
    """Require an explicit actual setting on each mounted overlay owner."""
    if type(enabled) is not bool:
        raise ValueError("read-only flush-elision setting must be boolean")
    return _validate_profile_setting(profiles, "virtio-fs-overlay", FLUSH_COUNTER, int(enabled), "enabled")


def validate_profile_lower_data_cache(profiles, enabled):
    """Require negotiated data-object settings rather than an environment echo."""
    if type(enabled) is not bool:
        raise ValueError("lower-data-cache setting must be boolean")
    return _validate_profile_setting(profiles, "virtio-fs-overlay", DATA_COUNTER, int(enabled), "enabled")


def validate_profile_factorial_settings(profiles, environment):
    """Require both negotiated factors and fixed host settings for each mount."""
    _validate_factorial_environment(environment, "profiles")
    return {
        "capacity_verification": validate_profile_capacity(profiles, 256),
        "negative_cache_verification": validate_profile_negative_cache(profiles, False),
        "upper_absence_cache_verification": validate_profile_upper_absence_cache(profiles, False),
        "readonly_flush_elision_verification": validate_profile_readonly_flush_elision(profiles, environment[FLUSH_ENV] == "1"),
        "lower_data_cache_verification": validate_profile_lower_data_cache(profiles, environment[DATA_ENV] == "1"),
    }


def validate_profile_negative_entry_cache(profiles, enabled):
    """Require actual qualification plus an explicit nonnegative reply counter."""
    if type(enabled) is not bool:
        raise ValueError("negative-entry setting must be boolean")
    settings = _validate_profile_setting(profiles, "virtio-fs-overlay", NEGATIVE_ENTRY_COUNTER, int(enabled), "enabled")
    replies = _validate_profile_setting(profiles, "virtio-fs-overlay", NEGATIVE_ENTRY_REPLIES, None if enabled else 0, "replies")
    return {scope: dict(actual, replies=replies[scope]["replies"]) for scope, actual in settings.items()}


def validate_profile_negative_entry_settings(profiles, environment):
    """Verify the selected negative-entry arm and every fixed mounted setting."""
    _validate_negative_entry_environment(environment, "profiles")
    return {
        "capacity_verification": validate_profile_capacity(profiles, 256),
        "negative_cache_verification": validate_profile_negative_cache(profiles, False),
        "upper_absence_cache_verification": validate_profile_upper_absence_cache(profiles, False),
        "readonly_flush_elision_verification": validate_profile_readonly_flush_elision(profiles, True),
        "lower_data_cache_verification": validate_profile_lower_data_cache(profiles, True),
        "negative_entry_cache_verification": validate_profile_negative_entry_cache(profiles, environment[NEGATIVE_ENTRY_ENV] == "1"),
    }


def artifact_identities(arguments, config, reference, rounds, mode):
    artifacts = {
        "binary": matrix.sha(arguments["binary"]),
        "firmware": matrix.sha(config["firmware_object"]),
        "adapter": matrix.sha(config["trial_adapter"]),
        "runner": matrix.sha(__file__),
        "matrix_helpers": matrix.sha(matrix.__file__),
        "reference_runner": matrix.sha(reference.__file__),
        "round_validator": matrix.sha(rounds.__file__),
    }
    if mode == "profiles":
        import phase_profiles
        artifacts["phase_profiles"] = matrix.sha(phase_profiles.__file__)
    return artifacts


def run(config_path, mode=None):
    """Collect one new timing or diagnostic cohort in the existing containment."""
    import reference_baselines as reference
    import filesystem_rounds as rounds

    config_path = Path(config_path)
    config = json.loads(config_path.read_text())
    mode = config.get("mode", "timing") if mode is None else mode
    if mode not in MODES:
        raise ValueError("mode must be timing or profiles")
    conditions = capacity_conditions(config)
    variants = configured_variants(config)
    factorial = config.get("treatment_design") == FACTORIAL_DESIGN
    negative_entry = config.get("treatment_design") == NEGATIVE_ENTRY_DESIGN
    shared_open = config.get("treatment_design") == SHARED_OPEN_DESIGN
    experiment_label = "SHARED OPEN" if shared_open else "NEGATIVE ENTRY" if negative_entry else "DATA/FLUSH FACTORIAL" if factorial else "CACHE VARIANTS" if variants is not None else "CACHE CAPACITY"
    arguments = matrix.load_arguments(config)
    arguments.update(seed=int(config.get("seed", 20261017)),
                     diagnostic_timing=mode == "profiles",
                     diagnostic_stderr_file=mode == "profiles")
    if mode == "profiles":
        arguments["samples"] = int(config.get("diagnostic_samples", 1))
    if arguments["samples"] < 1:
        raise ValueError("samples must be positive")
    output_key = "profiles" if mode == "profiles" else "formal"
    canonical_output = Path(config["outputs"][output_key])
    output = Path("/tmp/private-out") / canonical_output.relative_to(Path(config["raw_root"]))
    output.mkdir(parents=True, exist_ok=False)
    adapter = matrix.load_adapter(config["trial_adapter"])
    artifacts = artifact_identities(arguments, config, reference, rounds, mode)
    benchmark_id = "B-FS-DIAG" if mode == "profiles" else "B-FS-ENG"
    report = {
        "schema": 1, "benchmark_id": benchmark_id,
        "role": "diagnostic" if mode == "profiles" else "engineering A/B",
        "status": "running", "recorded_at": datetime.now(timezone.utc).isoformat(),
        "configuration": config, "configuration_sha256": matrix.sha(config_path),
        "artifacts": artifacts, "mode": mode,
        "arguments": {key: str(value) for key, value in arguments.items()},
        "assets": json.loads((arguments["assets"] / "assets.json").read_text()),
        "protocol": {
            "capacities": list(dict.fromkeys(capacity for _, capacity in conditions)),
            "variants": variants, "capacity_environment": CAPACITY_ENV,
            "treatment_design": config.get("treatment_design"),
            "factorial_factors": {DATA_ENV: ["0", "1"], FLUSH_ENV: ["0", "1"]} if factorial else None,
            "treatment": "same-OPEN qualification reuse only; data/FLUSH fixed enabled" if shared_open else "standard negative-entry responses only; data and local-FLUSH fixed enabled" if negative_entry else "2x2 lower-data and local-FLUSH factors, including their interaction" if factorial else "explicit capacity, physical-absence, read-only FLUSH or lower-data variants" if variants is not None else "directory-reference capacity only",
            "shared_open_environment": SHARED_OPEN_ENV if shared_open else None,
            "shared_open_scope": "qualification query attempts per actual OPEN, not full syscall totals; open spans have different boundaries" if shared_open else None,
            "expected_profile_components": OWNED_SDK_PROFILE_COMPONENTS if shared_open else None,
            "negative_entry_environment": NEGATIVE_ENTRY_ENV,
            "negative_entry_ttl_seconds": 1 if negative_entry else 0,
            "input_condition": "both", "rootfs_immutable": True,
            "workspace_immutable": True, "workspace_frozen_baseline": True,
            "samples_per_condition": arguments["samples"], "resident_rounds": 3,
            "warmups": 0, "seed": arguments["seed"], "writeback": 0,
            "dax": False, "guest_cache": 1, "profiling": mode == "profiles",
            "performance_samples": mode == "timing",
            "startup_metric": "launch to REFERENCE_READY; excludes tools and exit",
            "operation_metric": "worker_ms by resident round; excludes launch and exit",
            "diagnostic_boundary": "checked() process-tree startup, work and validation; not worker_ms",
            "independent_unit": "fresh environment; three resident rounds are dependent",
            "interference_gate": False, "source_inventory_gate": False,
            "retain_all_valid_slow_samples": True,
            "correctness": "original output/cache/Bundle/lower isolation, complete written and stream bytes, normal exit, and actual SnapshotBase ownership",
            "capacity_verification": "explicit parsed setting with one shared executable; diagnostic finals independently verify actual capacity for each mounted overlay instance",
            "variant_verification": "each mounted overlay must publish actual negative-entry qualification 0/1 and an explicit reply count (zero allowed), data/FLUSH=1, capacity=256; mounted Core absence flags=0" if negative_entry else "each mounted overlay must publish both factors as explicit units=0/1; mounted Cores must publish both absence caches as units=0 and overlays capacity=256" if factorial else "each mounted owner must publish the selected cache's explicit setting as units=0/1" if variants is not None else None,
            "snapshot_preparation": "reuse the cohort's previously imported/sealed inputs; preparation excluded from Ready and worker clocks",
            "driver_scope": "normal managed Job/runtime without CLI Host service",
        },
        "order": [], "rows": [], "failed_rows": [], "failures": [],
    }
    for condition, _ in conditions:
        folder = output / "conditions" / condition / "bin"
        folder.mkdir(parents=True)
        shutil.copy2(arguments["binary"], folder / "pvisor")
        if matrix.sha(folder / "pvisor") != artifacts["binary"]:
            raise ValueError("condition executable differs from the shared artifact")
    matrix.save(output / "report.json", report)
    rng = random.Random(arguments["seed"])
    manifest_sha256 = None
    for trial in range(arguments["samples"]):
        order = conditions.copy()
        rng.shuffle(order)
        report["order"].append({"trial": trial, "conditions": [name for name, _ in order]})
        for condition, capacity in order:
            args = SimpleNamespace(**arguments)
            args.output = output / "conditions" / condition
            row, prepared = None, None
            try:
                settings = capacity_environment(config, capacity, mode, condition)
                args.cache_matrix_environment = settings.copy()
                prepared = adapter.prepare_trial(args, "both", trial)
                if not isinstance(prepared, dict):
                    raise ValueError("adapter preparation must return a retained dictionary")
                if manifest_sha256 is None:
                    manifest_sha256 = prepared["manifest_sha256"]
                    report["owned_inputs_manifest_sha256"] = manifest_sha256
                elif prepared["manifest_sha256"] != manifest_sha256:
                    raise ValueError("capacity conditions do not share the same sealed input manifest")
                if args.binary != arguments["binary"] or args.firmware != arguments["firmware"]:
                    raise ValueError("adapter changed the shared executable or firmware")
                extra = prepared.get("environment", {})
                if any(key in settings and settings[key] != value for key, value in extra.items()):
                    raise ValueError("adapter changed a fixed experimental control")
                settings.update(extra)
                if factorial:
                    _validate_factorial_environment(settings, mode)
                elif negative_entry:
                    _validate_negative_entry_environment(settings, mode)
                elif shared_open:
                    _validate_shared_open_environment(settings, mode)
                with matrix.trial_environment(settings):
                    row = reference.run_trial(args, report, "pvisor-vm", "filesystem", trial)
                row.update(condition=condition, directory_cache_capacity=capacity,
                           input_condition="both", writeback=0, benchmark_id=benchmark_id,
                           prepared=prepared)
                if variants is not None:
                    row["negative_cache_enabled"] = settings[NEGATIVE_ENV] == "1"
                    row["exclusive_upper_absence_cache_enabled"] = settings[UPPER_ENV] == "1"
                    row["readonly_flush_elision_enabled"] = settings[FLUSH_ENV] == "1"
                    row["lower_data_cache_enabled"] = settings[DATA_ENV] == "1"
                    row["negative_entry_cache_enabled"] = settings[NEGATIVE_ENTRY_ENV] == "1"
                    if shared_open:
                        row["shared_open_qualification_enabled"] = settings[SHARED_OPEN_ENV] == "1"
                rounds.validate_rounds(row["result"])
                rounds.validate_stream_files(row, "pvisor-vm")
                if config.get("python_version"):
                    versions = {value["python"] for operation in row["result"]["filesystem"].values()
                                for value in operation["rounds"]}
                    if versions != {config["python_version"]}:
                        raise ValueError("workloads did not use the frozen common Python")
                qualification = adapter.validate_trial(args, row, "both", trial, prepared)
                matrix.validate_qualification(qualification, "both")
                row["qualification"] = qualification
                if mode == "profiles":
                    from phase_profiles import collect_phase_profiles
                    lines = (Path(row["logs"]) / "stderr.log").read_text().splitlines()
                    profiles = collect_phase_profiles(lines)
                    if shared_open:
                        row.update(validate_profile_shared_open_settings(profiles, settings))
                        row["profile_inventory"] = validate_shared_open_inventory(profiles, lines)
                    elif negative_entry:
                        row.update(validate_profile_negative_entry_settings(profiles, settings))
                    elif factorial:
                        row.update(validate_profile_factorial_settings(profiles, settings))
                    else:
                        row["capacity_verification"] = validate_profile_capacity(profiles, capacity)
                    if variants is not None and not (factorial or negative_entry or shared_open):
                        row["negative_cache_verification"] = validate_profile_negative_cache(profiles, row["negative_cache_enabled"])
                        if set(variants[0]["env"]) & {UPPER_ENV, FLUSH_ENV, DATA_ENV}:
                            row["upper_absence_cache_verification"] = validate_profile_upper_absence_cache(profiles, row["exclusive_upper_absence_cache_enabled"])
                        if set(variants[0]["env"]) & {FLUSH_ENV, DATA_ENV}:
                            row["readonly_flush_elision_verification"] = validate_profile_readonly_flush_elision(profiles, row["readonly_flush_elision_enabled"])
                        if DATA_ENV in variants[0]["env"]:
                            row["lower_data_cache_verification"] = validate_profile_lower_data_cache(profiles, row["lower_data_cache_enabled"])
                    row["phase_profiles"] = profiles
                report["rows"].append(row)
            except Exception as error:
                report["failures"].append({"condition": condition, "trial": trial,
                    "directory_cache_capacity": capacity, "reason": str(error), "prepared": prepared,
                    "logs": str(args.output / "trials" / f"filesystem-pvisor-vm-{trial:03d}")})
                if row is not None:
                    report["failed_rows"].append(row)
            matrix.save(output / "report.json", report)
        print(f"{experiment_label} {mode} {trial + 1}/{arguments['samples']}; valid={len(report['rows'])}; failed={len(report['failures'])}", flush=True)
    report["artifacts_after"] = artifact_identities(arguments, config, reference, rounds, mode)
    if report["artifacts_after"] != artifacts:
        report["failures"].append({"reason": "shared executable, firmware or harness changed during sampling"})
    report["status"] = "failed" if report["failures"] else "complete"
    report["finished_at"] = datetime.now(timezone.utc).isoformat()
    matrix.save(output / "report.json", report)
    print(f"{experiment_label} {mode} {report['status']}: {canonical_output}", flush=True)
    if report["failures"]:
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--mode", choices=MODES)
    options = parser.parse_args()
    run(options.config, options.mode)
