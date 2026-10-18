#!/usr/bin/env python3
"""Offline semantic audit of one complete same-host-staged lower cohort.

Only retained files are read. No subjects, services, mounts or Docker commands
are executed. Failures write audit.json and raise instead of validating a
partial cohort. Frame totals retain the observer's recorded per-request sizes;
the original serialized cache response headers were not retained.
"""

import argparse
import ast
import hashlib
import json
import math
import os
import re
import shlex
import signal
import sys
from pathlib import Path

sys.dont_write_bytecode = True
from image_compare_analysis import Evidence, clock, positive_ns, require  # noqa: E402 - import after module-loading setup

SCOPE = "same-host-staged-lower-readonly"
GROUPS = ("pvisor", "turbo")
CACHES = ("cold", "warm")
MARKER = b"STAGE_READY files 4 bytes 2575738\n"
FILES = (
    "usr/lib/os-release",
    "usr/bin/dash",
    "usr/lib/x86_64-linux-gnu/libc.so.6",
    "usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
)
BUILDERS = {"cargo", "rustc", "ninja", "make", "pytest", "cargo-nextest", "nextest", "hyperfine"}
EXECUTABLE = "/opt/pvisor-stage-bench/pvisor"
MOUNTER = "/opt/pvisor-stage-bench/lazy-stage-mount"
EMPTY_SHA = "sha256:" + hashlib.sha256(b"").hexdigest()


def digest(body):
    return hashlib.sha256(body).hexdigest()


def integer(value, minimum=0):
    return type(value) is int and value >= minimum


def key_values(body):
    result = {}
    for line in body.decode().splitlines():
        key, separator, value = line.partition("=")
        require(separator and key not in result, "invalid or duplicate receipt field")
        result[key] = value
    return result


def cpus(value):
    result = set()
    for part in value.split(","):
        low, _, high = part.partition("-")
        result.update(range(int(low), int(high or low) + 1))
    return result


def inventory(directory, baseline, evidence):
    after = evidence.json(directory / "host-after.json")
    require(
        not baseline["errors"] and not after["errors"] and baseline["boot_id"] == after["boot_id"],
        "inventory errors or changed boot",
    )
    expected = {"boot_changed": False, "errors": []}
    for kind in ("targets", "block_devices", "uio"):
        old, new = baseline[kind], after[kind]
        expected[kind] = {
            "added": sorted(new.keys() - old.keys()),
            "removed": sorted(old.keys() - new.keys()),
            "changed": sorted(key for key in old.keys() & new.keys() if old[key] != new[key]),
        }
        require(
            not any(expected[kind].values()),
            "host kernel objects were not restored: " + str(directory),
        )
    require(not baseline["targets"] and not after["targets"], "TCMU targets remain after cleanup")
    require(
        evidence.json(directory / "host-delta.json") == expected,
        "host-delta differs from raw inventory",
    )


def blob_roles(source, derived):
    result = {}
    for manifest in (source, derived):
        result[manifest["config"]["digest"]] = "metadata"
        for layer in manifest["layers"]:
            target = layer.get("annotations", {}).get(
                "containerd.io/snapshot/overlaybd/turbo-oci/target-digest"
            )
            result[layer["digest"]] = "metadata" if target else "content"
            if target:
                result[target] = "content"
    return result


