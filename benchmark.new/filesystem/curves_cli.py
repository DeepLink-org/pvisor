#!/usr/bin/env python3
"""Benchmark: B-FS-ENG/B-FS-DIAG, ordinary CLI anchor for host-owned write curves.

Preparation copies the audited AF private-Host worker and its unchanged workload
and correctness helpers into a NEW batch. It reuses prepared input generations,
never historical timing. The current CLI build receipt supplies the binaries and
retained product source. No command here builds a product or changes a listener.

  curves_cli.py --prepare NEW_BATCH --setup OWNED_BUDGET_SETUP
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-pre/config.json --check
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-pre/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-profiles/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-timing/config.json --run
  NEW_BATCH/entry.py --config NEW_BATCH/campaign-timing/config.json --analyze

Run each campaign inside the setup's delegated slice, with CPU affinity 0,1 and
RLIMIT_NOFILE=65536. Only --run launches processes/VMs. Ready includes the fresh
private Host and VM, excludes exit. Worker clocks retain the original workload's
fsync/close semantics; they are not the microcurve accepted/applied boundaries.
"""

import argparse
import hashlib
import importlib.util
import json
import math
import random
import shutil
import statistics
import subprocess
import sys
import traceback
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE = Path(__file__).resolve().parent
DESIGN = "ordinary-cli-host-owned-write-buffer-anchor"
ARMS = ("off", "on")
FACTORS = dict(off=0, on=1)
HOST_ENV = "PVISOR_VM_FS_HOST_WRITEBACK"
FLUSH_ENV = "PVISOR_VM_FS_READONLY_FLUSH_ELISION"
FIXED = {
    "PVISOR_VM_GUEST_CACHE": "1",
    "PVISOR_VM_FS_WRITEBACK": "0",
    "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY": "256",
    "PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE": "0",
    "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE": "0",
    "PVISOR_VM_FS_LOWER_DATA_CACHE": "0",
    "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE": "0",
    "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION": "0",
    FLUSH_ENV: "0",
    "PVISOR_FS_PROFILE": "0",
    "PVISOR_STARTUP_TIMING": "0",
    "PVISOR_STARTUP_PROFILE": "0",
    "RUST_LOG": "off",
}
OPERATIONS = ("metadata", "read", "write", "git", "rg", "cargo", "npm", "hot_open", "stream_write")
HISTOGRAM = {
    "overlay-core": 3,
    "preimage-log": 2,
    "preimage-log-read": 2,
    "virtio-fs-overlay": 2,
    "virtio-fs-protocol": 2,
    "virtio-fs-dispatch": 2,
}


def require(value, message):
    if not value:
        raise ValueError(message)


def load(path):
    return json.loads(Path(path).read_text())


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def record(path):
    return dict(path=str(Path(path).resolve()), sha256=sha(path))


def environment(condition, diagnostic=False):
    require(condition in ARMS, "unknown host-write-buffer condition")
    return {
        **FIXED,
        HOST_ENV: str(FACTORS[condition]),
        "PVISOR_FS_PROFILE": "1" if diagnostic else "0",
        "PVISOR_STARTUP_TIMING": "1" if diagnostic else "0",
    }


def source_audit(config, destination):
    """One retained-source check before collection and one after, never per VM."""
    receipt = load(config["build_receipt"]["path"])
    root = Path(receipt["source_root"])
    hashes = receipt["source_hashes"]
    require(
        hashes and all(sha(root / name) == expected for name, expected in hashes.items()),
        "retained current CLI build source changed",
    )
    result = dict(
        state="verified",
        checked_at=datetime.now(timezone.utc).isoformat(),
        build_receipt=config["build_receipt"],
        files=len(hashes),
        source_root=str(root),
        source_hashes_sha256=hashlib.sha256(
            json.dumps(hashes, sort_keys=True).encode()
        ).hexdigest(),
        scope="retained current build input, no filesystem-input inventory gate",
    )
    save(destination, result)
    return result


