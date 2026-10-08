#!/usr/bin/env python3
"""Matched host native / direct / passthrough FUSE / staged comparison.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: separate FUSE transport cost from staging semantics cost.
Conclusion sought: how much of the staged-to-native gap is transport and how
much is OverlayCore, persistence and content fingerprints.
Design: one batch with native, direct host, benchmark-only passthrough FUSE
and staged; identical fuser version, TTL and mount options. The passthrough
driver is a lower bound, never a product mode.
"""
import argparse
import csv
import json
import os
import random
import shutil
import traceback
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from zoneinfo import ZoneInfo

from filesystem_stage_ab import WORKLOADS, summarize
from reference_baselines import digest, run_trial, verified_build_receipt
from reference_inputs import verify_reference_inputs
from prepare_fuse_driver import verify_driver_receipt
from filesystem_diagnostic import summarize_trial, write_counter_csv


BACKENDS = ("native", "pvisor-host", "pvisor-fuse", "pvisor-staged")


def verified_inputs(args):
    build=verified_build_receipt(args.build_receipt,args.binary)
    driver=verify_driver_receipt(args.driver_build_receipt,args.fuse_driver)
    manifest=json.loads((args.build_receipt.parent/'source-manifest.json').read_text())
    product_fuser={row['path'].removeprefix('vendor/fuser/'):row['sha256'] for row in manifest
                   if row['path'].startswith('vendor/fuser/')}
    if product_fuser!={row['path']:row['sha256'] for row in driver['fuser_source']}:
        raise ValueError('driver fuser source differs from measured product')
    if driver['driver_source_sha256']!=digest(Path(__file__).with_name('filesystem_fuse_passthrough.rs')):
        raise ValueError('driver implementation differs from current benchmark source')
    return dict(reference=verify_reference_inputs(args.assets),binary_build=build,driver_build=driver,
                build_receipt_sha256=digest(args.build_receipt),driver_build_receipt_sha256=digest(args.driver_build_receipt))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("assets", "binary", "build-receipt", "fuse-driver", "driver-build-receipt", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--cpu-affinity", default="0,1")
    parser.add_argument('--tool-scratch',choices=('executor','workspace'),default='executor')
    parser.add_argument('--profiles',action='store_true',help='independent instrumented cohort; never task-performance data')
    parser.add_argument('--resource-budget',type=Path)
    parser.add_argument('--budget-memory-mib',type=int,default=16384)
    parser.add_argument('--budget-cpu-placement',choices=('affinity','cpuset'),default='affinity')
    parser.add_argument('--resource-observation',choices=('off','sampled'),default='sampled')
    args = parser.parse_args()
    if args.samples<1 or args.warmups<0:parser.error('samples must be positive and warmups nonnegative')
    for name in ("assets", "binary", "build_receipt", "fuse_driver", "driver_build_receipt", "output"):
        setattr(args, name, getattr(args, name).resolve())
    if '.data' not in args.output.parts:parser.error('raw outputs must stay under .data')
    inputs_before=verified_inputs(args)
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    shutil.copy2(args.fuse_driver, args.output / "bin/fuse-passthrough")
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json')
    shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    driver_frozen=args.output/'driver-build';driver_frozen.mkdir()
    shutil.copy2(args.driver_build_receipt,driver_frozen/'build-receipt.json')
    shutil.copy2(args.driver_build_receipt.parent/'source-manifest.json',driver_frozen/'source-manifest.json')
    shutil.copytree(args.driver_build_receipt.parent/'source',driver_frozen/'source')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    configuration = SimpleNamespace(
        assets=args.assets, output=args.output, firmware=None,
        cpu_affinity=args.cpu_affinity, staged_isolation="rootless_process",
        host_isolation="rootless_process", docker_root_pid=None,
        fuse_driver=args.output / "bin/fuse-passthrough", fuse_ttl_seconds=1,
        tool_scratch=args.tool_scratch,diagnostic_timing=args.profiles,diagnostic_stderr_file=args.profiles,
        memory_mib=16384,resource_observation=args.resource_observation,
        resource_budget=args.resource_budget,budget_memory_mib=args.budget_memory_mib,
        budget_cpu_placement=args.budget_cpu_placement,backends=','.join(BACKENDS),
    )
    os.environ["PVISOR_FS_PROFILE"] = "1" if args.profiles else "0"
    os.environ.pop("PVISOR_VM_FS_WORKERS", None)
    backends = list(BACKENDS)
    metadata = {
        "schema": "pvisor-filesystem-fuse-ab/v1",
        'benchmark_id':'B-FS-DIAG','role':'diagnostic',
        'input_verification':inputs_before,
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "binary_sha256": {name: digest(args.output / "bin" / name)
                          for name in ("pvisor", "fuse-passthrough")},
        "workload_sha256": digest(args.assets / "rootfs/bench/reference_workload.py"),
        "fs_workload_sha256": digest(args.assets / "rootfs/bench/harness/v1/workload.py"),
        "harness_sha256": {name: digest(Path(__file__).parent / name) for name in
                           ("filesystem_fuse_ab.py", "filesystem_fuse_passthrough.rs",
                            "reference_baselines.py", "filesystem_stage_ab.py")},
        "host_kernel": os.uname().release,
        "protocol": {
            "samples": args.samples, "warmups": args.warmups,
            "order": "four cells randomly interleaved each round; seed 20261005",
            "cache": "warm host caches; fresh copied workspace and mount/upper per job",
            'tool_scratch':args.tool_scratch,'cross_task_cache_reuse':False,
            'profiles':args.profiles,'stderr_capture':'regular-file diagnostic' if args.profiles else 'pipe',
            'resource_observation':args.resource_observation,
            'resource_scope':'common private parent if supplied; sampled observations may miss lifetimes, affinity alone is not strict placement; no cross-product ranking',
            "cpu_affinity": args.cpu_affinity, "host_isolation": "rootless_process",
            "fuse": "same frozen vendored fuser 0.15.1, abi-7-31, default init flags; one synchronous request loop",
            "ttl_seconds": 1, "writeback_cache": False, "keep_cache": False,
            "mount_options": "RW, NoAtime, DefaultPermissions; no kernel backing-FD passthrough",
            "native_control": "filesystem_fuse_passthrough.rs: libc/native files only; no OverlayCore, copy-up, access policy or preimage journal",
            "timing": "seven unchanged workers and checks; wall includes FUSE mount, child pvisor Run and unmount for passthrough; staged CLI mounts internally",
            "correctness": "Run Bundle/isolation, full tools, 256 backing or upper writes; passthrough mountinfo plus actual LOOKUP/READ/WRITE request and byte counts",
            "counters": "eight relaxed integer counters in passthrough control; no per-request clocks or logging",
        }, "load_before": os.getloadavg(),
    }
    rows, preflights, failures = [], {}, []

    def save():
        report = metadata | {"rows": rows, "preflights": preflights,'failures':failures,
                             "load_after": os.getloadavg()}
        temporary = args.output / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(args.output / "report.json")

    save()
    for backend in backends:
        try:
            row=run_trial(configuration, metadata, backend, "filesystem", -100)
        except Exception as error:
            preflights[backend] = {"state": "failed", "reason": str(error)}
            failures.append(dict(phase='preflight',backend=backend,error=str(error),traceback=traceback.format_exc()))
            save()
            continue
        preflights[backend] = {"state": "passed",'row':row}
        save()
        print("preflight passed", backend, flush=True)
    rng = random.Random(20261005)
    for trial in (range(-args.warmups, args.samples) if not failures else ()):
        order = backends.copy()
        rng.shuffle(order)
        for backend in order:
            try:
                row = run_trial(configuration, metadata, backend, "filesystem", trial)
                if args.profiles:
                    row=summarize_trial(row,Path(row['logs']))
                    if backend=='pvisor-staged' and (not row['filesystem'] or row['profile_coverage']['partial_instances']):
                        raise ValueError('missing complete final staged profiles')
            except Exception as error:
                failures.append(dict(phase='trial',backend=backend,trial=trial,error=str(error),traceback=traceback.format_exc()))
                save()
                continue
            if trial >= 0:
                rows.append(row)
                save()
        print("round", trial + 1, "/", args.samples, flush=True)
    try:
        inputs_after=verified_inputs(args)
        if inputs_after!=inputs_before:raise ValueError('frozen input identities changed')
        if digest(args.output/'bin/pvisor')!=inputs_before['binary_build']['pvisor_sha256']:
            raise ValueError('frozen CLI changed')
        verify_driver_receipt(driver_frozen/'build-receipt.json',args.output/'bin/fuse-passthrough')
        metadata['input_final_verification']=dict(state='passed',**inputs_after)
    except Exception as error:
        metadata['input_final_verification']=dict(state='failed',error=str(error))
        failures.append(dict(phase='final-input-verification',error=str(error),traceback=traceback.format_exc()))
    metadata['state']='passed' if not failures and len(rows)==len(backends)*args.samples else 'failed'
    if metadata['state']=='passed':metadata["summary"] = summarize(rows, backends, args.samples)
    if args.profiles:write_counter_csv(rows,args.output/'counter-summary.csv')
    save()
    if metadata['state']!='passed':raise SystemExit(1)
    with (args.output / "summary.tsv").open("w") as stream:
        writer = csv.writer(stream, delimiter="\t")
        writer.writerow(("backend", "operation", "n", "p50_ms", "p95_ms"))
        for backend, summary in metadata["summary"].items():
            for op in (*WORKLOADS, "completion_ms"):
                values = summary["timings_ms"][op]
                writer.writerow((backend, op, summary["n"], values["p50"], values.get("p95",'')))
    print("report", args.output / "report.json", flush=True)


if __name__ == "__main__":
    main()