def registry(directory, roles, evidence):
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
            require(match is not None, "registry response lacks an auditable field")
            fields[name] = json.loads(match[1]) if match[1].startswith('"') else match[1]
        status, size = int(fields["http.response.status"]), int(fields["http.response.written"])
        method, uri = fields["http.request.method"], fields["http.request.uri"]
        require(
            status in (200, 206) and size >= 0 and method in ("GET", "HEAD"),
            "registry error/unsupported response",
        )
        require(method != "HEAD" or size == 0, "HEAD response contains a body")
        role = (
            roles.get(uri.split("?", 1)[0].rsplit("/", 1)[-1], "unclassified")
            if "/blobs/" in uri
            else "metadata"
        )
        require(role != "unclassified" or size == 0, "unknown nonempty registry blob")
        rows.append(dict(method=method, uri=uri, status=status, body_bytes=size, role=role))
    blobs = [row for row in rows if "/blobs/" in row["uri"] and row["method"] == "GET"]
    totals = dict(
        requests=rows,
        request_count=len(rows),
        response_body_bytes=sum(row["body_bytes"] for row in rows),
        blob_body_bytes=sum(row["body_bytes"] for row in blobs),
        blob_get_requests=len(blobs),
        http_206_requests=sum(row["status"] == 206 for row in rows),
        http_200_requests=sum(row["status"] == 200 for row in rows),
    )
    totals.update(
        {
            role + "_body_bytes": sum(row["body_bytes"] for row in rows if row["role"] == role)
            for role in ("content", "metadata", "error", "unclassified")
        }
    )
    retained = evidence.json(directory / "registry-responses.json")
    require(
        all(retained.get(key) == value for key, value in totals.items()),
        "registry aggregate differs from raw response log",
    )
    require(
        retained.get("scope") == "completion-observation"
        and retained.get("ready_boundary_available") is False,
        "registry claims an unobserved Ready transfer boundary",
    )
    return totals


def cache_requests(directory, protocol, handle, evidence):
    observed = evidence.json(directory / "requests.json")
    require(
        isinstance(observed.get("rows"), list) and observed.get("errors") == [],
        "cache observer recorded errors",
    )
    totals = dict(content_bytes=0, metadata_bytes=0, response_bytes=0)
    for row in observed["rows"]:
        request, response = row["request"], row["response"]
        op, status = request["op"], response["status"]
        require(
            row["op"] == op
            and row["path"] == request.get("path")
            and row["version"] in (1, 2)
            and integer(row["connection_id"], 1),
            "cache request identity mismatch",
        )
        require(
            op in ("ping", "open", "metadata", "read") and status in ("ready", "prepared", "data"),
            "cache protocol returned an error or unsupported operation",
        )
        if op == "ping":
            require(status == "ready", "ping was not ready")
        elif op == "open":
            require(
                status == "prepared"
                and request["handle"] == handle
                and response["image_handle"] == handle
                and response["digest"] == protocol["source_digest"],
                "cache open returned a different image",
            )
        else:
            identity = request.get("digest") if op == "read" else request.get("handle")
            require(status == "data" and identity == handle, "data read used a different image")
        length = response.get("length", 0) if status == "data" else 0
        require(
            integer(length) and integer(row["response_bytes"], length + 4),
            "invalid cache body/frame size",
        )
        content, metadata = (length if op == "read" else 0), (length if op == "metadata" else 0)
        require(
            row["content_bytes"] == content and row["metadata_bytes"] == metadata,
            "cache body classification differs",
        )
        body_hash = row["forwarded_body_sha256"]
        require(
            re.fullmatch(r"sha256:[a-f0-9]{64}", body_hash) is not None,
            "missing forwarded body hash",
        )
        if status == "data":
            require(
                row["server_body_sha256"] == response.get("sha256") == body_hash,
                "server/forwarded/cache response body hashes differ",
            )
        else:
            require(
                body_hash == EMPTY_SHA and row["server_body_sha256"] is None,
                "non-data response unexpectedly has payload",
            )
        require(
            isinstance(row["elapsed_ms"], (int, float))
            and math.isfinite(row["elapsed_ms"])
            and row["elapsed_ms"] >= 0,
            "invalid cache observer duration",
        )
        for key, amount in (
            ("content_bytes", content),
            ("metadata_bytes", metadata),
            ("response_bytes", row["response_bytes"]),
        ):
            totals[key] += amount
    return observed["rows"], totals


def frozen_script(batch, evidence):
    tree = ast.parse(evidence.read(batch / "harness/stage_lazy_compare.py"))
    candidates = [
        node.value.value
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        and isinstance(node.value, ast.Constant)
        and isinstance(node.value.value, str)
        and node.value.value.startswith("set -eu\ntarget=$1\nkey=$2\n")
    ]
    require(len(candidates) == 1, "could not identify the frozen OverlayBD subject wrapper")
    return candidates[0]


