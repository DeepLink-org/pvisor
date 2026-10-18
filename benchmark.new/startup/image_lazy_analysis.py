"""Offline evidence audit for the independent cold/warm-client image cohort.

This module executes no workloads or service commands. It reuses the prepared
cohort's file receipts, raw-clock checks and statistical functions, while
auditing the different lazy-image/cache contracts independently. Response-body
traffic is a completion observation, not a Ready cutoff or network wire count.
"""

import json
import re
import shlex
import statistics
from pathlib import Path

from image_compare_analysis import (
    BOOTSTRAPS,
    CANARY,
    GROUPS,
    MARKER,
    Evidence,
    artifacts,
    audit_adapter_teardown,
    audit_background,
    audit_inventory,
    clock,
    compare,
    distribution,
    positive_ns,
    require,
    sha,
    write_csv,
)

STATES = ("cold", "warm")
SEED = 20261018


def pvisor_totals(rows):
    """Recompute payload/framing totals from retained individual responses."""
    totals = {
        "requests": len(rows),
        "content_bytes": 0,
        "metadata_bytes": 0,
        "response_bytes": 0,
        "framing_and_json_bytes": 0,
        "metadata_response_bytes": 0,
    }
    for row in rows:
        for name in ("content_bytes", "metadata_bytes", "response_bytes"):
            require(
                type(row.get(name)) is int and row[name] >= 0, "invalid pvisor response byte count"
            )
        response = row["response"]
        length = response.get("length", 0) if response.get("status") == "data" else 0
        require(type(length) is int and length >= 0, "invalid pvisor response length")
        require(
            row["content_bytes"] == (length if row["op"] == "read" else 0)
            and row["metadata_bytes"] == (length if row["op"] == "metadata" else 0),
            "pvisor response role/length differs from counted payload",
        )
        if length:
            require(
                row.get("forwarded_body_sha256")
                == row.get("server_body_sha256")
                == response.get("sha256"),
                "pvisor proxy body digest differs from the server response",
            )
        require(
            row["response_bytes"] >= row["content_bytes"] + row["metadata_bytes"],
            "pvisor total response is smaller than its payload",
        )
        for name in ("content_bytes", "metadata_bytes", "response_bytes"):
            totals[name] += row[name]
        totals["framing_and_json_bytes"] += (
            row["response_bytes"] - row["content_bytes"] - row["metadata_bytes"]
        )
        if row["op"] != "read":
            totals["metadata_response_bytes"] += row["response_bytes"]
        require(positive_ns(row.get("response_finished_ns")), "missing response observation clock")
    return totals


