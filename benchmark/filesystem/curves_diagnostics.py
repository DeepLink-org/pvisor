#!/usr/bin/env python3
"""B-FS-DIAG counters for the frozen host-writeback curves mechanism subject.

All startup instances require unique final records. The current subject has
one rootfs connection and exactly four profile owners. Phase counters cover
work through directory fsync, excluding fixture generation and verification.
Inclusive durations are retained as diagnostics, never performance samples.
"""

import argparse
import json
from collections import Counter
from pathlib import Path

START = "pvisor-fs-profile-start "
REPORT = "pvisor-fs-profile "
PHASE = "pvisor-fs-profile-phase "
COMPONENTS = frozenset(
    ("virtio-fs-protocol", "virtio-fs-dispatch", "virtio-fs-overlay", "overlay-core")
)
ADDITIVE = ("calls", "units", "total_ns")
HOST_COUNTERS = (
    "host_writeback_enabled",
    "host_writeback_accepted_requests",
    "host_writeback_accepted_bytes",
    "host_writeback_backing_batches",
    "host_writeback_backing_bytes",
    "host_writeback_backing_errors",
    "host_writeback_queue_sessions",
    "host_writeback_queue_peak_bytes_sum",
    "host_writeback_queue_peak_jobs_sum",
    "host_writeback_queue_batches",
    "host_writeback_queue_jobs",
    "host_writeback_reserve_wait_count",
    "host_writeback_reserve_wait_ns",
)


def integer(value, name, *, positive=False):
    if type(value) is not int or value < (1 if positive else 0):
        raise ValueError(f"{name} must be a {'positive' if positive else 'nonnegative'} integer")
    return value


def identity(record):
    component = record.get("component")
    if not isinstance(component, str) or not component:
        raise ValueError("profile component must be a nonempty string")
    return (
        integer(record.get("pid"), "pid", positive=True),
        component,
        integer(record.get("instance"), "instance", positive=True),
    )


def measurements(record):
    if (
        type(record.get("schema")) is not int
        or record["schema"] != 2
        or record.get("inclusive_spans") is not True
        or type(record.get("final_record")) is not bool
    ):
        raise ValueError("unsupported profile schema or span/final contract")
    result = record.get("measurements")
    if not isinstance(result, dict):
        raise ValueError("profile measurements must be an object")
    for label, value in result.items():
        if not isinstance(label, str) or not isinstance(value, dict):
            raise ValueError("invalid profile measurement")
        for field in (*ADDITIVE, "max_ns"):
            integer(value.get(field), f"{label}/{field}")
        buckets = value.get("latency_buckets")
        if not isinstance(buckets, list) or len(buckets) != 5:
            raise ValueError("profile requires five cumulative latency buckets")
        for bucket in buckets:
            integer(bucket, f"{label}/latency_buckets")
    return result


def delta(before, after):
    """Subtract additive measurements; maxima cannot describe an interval."""
    result = {}
    for label in before.keys() | after.keys():
        old, new = before.get(label, {}), after.get(label, {})
        values = {}
        for field in ADDITIVE:
            a, b = old.get(field, 0), new.get(field, 0)
            if b < a:
                raise ValueError(f"nonmonotonic profile {label}/{field}")
            values[field] = b - a
        old_buckets = old.get("latency_buckets", [0] * 5)
        new_buckets = new.get("latency_buckets", [0] * 5)
        if any(b < a for a, b in zip(old_buckets, new_buckets)):
            raise ValueError(f"nonmonotonic latency buckets {label}")
        values["latency_buckets"] = [b - a for a, b in zip(old_buckets, new_buckets)]
        if new.get("max_ns", 0) < old.get("max_ns", 0):
            raise ValueError(f"nonmonotonic maximum {label}")
        result[label] = values
    return result