def command(directory, row, protocol, handle, script, evidence, tokens):
    argv = evidence.json(directory / "timed/argv.json")
    require(argv[:2] == ["docker", "exec"], "subject did not use the common Docker wrapper")
    env, index = {}, 2
    while index < len(argv) and argv[index] == "-e":
        key, separator, value = argv[index + 1].partition("=")
        require(separator and key not in env, "invalid/duplicate subject environment")
        env[key] = value
        index += 2
    require(
        argv[index : index + 4]
        == [
            protocol["container"],
            "/lab/tools/taskset",
            "-c",
            ",".join(map(str, protocol["cpus"])),
        ],
        "subject container or CPU affinity differs",
    )
    name = f"{row['phase']}-{row['round']:02d}-{row['group']}-{row['cache']}"
    inside = "/lab/" + Path(protocol["batch"]).name
    target = inside + "/trials/" + name
    cache = inside + f"/clients/{row['phase']}-{row['round']:02d}-{row['group']}"
    expected = dict(
        PVISOR_STARTUP_TIMING="0",
        PVISOR_FS_PROFILE="0",
        PVISOR_LAZY_IMAGE_V2="1",
        PVISOR_LAZY_INDEX_PAGES="1",
        XDG_CACHE_HOME=cache,
        XDG_CONFIG_HOME=target + "/config",
        PVISOR_RUN_HOME=target + "/runs",
        PVISOR_IMAGE_STORE=cache + "/store",
    )
    require(
        set(env) == set(expected) | {"PVISOR_CACHE_TOKEN"}
        and all(env[key] == value for key, value in expected.items()),
        "subject environment differs from fixed client paths",
    )
    require(re.fullmatch(r"[a-f0-9]{48}", env["PVISOR_CACHE_TOKEN"]), "invalid private cache token")
    tokens.add(env["PVISOR_CACHE_TOKEN"])
    require(len(tokens) == 1, "cache authentication context changed within the cohort")
    common = [
        EXECUTABLE,
        "run",
        "--executor",
        "host",
        "--no-agent-defaults",
        "--overlaynet",
        "off",
        "--gateway-mode",
        "off",
        "--stdio",
        "inherit",
        "--timeout",
        "30s",
        "--stage",
        target + "/stage",
        "--",
        *protocol["workload_argv"],
    ]
    if row["group"] == "pvisor":
        invocation = [
            MOUNTER,
            "unix://" + inside + "/proxy.sock",
            handle,
            cache + "/store",
            target + "/mount.receipt",
            "--",
            *common,
        ]
    else:
        invocation = [
            "sh",
            "-ec",
            script,
            "sh",
            target,
            "stage-" + name,
            protocol["chain_id"],
            protocol["derived_reference"],
            MOUNTER,
            "--plain",
            target + "/lower",
            "-",
            target + "/mount.receipt",
            "--",
            *common,
        ]
    require(
        argv[index + 4 :] == invocation,
        "subject lower owner, image, stage path or common workload changed",
    )
    return target, cache