def audit_pvisor(result, directory, state, protocol, evidence, roots):
    """Audit an actual lazy VM, its raw proxy responses and staged filesystem."""
    require(
        result.get("mode") == "lazy"
        and result.get("actualMode") == "virtual_machine"
        and result.get("cache_state") == ("fresh" if state == "cold" else "reuse"),
        "pvisor sample has the wrong mode/cache state",
    )
    interval = clock(result, "clock_start_ns", "ready_observed_ns", "exit_observed_ns")
    adapter = directory.parent.parent
    prepared = evidence.checked(adapter / "prepared-input.json", result["prepared_sha"])
    require(
        prepared["digest"] == protocol["source_digest"]
        and prepared["source"].endswith("@" + protocol["source_digest"])
        and prepared["workload_argv"] == protocol["workload_argv"]
        and prepared["memory_mib"] == 2048
        and prepared["cpus"] == protocol["cpus"]
        and prepared["service_cpus"] == protocol["service_cpus"],
        "pvisor immutable inputs/budget differ",
    )
    evidence.checked(adapter / "source-manifest.json", protocol["source_digest"][7:])
    command = evidence.json(directory / "command.json")
    require(
        command == result["timed_argv"]
        and command[command.index("--") + 1 :] == protocol["workload_argv"]
        and command[command.index("--rootfs") + 1] == "image=" + prepared["source"]
        and command[command.index("--cpu") + 1] == "2"
        and command[command.index("--memory") + 1] == "2048MiB"
        and command[:3] == ["taskset", "-c", ",".join(map(str, protocol["cpus"]))],
        "pvisor raw argv differs from lazy source/workload/budget",
    )
    stdout = evidence.read(directory / "stdout.log")
    require(
        stdout.count(MARKER) == 1 and stdout.splitlines().count(MARKER.rstrip(b"\n")) == 1,
        "pvisor stdout lacks the exact unique Ready marker",
    )
    bundles = list((directory / "stage").rglob("run-bundle.json"))
    require(
        len(bundles) == 1 and Path(result["bundle"]).resolve() == bundles[0].resolve(),
        "VM Bundle identity differs",
    )
    bundle = evidence.json(bundles[0])
    run = bundle["run"]
    require(
        run["state"] == "completed"
        and run["exit_code"] == 0
        and run["command"] == protocol["workload_argv"]
        and run["executor"]["isolation"] == "virtual_machine"
        and bundle["safety"]["filesystem_changes_staged"] is True,
        "Bundle does not establish the successful VM",
    )
    workspace = directory / "workspace"
    require(
        sorted(path.name for path in workspace.iterdir()) == ["startup-canary"]
        and evidence.read(workspace / "startup-canary") == CANARY,
        "host workspace/canary changed",
    )
    require(
        not any(
            path.is_file() or path.is_symlink() for path in (directory / "stage/upper").rglob("*")
        ),
        "read-only workload produced staged changes",
    )
    raw = evidence.json(directory / "requests.json")
    require(raw.get("errors") == [], "pvisor counting proxy reported errors")
    totals = pvisor_totals(raw["rows"])
    require(
        all(
            result.get(name) == totals[name]
            for name in ("requests", "content_bytes", "metadata_bytes", "response_bytes")
        ),
        "pvisor response totals differ from individual response records",
    )
    ready = pvisor_totals(
        [row for row in raw["rows"] if row["response_finished_ns"] <= result["ready_observed_ns"]]
    )
    require(
        all(result["at_ready"].get(name) == ready[name] for name in result["at_ready"]),
        "pvisor Ready response totals differ from raw response clocks",
    )
    require(
        totals["content_bytes"] > 0 if state == "cold" else totals["content_bytes"] == 0,
        "pvisor cold must fetch content and warm must reuse content",
    )
    for name, path in (
        ("stdout_sha256", directory / "stdout.log"),
        ("stderr_sha256", directory / "stderr.log"),
        ("bundle_sha256", bundles[0]),
        ("requests_sha256", directory / "requests.json"),
    ):
        if name in result:
            evidence.checked(path, result[name]) if path.suffix == ".json" else require(
                sha(path) == result[name], "raw log hash differs"
            )
    require(
        roots.setdefault(str(adapter), prepared) == prepared,
        "pvisor prepared inputs changed between samples",
    )
    return interval, {
        "content_bytes": totals["content_bytes"],
        "metadata_bytes": totals["metadata_bytes"],
        "response_bytes": totals["response_bytes"],
        "requests": totals["requests"],
        "http_206_requests": 0,
    }


def manifest_roles(source, derived):
    roles = {}
    for manifest in (source, derived):
        roles[manifest["config"]["digest"]] = "metadata"
        for layer in manifest["layers"]:
            target = layer.get("annotations", {}).get(
                "containerd.io/snapshot/overlaybd/turbo-oci/target-digest"
            )
            roles[layer["digest"]] = "metadata" if target else "content"
            if target:
                roles[target] = "content"
    return roles


def registry_traffic(directory, roles, evidence):
    """Parse only structured completed responses; count actual 206 body bytes."""
    rows = []
    for line in evidence.read(directory / "registry-responses.log").decode().splitlines():
        if 'msg="response completed"' not in line:
            continue
        fields = {}
        for name in (
            "http.request.method",
            "http.request.uri",
            "http.response.status",
            "http.response.written",
        ):
            match = re.search(re.escape(name) + r'=("(?:\\.|[^"\\])*"|\S+)', line)
            require(match is not None, "registry completed response lacks an auditable field")
            fields[name] = json.loads(match[1]) if match[1].startswith('"') else match[1]
        status, size = int(fields["http.response.status"]), int(fields["http.response.written"])
        uri = fields["http.request.uri"]
        require(
            status in (200, 206) and size >= 0, "registry returned a failed/unsupported response"
        )
        role = (
            roles.get(uri.split("?", 1)[0].rsplit("/", 1)[-1], "unclassified")
            if "/blobs/" in uri
            else "metadata"
        )
        require(
            role != "unclassified" or size == 0,
            "registry blob role cannot be established from manifests",
        )
        require(
            fields["http.request.method"] != "HEAD" or size == 0,
            "HEAD response unexpectedly has a body",
        )
        rows.append(
            {
                "method": fields["http.request.method"],
                "uri": uri,
                "status": status,
                "body_bytes": size,
                "role": role,
            }
        )
    blobs = [row for row in rows if "/blobs/" in row["uri"] and row["method"] == "GET"]
    totals = {
        "requests": rows,
        "request_count": len(rows),
        "response_body_bytes": sum(row["body_bytes"] for row in rows),
        "blob_body_bytes": sum(row["body_bytes"] for row in blobs),
        "blob_get_requests": len(blobs),
        "http_206_requests": sum(row["status"] == 206 for row in rows),
        "http_200_requests": sum(row["status"] == 200 for row in rows),
        **{
            role + "_body_bytes": sum(row["body_bytes"] for row in rows if row["role"] == role)
            for role in ("content", "metadata", "error", "unclassified")
        },
    }
    retained = evidence.json(directory / "registry-responses.json")
    require(
        all(retained.get(key) == value for key, value in totals.items()),
        "registry totals differ from completed response logs",
    )
    require(
        retained.get("scope") == "completion-observation"
        and retained.get("ready_boundary_available") is False,
        "unmeasured registry Ready boundary was claimed",
    )
    return retained