def collect_profiles(lines, expected_instances=None):
    """Require exact startup/final identities and declared component inventory."""
    expected = Counter(
        {component: 1 for component in COMPONENTS}
        if expected_instances is None
        else expected_instances
    )
    registered, previous, final = set(), {}, {}
    for line in lines:
        if line.startswith(START):
            record = json.loads(line[len(START) :])
            if type(record.get("schema")) is not int or record["schema"] != 1:
                raise ValueError("unsupported startup profile schema")
            key = identity(record)
            if key in registered:
                raise ValueError(f"duplicate startup profile {key}")
            registered.add(key)
        elif line.startswith(REPORT):
            record = json.loads(line[len(REPORT) :])
            key, current = identity(record), measurements(record)
            if key not in registered:
                raise ValueError(f"unregistered profile {key}")
            if key in final:
                raise ValueError(f"profile after unique final record {key}")
            if key in previous:
                delta(previous[key], current)
            previous[key] = current
            if record["final_record"]:
                final[key] = record
    if set(final) != registered:
        raise ValueError(f"startup/final inventory mismatch: missing={registered - final.keys()}")
    observed = Counter(key[1] for key in registered)
    if observed != expected:
        raise ValueError(
            f"unexpected profile instance inventory: {dict(observed)} != {dict(expected)}"
        )
    return [final[key] for key in sorted(final)]


def corrected_counts(component, raw):
    result = {
        label: {field: value[field] for field in ("calls", "units")} for label, value in raw.items()
    }
    corrections = (
        {"IOCTL": ("calls", 1), "request_bytes": ("units", 80)}
        if component == "virtio-fs-protocol"
        else {"direct_requests": ("units", 1)}
        if component == "virtio-fs-dispatch"
        else {}
    )
    for label, (field, amount) in corrections.items():
        if result.get(label, {}).get(field, 0) < amount:
            raise ValueError(f"missing marker counter {component}/{label}")
        result[label][field] -= amount
    return result


def collect_phases(lines, final, cell_order, rounds):
    """No phase lines means unavailable; any phase lines require full coverage."""
    by_identity = {identity(profile): profile for profile in final}
    phases, mounted_ids = {}, None
    for line in lines:
        if not line.startswith(PHASE):
            continue
        record = json.loads(line[len(PHASE) :])
        if record.get("schema") != 1 or record.get("scope") != "rootfs":
            raise ValueError("unexpected phase schema/scope for single-rootfs subject")
        token = integer(record.get("token"), "token", positive=True)
        if token in phases:
            raise ValueError(f"duplicate phase token {token}; mounts may share one connection")
        profiles = record.get("profiles")
        if not isinstance(profiles, list):
            raise ValueError("phase profiles must be a list")
        ids = [identity(profile) for profile in profiles]
        if (
            len(ids) != len(COMPONENTS)
            or len(set(ids)) != len(ids)
            or {key[1] for key in ids} != COMPONENTS
            or not set(ids) <= by_identity.keys()
        ):
            raise ValueError("incomplete or unregistered mounted phase inventory")
        if mounted_ids is not None and set(ids) != mounted_ids:
            raise ValueError("mounted profile identities changed")
        mounted_ids = set(ids)
        phases[token] = {identity(profile): profile for profile in profiles}
        for profile in profiles:
            measurements(profile)
            if profile["final_record"]:
                raise ValueError("phase contains a final record")
    if not phases:
        return []
    expected = set(range(1, 2 * len(cell_order) * rounds + 1))
    if set(phases) != expected:
        raise ValueError("incomplete phase token coverage")
    previous = {}
    for token in sorted(phases):
        for key, profile in phases[token].items():
            if key in previous:
                delta(previous[key], profile["measurements"])
            previous[key] = profile["measurements"]
    for key, last in previous.items():
        delta(last, by_identity[key]["measurements"])
    rows = []
    for round_ in range(1, rounds + 1):
        for index, cell_id in enumerate(cell_order):
            token = 2 * ((round_ - 1) * len(cell_order) + index) + 1
            for key in sorted(mounted_ids):
                raw = delta(
                    phases[token][key]["measurements"], phases[token + 1][key]["measurements"]
                )
                rows.append(
                    dict(
                        id=cell_id,
                        round=round_,
                        scope="rootfs",
                        component=key[1],
                        pid=key[0],
                        instance=key[2],
                        before_token=token,
                        after_token=token + 1,
                        raw_measurements=raw,
                        measurements=corrected_counts(key[1], raw),
                    )
                )
    return rows


