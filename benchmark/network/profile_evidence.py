"""Read-only parser and statistics for retained network profile observations."""

import json
import math
import random
import statistics
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCENARIOS = (
    "connect",
    "rpc_fresh",
    "rpc_keepalive",
    "dns",
    "stream",
    "download_1",
    "upload_1",
    "download_8",
    "upload_8",
    "download_32",
    "upload_32",
)
ARMS = ("native", "release", "net_o3")


def parse_results(text, arm, expected=SCENARIOS):
    records = [
        json.loads(line.removeprefix("NET_RESULT "))
        for line in text.splitlines()
        if line.startswith("NET_RESULT ")
    ]
    names = [r["scenario"] for r in records]
    wanted = set(expected) - ({"dns"} if arm == "native" else set())
    if len(names) != len(set(names)) or set(names) != wanted:
        raise ValueError(f"incomplete/duplicate scenarios: {names}, expected {sorted(wanted)}")
    if text.splitlines().count("NET_COMPLETE") != 1:
        raise ValueError("missing unique completion marker")
    ready = [
        json.loads(line.removeprefix("NET_READY "))
        for line in text.splitlines()
        if line.startswith("NET_READY ")
    ]
    if len(ready) != 1 or ready[0]["kind"] != ("native" if arm == "native" else "vm"):
        raise ValueError("missing correct client readiness")
    for record in records:
        name = record["scenario"]
        if record["elapsed_ns"] <= 0:
            raise ValueError("nonpositive clock")
        if name.startswith(("upload_", "download_")):
            expected_ops = int(name.rsplit("_", 1)[-1])
            if record["bytes"] != 64 * 1024 * 1024 or record["operations"] != expected_ops:
                raise ValueError("incorrect bulk byte/connection count")
        else:
            operations, byte_count, samples = {
                "connect": (128, 0, 128),
                "rpc_fresh": (128, 128 * 2048, 128),
                "rpc_keepalive": (256, 256 * 2048, 256),
                "dns": (128, 0, 128),
                "stream": (32, 2048, 31),
            }[name]
            if (record["operations"], record["bytes"], len(record["samples_ns"])) != (
                operations,
                byte_count,
                samples,
            ):
                raise ValueError("incorrect operation/byte/sample count")
            if any(not isinstance(v, int) or v <= 0 for v in record["samples_ns"]):
                raise ValueError("invalid per-operation clock")
            if name == "stream" and not 0 < record["first_byte_ns"] <= record["elapsed_ns"]:
                raise ValueError("invalid first-byte clock")
    return records


def metric(record, name=None):
    name = name or record["scenario"]
    if name.startswith(("upload_", "download_")):
        return record["bytes"] / (1024 * 1024) * 1e9 / record["elapsed_ns"], "MiB/s"
    if name == "stream":
        return record["elapsed_ns"] / 1e6, "ms"
    if name == "stream_first_byte":
        return record["first_byte_ns"] / 1e6, "ms"
    return statistics.median(record["samples_ns"]) / 1e6, "ms"


def validate_bundle(bundle, port):
    run = bundle["run"]
    if (
        run["executor"]["kind"] != "virtual_machine"
        or run["state"] != "completed"
        or run["exit_code"] != 0
        or run["metrics"]["resource.vm_memory_bytes"] != 256 * 1024 * 1024
        or not bundle["safety"]["network_non_bypassable"]
        or bundle["network"]["interception"]["driver"] != "vm-smoltcp"
        or bundle["network"]["interception"]["strength"] != "non-bypassable"
    ):
        raise ValueError("VM/network boundary or completed execution not observed")
    policy = bundle["network"]["policy"]
    if (
        policy["mode"] != "allowlist"
        or policy["limits"]
        or policy["rules"]
        != [dict(host="localhost.localdomain", ports=[port], allow_private_ips=True)]
    ):
        raise ValueError("unexpected endpoint policy or bandwidth limit")
    counters = bundle["network"]["intercepted"]
    expected = dict(
        policy_allowed=340,
        policy_denied=0,
        failures=0,
        dns_queries=129,
        dns_answers=129,
        tcp_flows_opened=340,
        tcp_flows_denied=0,
        tcp_connect_failures=0,
        bytes_guest_to_host=201723200,
        bytes_host_to_guest=201722184,
        unsupported_packets=0,
        active_tcp_flows=0,
    )
    if any(counters[key] != value for key, value in expected.items()):
        raise ValueError(f"incomplete/unexpected network accounting: {counters}")
    return counters


def clusters(values):
    """Descriptive split; never removes an observation."""
    ordered = sorted(values)
    threshold = max(0.005, 0.1 * statistics.median(ordered))
    splits = [i for i in range(3, len(ordered) - 2) if ordered[i] - ordered[i - 1] > threshold]
    groups, previous = [], 0
    for stop in splits + [len(ordered)]:
        if stop - previous >= 3:
            groups.append(ordered[previous:stop])
            previous = stop
    if previous != len(ordered):
        return [ordered]
    return groups or [ordered]


def paired_ci(left, right, seed=20261115):
    if len(left) != len(right) or not left:
        raise ValueError("nonempty paired observations required")
    deltas = [b - a for a, b in zip(left, right)]
    rng = random.Random(seed)
    bootstrap = sorted(statistics.median(rng.choices(deltas, k=len(deltas))) for _ in range(5000))
    return dict(
        median_delta=statistics.median(deltas),
        ci95=[bootstrap[124], bootstrap[4874]],
        median_paired_percent=statistics.median((b / a - 1) * 100 for a, b in zip(left, right)),
    )


def summarize(trials, rounds):
    formal = [t for t in trials if t["phase"] == "formal"]
    if len(formal) != rounds * len(ARMS):
        raise ValueError("incomplete formal cohort")
    by_arm = {arm: {} for arm in ARMS}
    for trial in formal:
        arm = by_arm[trial["arm"]]
        if trial["round"] in arm:
            raise ValueError("duplicate formal round")
        arm[trial["round"]] = {r["scenario"]: r for r in trial["results"]}
    for arm in ARMS:
        if set(by_arm[arm]) != set(range(rounds)):
            raise ValueError("missing formal round")
    rows = []
    for name in (*SCENARIOS, "stream_first_byte", "stream_gap"):
        values, unit = {}, None
        for arm in ARMS:
            if name == "dns" and arm == "native":
                continue
            values[arm] = []
            for index in range(rounds):
                key = "stream" if name.startswith("stream_") else name
                value, unit = metric(by_arm[arm][index][key], name)
                values[arm].append(value)
        statistics_by_arm = {}
        for arm, numbers in values.items():
            groups = clusters(numbers)
            statistics_by_arm[arm] = dict(
                n=len(numbers),
                median=statistics.median(numbers),
                p95_reference=sorted(numbers)[math.ceil(len(numbers) * 0.95) - 1],
                clusters=[dict(n=len(g), median=statistics.median(g)) for g in groups],
                values=numbers,
            )
        rows.append(
            dict(
                scenario=name,
                unit=unit,
                arms=statistics_by_arm,
                comparison=paired_ci(values["release"], values["net_o3"]),
            )
        )
    return dict(schema=1, benchmark="B-NETWORK-PROFILE-ENG", valid=True, rounds=rounds, rows=rows)