def audit_container_pair(result, directory, protocol, evidence, provenances):
    """Audit ownership, source, reset and cleanup once per cold/warm pair."""
    group, provenance = result["condition"], result["provenance"]
    require(
        result.get("valid") is True
        and result.get("action") == "trial"
        and result.get("scenario") == "cold-warm-pair",
        "container pair failed or used the wrong scenario",
    )
    require(
        provenance["source_digest"] == protocol["source_digest"], "container source digest differs"
    )
    source = evidence.checked(directory / "source-manifest.json", protocol["source_digest"][7:])
    derived = evidence.checked(
        directory / "derived-manifest.json", provenance["derived_digest"][7:]
    )
    frozen = evidence.checked(directory / "frozen-images.json", provenance["frozen_images_sha256"])
    require(
        all(
            frozen[key] == provenance[key]
            for key in ("source_digest", "derived_digest", "source_reference", "derived_reference")
        ),
        "frozen image identities differ",
    )
    for kind in ("configs", "harness"):
        for name, digest in provenance[kind].items():
            require(sha(directory / name) == digest, "retained " + kind + " checksum differs")
            evidence.read(directory / name)
    require(
        evidence.json(directory / "provenance.json") == provenance, "container provenance differs"
    )
    require(
        provenances.setdefault("container", provenance) == provenance,
        "container generation/configuration changed",
    )
    receipt = evidence.checked(directory / "prepare-receipt.json", result["prepare_receipt_sha256"])
    require(
        receipt.get("valid") is True
        and receipt.get("budgets_verified") is True
        and receipt.get("action") == "prepare"
        and receipt["condition"] == group
        and receipt["cpus"] == protocol["cpus"]
        and receipt["boot_id"] == result["boot_id"]
        and receipt["workload_argv"] == protocol["workload_argv"]
        and receipt["memory_bytes"] == 2147483648
        and receipt["cpu_quota"] == 2,
        "container reused budget proof differs",
    )
    require(
        all(
            receipt["provenance"].get(key) == value
            for key, value in provenance.items()
            if key != "harness"
        )
        and result.get("budget_preflight_harness") == receipt["provenance"]["harness"],
        "new pair adapter changed runtime/images beyond its explicitly different harness",
    )
    if "prepared_receipts" in protocol:
        require(
            result["prepare_receipt_sha256"] == protocol["prepared_receipts"][group]["sha256"],
            "pair did not use the frozen budget receipt",
        )
    reset_path = directory / "reset-receipt.json"
    reset = evidence.checked(reset_path, result["reset_receipt_sha256"])
    require(
        result["reset_receipt"] == reset
        and Path(result["reset_receipt_file"]).resolve() == reset_path.resolve(),
        "pair reset receipt identity differs",
    )
    require(
        reset.get("valid") is True
        and reset.get("phase") == "empty-client-ready"
        and all(
            reset.get(key) is True
            for key in (
                "empty_directories_verified",
                "registry_process_restarted",
                "registry_metadata_rechecked_outside_client",
                "metadata_and_content_empty",
            )
        )
        and reset["reset_paths"] == ["containerd-root", "cache", "run"]
        and reset["docker_container_id"] == provenance["docker_container_id"]
        and reset["lab_id"] == provenance["lab_id"],
        "cold client reset was not verified complete",
    )
    affinity = reset["service_affinity"]
    require(
        affinity["cpus"] == protocol["service_cpus"]
        and set(affinity["services"]) == {"registry", "tcmu", "snapshotter", "containerd"}
        and affinity["threads"],
        "service CPU controls differ",
    )
    for thread in affinity["threads"]:
        allowed = set()
        for item in thread["cpus"].split(","):
            limits = list(map(int, item.split("-")))
            allowed.update(range(limits[0], limits[-1] + 1))
        require(
            allowed == set(protocol["service_cpus"]), "service thread affinity differs from receipt"
        )
    require(
        len(reset["preserved_directory_identity"]) == 2
        and provenance["namespace"] in reset["namespaces_checked"],
        "reset did not retain server directories/private namespace proof",
    )
    baseline = evidence.json(directory / "host-before.json")
    for name in ("reset-after-stop", "reset-after-start"):
        after = evidence.json(directory / name / "host-after.json")
        require(
            not baseline["errors"]
            and not after["errors"]
            and baseline["boot_id"] == after["boot_id"],
            "reset host inventory failed or boot identity changed",
        )
        expected = {"boot_changed": False, "errors": []}
        for kind in ("targets", "block_devices", "uio"):
            require(baseline[kind] == after[kind], "reset changed shared host objects")
            expected[kind] = {"added": [], "removed": [], "changed": []}
        require(
            not baseline["targets"]
            and evidence.json(directory / name / "host-delta.json") == expected,
            "reset host delta differs from its raw baseline",
        )
    roles = manifest_roles(source, derived)
    require(
        evidence.json(directory / "registry-blob-roles.json") == roles,
        "registry roles differ from immutable manifests",
    )
    # These retained commands contain the independent namespace emptiness,
    # cgroup removal and reset readbacks; any command failure rejects the pair.
    for path in sorted(directory.glob("*-command/status.json")):
        status = evidence.json(path)
        require(
            status.get("returncode") == 0 and status.get("timed_out") is False,
            "retained pair command failed",
        )
        evidence.read(path.with_name("argv.json"))
        evidence.read(path.with_name("stdout.txt"))
        evidence.read(path.with_name("stderr.txt"))
    return roles