def prepare(args):
    repo = REPO
    template = args.template.resolve()
    original = load(template / "campaign-pre/config.json")
    build_path = args.build_receipt.resolve()
    build = load(build_path)
    require(
        build.get("build_profile") in ("release", "performance")
        and isinstance(build.get("source_hashes"), dict)
        and build["source_hashes"]
        and Path(build["source_root"]).is_dir(),
        "current build/source receipt is incomplete",
    )
    require(
        "pvisor" in build["artifacts"]
        and sha(build["artifacts"]["pvisor"]["path"]) == build["artifacts"]["pvisor"]["sha256"],
        "current CLI build artifact differs: pvisor",
    )
    setup = load(args.setup)
    require(
        setup.get("state") == "ready"
        and setup.get("cgroup")
        and setup.get("slice_unit")
        and setup.get("memory_bytes") == 16 * 1024**3
        and setup.get("swap_bytes") == 0
        and setup.get("cpu_ids") == [0, 1]
        and setup.get("cpu_quota") == 2,
        "new owned two-core/16-GiB budget is not ready",
    )
    batch = args.prepare.resolve()
    require(not batch.exists(), "batch already exists; retained evidence is never overwritten")
    # Every original path used by the worker must be visible under one physical
    # bind. Repository root also covers the new benchmark.new output directory.
    for path in (batch, template, Path(build["source_root"]), args.setup.resolve()):
        path.relative_to(repo)
    batch.mkdir(parents=True)
    shutil.copy2(__file__, batch / "entry.py")
    shutil.copy2(template / "entry.py", batch / "af_entry.py")
    shutil.copy2(template / "workspace_sources.py", batch / "workspace_sources.py")
    shutil.copy2(template / "reference-arguments.json", batch / "reference-arguments.json")
    for path in sorted((template / "harness").rglob("*.py")):
        target = batch / "harness" / path.relative_to(template / "harness")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
    artifacts = {"pvisor": build["artifacts"]["pvisor"]}
    artifacts["firmware"] = original["artifacts"]["firmware"]
    references = dict(
        current_build=record(build_path),
        original_preparation=record(original["input_preparation"]),
        prepared_input_association=record(original["reuse_artifacts"]),
        inherited_worker=record(template / "entry.py"),
        inherited_config_controls=record(template / "campaign-pre/config.json"),
        scope="current binaries/source; earlier public input generations and validators only; no earlier samples or new import cost",
    )
    save(batch / "origins.json", references)
    common = dict(
        schema=1,
        benchmark_id="B-FS-ENG",
        role="engineering A/B",
        design=DESIGN,
        repo=str(repo),
        raw_root=str(repo),
        conditions=list(ARMS),
        factors=FACTORS,
        contrasts=[["off", "on"]],
        resident_rounds=3,
        seed=20261017,
        memory_mib=16384,
        cpu_affinity="0,1",
        dax=False,
        environment=FIXED,
        setup=str(args.setup.resolve()),
        setup_sha256=sha(args.setup),
        slice_unit=setup["slice_unit"],
        budget_binding="bound-new-owned-budget",
        quick_source_report=str(batch / "reference-arguments.json"),
        firmware=original["firmware"],
        artifacts=artifacts,
        build_receipt=record(build_path),
        product_source_root=build["source_root"],
        product_source_files=len(build["source_hashes"]),
        input_preparation=original["input_preparation"],
        inputs=original["inputs"],
        origins=record(batch / "origins.json"),
        profile_inventory={arm: HISTOGRAM for arm in ARMS},
        treatment_scope="host-owned FIFO write buffering default options only; WB0/DAX0 and DATA/FLUSH/NE/SQ/Core absence0",
    )
    for phase in ("pre", "profiles", "timing"):
        directory = batch / f"campaign-{phase}"
        directory.mkdir()
        config = {
            **common,
            "phase": phase,
            "samples": 5 if phase == "timing" else 1,
            "benchmark_id": "B-FS-DIAG" if phase == "profiles" else "B-FS-ENG",
            "output": str(batch / phase),
        }
        save(directory / "config.json", config)
    source_audit(common, batch / "source-before.json")
    files = [path for path in batch.rglob("*") if path.is_file()]
    save(
        batch / "harness-manifest.json",
        dict(
            schema=1,
            files={str(path.relative_to(batch)): sha(path) for path in sorted(files)},
            origins=references,
            frozen_at=datetime.now(timezone.utc).isoformat(),
        ),
    )
    print(
        json.dumps(
            dict(
                state="prepared",
                batch=str(batch),
                frozen_files=len(files),
                launch="entry.py --config campaign-{pre,profiles,timing}/config.json --run",
                analysis="entry.py --config campaign-timing/config.json --analyze",
            ),
            indent=2,
        )
    )