def mount_and_bundle(directory, row, result, protocol, target, cache, evidence):
    receipt = key_values(evidence.read(directory / "mount.receipt"))
    require(
        receipt == result["mount_receipt"] and integer(int(receipt["mount_ns"])),
        "mount receipt differs",
    )
    stage = key_values(evidence.read(directory / "mount.receipt.stage"))
    require(
        positive_ns(row["stage_completion_ns"])
        and int(stage["completion_ns"]) == row["stage_completion_ns"]
        and row["stage_completion_ns"] <= row["completion_ns"],
        "stage subphase differs from helper clock",
    )
    root = receipt["rootfs"]
    matches = []
    for line in evidence.read(directory / "mount.receipt.mountinfo").decode().splitlines():
        before, separator, after = line.partition(" - ")
        require(separator, "invalid mountinfo row")
        fields = before.split()
        if fields[4] == root:
            matches.append((line, fields, after.split()))
    require(
        len(matches) == 1
        and matches[0][0] == result["lower_mount"]
        and "ro" in matches[0][1][5].split(","),
        "lower is not its unique retained read-only mount",
    )
    line, fields, filesystem = matches[0]
    require(re.fullmatch(r"[0-9]+:[0-9]+", fields[2]), "mount lacks a device identity")
    removed = key_values(evidence.read(directory / "mount.receipt.unmounted"))
    require(removed == {"mountpoint_removed": "true"}, "lower owner did not report release")
    if row["group"] == "pvisor":
        require(
            filesystem[:2] == ["fuse", "pvisor-image"]
            and receipt["digest"] == protocol["source_digest"]
            and root.startswith(cache + "/store/.lazy-mount-"),
            "cache lower is not the expected host FUSE image",
        )
    else:
        require(
            filesystem[0] == "ext4"
            and root == target + "/lower"
            and receipt["digest"] == "overlaybd-view",
            "TurboOCI lower is not its owned ext4 mount",
        )
        mount = shlex.split(evidence.read(directory / "mount-command.sh").decode())
        require(
            len(mount) >= 5 and mount[:3] == ["mount", "-t", "bind"] and mount[4] == root,
            "snapshotter view mount does not match the lower",
        )
        match = re.fullmatch(
            r"/lab/containerd-root/io\.containerd\.snapshotter\.v1\.overlaybd/snapshots/([0-9]+)/block/mountpoint",
            mount[3],
        )
        require(match is not None, "snapshotter source escapes owned OverlayBD state")
        info = evidence.read(directory / "target-info.txt").decode()
        configured = re.findall(
            r"overlaybd//lab/containerd-root/io\.containerd\.snapshotter\.v1\.overlaybd/snapshots/([0-9]+)/block/config\.v1\.json",
            info,
        )
        require(
            configured and set(configured) == {match[1]},
            "TCMU target config is not the selected owned snapshot",
        )
        device = evidence.read(directory / "device.txt").decode().strip()
        require(
            re.fullmatch(r"/dev/sd[a-z]+", device) and filesystem[1] == device,
            "ext4 mount source differs from OverlayBD backstore device",
        )
    bundle = evidence.checked(directory / "stage/run-bundle.json", result["bundle_sha256"])
    run, filesystem_state = bundle["run"], bundle["filesystem"]
    require(
        run["state"] == "completed"
        and type(run["exit_code"]) is int
        and run["exit_code"] == 0
        and run["executor"]["isolation"] == "rootless_process"
        and run["command"] == protocol["workload_argv"]
        and bundle["safety"]["filesystem_changes_staged"] is True,
        "Bundle is not a successful common staged host task",
    )
    require(
        filesystem_state["state"] == "staged"
        and filesystem_state["target"] == root
        and filesystem_state["upper"] == target + "/stage/upper"
        and filesystem_state["changed_files"] == 0
        and filesystem_state["changes"] == []
        and filesystem_state.get("whiteouts", 0) == 0,
        "Bundle has mutations or a different workspace",
    )
    observed = bundle["run_observation"]["filesystem"]["paths"]
    for name, expected in protocol["source_files"].items():
        read = observed.get(name, {}).get("read", {})
        require(
            read.get("bytes_read") == expected["bytes"] and read.get("failed", 0) == 0,
            "Bundle did not observe the exact file bytes: " + name,
        )


