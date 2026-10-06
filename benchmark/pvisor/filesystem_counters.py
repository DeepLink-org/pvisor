#!/usr/bin/env python3
"""Collect filesystem and startup counters from independent, instrumented jobs.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: explain filesystem and complete-task gaps using measured requests,
work counters and service/queue spans, rather than ratios alone.
Conclusion sought: counts and inclusive costs with explicit final/partial
coverage for every process/component/instance; never timing acceptance.
Design: same frozen binary/assets as formal cohorts; independent fresh jobs;
three repetitions per staged/VM workload; output and isolation gates retained.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import shutil
import traceback

from filesystem_diagnostic import summarize_trial, write_counter_csv
from reference_baselines import digest, run_trial, verified_build_receipt
from reference_inputs import verify_reference_inputs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('assets', 'binary', 'build-receipt', 'firmware', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--samples', type=int, default=3)
    parser.add_argument('--modes', default='filesystem,tools')
    parser.add_argument('--backends', default='pvisor-staged,pvisor-vm')
    parser.add_argument('--cpu-affinity', default='0,1')
    parser.add_argument('--memory-mib', type=int, default=16384)
    parser.add_argument('--resource-budget', type=Path)
    parser.add_argument('--budget-memory-mib', type=int, default=16384)
    parser.add_argument('--budget-cpu-placement', choices=('affinity', 'cpuset'), default='affinity')
    args = parser.parse_args()
    if args.samples < 1 or set(args.modes.split(',')) - {'filesystem', 'tools'} or set(args.backends.split(',')) - {'pvisor-staged', 'pvisor-vm'}:
        parser.error('invalid sample count, workload or backend')
    for name in ('assets', 'binary', 'build_receipt', 'firmware', 'output'):
        setattr(args, name, getattr(args, name).resolve())
    if '.data' not in args.output.parts:
        parser.error('raw diagnostics must remain under .data')
    receipt = verified_build_receipt(args.build_receipt, args.binary)
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / 'bin').mkdir()
    shutil.copy2(args.binary, args.output / 'bin/pvisor')
    shutil.copy2(args.build_receipt, args.output / 'build-receipt.json')
    shutil.copy2(args.build_receipt.parent / 'source-manifest.json', args.output / 'source-manifest.json')
    shutil.copytree(Path(__file__).parent, args.output / 'harness',
                    ignore=shutil.ignore_patterns('__pycache__', '.pytest_cache', '.data'))
    for name in ('filesystem_counters.py', 'filesystem_diagnostic.py', 'reference_baselines.py'):
        shutil.copy2(Path(__file__).with_name(name), args.output / name)
    args.staged_isolation = args.host_isolation = 'rootless_process'
    args.docker_root_pid = None
    args.diagnostic_timing = True
    args.resource_observation = 'off'
    os.environ['PVISOR_FS_PROFILE'] = '1'
    metadata = dict(benchmark_id='B-FS-DIAG', role='diagnostic',
        arguments={key: str(value) if isinstance(value, Path) else value
                   for key, value in vars(args).items()},
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(), host_kernel=os.uname().release,
        input_manifest_sha256=digest(args.assets / 'input-manifest.json') if (args.assets / 'input-manifest.json').is_file() else 'unknown',
        assets=json.loads((args.assets / 'assets.json').read_text()), binary_build=receipt,
        pvisor_sha256=digest(args.output / 'bin/pvisor'),
        firmware_sha256=digest(args.firmware / 'libkrunfw.so.5'),
        harness_sha256={name: digest(args.output / name) for name in ('filesystem_counters.py', 'filesystem_diagnostic.py', 'reference_baselines.py')},
        harness_file_sha256={str(path.relative_to(args.output / 'harness')): digest(path)
                             for path in sorted((args.output / 'harness').rglob('*')) if path.is_file()},
        protocol=dict(diagnostic_timing=True, filesystem_profile=True, samples=args.samples,
            cpu_affinity=args.cpu_affinity, memory_mib=args.memory_mib,
            resource_observation=args.resource_observation,
            rss_scope='not sampled; unknown, never zero',
            interpretation='Instrumented runs only. Spans inclusive, snapshots cumulative. Partial records provide lower bounds, not complete-run totals.'),
        rows=[], failures=[])
    def save():
        temp = args.output / 'report.tmp'
        temp.write_text(json.dumps(metadata, indent=2) + '\n')
        temp.replace(args.output / 'report.json')
    save()
    print('Verify complete frozen inputs before diagnostic jobs', flush=True)
    try:
        inputs_before = verify_reference_inputs(args.assets)
    except (OSError, ValueError) as error:
        metadata['input_verification'] = dict(state='failed', reason=str(error), error_type=type(error).__name__)
        save()
        raise
    metadata['input_verification'] = dict(state='passed', **inputs_before)
    save()
    for mode in args.modes.split(','):
        for backend in args.backends.split(','):
            for trial in range(args.samples):
                print(mode, backend, trial, flush=True)
                try:
                    row = run_trial(args, metadata, backend, mode, trial)
                    row = summarize_trial(row, Path(row['logs']))
                    if not row['filesystem']:
                        raise ValueError('no filesystem counters emitted')
                    metadata['rows'].append(row)
                except Exception as error:
                    metadata['failures'].append(dict(mode=mode, backend=backend, trial=trial,
                        error=str(error), traceback=traceback.format_exc()))
                save()
    print('Verify complete frozen inputs after diagnostic jobs', flush=True)
    try:
        inputs_after = verify_reference_inputs(args.assets)
        if inputs_after != inputs_before:
            raise ValueError('prepared input identities changed during diagnostics')
    except (OSError, ValueError) as error:
        metadata['input_final_verification'] = dict(state='failed', reason=str(error), error_type=type(error).__name__)
        metadata['failures'].append(dict(phase='final-input-verification', error=str(error)))
    else:
        metadata['input_final_verification'] = dict(state='passed', **inputs_after)
    save()
    write_counter_csv(metadata['rows'], args.output / 'counter-summary.csv')
    if metadata['failures']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