def verify(config):
    require(
        config["design"] == DESIGN
        and config["conditions"] == list(ARMS)
        and config["factors"] == FACTORS
        and config["contrasts"] == [["off", "on"]],
        "anchor design/condition differs",
    )
    require(
        config["phase"] in ("pre", "profiles", "timing")
        and config["samples"] == (5 if config["phase"] == "timing" else 1)
        and config["resident_rounds"] == 3
        and config["seed"] == 20261017
        and config["environment"] == FIXED
        and config["dax"] is False
        and config["memory_mib"] == 16384
        and config["cpu_affinity"] == "0,1",
        "anchor controls differ",
    )
    require(
        config["budget_binding"] == "bound-new-owned-budget"
        and sha(config["setup"]) == config["setup_sha256"],
        "owned budget binding changed",
    )
    require(sha(config["origins"]["path"]) == config["origins"]["sha256"], "origins changed")
    origins = load(config["origins"]["path"])
    for name in ("current_build", "original_preparation", "prepared_input_association"):
        ref = origins[name]
        require(sha(ref["path"]) == ref["sha256"], f"input/build association changed: {name}")
    require(
        config["build_receipt"] == origins["current_build"], "current build association differs"
    )
    build = load(config["build_receipt"]["path"])
    require(
        build["source_root"] == config["product_source_root"]
        and len(build["source_hashes"]) == config["product_source_files"],
        "source binding differs",
    )
    for name, ref in config["artifacts"].items():
        require(sha(ref["path"]) == ref["sha256"], f"artifact changed: {name}")
    require(
        config["artifacts"]["pvisor"] == build["artifacts"]["pvisor"],
        "CLI receipt/artifact join differs",
    )
    association = load(origins["prepared_input_association"]["path"])
    require(
        config["inputs"] == association["inputs"]
        and config["input_preparation"] == origins["original_preparation"]["path"]
        and load(config["input_preparation"])["state"] == "verified",
        "public prepared input association differs",
    )
    frozen = load(BASE / "harness-manifest.json")
    for name, expected in frozen["files"].items():
        require(sha(BASE / name) == expected, f"frozen anchor input changed: {name}")
    selected = config["inputs"]["profiles" if config["phase"] == "profiles" else "timing"]
    root = Path(selected["store"]) / "bases" / selected["base_id"]
    for name, expected in selected["small_files_sha256"].items():
        require(sha(root / name) == expected, f"prepared input identity changed: {name}")
    workspace = Path(selected["workspace_store"]) / "bases" / selected["workspace_base_id"]
    require(
        (root / "rootfs").is_dir()
        and not (root / "rootfs").is_symlink()
        and (workspace / "rootfs").is_dir()
        and not (workspace / "rootfs").is_symlink()
        and sha(workspace / "seal.json") == selected["workspace_base_id"],
        "owned roots are absent or changed",
    )
    require(
        sha(workspace / "rootfs/_fs/fixture.json")
        == sha(Path(selected["assets"]) / "rootfs/work/_fs/fixture.json")
        and load(workspace / "rootfs/_fs/fixture.json")["rounds"] == 3,
        "prepared workspace/live trial fixture differs",
    )
    return dict(
        state="verified-no-VM-launched",
        phase=config["phase"],
        build_receipt_sha256=config["build_receipt"]["sha256"],
        frozen_harness_sha256=sha(BASE / "harness-manifest.json"),
        product_source_files=config["product_source_files"],
        artifact_hashes={k: v["sha256"] for k, v in config["artifacts"].items()},
    )