def audit_container(result, directory, state, pair, roles, protocol, evidence):
    group, provenance = result["condition"], result["provenance"]
    require(
        result.get("scenario") == state + "-client"
        and result.get("phase") == state
        and result.get("valid") is True
        and result["provenance"] == pair["provenance"]
        and result["cpus"] == protocol["cpus"]
        and result["service_cpus"] == protocol["service_cpus"]
        and result["memory_bytes"] == 2147483648
        and result["cpu_quota"] == 2,
        "container phase inputs/budgets differ",
    )
    expected_snapshotter = "overlaybd" if group == "turbo" else "overlayfs"
    reference = provenance["derived_reference" if group == "turbo" else "source_reference"]
    require(
        result["snapshotter"] == expected_snapshotter
        and result["image"] == reference
        and result["reset_receipt_sha256"] == pair["reset_receipt_sha256"],
        "container actual image/reset differs",
    )
    pair_dir = directory.parent
    require(
        Path(result["pair_evidence_dir"]).resolve() == pair_dir.resolve()
        and Path(result["reset_receipt_path"]).resolve()
        == (pair_dir / "reset-receipt.json").resolve(),
        "phase points to a different pair/reset",
    )
    timing = evidence.json(directory / "trial/timing.json")
    require(
        timing.get("valid") is True and timing.get("clock") == "perf_counter_ns",
        "invalid container raw clock",
    )
    interval = clock(timing, "start_ns", "marker_ns", "end_ns")
    require(
        all(result[key] == timing[key] for key in ("ready_ns", "completion_ns", "exit_code")),
        "container durations differ",
    )
    require(
        evidence.read(directory / "trial/stdout.txt") == MARKER,
        "container output differs from exact Ready marker",
    )
    command = evidence.json(directory / "trial/argv.json")
    inside = evidence.json(directory / "in-lab-argv.json")
    cpus = ",".join(map(str, protocol["cpus"]))
    require(
        command
        == [
            "docker",
            "exec",
            provenance["docker_container_id"],
            "/lab/tools/taskset",
            "-c",
            cpus,
            "/bin/sh",
            "-ec",
            shlex.join(inside["pull"]) + " 1>&2\nexec " + shlex.join(inside["run"]),
        ],
        "container shell command differs from retained pull/run argv",
    )
    pull, run = inside["pull"], inside["run"]
    require(
        pull[pull.index("--snapshotter") + 1] == expected_snapshotter
        and pull[-1] == reference
        and "--plain-http" in pull
        and ("rpull" in pull if group == "turbo" else ["images", "pull"] == pull[5:7]),
        "timed pull uses the wrong image or snapshotter",
    )
    require(
        run[-len(protocol["workload_argv"]) :] == protocol["workload_argv"]
        and "--rm" in run
        and run[run.index("--snapshotter") + 1] == expected_snapshotter
        and run[run.index("--cpus") + 1] == "2"
        and run[run.index("--cpuset-cpus") + 1] == cpus
        and run[run.index("--memory-limit") + 1] == "2147483648"
        and run[-len(protocol["workload_argv"]) - 2 : -len(protocol["workload_argv"])]
        == [reference, result["task_id"]],
        "timed container run differs from workload/budget/image",
    )
    require(
        run[run.index("--cgroup") + 1]
        == "/system.slice/pvisor-overlaybd-lab-" + provenance["lab_id"] + "/" + result["task_id"],
        "container did not use its owned cgroup",
    )
    audit_inventory(directory, evidence)
    traffic = registry_traffic(directory, roles, evidence)
    require(result["registry"] == traffic, "phase response totals differ from retained observation")
    require(
        traffic["content_body_bytes"] > 0 if state == "cold" else traffic["blob_body_bytes"] == 0,
        "cold must fetch content and warm must reuse all blobs",
    )
    return interval, {
        "content_bytes": traffic["content_body_bytes"],
        "metadata_bytes": traffic["metadata_body_bytes"],
        "response_bytes": traffic["response_body_bytes"],
        "requests": traffic["request_count"],
        "http_206_requests": traffic["http_206_requests"],
    }