def reset_pair(directory, protocol, baseline, evidence):
    receipt = evidence.json(directory / "reset-receipt.json")
    require(
        evidence.json(directory / "reset-return.json") == receipt, "returned reset receipt differs"
    )
    require(
        receipt.get("valid") is True
        and receipt.get("phase") == "empty-client-ready"
        and receipt["lab_id"] == protocol["lab_id"]
        and receipt["reset_paths"] == ["containerd-root", "cache", "run"]
        and all(
            receipt.get(key) is True
            for key in (
                "empty_directories_verified",
                "registry_process_restarted",
                "registry_metadata_rechecked_outside_client",
                "metadata_and_content_empty",
            )
        ),
        "cold reset was not complete",
    )
    affinity = receipt["service_affinity"]
    require(
        affinity["cpus"] == protocol["service_cpus"]
        and set(affinity["services"]) == {"registry", "tcmu", "snapshotter", "containerd"}
        and affinity["threads"],
        "reset service affinity lacks four-service proof",
    )
    require(
        all(
            cpus(thread["cpus"]) == set(protocol["service_cpus"]) for thread in affinity["threads"]
        ),
        "reset service thread used different CPUs",
    )
    for name in ("reset-before-stop", "reset-after-stop", "reset-after-start"):
        inventory(directory / name, baseline, evidence)
    commands = []
    for path in sorted(
        directory.glob("*-command/argv.json"),
        key=lambda path: int(path.parent.name.split("-", 1)[0]),
    ):
        argv = evidence.json(path)
        status = evidence.json(path.with_name("status.json"))
        success = (
            status.get("returncode") == 0
            and status.get("timed_out") is False
            and "error" not in status
        )
        require(
            success or argv[-2:] == ["plugins", "list"],
            "reset command failed outside health polling",
        )
        commands.append((argv, evidence.read(path.with_name("stdout.txt")), success))
        evidence.read(path.with_name("stderr.txt"))
    workers = [
        (argv, out) for argv, out, success in commands if success and argv[:2] == ["docker", "run"]
    ]
    require(
        len(workers) == 1 and receipt["worker_container"] in workers[0][0],
        "reset lacks its owned archive worker",
    )
    archive = workers[0][1].decode().splitlines()
    require(
        {line for line in archive if line.startswith("EMPTY ")}
        == {"EMPTY containerd-root", "EMPTY cache", "EMPTY run"},
        "reset worker did not prove three empty client directories",
    )
    preserved = [line for line in archive if line.startswith("PRESERVED ")]
    require(
        len(preserved) == 4
        and preserved[:2] == preserved[2:] == receipt["preserved_directory_identity"],
        "reset registry/conversion preservation differs from worker readback",
    )
    for resource in ("images", "content", "tasks", "containers", "leases"):
        empty = [
            out
            for argv, out, success in commands
            if success
            and argv[-2:] == [resource, "list"]
            and not any(line.strip() for line in out.decode().splitlines()[1:])
        ]
        require(empty, "reset did not retain empty metadata readback: " + resource)
    inventory(directory, baseline, evidence)


def background(batch, protocol, evidence):
    ticks = protocol.get("clock_ticks", os.sysconf("SC_CLK_TCK"))
    require(integer(ticks, 1), "invalid background clock tick frequency")
    threshold = max(1, int(ticks * 0.2))
    expected = set()
    for phase, count in (("warmup", 3), ("sample", 30)):
        for number in range(count):
            path = batch / f"background-{phase}-{number:02d}.json"
            expected.add(path.name)
            observed = evidence.json(path)
            before, after = observed["before"], observed["after"]
            flags = []
            for pid, process in after.items():
                old = before.get(pid)
                used = (
                    process["ticks"] - old["ticks"]
                    if old and old["starttime"] == process["starttime"]
                    else process["ticks"]
                )
                if (process["comm"] in BUILDERS and used > 0) or (
                    process["last_cpu"] in protocol["cpus"] and used >= threshold
                ):
                    flags.append(dict(pid=pid, used_ticks=used, **process))
            require(
                observed["interference"] == flags == [],
                "background interference invalidates this cohort",
            )
    require(
        {path.name for path in batch.glob("background-*.json")} == expected,
        "missing or extra background rounds",
    )
    return ticks