def inherited_worker(config, condition, trial, directory):
    """Keep the audited normal CLI/Bundle/full-byte validators unchanged."""
    sys.path[:0] = [str(BASE / "harness"), str(BASE)]
    spec = importlib.util.spec_from_file_location("anchor_af_worker", BASE / "af_entry.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    old_profiles = module.validate_profiles
    module.verify_inputs = verify
    module.ARMS = ARMS
    # In the inherited worker this map means readonly FLUSH, fixed OFF here.
    module.FACTORS = dict(off=0, on=0)
    module.condition_environment = environment

    def caller(_condition, receipt):
        require(
            receipt == dict(variable=FLUSH_ENV, present=True, value="0")
            and type(receipt["present"]) is bool,
            "FLUSH must stay explicitly off",
        )
        return receipt

    module.validate_caller_environment = caller

    def profiles(lines, arm, inventory):
        evidence = old_profiles(lines, arm, inventory)
        collected = evidence["collected"]
        lookup = {(p["pid"], p["component"], p["instance"]): p for p in collected["final_profiles"]}
        for scope in ("rootfs", "pvisor-workspace"):
            owner = evidence["actual"][scope]["virtio-fs-overlay"]
            values = lookup[(owner["pid"], "virtio-fs-overlay", owner["instance"])]["measurements"]
            flag = values.get("host_writeback_enabled")
            if FACTORS[arm]:
                require(
                    flag is not None and type(flag["units"]) is int and flag["units"] == 1,
                    f"host buffer not actually enabled: {scope}",
                )
            else:
                require(
                    flag is None or (type(flag["units"]) is int and flag["units"] == 0),
                    f"host buffer unexpectedly enabled: {scope}",
                )
            owner["host_writeback"] = dict(
                requested=FACTORS[arm],
                actual_enabled_units=None if flag is None else flag["units"],
                disabled_counter_absence=flag is None,
                disabled_evidence="source-bound configure early-return plus explicit actual frontend env0"
                if flag is None
                else None,
            )
        return evidence

    module.validate_profiles = profiles
    # This records the exact env passed to the real frontend Popen, not a config
    # echo; inherited reference already separately records FLUSH presence/value.
    actual = []
    original_popen = subprocess.Popen

    def popen(argv, *args, **kwargs):
        if (
            isinstance(argv, (list, tuple))
            and "run" in argv
            and any(str(part).endswith("/bin/pvisor") for part in argv)
        ):
            env = kwargs.get("env")
            expected = environment(condition, config["phase"] == "profiles")
            require(
                env is not None and all(env.get(k) == v for k, v in expected.items()),
                "actual frontend environment differs from treatment/fixed controls",
            )
            require(
                "PVISOR_VM_FS_HOST_WRITEBACK_CONFIG" not in env,
                "unexpected private queue tuning in ordinary CLI anchor",
            )
            actual.append(
                dict(
                    argv=list(map(str, argv)),
                    controls={k: env[k] for k in expected},
                    host_writeback_config_present=False,
                )
            )
        return original_popen(argv, *args, **kwargs)

    subprocess.Popen = popen
    try:
        module.worker(config, condition, trial, directory)
    finally:
        subprocess.Popen = original_popen
    require(len(actual) == 1, "not exactly one observed ordinary CLI frontend launch")
    output = module.alias(directory, config)
    row = load(output / "row.json")
    row["factors"] = dict(HOST_WRITEBACK=FACTORS[condition], DATA=0, FLUSH=0, NE=0, SQ=0)
    row["frontend_host_writeback_environment"] = actual[0]
    row["current_cli_build_receipt"] = config["build_receipt"]
    row["worker_clock_scope"] = (
        "original seven tools plus hot_open and stream_write; fsync/close retained; exit excluded"
    )
    save(output / "frontend-environment.json", actual[0])
    save(output / "row.json", row)


def run(config):
    checked = verify(config)
    output = Path(config["output"])
    output.mkdir(parents=True, exist_ok=False)
    phase = config["phase"]
    report = dict(
        schema=1,
        state="running",
        benchmark_id=config["benchmark_id"],
        started_at=datetime.now(timezone.utc).isoformat(),
        config=config,
        input_verification=checked,
        rows=[],
        failures=[],
        order=[],
        protocol=dict(
            design=DESIGN,
            factors=FACTORS,
            conditions=list(ARMS),
            samples_per_condition=config["samples"],
            resident_rounds=3,
            seed=20261017,
            startup="actual frontend Popen to unique REFERENCE_READY; fresh private Host+VM; excludes exit",
            task="original worker_ms; three dependent rounds; original fsync/close semantics",
            independent_unit="five fresh paired environment blocks, never fifteen rounds",
            statistics="32 metrics; one aggregate primary; 5000 paired bootstrap; uncorrected secondary intervals",
            primary="three_rounds.seven_tools_ms",
            warmups=0,
            performance_sample=phase == "timing",
            writeback=False,
            dax=False,
            host_writeback_default_options=True,
            interference_gate=False,
            quiet_gate=False,
            full_inventory_gate=False,
            retain_all_valid_slow_samples=True,
            reused_preparation="existing public owned generations only; no new import/seal performed or charged",
            cli_scope="real ordinary CLI; prepared rootfs/workspace; declared checkpoint capability false; native binding unobserved",
        ),
    )
    save(output / "report.json", report)
    rng = random.Random(20261017)
    for pair in range(config["samples"]):
        order = list(ARMS)
        rng.shuffle(order)
        for arm in order:
            directory = BASE / "raw" / phase / f"{arm}-{pair:03d}"
            directory.mkdir(parents=True, exist_ok=False)
            argv = [
                "unshare",
                "--user",
                "--map-root-user",
                "--mount",
                "--pid",
                "--fork",
                "--kill-child",
                "--mount-proc",
                sys.executable,
                str(BASE / "entry.py"),
                "--config",
                config["_config_path"],
                "--worker",
                arm,
                "--trial",
                str(pair),
                "--directory",
                str(directory),
            ]
            save(directory / "worker-command.json", dict(argv=argv))
            report["order"].append(dict(pair=pair, condition=arm))
            save(output / "report.json", report)
            try:
                with (
                    (directory / "namespace.stdout").open("wb") as out,
                    (directory / "namespace.stderr").open("wb") as err,
                ):
                    completed = subprocess.run(argv, stdout=out, stderr=err, timeout=300)
                save(directory / "worker-exit.json", dict(exit_code=completed.returncode))
                require(completed.returncode == 0, f"failed CLI sample retained: {directory}")
                row = load(directory / "row.json")
                require(
                    row["condition"] == arm
                    and row["pair"] == pair
                    and row["factors"]["HOST_WRITEBACK"] == FACTORS[arm],
                    "worker treatment mismatch",
                )
                report["rows"].append(row)
                save(output / "report.json", report)
            except BaseException as error:
                report["failures"].append(
                    dict(condition=arm, pair=pair, error=str(error), directory=str(directory))
                )
                report["state"] = "failed"
                save(output / "report.json", report)
                raise
    report.update(
        state="complete",
        ended_at=datetime.now(timezone.utc).isoformat(),
        final_input_verification=verify(config),
    )
    save(output / "report.json", report)
    print(
        json.dumps(
            dict(state="complete", rows=len(report["rows"]), report=str(output / "report.json"))
        )
    )


def metrics(row):
    result = dict(ready_ms=row["ready_ms"])
    for number in range(1, 4):
        for operation in OPERATIONS:
            result[f"r{number}.{operation}_ms"] = row["result"]["filesystem"][operation]["rounds"][
                number - 1
            ]["worker_ms"]
        result[f"r{number}.seven_tools_ms"] = sum(
            result[f"r{number}.{op}_ms"] for op in OPERATIONS[:7]
        )
    result["three_rounds.seven_tools_ms"] = sum(result[f"r{n}.seven_tools_ms"] for n in range(1, 4))
    require(
        len(result) == 32
        and all(type(v) in (int, float) and math.isfinite(v) and v > 0 for v in result.values()),
        "invalid worker metric",
    )
    return result


def percentile(values, q):
    values = sorted(values)
    index = (len(values) - 1) * q
    low = int(index)
    return values[low] + (values[min(low + 1, len(values) - 1)] - values[low]) * (index - low)


def analyze(config):
    verify(config)
    report_path = Path(config["output"]) / "report.json"
    report = load(report_path)
    require(
        config["phase"] == "timing"
        and report["state"] == "complete"
        and not report["failures"]
        and len(report["rows"]) == 10,
        "only complete independent five-pair timing cohort can be analyzed",
    )
    expected_order = []
    rng = random.Random(20261017)
    for pair in range(5):
        arms = list(ARMS)
        rng.shuffle(arms)
        expected_order.extend(dict(pair=pair, condition=arm) for arm in arms)
    require(report["order"] == expected_order, "paired randomized order differs")
    rows = {(r["condition"], r["pair"]): r for r in report["rows"]}
    require(set(rows) == {(a, p) for a in ARMS for p in range(5)}, "missing/duplicate environment")
    for (arm, pair), row in rows.items():
        raw = BASE / "raw" / "timing" / f"{arm}-{pair:03d}"
        require(
            load(raw / "worker-exit.json")["exit_code"] == 0
            and load(raw / "row.json") == row
            and row["normal_cli"] is True,
            "retained normal-exit row differs",
        )
    extracted = {key: metrics(row) for key, row in rows.items()}
    summary, comparisons = [], []
    for metric in next(iter(extracted.values())):
        samples = {a: [extracted[(a, p)][metric] for p in range(5)] for a in ARMS}
        for arm, values in samples.items():
            summary.append(
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
        reduction = 100 * (1 - statistics.median(samples["on"]) / statistics.median(samples["off"]))
        rng = random.Random(20261017)
        bootstrap = []
        for _ in range(5000):
            indexes = [rng.randrange(5) for _ in range(5)]
            bootstrap.append(
                100
                * (
                    1
                    - statistics.median(samples["on"][i] for i in indexes)
                    / statistics.median(samples["off"][i] for i in indexes)
                )
            )
        comparisons.append(
            dict(
                metric=metric,
                primary=metric == "three_rounds.seven_tools_ms",
                reduction_percent=reduction,
                ci95_low=percentile(bootstrap, 0.025),
                ci95_high=percentile(bootstrap, 0.975),
                off_samples_ms=samples["off"],
                on_samples_ms=samples["on"],
                bootstrap_replicates=5000,
                seed=20261017,
            )
        )
    result = dict(
        state="verified",
        report=record(report_path),
        n_paired_environments=5,
        dependent_rounds_per_environment=3,
        summary=summary,
        comparisons=comparisons,
        multiple_comparison_adjustment="none; 32 intervals, one preregistered aggregate primary",
        scope="same-cohort real ordinary CLI anchor only; no historical or SDK curve timing pooled",
    )
    save(BASE / "timing-analysis.json", result)
    source_audit(config, BASE / "source-after.json")
    print(
        json.dumps(
            dict(
                state="verified", metrics=32, primary=next(c for c in comparisons if c["primary"])
            ),
            indent=2,
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path)
    parser.add_argument("--setup", type=Path)
    parser.add_argument("--template", type=Path, default=REPO / "benchmark/.data/af17-cli")
    parser.add_argument(
        "--build-receipt",
        type=Path,
        default=REPO
        / "benchmark.new/filesystem/.data/curves-cli-artifacts-20261017/build-receipt.json",
    )
    parser.add_argument("--config", type=Path)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true")
    action.add_argument("--run", action="store_true")
    action.add_argument("--analyze", action="store_true")
    action.add_argument("--worker", choices=ARMS)
    parser.add_argument("--trial", type=int)
    parser.add_argument("--directory", type=Path)
    args = parser.parse_args()
    if args.prepare:
        require(
            args.setup is not None
            and args.config is None
            and not any((args.check, args.run, args.analyze, args.worker)),
            "--prepare requires --setup only",
        )
        return prepare(args)
    require(
        args.config is not None and any((args.check, args.run, args.analyze, args.worker)),
        "select --prepare or --config with an action",
    )
    config = load(args.config)
    config["_config_path"] = str(args.config.resolve())
    try:
        if args.check:
            print(json.dumps(verify(config), indent=2))
        elif args.run:
            run(config)
        elif args.analyze:
            analyze(config)
        else:
            require(args.trial is not None and args.directory is not None, "worker identity absent")
            inherited_worker(config, args.worker, args.trial, args.directory)
    except BaseException as error:
        if args.worker and args.directory and args.directory.is_dir():
            save(
                args.directory / "failure.json",
                dict(error=str(error), traceback=traceback.format_exc()),
            )
        raise


if __name__ == "__main__":
    main()