def analyze(batch: Path) -> dict:
    """Reject failed/incomplete evidence; emit cold/warm summaries and paired CIs."""
    batch = Path(batch).resolve(strict=True)
    evidence = Evidence()
    protocol = evidence.json(batch / "protocol.json")
    require(
        protocol.get("scope") == "cold-warm-client"
        and protocol.get("groups") == list(GROUPS)
        and protocol.get("states") == list(STATES)
        and protocol.get("runs") == 30
        and protocol.get("warmups") == 3,
        "unexpected cold/warm cohort protocol",
    )
    require(
        re.fullmatch(r"sha256:[a-f0-9]{64}", protocol.get("source_digest", "")) is not None,
        "source is not digest-pinned",
    )
    require(
        isinstance(protocol.get("workload_argv"), list)
        and protocol["workload_argv"]
        and all(isinstance(arg, str) and "\0" not in arg for arg in protocol["workload_argv"]),
        "invalid common workload",
    )
    artifacts(protocol["artifacts"])
    audit_background(batch, protocol, evidence)
    planned = []
    for phase, count in (("warmup", 3), ("sample", 30)):
        orders = protocol["orders"][phase]
        require(len(orders) == count, "wrong planned round count")
        for index, order in enumerate(orders):
            require(
                isinstance(order, list) and sorted(order) == sorted(GROUPS),
                "planned round is not a permutation",
            )
            planned.extend((phase, index, group) for group in order)
    records = evidence.json(batch / "records.json")
    pairs = evidence.json(batch / "pair_receipts.json")
    require(
        len(records) == 198 and len(pairs) == 99, "exactly 99 complete cold/warm pairs are required"
    )
    values = {
        group: {state: {"ready": [], "completion": []} for state in STATES} for group in GROUPS
    }
    traffic = {group: {state: [] for state in STATES} for group in GROUPS}
    roots, provenances, paths, cache_keys, task_ids = {}, {}, set(), set(), set()
    prior_end = None
    for pair_index, (pair_record, expected) in enumerate(zip(pairs, planned, strict=True)):
        require(
            (pair_record.get("phase"), pair_record.get("round"), pair_record.get("condition"))
            == expected,
            "pair receipt sequence differs from frozen plan",
        )
        group = expected[2]
        pair, roles = None, None
        if group != "pvisor":
            pair = evidence.checked(pair_record["result_path"], pair_record["result_sha256"])
            pair_dir = Path(pair["evidence_dir"]).resolve(strict=True)
            require(
                evidence.json(pair_dir / "result.json") == pair and pair["condition"] == group,
                "pair wrapper/raw result differs",
            )
            roles = audit_container_pair(pair, pair_dir, protocol, evidence, provenances)
        else:
            require(
                pair_record["cache_key"] not in cache_keys,
                "pvisor private cache reused across independent pairs",
            )
            cache_keys.add(pair_record["cache_key"])
        for state_index, state in enumerate(STATES):
            record = records[pair_index * 2 + state_index]
            require(
                (
                    record.get("phase"),
                    record.get("round"),
                    record.get("condition"),
                    record.get("state"),
                )
                == expected + (state,),
                "cold must be immediately followed by its matching warm record",
            )
            path = Path(record["result_path"])
            require(
                path.is_absolute() and str(path.resolve()) not in paths,
                "result path is relative or reused",
            )
            paths.add(str(path.resolve()))
            result = evidence.checked(path, record["result_sha256"])
            require(
                result == record["result"]
                and result.get("valid") is True
                and result.get("workload_argv") == protocol["workload_argv"],
                "failed/mismatched state result",
            )
            directory = Path(result["evidence_dir"]).resolve(strict=True)
            require(
                evidence.json(directory / "result.json") == result,
                "adapter result differs from coordinator copy",
            )
            if group == "pvisor":
                require(
                    pair_record["results"][state] == str(path)
                    and result["cache_key"] == pair_record["cache_key"],
                    "pvisor cold and warm did not share their private cache",
                )
                interval, observed = audit_pvisor(
                    result, directory, state, protocol, evidence, roots
                )
            else:
                require(
                    pair[state] == result and result["task_id"] not in task_ids,
                    "container phase/task identity reused",
                )
                task_ids.add(result["task_id"])
                interval, observed = audit_container(
                    result, directory, state, pair, roles, protocol, evidence
                )
            require(
                prior_end is None or interval[0] >= prior_end,
                "raw sample clocks overlap or violate planned order",
            )
            prior_end = interval[1]
            if expected[0] == "sample":
                for metric in ("ready", "completion"):
                    values[group][state][metric].append(result[metric + "_ns"])
                traffic[group][state].append(observed)
    audit_adapter_teardown(roots, evidence)
    summary = {
        group: {
            state: {metric: distribution(xs) for metric, xs in metrics.items()}
            for state, metrics in states.items()
        }
        for group, states in values.items()
    }
    comparisons = []
    for state in STATES:
        for left, right in (("turbo", "oci"), ("pvisor", "oci"), ("pvisor", "turbo")):
            for metric in ("ready", "completion"):
                comparison = compare(
                    left,
                    right,
                    metric,
                    {g: values[g][state] for g in GROUPS},
                    {g: summary[g][state] for g in GROUPS},
                    SEED + len(comparisons),
                )
                comparisons.append({"state": state, **comparison})
    response_summary = {
        group: {
            state: {
                name: {
                    "median": statistics.median(row[name] for row in rows),
                    "min": min(row[name] for row in rows),
                    "max": max(row[name] for row in rows),
                }
                for name in rows[0]
            }
            for state, rows in states.items()
        }
        for group, states in traffic.items()
    }
    artifacts(protocol["artifacts"])
    evidence.finish()
    analysis = {
        "status": "passed",
        "scope": "cold-warm-client",
        "source_digest": protocol["source_digest"],
        "groups": list(GROUPS),
        "states": list(STATES),
        "runs": 30,
        "warmups": 3,
        "audited_pairs": 99,
        "audited_trials": 198,
        "failed": 0,
        "excluded_slow_samples": 0,
        "workload_cpus": protocol["cpus"],
        "service_cpus": protocol["service_cpus"],
        "summary": summary,
        "comparisons": comparisons,
        "completion_response_bodies": response_summary,
        "statistics": {
            "bootstrap_replicates": BOOTSTRAPS,
            "bootstrap_seed": SEED,
            "unit": "paired round within each state",
            "estimand": "difference and ratio of marginal medians; all valid slow samples retained",
        },
        "traffic_limits": "Completion response-body observation only; not Ready traffic or TCP/HTTP wire overhead. Registry 206 uses actual response.written; no independent byte-for-byte Range-slice validation.",
        "audit": {
            "protocol_sha256": sha(batch / "protocol.json"),
            "records_sha256": sha(batch / "records.json"),
            "pair_receipts_sha256": sha(batch / "pair_receipts.json"),
            "evidence_sha256": evidence.files,
            "frozen_artifacts": protocol["artifacts"],
        },
    }
    (batch / "summary.json").write_text(json.dumps(analysis, indent=2) + "\n")
    write_csv(
        batch / "summary.csv",
        [
            {"group": group, "state": state, "metric": metric, **summary[group][state][metric]}
            for group in GROUPS
            for state in STATES
            for metric in ("ready", "completion")
        ],
    )
    write_csv(batch / "comparisons.csv", comparisons)
    write_csv(
        batch / "completion-traffic.csv",
        [
            {"group": group, "state": state, "quantity": name, **stats}
            for group, states in response_summary.items()
            for state, quantities in states.items()
            for name, stats in quantities.items()
        ],
    )
    return analysis


