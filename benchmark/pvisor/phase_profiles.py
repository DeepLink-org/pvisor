#!/usr/bin/env python3
"""Synchronize filesystem request counts with complete resident tool calls.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: determine which mounted filesystem stops issuing host requests when
the managed Job path uses sealed rootfs or snapshot-workspace inputs.
Conclusion sought: per-round/per-tool protocol and physical Core count deltas.
Design: a separate four-condition campaign, WB=0/DAX=0, one fresh environment
per condition by default and three resident rounds. Preopened directory fds
issue a synchronous, eight-byte-token diagnostic ioctl before/after checked().
Intervals include subprocess startup and the original post-operation validation;
they are not the narrower worker_ms boundary. No diagnostic elapsed time is a
performance sample. All registrations must have final records, including Core
instances created outside mounted devices. Inclusive span times are not added.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import random
import shutil
import struct
from types import SimpleNamespace


PROFILE_PHASE_IOCTL = 0x400876F1  # Linux _IOW('v', 0xf1, u64)
PHASE_PREFIX = "pvisor-fs-profile-phase "
SCOPES = ("rootfs", "pvisor-workspace")
COMPONENTS = frozenset(("virtio-fs-protocol", "virtio-fs-dispatch",
                        "virtio-fs-overlay", "overlay-core"))
OPERATIONS = ("metadata", "read", "write", "git", "rg", "cargo", "npm",
              "hot_open", "stream_write")


@contextmanager
def open_mounts(workspace=None):
    """Open diagnostic descriptors before any measured tool interval."""
    descriptors = []
    try:
        for path in ("/", Path.cwd() if workspace is None else workspace):
            descriptors.append(os.open(path, os.O_RDONLY | os.O_DIRECTORY))
        yield descriptors
    finally:
        for descriptor in descriptors:
            os.close(descriptor)


def checkpoint(descriptors, token):
    """Wait for both mount owners to emit the exact cumulative token."""
    if type(token) is not int or not 0 < token < 2**64:
        raise ValueError("phase token must be a nonzero u64")
    payload = struct.pack("<Q", token)
    for descriptor in descriptors:
        fcntl.ioctl(descriptor, PROFILE_PHASE_IOCTL, payload)


def instrument_rootfs(rootfs):
    """Modify only an independent diagnostic rootfs, before it is sealed.

    The caller owns the copy. Refuse already instrumented/unexpected source;
    never call this on the timing rootfs or after immutable preparation.
    """
    rootfs = Path(rootfs)
    target = rootfs / "bench/filesystem_rounds_workload.py"
    source = target.read_text()
    anchor = "    extra = Path(__file__).resolve()\n"
    before = "            output = checked([str(root / \"usr/bin/python3\"), str(script), operation],\n"
    after = "                             cwd=\"_fs\", env=tools_env())\n"
    if "phase_descriptors" in source or any(source.count(part) != 1 for part in (anchor, before, after)):
        raise ValueError("unexpected resident workload; cannot instrument safely")
    # Keep exactly the same checked() command, environment and validations.
    source = source.replace(anchor, anchor +
        "    from phase_profiles import open_mounts, checkpoint\n"
        "    phase_mounts = open_mounts()\n"
        "    phase_descriptors = phase_mounts.__enter__()\n"
        "    phase_token = 0\n")
    source = source.replace(before,
        "            phase_token += 1\n"
        "            checkpoint(phase_descriptors, phase_token)\n" + before)
    source = source.replace(after, after +
        "            phase_token += 1\n"
        "            checkpoint(phase_descriptors, phase_token)\n")
    return_anchor = "    return {operation: dict(values[0], rounds=values)\n"
    if source.count(return_anchor) != 1:
        raise ValueError("unexpected resident workload return")
    source = source.replace(return_anchor,
        "    phase_mounts.__exit__(None, None, None)\n" + return_anchor)
    target.write_text(source)
    shutil.copy2(__file__, target.with_name("phase_profiles.py"))


def identity(profile):
    return profile["pid"], profile["component"], profile["instance"]


def cumulative_delta(before, after):
    """Subtract additive counts; cumulative maxima have no interval meaning."""
    result = {}
    for label in before.keys() | after.keys():
        old, new = before.get(label, {}), after.get(label, {})
        values = {}
        for field in ("calls", "units", "total_ns"):
            a, b = old.get(field, 0), new.get(field, 0)
            if type(a) is not int or type(b) is not int or not 0 <= a <= b:
                raise ValueError(f"nonmonotonic cumulative phase measurement {label}/{field}")
            values[field] = b - a
        result[label] = values
    return result


def interval_counts(component, raw):
    """Keep raw deltas separately; remove exactly one marker's own counters.

    Each interval contains one completed protocol IOCTL, one admitted dispatch
    request and 40-byte InHeader + 32-byte IoctlIn + 8-byte token. Span durations
    cannot be corrected exactly, and are deliberately absent from this view.
    """
    filtered = {label: {field: value[field] for field in ("calls", "units")}
                for label, value in raw.items()}
    corrections = ({"IOCTL": ("calls", 1), "request_bytes": ("units", 80)}
                   if component == "virtio-fs-protocol" else
                   {"direct_requests": ("units", 1)}
                   if component == "virtio-fs-dispatch" else {})
    for label, (field, amount) in corrections.items():
        if filtered.get(label, {}).get(field, 0) < amount:
            raise ValueError(f"missing diagnostic marker accounting: {component}/{label}")
        filtered[label][field] -= amount
    return filtered


def collect_phase_profiles(lines, operations=OPERATIONS, rounds=3):
    """Require every expected token, scope, component and registered final."""
    from filesystem_diagnostic import collect_profiles

    lines = list(lines)
    final = collect_profiles(lines, require_complete=True, expected_components=COMPONENTS)
    by_identity = {identity(profile): profile for profile in final}
    phases = {scope: {} for scope in SCOPES}
    scoped = {}
    for line in lines:
        if not line.startswith(PHASE_PREFIX):
            continue
        phase = json.loads(line.removeprefix(PHASE_PREFIX))
        scope, token = phase.get("scope"), phase.get("token")
        if phase.get("schema") != 1 or scope not in phases:
            raise ValueError(f"unexpected phase schema or scope {scope!r}")
        if type(token) is not int or token in phases[scope]:
            raise ValueError(f"invalid or duplicate phase token {scope}/{token}")
        profiles = phase.get("profiles", [])
        components = [profile["component"] for profile in profiles]
        if len(components) != len(COMPONENTS) or set(components) != COMPONENTS:
            raise ValueError(f"incomplete mounted component coverage {scope}/{token}")
        ids = {identity(profile) for profile in profiles}
        if not ids <= by_identity.keys():
            raise ValueError("checkpoint has an unregistered or unfinished instance")
        if scope in scoped and scoped[scope] != ids:
            raise ValueError("mounted profile identities changed between phases")
        if any(ids & other for name, other in scoped.items() if name != scope):
            raise ValueError("profile instance belongs to multiple mounted scopes")
        scoped[scope] = ids
        phases[scope][token] = {profile["component"]: profile for profile in profiles}
    expected = set(range(1, 2 * len(operations) * rounds + 1))
    rows = []
    for scope in SCOPES:
        if set(phases[scope]) != expected:
            raise ValueError(f"incomplete phase token coverage {scope}: {sorted(expected - phases[scope].keys())}")
        previous = {}
        for token in sorted(expected):
            for component, profile in phases[scope][token].items():
                if component in previous:
                    cumulative_delta(previous[component]["measurements"], profile["measurements"])
                previous[component] = profile
        for profile in previous.values():
            cumulative_delta(profile["measurements"], by_identity[identity(profile)]["measurements"])
        for number in range(rounds):
            for index, operation in enumerate(operations):
                token = (number * len(operations) + index) * 2 + 1
                for component in sorted(COMPONENTS):
                    before = phases[scope][token][component]
                    after = phases[scope][token + 1][component]
                    raw = cumulative_delta(before["measurements"], after["measurements"])
                    rows.append(dict(scope=scope, round=number + 1, operation=operation,
                                     component=component, pid=after["pid"], instance=after["instance"],
                                     before_token=token, after_token=token + 1,
                                     raw_measurements=raw,
                                     measurements=interval_counts(component, raw)))
    assigned = set().union(*scoped.values())
    return dict(rows=rows, final_profiles=final,
                unscoped_final_profiles=[profile for profile in final if identity(profile) not in assigned],
                coverage="complete registered/final instances and exact mounted phase tokens",
                interval="checked() subprocess startup, work and original validation; not worker_ms",
                marker_correction="one IOCTL, 80 request bytes and one dispatch direct request per interval; raw additive deltas retained",
                timing="diagnostic only; inclusive span times are not phase performance samples")


def run(config_path):
    """Run after performance sampling using a separately sealed diagnostic root."""
    import reference_baselines as reference
    from filesystem_rounds import validate_rounds, validate_stream_files
    from filesystem_cache_matrix import (CONDITIONS, CONTROL_ENVIRONMENT, condition_environment,
        load_adapter, load_arguments, save, sha, trial_environment, validate_qualification)

    config_path = Path(config_path)
    config = json.loads(config_path.read_text())
    arguments = load_arguments(config)
    arguments.update(samples=int(config.get("diagnostic_samples", 1)),
                     diagnostic_timing=True, diagnostic_stderr_file=True)
    canonical = Path(config["outputs"]["profiles"])
    output = Path("/tmp/private-out") / canonical.relative_to(Path(config["raw_root"]))
    output.mkdir(parents=True, exist_ok=False)
    adapter = load_adapter(config["trial_adapter"])
    artifacts = {"binary": sha(arguments["binary"]), "firmware": sha(config["firmware_object"]),
                 "adapter": sha(config["trial_adapter"]), "runner": sha(__file__)}
    report = dict(schema=1, benchmark_id="B-FS-DIAG", role="diagnostic", configuration=config,
                  assets=json.loads((arguments["assets"] / "assets.json").read_text()), artifacts=artifacts,
                  protocol=dict(profile=True, writeback=0, dax=False, resident_rounds=3,
                                samples_per_condition=arguments["samples"], interference_gate=False,
                                boundary="checked() subprocess startup, operation and validation",
                                performance_samples=False), rows=[], failures=[], order=[])
    for condition in CONDITIONS:
        folder = output / "conditions" / condition / "bin"
        folder.mkdir(parents=True)
        shutil.copy2(arguments["binary"], folder / "pvisor")
    save(output / "report.json", report)
    rng = random.Random(arguments["seed"])
    for trial in range(arguments["samples"]):
        order = list(CONDITIONS)
        rng.shuffle(order)
        report["order"].append(dict(trial=trial, conditions=order))
        for condition in order:
            args = SimpleNamespace(**arguments)
            args.output = output / "conditions" / condition
            row = None
            try:
                args.cache_matrix_environment = condition_environment(config, condition)
                prepared = adapter.prepare_trial(args, condition, trial)
                settings = condition_environment(config, condition)
                settings.update(prepared.get("environment", {}))
                settings.update(CONTROL_ENVIRONMENT, PVISOR_FS_PROFILE="1")
                with trial_environment(settings):
                    row = reference.run_trial(args, report, "pvisor-vm", "filesystem", trial)
                validate_rounds(row["result"])
                validate_stream_files(row, "pvisor-vm")
                qualification = adapter.validate_trial(args, row, condition, trial, prepared)
                validate_qualification(qualification, condition)
                counters = collect_phase_profiles((Path(row["logs"]) / "stderr.log").read_text().splitlines())
                row.update(condition=condition, qualification=qualification, phase_profiles=counters)
                report["rows"].append(row)
            except Exception as error:
                report["failures"].append(dict(condition=condition, trial=trial, reason=str(error), row=row))
            save(output / "report.json", report)
    report["status"] = "failed" if report["failures"] else "complete"
    report["finished_at"] = datetime.now(timezone.utc).isoformat()
    save(output / "report.json", report)
    if report["failures"]:
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path)
    parser.add_argument("--instrument-rootfs", type=Path)
    options = parser.parse_args()
    if bool(options.config) == bool(options.instrument_rootfs):
        parser.error("choose exactly one of --config or --instrument-rootfs")
    if options.instrument_rootfs:
        instrument_rootfs(options.instrument_rootfs)
    else:
        run(options.config)