def audit(batch):
    """Independently recompute retained semantic gates; write audit.json."""
    batch = Path(batch).resolve(strict=True)
    evidence = Evidence()
    result = {"valid": False, "status": "failed", "scope": SCOPE}
    try:
        protocol = evidence.json(batch / "protocol.json")
        require(
            protocol["scope"] == SCOPE
            and protocol["groups"] == list(GROUPS)
            and protocol["runs"] == 30
            and protocol["warmups"] == 3,
            "unexpected cohort scope",
        )
        require(
            list(protocol["source_files"]) == list(FILES)
            and sum(item["bytes"] for item in protocol["source_files"].values()) == 2575738,
            "source file selection changed",
        )
        expected = "".join(
            item["sha256"] + "  " + name + "\n" for name, item in protocol["source_files"].items()
        )
        probe = (
            "printf %s "
            + shlex.quote(expected)
            + " | sha256sum -c - >/dev/null; printf "
            + shlex.quote(MARKER.decode())
        )
        require(
            protocol["workload_argv"] == ["/bin/sh", "-ec", probe]
            and protocol["marker"] == MARKER.decode(),
            "probe does not validate all four expected file hashes before Ready",
        )
        require(
            evidence.read(batch / "expected.sha256").decode() == expected,
            "expected file digest receipt differs",
        )
        require(
            isinstance(protocol["artifacts"], dict) and protocol["artifacts"],
            "missing frozen artifacts",
        )
        for name, checksum in protocol["artifacts"].items():
            require(digest(evidence.read(name)) == checksum, "frozen artifact changed: " + name)
        source = evidence.checked(batch / "source-manifest.json", protocol["source_digest"][7:])
        derived = evidence.checked(batch / "derived-manifest.json", protocol["derived_digest"][7:])
        frozen = evidence.json(batch / "frozen-images.json")
        require(
            frozen["source_digest"] == protocol["source_digest"]
            and frozen["derived_digest"] == protocol["derived_digest"]
            and frozen["derived_reference"] == protocol["derived_reference"],
            "frozen image identity differs",
        )
        roles = blob_roles(source, derived)
        prepared = evidence.json(batch / "cache-prepared.json")
        require(prepared["digest"] == protocol["source_digest"], "cache prepared a different image")
        handle = prepared["image_handle"]
        root = batch / "service-store/rootfs-v3/sha256" / protocol["source_digest"][7:]
        for name, expected_file in protocol["source_files"].items():
            path = root / name
            require(
                not any(parent.is_symlink() for parent in (path, *path.parents)),
                "source file path follows a symlink",
            )
            body = evidence.read(path)
            require(
                len(body) == expected_file["bytes"] and digest(body) == expected_file["sha256"],
                "source file changed: " + name,
            )
        baseline = evidence.json(batch / "host-before.json")
        require(
            not baseline["errors"] and not baseline["targets"], "invalid kernel-object baseline"
        )
        planned = []
        for phase, count in (("warmup", 3), ("sample", 30)):
            orders = protocol["orders"][phase]
            require(len(orders) == count, "wrong paired round count")
            for number, order in enumerate(orders):
                require(sorted(order) == sorted(GROUPS), "round is not a two-group permutation")
                planned.extend((phase, number, group, cache) for group in order for cache in CACHES)
        records = evidence.json(batch / "records.json")
        require(
            isinstance(records, list) and len(records) == 132, "complete 132-record cohort required"
        )
        script = frozen_script(batch, evidence)
        tokens, pairs, intervals = set(), set(), []
        for row, expected_row in zip(records, planned, strict=True):
            require(
                (row["phase"], row["round"], row["group"], row["cache"]) == expected_row
                and row["valid"] is True,
                "invalid or out-of-order trial",
            )
            name = f"{row['phase']}-{row['round']:02d}-{row['group']}-{row['cache']}"
            directory = batch / "trials" / name
            require(
                Path(row["evidence_dir"]).resolve(strict=True) == directory.resolve(),
                "trial evidence path differs",
            )
            trial = evidence.json(directory / "result.json")
            require(
                all(trial.get(key) == value for key, value in row.items()),
                "trial result differs from coordinator projection",
            )
            timing = evidence.json(directory / "timed/timing.json")
            require(
                timing == trial["timing"]
                and timing.get("valid") is True
                and timing.get("clock") == "perf_counter_ns",
                "raw timer/result mismatch",
            )
            interval = clock(timing, "start_ns", "marker_ns", "end_ns")
            require(
                all(row[key] == timing[key] for key in ("ready_ns", "completion_ns")),
                "durations differ from raw clock",
            )
            require(
                not intervals or interval[0] >= intervals[-1][1], "subject clock intervals overlap"
            )
            intervals.append(interval)
            require(
                evidence.read(directory / "timed/stdout.txt") == MARKER,
                "stdout is not the unique verified marker",
            )
            evidence.read(directory / "timed/stderr.txt")
            target, cache = command(directory, row, protocol, handle, script, evidence, tokens)
            mount_and_bundle(directory, row, trial, protocol, target, cache, evidence)
            evidence.checked(directory / "requests.json", trial["requests_sha256"])
            evidence.checked(directory / "registry-responses.json", trial["registry_sha256"])
            requests, observed = cache_requests(directory, protocol, handle, evidence)
            traffic = registry(directory, roles, evidence)
            if row["group"] == "pvisor":
                require(
                    traffic["response_body_bytes"] == 0,
                    "cache lower unexpectedly used the registry",
                )
            else:
                require(requests == [], "OverlayBD unexpectedly used the cache API")
                observed = dict(
                    content_bytes=traffic["content_body_bytes"],
                    metadata_bytes=traffic["metadata_body_bytes"],
                    response_bytes=traffic["response_body_bytes"],
                )
            require(
                all(row[key] == value for key, value in observed.items()),
                "trial transfer totals differ from retained requests",
            )
            require(
                observed["content_bytes"] > 0
                if row["cache"] == "cold"
                else observed["content_bytes"] == 0,
                "cold content/warm cache reuse gate failed",
            )
            inventory(directory, baseline, evidence)
            pair_key = row["phase"], row["round"], row["group"]
            if pair_key not in pairs:
                pair = batch / "pairs" / f"{row['phase']}-{row['round']:02d}-{row['group']}"
                if row["group"] == "turbo":
                    reset_pair(pair, protocol, baseline, evidence)
                else:
                    inventory(pair, baseline, evidence)
                pairs.add(pair_key)
        ticks = background(batch, protocol, evidence)
        teardown = evidence.json(batch / "teardown.json")
        require(
            teardown.get("cache_service_exit") in (0, -signal.SIGTERM)
            and teardown.get("frozen_artifacts_unchanged") is True,
            "owned service teardown failed or artifacts changed",
        )
        require(not (batch / "failure.txt").exists(), "cohort retained a measurement failure")
        evidence.finish()
        result.update(
            valid=True,
            status="passed",
            trials=132,
            pairs=len(pairs),
            background_rounds=33,
            source_digest=protocol["source_digest"],
            derived_digest=protocol["derived_digest"],
            background_clock_ticks=ticks,
            background_clock_ticks_frozen="clock_ticks" in protocol,
            limitations=[
                "Cache framed totals sum retained observer counters; original serialized response headers were not saved.",
                "Body hash agreement is retained observer evidence; delivered file correctness follows the SHA256 probe and Bundle.",
                "Only completion traffic is audited; no Ready boundary or wire-overhead reconstruction is claimed.",
                "Mount/target/device evidence proves retained identities, not an unrecorded intermediate kernel-cache state.",
            ],
        )
    except BaseException as error:
        result["error"] = str(error)
        raise
    finally:
        result["evidence_sha256"] = evidence.files
        result["auditor_sha256"] = digest(Path(__file__).read_bytes())
        (batch / "audit.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    try:
        result = audit(args.batch)
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(1, f"Audit refused: {error}\n")
    print(
        json.dumps(
            {
                key: result[key]
                for key in ("valid", "status", "trials", "pairs", "background_rounds")
            }
        )
    )


if __name__ == "__main__":
    sys.exit(main())