def render(batch, analysis) -> str:
    """Render audited cold/warm states separately with distribution limitations."""
    require(analysis.get("status") == "passed", "cannot render an unaudited cohort")
    lines = [
        "# 冷／热客户端的同源镜像启动对照",
        "",
        "本机 loopback 服务、服务端镜像已准备、宿主页缓存保留；每对先重置私有客户端，再冷启动并紧接热启动。",
        "每组每状态 3 次预热、30 个正式配对样本；全部 198 次启动通过审计，无失败重试或慢样本剔除。",
        "",
        "| 条件 | 客户端状态 | Ready 中位数／簇中位数（ms） | Completion 中位数／簇中位数（ms） |",
        "| --- | --- | ---: | ---: |",
    ]

    def cell(row):
        return (
            "; ".join(f"{c['median_ms']:.2f} ({c['n']}/30)" for c in row["clusters"])
            if row["clusters"]
            else f"{row['median_ms']:.2f}"
        )

    for group in GROUPS:
        for state in STATES:
            lines.append(
                f"| {group} | {state} | {cell(analysis['summary'][group][state]['ready'])} | {cell(analysis['summary'][group][state]['completion'])} |"
            )
    lines += [
        "",
        "| 状态 | 比较（左减右） | 指标 | 中位数差（ms）及配对 95% CI | 中位数比值及 95% CI |",
        "| --- | --- | --- | --- | --- |",
    ]
    for row in analysis["comparisons"]:
        low, high = row["median_difference_ci95_ms"]
        lo, hi = row["median_ratio_ci95"]
        caveat = "；分离分布" if not row["single_mode_ranking"] else ""
        lines.append(
            f"| {row['state']} | {row['left']} − {row['right']}{caveat} | {row['metric']} | "
            f"{row['median_difference_ms']:.2f} [{low:.2f}, {high:.2f}] | {row['median_ratio']:.3f} [{lo:.3f}, {hi:.3f}] |"
        )
    lines += [
        "",
        "| 条件 | 状态 | 内容响应体中位数（bytes） | 元数据响应体中位数（bytes） | 全部响应体中位数（bytes） |",
        "| --- | --- | ---: | ---: | ---: |",
    ]
    for group in GROUPS:
        for state in STATES:
            row = analysis["completion_response_bodies"][group][state]
            lines.append(
                f"| {group} | {state} | {row['content_bytes']['median']:.0f} | {row['metadata_bytes']['median']:.0f} | {row['response_bytes']['median']:.0f} |"
            )
    lines += [
        "",
        "传输量是 completion 后观察到的应用响应体；没有容器 Ready 传输截点，不计 HTTP/TCP 开销。"
        "206 按服务实际写出字节计数，未逐片独立校验 Range 内容。pVisor 总响应还包含缓存协议 JSON／帧开销。",
        "",
        "容器时钟包含 Docker exec、shell/taskset 和 pull+run；VM 时钟包含 lazy VM 命令。"
        "重置、准备、转换和校验不计时。2 GiB guest RAM 与容器限制不代表等整机预算。"
        "容器共享宿主内核，pVisor 使用 VM 隔离；跨组只比较端到端路径，不能归因于存储路径优劣。"
        "CPU 0,1 另有后台测试，亲和性不能消除共享资源干扰；P95 仅供参考。"
        "结果不代表冷磁盘、真实 WAN、并发或纯镜像格式的贡献。",
        "",
        "数据：summary.json、summary.csv、comparisons.csv、completion-traffic.csv；原证据保留于本批次目录。",
        "",
    ]
    return "\n".join(lines)
