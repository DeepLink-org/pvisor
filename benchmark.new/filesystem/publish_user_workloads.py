#!/usr/bin/env python3
"""Publish a fresh B-FS-TOOLS layered cross-mode cohort without editing prose.

Raw observations, the frozen protocol, actual semspec execution and recomputed
analysis must agree. Only current reference levels/Ready/failures and SVGs are
public downloads; complete tables and provenance are archived beside the batch.
Failed cells retain their evidence and receive no median; a partial publication
explicitly reports that state. Engineering cohorts cannot
be inputs. Use --destination-root /tmp/NEW to stage a reviewable publication.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import shlex
import sys
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import user_workloads_analysis as ANALYSIS  # noqa: E402 - import after module-loading setup
from public_assets import csv_path, is_public_csv  # noqa: E402 - import after module-loading setup
from publication_archive import archive_targets, preflight_archive, write_archive  # noqa: E402 - import after module-loading setup

CASES = (
    [f"M{i:02}" for i in range(1, 13)] + [f"R{i:02}" for i in range(1, 5)] + ["S01", "S02", "S03"]
)
BACKENDS = ["native", "host", "staged", "vm", "vm-writeback"]
REFERENCES = ["docker", "fc-system", "qemu", "qemu-microvm"]
REFERENCE_BACKENDS = BACKENDS + REFERENCES
TABLES = dict(
    levels="levels",
    ready="ready",
    environments="environments",
    raw_rows="rows",
    failures="failures",
)
STEMS = ("workflow-rounds", "mechanism-relative-time", "resource-workers", "write-request-size")
FIGURES = [f"{stem}.{extension}" for stem in STEMS for extension in ("svg", "pdf")]
PREFIX = "filesystem-layered"
REFERENCE_PREFIX = "filesystem-layered-references"


def require(value, message):
    if not value:
        raise ValueError(message)


def publication_backends(protocol):
    """Keep the frozen five-mode cohort separate from the fresh reference cohort."""
    modes = [mode["id"] for mode in protocol.get("backends", [])]
    require(
        modes in (BACKENDS, REFERENCE_BACKENDS),
        "publication requires the fixed five-mode or nine-mode inventory",
    )
    return modes


def publication_prefix(protocol):
    return REFERENCE_PREFIX if publication_backends(protocol) == REFERENCE_BACKENDS else PREFIX


def validate_references(protocol):
    """Reference configuration is evidence; differing storage is not a VMM-only result."""
    if publication_backends(protocol) == BACKENDS:
        return
    references = protocol.get("reference_configuration")
    require(
        isinstance(references, dict)
        and set(references) == set(REFERENCES)
        and all(isinstance(value, dict) and value for value in references.values()),
        "nine-mode publication requires configuration evidence for every reference runtime",
    )


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load(path):
    return json.loads(Path(path).read_text())


def inside(root, relative):
    relative = Path(relative)
    require(
        not relative.is_absolute()
        and ".." not in relative.parts
        and str(relative) not in ("", "."),
        "unsafe relative evidence path",
    )
    path = (root / relative).resolve()
    require(path.is_relative_to(root), "evidence escapes the raw batch")
    return path


def csv_bytes(rows):
    fields = list(dict.fromkeys(key for row in rows for key in row)) or ["no_records"]
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    for row in rows:
        writer.writerow(
            {
                key: json.dumps(value, separators=(",", ":"))
                if isinstance(value, (list, dict))
                else value
                for key, value in row.items()
            }
        )
    return stream.getvalue().encode()


def public_host(metadata):
    """Retain platform and mount properties while leaving host paths local."""
    result = {key: value for key, value in metadata.items() if key != "filesystem"}
    filesystem = metadata.get("filesystem")
    if isinstance(filesystem, str):
        try:
            filesystem = json.loads(filesystem)
        except json.JSONDecodeError:
            filesystem = None
    if isinstance(filesystem, dict):
        result["filesystems"] = [
            {key: mount[key] for key in ("fstype", "options") if key in mount}
            for mount in filesystem.get("filesystems", [])
        ]
    return result


def manifest_summary(artifacts):
    """Bind the local manifest without copying its complete path inventory."""
    return dict(
        files=len(artifacts),
        sha256=hashlib.sha256(json.dumps(artifacts, sort_keys=True).encode()).hexdigest(),
    )


def public_reference_configuration(configurations, batch):
    """Publish runtime/storage facts and input hashes; retain launch paths locally."""
    if configurations is None:
        return None
    common = (
        "executor",
        "guest_vcpus",
        "guest_memory_mib",
        "storage",
        "transport",
        "userspace",
        "durability",
        "device_cache",
        "network",
    )
    docker = (
        "storage_driver",
        "cpu_placement",
        "output",
        "input",
        "ready",
        "limitation",
        "actual_parent_limits",
        "runtime_cgroup_observation",
        "image_id",
        "runtime_sha256",
        "runtime_source_sha256",
        "image_inspection_sha256",
        "setup_sha256",
    )
    kernel = (
        "origin",
        "distribution",
        "package",
        "package_version",
        "source_url",
        "pvisor_firmware",
        "kernel_version",
        "version",
    )
    result = {}
    for backend, configuration in configurations.items():
        item = {key: configuration[key] for key in common + docker if key in configuration}
        if backend == "docker":
            for private_key, public_key in (
                ("runtime_sha256", "launcher_sha256"),
                ("runtime_source_sha256", "launcher_source_sha256"),
            ):
                if private_key in item:
                    item[public_key] = item.pop(private_key)
        if "kernel" in configuration:
            value = configuration["kernel"]
            item["kernel"] = (
                {key: value[key] for key in kernel if key in value}
                if isinstance(value, dict)
                else value
            )
        runtime = configuration.get("runtime")
        if isinstance(runtime, dict):
            item["runtime"] = {key: runtime[key] for key in ("sha256", "version") if key in runtime}
        boot = configuration.get("boot_firmware")
        if isinstance(boot, dict) and isinstance(boot.get("path"), str) and "sha256" in boot:
            item["boot_firmware"] = dict(name=Path(boot["path"]).name, sha256=boot["sha256"])
        assets = configuration.get("assets")
        if isinstance(assets, dict):
            artifacts = assets.get("artifacts", {})

            def asset_hash(path):
                if not isinstance(path, str):
                    return None
                value = Path(path)
                if value.is_absolute():
                    try:
                        value = value.relative_to(batch)
                    except ValueError:
                        return None
                return artifacts.get(str(value))

            item["assets_summary"] = {
                **{
                    key: assets[key]
                    for key in ("schema", "root_disk_bytes", "work_disk_bytes")
                    if key in assets
                },
                "manifest": manifest_summary(artifacts),
                "kernel_sha256": {
                    key: hashed
                    for key, path in assets.get("kernel_paths", {}).items()
                    if (hashed := asset_hash(path))
                },
                "disk_sha256": {
                    key: hashed
                    for key in ("root_disk", "work_disk")
                    if (hashed := asset_hash(assets.get(key)))
                },
            }
            receipt_hash = asset_hash(
                configuration.get("kernel_receipt", assets.get("kernel_receipt"))
            )
            if receipt_hash:
                item["assets_summary"]["kernel_receipt_sha256"] = receipt_hash
            init_hashes = {
                Path(key).name: value
                for key, value in artifacts.items()
                if Path(key).name in ("init", "init.c")
            }
            if init_hashes:
                item["assets_summary"]["minimal_init_sha256"] = init_hashes
        result[backend] = item
    return result


def public_reference_runtime_observation(observation):
    """Expose Docker versions without client context, socket, paths or environment."""
    docker = observation.get("docker_version")
    if isinstance(docker, str):
        docker = json.loads(docker)
    require(isinstance(docker, dict), "reference runtime observation has no Docker version object")
    result = {}
    fields = ("Version", "ApiVersion", "MinAPIVersion", "Os", "Arch")
    for kind in ("Client", "Server"):
        data = docker.get(kind)
        require(
            isinstance(data, dict),
            "reference runtime observation has no Docker " + kind + " version",
        )
        result[kind] = {key: data[key] for key in fields if key in data}
        components = data.get("Components")
        if isinstance(components, list):
            result[kind]["Components"] = [
                {key: component[key] for key in ("Name", "Version") if key in component}
                for component in components
                if isinstance(component, dict)
            ]
    return dict(docker_version=result)


def validate_execution(batch, status, records):
    """Require the actual single semspec command and its retained result log."""
    template = load(batch / "in.json")
    require(set(template) == {"filesystem-user-layered"}, "unexpected semspec entry point")
    expression = template["filesystem-user-layered"]
    require(
        isinstance(expression, str) and expression.startswith("$ "),
        "missing semspec command expression",
    )
    command = expression[2:]
    argv = shlex.split(command)
    require(
        len(argv) == 4
        and Path(argv[1]).resolve() == batch / "harness/user_workloads.py"
        and argv[2] == "--measure"
        and Path(argv[3]).resolve() == batch,
        "semspec command does not execute the frozen user cohort",
    )
    bench, rendered = load(batch / "in.bench.json"), load(batch / "in.render.json")
    require(
        bench.get("format") == 1
        and len(bench.get("commands", [])) == len(bench.get("rows", [])) == 1,
        "actual semspec execution missing",
    )
    executed, row = bench["commands"][0], bench["rows"][0]
    expected = dict(
        trials=len(records), failed=sum(record.get("valid") is not True for record in records)
    )
    require(
        status.get("trials") == expected["trials"]
        and status.get("failed") == expected["failed"]
        and status.get("excluded") == 0,
        "campaign status differs from retained records",
    )
    require(
        Path(bench.get("template", "")).resolve() == batch / "in.json"
        and Path(bench.get("cwd", "")).resolve() == batch
        and executed.get("command") == row.get("command") == command
        and executed.get("kind") == "script-row"
        and executed.get("raw") == executed.get("value") == expected
        and row.get("row") == "filesystem-user-layered"
        and rendered == {"filesystem-user-layered": expected},
        "semspec execution/result does not match retained user observations",
    )
    log = Path(executed.get("log", "")).resolve()
    require(
        log.parent == batch / "in.bench.logs" and load(log) == expected,
        "actual semspec command log missing or changed",
    )


def validate_inputs(batch, protocol):
    artifacts = protocol.get("artifacts", {})
    required = (
        "bin/payload",
        "bin/libkrunfw.so",
        "harness/user_workloads.py",
        "harness/user_workloads_backends.py",
        "harness/user_workloads_vm.py",
        "harness/user_workloads_analysis.py",
        "harness/workloads_v2_guest.rs",
        "harness/workloads_v2_scenarios.rs",
        "toolroot-receipt.json",
        "cli-build-receipt.json",
        "firmware-source.json",
    )
    require(
        all(name in artifacts for name in required), "frozen artifact/receipt inventory incomplete"
    )
    for relative, expected in artifacts.items():
        require(sha(inside(batch, relative)) == expected, "frozen input changed: " + relative)
    require(
        sha(HERE / "user_workloads_analysis.py") == artifacts["harness/user_workloads_analysis.py"],
        "available analyzer differs from this cohort's frozen analyzer",
    )
    require(
        sha(batch / "fixture-manifest.json") == protocol.get("fixture_manifest_sha256"),
        "fixture manifest changed",
    )
    for relative, expected in load(batch / "fixture-manifest.json").items():
        require(
            sha(inside(batch / "fixture-layer", relative)) == expected,
            "frozen fixture changed: " + relative,
        )
    toolroot = Path(protocol["toolroot"]).resolve()
    tool_receipt = load(batch / "toolroot-receipt.json")
    require(
        tool_receipt.get("benchmark_id") == "B-FS-TOOLS"
        and Path(tool_receipt["toolroot"]).resolve() == toolroot,
        "prepared toolroot receipt belongs to another experiment",
    )
    installed = {item["path"]: item for item in tool_receipt["installed"]}
    for item in tool_receipt["installed"] + tool_receipt["runtime_dependencies"]:
        require(
            sha(inside(toolroot, item["path"])) == item["sha256"],
            "executed toolroot input changed: " + item["path"],
        )
    for relative, expected in protocol["prepared_tools"].items():
        require(
            sha(inside(toolroot, relative)) == expected, "prepared common tool changed: " + relative
        )
    require(
        installed.get("bench-artifacts/workload", {}).get("sha256") == sha(batch / "bin/payload")
        and installed.get("bench-artifacts/libkrunfw.so.5.6.2", {}).get("sha256")
        == sha(batch / "bin/libkrunfw.so"),
        "installed payload or selected firmware differs from frozen bytes",
    )
    firmware, source = load(batch / "firmware-source.json"), load(batch / "cli-build-receipt.json")
    require(
        firmware.get("firmware_sha256") == sha(batch / "bin/libkrunfw.so"),
        "firmware differs from SOURCE receipt",
    )
    require(
        source.get("state") == "built"
        and isinstance(source.get("source_hashes"), dict)
        and source.get("head"),
        "actual CLI source/build receipt missing",
    )
    for name, record in source["artifacts"].items():
        filename = "pvisor-guest" if name == "embedded-guest" else name
        require(
            installed.get("bench-artifacts/" + filename, {}).get("sha256") == record["sha256"],
            "executed CLI artifact differs from its source build receipt: " + name,
        )
    return tool_receipt, firmware, source


def validate_records(batch, protocol, records):
    orders = protocol.get("orders", [])
    planned = [
        (order["case_id"], backend, order["pair"])
        for order in orders
        for backend in order["backends"]
    ]
    modes = publication_backends(protocol) if "backends" in protocol else BACKENDS
    expected = {
        (case, backend, sample) for case in CASES for backend in modes for sample in (1, 2, 3)
    }
    require(
        len(planned) == len(expected) and set(planned) == expected,
        "predeclared schedule differs from the full case/mode/environment inventory",
    )
    observed = [(record["case_id"], record["backend"], record["pair"]) for record in records]
    require(
        len(set(observed)) == len(observed) and set(observed) <= expected,
        "unexpected or duplicated raw environment",
    )
    require(
        observed == [key for key in planned if key in set(observed)],
        "observed environment order differs from the frozen schedule",
    )
    for record in records:
        folder = inside(batch, record["folder"])
        require(
            folder
            == batch / "trials" / f"{record['case_id']}-{record['pair']:03}-{record['backend']}",
            "unexpected raw environment evidence path",
        )
        require(
            load(folder / "result.json") == record,
            "raw environment receipt differs from records.json",
        )
        stdout = (folder / "stdout.log").read_text()
        require((folder / "stderr.log").is_file(), "raw environment stderr missing")
        if record.get("valid") is not True:
            continue
        rows = [
            json.loads(line.split(" ", 1)[1])
            for line in stdout.splitlines()
            if line.startswith("WORKLOADS_V2_RESULT ")
        ]
        require(
            rows == record["rows"]
            and stdout.count("WORKLOADS_V2_READY ") == 1
            and stdout.count("WORKLOADS_V2_DONE ") == 1
            and stdout.count("USER_WORKLOADS_LAUNCH ") == 1,
            "accepted environment differs from actual workload output",
        )
        require(
            "pvisor-fs-profile " not in (folder / "stderr.log").read_text(),
            "diagnostic run in formal timing",
        )
        require(
            record.get("returncode") == 0
            and record.get("timed_out") is False
            and record.get("host_verified") is True,
            "valid environment lacks successful exit/host oracle",
        )
        if record["backend"] in ("staged", "vm", "vm-writeback"):
            require(
                record.get("original_workspace_unchanged") is True,
                "staged input isolation not verified",
            )
        verified = [
            json.loads(line.split(" ", 1)[1])
            for line in (folder / "host-verification.log").read_text().splitlines()
            if line.startswith("WORKLOADS_V2_VERIFIED ")
        ]
        require(
            len(verified) == 1
            and verified[0].get("verified") is True
            and record.get("host_verification") == verified[0],
            "independent retained-state oracle missing",
        )


def validate(batch, directory):
    protocol, loaded = load(batch / "protocol.json"), load(batch / "records.json")
    require(
        protocol.get("benchmark_id") == "B-FS-TOOLS"
        and protocol.get("role") == "user-facing"
        and protocol.get("samples") == 3
        and protocol.get("rounds") == 3
        and protocol.get("sample_index_base") == 1,
        "publisher accepts the new N3 user-facing cohort only; engineering observations are excluded",
    )
    require(
        protocol.get("cases") == CASES
        and [case["id"] for case in protocol.get("catalog", [])] == CASES,
        "publication requires the fixed 19-case inventory",
    )
    publication_backends(protocol)
    validate_references(protocol)
    records = loaded.get("records") if isinstance(loaded, dict) else loaded
    require(isinstance(records, list), "raw environment array missing")
    status = load(batch / "status.json")
    require(
        status.get("benchmark_id") == "B-FS-TOOLS" and status.get("role") == "user-facing",
        "campaign receipt belongs to another benchmark role",
    )
    tool_receipt, firmware, source = validate_inputs(batch, protocol)
    validate_execution(batch, status, records)
    validate_records(batch, protocol, records)
    summary, fresh = load(directory / "summary.json"), ANALYSIS.summarize(protocol, records)
    bound = dict(
        protocol_sha256=sha(batch / "protocol.json"),
        records_sha256=sha(batch / "records.json"),
        analyzer_sha256=protocol["artifacts"]["harness/user_workloads_analysis.py"],
    )
    require(
        summary.get("provenance") == bound, "analysis is stale or bound to different raw evidence"
    )
    require(
        all(summary.get(key) == value for key, value in fresh.items()),
        "derived analysis differs from raw-record recomputation",
    )
    require(
        status.get("measurements")
        == sum(len(r.get("rows", [])) for r in records if r.get("valid") is True),
        "campaign valid-row count differs from retained records",
    )
    require(
        status.get("complete") == (status["failed"] == 0 and status.get("skipped", 0) == 0),
        "campaign complete flag differs from failures/skips",
    )
    for table in TABLES:
        require(
            (directory / (table + ".csv")).read_bytes() == csv_bytes(fresh[table]),
            "CSV differs from fresh analysis: " + table,
        )
    require(
        summary.get("plots") == FIGURES,
        "SVG/PDF figure inventory differs from the registered four plots",
    )
    for name in FIGURES:
        data = (directory / name).read_bytes()
        require(
            data.startswith(b"%PDF-") if name.endswith(".pdf") else b"<svg" in data[:3000],
            "invalid figure: " + name,
        )
    complete = fresh["status"] == "complete_exploratory"
    return protocol, summary, bound, tool_receipt, firmware, source, complete


def publish(batch, directory=None, destination_root=REPO, *, check=False):
    batch = Path(batch).resolve()
    directory = Path(directory).resolve() if directory else batch / "analysis"
    destination_root = Path(destination_root).resolve()
    require(
        ".data" in batch.parts and ".data" in directory.parts,
        "raw evidence and analysis belong under .data",
    )
    protocol, summary, bound, tool_receipt, firmware, source, complete = validate(batch, directory)
    prefix = publication_prefix(protocol)
    status = load(batch / "status.json")
    loaded = load(batch / "records.json")
    records = loaded.get("records") if isinstance(loaded, dict) else loaded
    outputs, archived = {}, {}
    for table, suffix in TABLES.items():
        data = (directory / (table + ".csv")).read_bytes()
        name = f"{prefix}-{suffix}.csv"
        archived[name] = data
        if (
            prefix == REFERENCE_PREFIX
            and suffix in ("levels", "ready", "failures")
            and is_public_csv(name)
        ):
            target = csv_path(REPO, name)
            outputs[str(target.relative_to(REPO))] = data
    asset_root = "docs/overrides/assets/benchmarks/" + prefix
    for name in FIGURES:
        archived[name] = (directory / name).read_bytes()
        if name.endswith(".svg"):
            outputs[asset_root + "/" + name] = archived[name]
    inputs = (
        "protocol.json",
        "records.json",
        "status.json",
        "in.json",
        "in.bench.json",
        "in.render.json",
        "fixture-manifest.json",
        "toolroot-receipt.json",
        "cli-build-receipt.json",
        "firmware-source.json",
    )
    if (batch / "skipped.json").is_file():
        inputs += ("skipped.json",)
    host_observation = None
    if (batch / "host-observation.json").is_file():
        inputs += ("host-observation.json",)
        observed = load(batch / "host-observation.json")
        host_observation = {
            name: observed.get(name) for name in ("phase", "observed_unix", "cpu_model", "uname")
        }
    runtime_observation = None
    if (batch / "reference-runtime-observation.json").is_file():
        inputs += ("reference-runtime-observation.json",)
        runtime_observation = public_reference_runtime_observation(
            load(batch / "reference-runtime-observation.json")
        )
    observed_guest_memory = {}
    for backend in (
        mode["id"]
        for mode in protocol["backends"]
        if mode["id"] in ("vm", "vm-writeback", "fc-system", "qemu", "qemu-microvm")
    ):
        eligible = [
            record
            for record in records
            if record.get("backend") == backend and record.get("valid") is True
        ]
        values = [
            record.get("ready_record", {}).get("actual_guest_memtotal_kib") for record in eligible
        ]
        measured = [value for value in values if type(value) is int and value > 0]
        observed_guest_memory[backend] = dict(
            configured_guest_memory_mib=protocol["budget"].get("guest_mib"),
            configured_value_scope="requested VM allocation; distinct from guest-visible MemTotal",
            observed_ready_memtotal_kib=sorted(set(measured)),
            valid_environments=len(eligible),
            environments_reporting_memtotal=len(measured),
        )
    limitations = list(protocol.get("limitations", [])) + [
        "Native/host share a 2 GiB parent budget; VM has 512 MiB guest RAM inside that same parent. These are different memory/cache boundaries.",
        "Resource WSS uses a fixed 512 MiB reference across modes, not measured native RAM; 128/640 MiB anchors also change retained bytes and final synchronization volume.",
        "Full round validation can warm pages for later resident rounds; no cold-disk or independent-round claim.",
        "Declared work includes periodic fsync/checked closes; completed adds final synchronization. Setup, validation, startup and teardown are separate.",
        "Directory rootfs has no authenticated Immutable generation; user results do not establish directory-image or lower-data-cache coverage.",
        "Formal timing disables profiling; no per-op request elimination, dirty-page/queue peaks, tail latency or power-loss durability is inferred.",
        "Mode levels are descriptive N3 exploration; no composite system score or paired causal optimization estimate.",
    ]
    if prefix == REFERENCE_PREFIX:
        limitations += [
            "Docker overlay2 output with readonly bind input, reference VM ext4 block storage and pVisor staged/virtio-fs are different storage paths. Cross-mode timing includes those configured paths; it does not isolate VMM overhead.",
            "Reference VM stock kernels differ from the pVisor guest kernel and firmware; configured memory and observed guest MemTotal are reported separately.",
        ]
    provenance = dict(
        schema=1,
        benchmark_id="B-FS-TOOLS",
        role="user-facing",
        batch=batch.name,
        state="passed" if complete else "partial",
        scope="fresh cross-mode layered filesystem cohort; 12 mechanisms, four resource anchors, three separate complete workflows",
        counts=summary["counts"],
        complete_cells=summary["complete_cells"],
        sampling=summary["statistical_scope"],
        host_observation=host_observation,
        reference_runtime_observation=runtime_observation,
        modes=protocol["backends"],
        vm_configuration=protocol.get("vm_configuration"),
        reference_configuration=public_reference_configuration(
            protocol.get("reference_configuration"), batch
        ),
        frozen_artifact_manifest=manifest_summary(protocol["artifacts"]),
        configuration_scope="declared launch inputs; requested cache/writeback settings are not independent runtime-negotiation measurements",
        execution=dict(
            trial_timeout_seconds=protocol.get("trial_timeout_seconds"),
            skip_remaining_on_case_failure=protocol.get("skip_remaining_on_case_failure", False),
            attempted=status.get("trials"),
            valid=status.get("trials", 0) - status.get("failed", 0),
            failed=status.get("failed"),
            skipped=status.get("skipped", 0),
            failure_policy=protocol.get("failures"),
        ),
        observed_guest_memory=observed_guest_memory,
        host=public_host(protocol.get("host", {})),
        budget=protocol["budget"],
        resource_scale=protocol["resource_scale"],
        timing=protocol["timing"],
        cache=protocol["cache"],
        limitations=limitations,
        gates=dict(
            quiet=protocol.get("quiet_gate"),
            interference=protocol.get("interference_gate"),
            inventory=protocol.get("inventory_gate"),
        ),
        validation="guest round oracle, checked completion and independent host final retained-state oracle; physical overlay markers strictly interpreted",
        source=dict(
            head=source["head"],
            build_profile=source.get("build_profile"),
            source_files=len(source["source_hashes"]),
            source_hashes_sha256=hashlib.sha256(
                json.dumps(source["source_hashes"], sort_keys=True).encode()
            ).hexdigest(),
            build_receipt_sha256=sha(batch / "cli-build-receipt.json"),
            artifacts={name: item["sha256"] for name, item in source["artifacts"].items()},
        ),
        firmware=dict(
            sha256=sha(batch / "bin/libkrunfw.so"),
            source_receipt_sha256=sha(batch / "firmware-source.json"),
            build_id=firmware.get("build_id"),
            actual_config_sha256=firmware.get("actual_config_sha256"),
        ),
        payload=dict(
            sha256=sha(batch / "bin/payload"),
            source_sha256={
                name: protocol["artifacts"][name]
                for name in ("harness/workloads_v2_guest.rs", "harness/workloads_v2_scenarios.rs")
            },
        ),
        toolroot=dict(
            receipt_sha256=sha(batch / "toolroot-receipt.json"),
            prepared_tools=protocol["prepared_tools"],
            installed={item["path"]: item["sha256"] for item in tool_receipt["installed"]},
            runtime_dependencies={
                item["path"]: item["sha256"] for item in tool_receipt["runtime_dependencies"]
            },
        ),
        inputs={name: sha(batch / name) for name in inputs},
        analysis=dict(summary_sha256=sha(directory / "summary.json"), **bound),
        publisher_sha256=sha(__file__),
        archive_outputs={name: hashlib.sha256(data).hexdigest() for name, data in archived.items()},
        outputs={name: hashlib.sha256(data).hexdigest() for name, data in outputs.items()},
    )
    encoded = (json.dumps(provenance, indent=2, sort_keys=True) + "\n").encode()
    archives = archive_targets(batch, prefix, archived, encoded)
    preflight_archive(archives)
    if prefix == REFERENCE_PREFIX:
        outputs[asset_root + "/provenance.json"] = encoded
    for relative in outputs:
        path = destination_root / relative
        require(
            not path.is_symlink() and not any(parent.is_symlink() for parent in path.parents),
            "publication destination traverses a symlink",
        )
        require(not path.exists(), "refusing to overwrite published result: " + str(path))
    if not check:
        write_archive(archives)
        for relative, data in outputs.items():
            path = destination_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as stream:
                stream.write(data)
    return dict(
        status="checked" if check else "published",
        state=provenance["state"],
        batch=batch.name,
        counts=summary["counts"],
        files=len(outputs),
        archive_directory=str(batch / "publication" / prefix),
        archive_files=len(archives),
        outputs={name: hashlib.sha256(data).hexdigest() for name, data in outputs.items()},
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--batch", required=True, type=Path)
    parser.add_argument("--analysis", type=Path)
    parser.add_argument("--destination-root", type=Path, default=REPO)
    parser.add_argument("--check", action="store_true", help="verify without writing outputs")
    args = parser.parse_args(argv)
    try:
        print(
            json.dumps(publish(args.batch, args.analysis, args.destination_root, check=args.check))
        )
    except (ValueError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