def aggregate(profiles):
    """Add work counts by component; keep each label's largest cumulative max."""
    result = {}
    for profile in profiles:
        labels = result.setdefault(profile["component"], {})
        for label, value in profile["measurements"].items():
            total = labels.setdefault(
                label, dict(calls=0, units=0, total_ns=0, max_ns=0, latency_buckets=[0] * 5)
            )
            for field in ADDITIVE:
                total[field] += value[field]
            total["max_ns"] = max(total["max_ns"], value["max_ns"])
            total["latency_buckets"] = [
                a + b for a, b in zip(total["latency_buckets"], value["latency_buckets"])
            ]
    return result


def host_writeback(components, enabled):
    overlay = components.get("virtio-fs-overlay", {})
    result = {
        label: value["units"]
        for label, value in overlay.items()
        if label.startswith("host_writeback_")
    }
    if enabled:
        missing = set(HOST_COUNTERS) - result.keys()
        if missing:
            raise ValueError(f"missing explicit host-writeback counters: {sorted(missing)}")
        if result["host_writeback_enabled"] != 1:
            raise ValueError("host-writeback profile disagrees with condition")
        if (
            result["host_writeback_accepted_bytes"] != result["host_writeback_backing_bytes"]
            or result["host_writeback_accepted_requests"] != result["host_writeback_queue_jobs"]
            or result["host_writeback_backing_batches"] != result["host_writeback_queue_batches"]
            or result["host_writeback_backing_errors"] != 0
        ):
            raise ValueError("accepted/backing/queue completion counters disagree")
    elif result.get("host_writeback_enabled", 0) != 0:
        raise ValueError("synchronous condition unexpectedly enabled host writeback")
    return result


def summarize_batch(batch):
    batch = Path(batch)
    protocol = json.loads((batch / "protocol.json").read_text())
    if protocol.get("benchmark_id") != "B-FS-DIAG" or protocol.get("profiling") is not True:
        raise ValueError("requires a separate B-FS-DIAG profiling batch")
    records = json.loads((batch / "records.json").read_text())
    expected = {
        (order["pair"], condition)
        for order in protocol["orders"]
        for condition in order["conditions"]
    }
    observed = [(record["pair"], record["condition"]) for record in records]
    if len(observed) != len(expected) or set(observed) != expected:
        raise ValueError("diagnostic trial inventory mismatch")
    orders = {order["pair"]: order["cell_order"] for order in protocol["orders"]}
    all_profiles, grouped, trials = [], {}, []
    for record in records:
        if record.get("valid") is not True:
            raise ValueError(
                "failed diagnostic trial; retain logs and do not summarize as complete"
            )
        pair, condition = record["pair"], record["condition"]
        lines = (
            (batch / "trials" / f"{pair:03}-{condition}" / "stderr.log").read_text().splitlines()
        )
        native = condition == "native"
        profiles = collect_profiles(lines, {} if native else None)
        phase_rows = collect_phases(lines, profiles, orders[pair], protocol["resident_rounds"])
        if not native and not phase_rows:
            raise ValueError("missing expected per-cell diagnostic phase records")
        components = aggregate(profiles)
        enabled = protocol["conditions"][condition]["env"].get("PVISOR_VM_FS_HOST_WRITEBACK") == "1"
        counters = host_writeback(components, enabled)
        trials.append(
            dict(
                pair=pair,
                condition=condition,
                final_profiles=profiles,
                phase_rows=phase_rows,
                host_writeback=counters,
                native_counters="unavailable" if native else None,
            )
        )
        grouped.setdefault(condition, []).extend(profiles)
        all_profiles.extend(profiles)
    return dict(
        schema=1,
        benchmark_id="B-FS-DIAG",
        role="diagnostic",
        performance_samples=False,
        source=str(batch),
        trials=trials,
        conditions={condition: aggregate(profiles) for condition, profiles in grouped.items()},
        total=aggregate(all_profiles),
        coverage="exact current-subject startup/final identities and mounted phase tokens",
        counter_scope="final totals include preparation/verification; phase deltas exclude them",
        aggregation="condition/total counters sum independent diagnostic trials, not averages",
        marker_correction="one IOCTL, 80 request bytes and one direct request per cell interval",
        queue_peaks="peak_bytes_sum/peak_jobs_sum add per-session peaks; not one simultaneous peak",
        durations="inclusive per-label diagnostics; never add labels or use as timing samples",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = summarize_batch(args.batch)
    output = args.output or args.batch / "curves-diagnostics.json"
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(output)
    print(output)


if __name__ == "__main__":
    main()
