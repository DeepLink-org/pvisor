#!/usr/bin/env python3
"""Compare cache eligibility in the actual pVisor runtime.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: determine whether ordinary tool workloads benefit when their
rootfs and workspace inputs have real immutable ownership.
Conclusion sought: paired Ready and per-tool differences for mutable inputs,
sealed rootfs, snapshot workspace, and both; include failed qualifications.
Design: one host executable and firmware, WB=0, DAX=0, five seeded randomized
fresh environments per condition, three resident rounds in each. Retain the
reference output/Bundle/lower/byte checks. No interference or inventory gate;
profile collection is a separate campaign, never a timing arm.

``run(config_path)`` is called by the retained private-namespace entry. The
trial adapter connects this runner to an actual supported immutable-input
interface and checks its per-trial receipt. Labels or environment variables
alone cannot establish that a mutable directory became immutable.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import random
import shutil
from types import SimpleNamespace


CONDITIONS = ("baseline", "sealed-rootfs", "snapshot-workspace", "both")
EXPECTED_INPUTS = {
    "baseline": {"rootfs_immutable": False, "workspace_immutable": False},
    "sealed-rootfs": {"rootfs_immutable": True, "workspace_immutable": False},
    "snapshot-workspace": {"rootfs_immutable": False, "workspace_immutable": True},
    "both": {"rootfs_immutable": True, "workspace_immutable": True},
}
CONTROL_ENVIRONMENT = {
    "PVISOR_VM_GUEST_CACHE": "1",
    "PVISOR_VM_FS_WRITEBACK": "0",
    "PVISOR_FS_PROFILE": "0",
    "PVISOR_STARTUP_TIMING": "0",
    "PVISOR_STARTUP_PROFILE": "0",
}


def save(path, value):
    """Atomically retain a report, including partially completed campaigns."""
    path = Path(path)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load_arguments(config):
    """Reuse the original reference controls; no historical measurements recur."""
    previous = json.loads(Path(config["quick_source_report"]).read_text())
    setup = json.loads(Path(config["setup"]).read_text())
    arguments = previous["arguments"].copy()
    for key in ("samples", "warmups", "seed", "memory_mib", "budget_memory_mib", "docker_root_pid"):
        if arguments.get(key) not in (None, "None"):
            arguments[key] = int(arguments[key])
    for key in ("assets", "binary", "output", "build_receipt", "firmware", "resource_budget"):
        value = arguments.get(key)
        arguments[key] = None if value in (None, "None") else Path(value)
    for key in ("fc_system_receipt", "fc_reference_receipt", "qemu_system_receipt"):
        arguments[key] = None
    arguments.update(
        assets=Path(config["assets"]), binary=Path(config["binary"]),
        firmware=Path(config["firmware"]), build_receipt=None,
        resource_budget=Path(setup["cgroup"]), docker_host=setup["docker_host"],
        docker_root_pid=int(setup["daemon"]["pid"]), backends="pvisor-vm",
        samples=int(config.get("samples", 5)), warmups=0,
        seed=int(config.get("seed", 20261016)), resource_observation="off",
        diagnostic_timing=False, diagnostic_stderr_file=False,
    )
    if arguments["samples"] < 1:
        raise ValueError("samples must be positive")
    return arguments


def condition_environment(config, name):
    """Read an explicit interface mapping without inventing runtime switches."""
    mapping = config["conditions"]
    if set(mapping) != set(CONDITIONS):
        raise ValueError("all four cache conditions must be specified exactly once")
    values = dict(mapping[name].get("env", {}))
    if any(not isinstance(key, str) or not isinstance(value, str)
           for key, value in values.items()):
        raise ValueError("condition environment must contain string pairs")
    for key, value in CONTROL_ENVIRONMENT.items():
        if key in values and values[key] != value:
            raise ValueError(f"{name} changes fixed control {key}")
    return values


def load_adapter(path):
    """Load the frozen runtime-specific preparation and qualification checks."""
    spec = importlib.util.spec_from_file_location("cache_matrix_trial_adapter", path)
    if spec is None or spec.loader is None:
        raise ValueError("cannot load trial adapter")
    adapter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(adapter)
    for name in ("prepare_trial", "validate_trial"):
        if not callable(getattr(adapter, name, None)):
            raise ValueError(f"trial adapter lacks {name}")
    return adapter


@contextmanager
def trial_environment(values):
    """Keep previous interactive switches out of the four fixed conditions."""
    previous = dict(os.environ)
    try:
        for key in list(os.environ):
            if key.startswith("PVISOR_"):
                del os.environ[key]
        os.environ.update(values)
        yield
    finally:
        os.environ.clear()
        os.environ.update(previous)


def validate_qualification(receipt, condition):
    """Require observed ownership and disabled DAX for this exact trial."""
    if not isinstance(receipt, dict):
        raise ValueError("adapter did not return a qualification receipt")
    for key, expected in EXPECTED_INPUTS[condition].items():
        if receipt.get(key) is not expected:
            raise ValueError(f"{condition} actual {key} differs from the planned input")
    if receipt.get("dax") is not False:
        raise ValueError("actual DAX state was not verified disabled")
    if not receipt.get("evidence"):
        raise ValueError("qualification needs retained runtime/ownership evidence")


def run(config_path):
    """Run the matrix inside the already established private namespace."""
    import reference_baselines as reference
    from filesystem_rounds import validate_rounds, validate_stream_files

    config_path = Path(config_path)
    config = json.loads(config_path.read_text())
    arguments = load_arguments(config)
    # Validate every arm before starting; unsupported cache eligibility cannot
    # silently become another mutable baseline.
    for condition in CONDITIONS:
        condition_environment(config, condition)
    adapter_path = Path(config["trial_adapter"])
    adapter = load_adapter(adapter_path)
    canonical_output = Path(config["outputs"]["formal"])
    relative_output = canonical_output.relative_to(Path(config["raw_root"]))
    output = Path("/tmp/private-out") / relative_output
    output.mkdir(parents=True, exist_ok=False)
    binary = arguments["binary"]
    firmware_object = Path(config["firmware_object"])
    identities = {"binary": sha(binary), "firmware": sha(firmware_object),
                  "adapter": sha(adapter_path), "runner": sha(__file__)}
    report = {
        "schema": 1, "benchmark_id": "B-FS-ENG", "role": "engineering A/B",
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "configuration": config, "configuration_sha256": sha(config_path),
        "artifacts": identities,
        "arguments": {key: str(value) for key, value in arguments.items()},
        "assets": json.loads((arguments["assets"] / "assets.json").read_text()),
        "protocol": {
            "conditions": EXPECTED_INPUTS, "samples_per_condition": arguments["samples"],
            "resident_rounds": 3, "warmups": 0, "seed": arguments["seed"],
            "writeback": 0, "dax": False, "guest_cache": 1, "profiling": False,
            "startup_metric": "launch to REFERENCE_READY; excludes work and exit",
            "operation_metric": "worker_ms by resident round; excludes launch and exit",
            "independent_unit": "fresh environment; resident rounds are dependent",
            "interference_gate": False, "source_inventory_gate": False,
            "correctness": "normal exit, full tool output/cache/Bundle/lower isolation and written/stream bytes; actual immutable ownership checked by adapter",
            "retain_all_valid_slow_samples": True,
            "snapshot_preparation": "outside Ready and operation clocks; retained separately",
            "rootfs_treatment": "all arms use the same owned sealed physical rootfs; only its Mutable/Immutable declaration changes",
            "workspace_treatment": "baseline/rootfs use a fresh live Mutable workspace; workspace/both use equal initial bytes from a sealed snapshot with a frozen baseline and Immutable declaration; this is a combined treatment",
        },
        "order": [], "rows": [], "failed_rows": [], "failures": [],
    }
    for condition in CONDITIONS:
        folder = output / "conditions" / condition / "bin"
        folder.mkdir(parents=True)
        shutil.copy2(binary, folder / "pvisor")
        if sha(folder / "pvisor") != identities["binary"]:
            raise ValueError("condition binary differs from the shared artifact")
    save(output / "report.json", report)
    rng = random.Random(arguments["seed"])
    prepared_manifest = None
    for trial in range(arguments["samples"]):
        order = list(CONDITIONS)
        rng.shuffle(order)
        report["order"].append({"trial": trial, "conditions": order})
        for condition in order:
            args = SimpleNamespace(**arguments)
            args.output = output / "conditions" / condition
            row = None
            prepared = None
            try:
                args.cache_matrix_environment = condition_environment(config, condition)
                prepared = adapter.prepare_trial(args, condition, trial)
                if not isinstance(prepared, dict):
                    raise ValueError("adapter prepare_trial must return a retained dictionary")
                if "manifest_sha256" in prepared:
                    if prepared_manifest is None:
                        prepared_manifest = prepared["manifest_sha256"]
                        report["owned_inputs_manifest_sha256"] = prepared_manifest
                    elif prepared["manifest_sha256"] != prepared_manifest:
                        raise ValueError("cache arms do not share one prepared input manifest")
                if args.binary != binary or args.firmware != arguments["firmware"]:
                    raise ValueError("adapter changed the shared executable or firmware")
                settings = args.cache_matrix_environment.copy()
                settings.update(prepared.get("environment", {}))
                for key, value in CONTROL_ENVIRONMENT.items():
                    if key in settings and settings[key] != value:
                        raise ValueError(f"adapter changes fixed control {key}")
                settings.update(CONTROL_ENVIRONMENT)
                with trial_environment(settings):
                    row = reference.run_trial(args, report, "pvisor-vm", "filesystem", trial)
                row.update(condition=condition, benchmark_id="B-FS-ENG", writeback=0,
                           prepared=prepared)
                validate_rounds(row["result"])
                validate_stream_files(row, "pvisor-vm")
                if config.get("python_version"):
                    versions = {value["python"] for operation in row["result"]["filesystem"].values()
                                for value in operation["rounds"]}
                    if versions != {config["python_version"]}:
                        raise ValueError("workloads did not use the common frozen Python")
                qualification = adapter.validate_trial(args, row, condition, trial, prepared)
                validate_qualification(qualification, condition)
                row["qualification"] = qualification
                report["rows"].append(row)
            except Exception as error:
                report["failures"].append({"condition": condition, "trial": trial,
                                           "reason": str(error), "prepared": prepared})
                if row is not None:
                    report["failed_rows"].append(row)
            save(output / "report.json", report)
        print(f"CACHE MATRIX {trial + 1}/{arguments['samples']}; valid={len(report['rows'])}; failed={len(report['failures'])}", flush=True)
    final_identities = {"binary": sha(binary), "firmware": sha(firmware_object),
                        "adapter": sha(adapter_path), "runner": sha(__file__)}
    report["artifacts_after"] = final_identities
    if final_identities != identities:
        report["failures"].append({"reason": "shared executable, firmware or harness changed during sampling"})
    report["status"] = "failed" if report["failures"] else "complete"
    report["finished_at"] = datetime.now(timezone.utc).isoformat()
    save(output / "report.json", report)
    print(f"CACHE MATRIX {report['status']}: {canonical_output}", flush=True)
    if report["failures"]:
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    run(parser.parse_args().config)
