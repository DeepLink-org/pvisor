#!/usr/bin/env python3
"""Measure new-VM workload readiness, completion and opt-in host checkpoints.

Benchmark: B-STARTUP (benchmark/README.md#b-startup), role user-facing.
Motivation: users need the wait from launch until a workload can run in a VM.
Conclusion sought: ready and completion time of a new VM through the CLI.
Design: current CLI, completed Run Bundles required; checkpoints are opt-in
diagnostics and are not mixed into ready/completion distributions.

Prepared rootfs and warm host caches; not image download or cold-disk latency.
Uses the current CLI and validates completed Run Bundles after timing stops.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import signal
import statistics
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from startup import percentile, validate_bundle

MARKER = b"PVISOR_BENCH_READY"
WORKLOAD = ["/bin/sh", "-c", 'printf "PVISOR_BENCH_READY\\n"']
SHAPES = ((1, 128), (2, 128), (4, 128), (2, 2048))


def checkpoints(marks: list[dict], started: int, ready: int) -> dict:
    entries = sorted(int(m['monotonic_us']) for m in marks if m['stage'] == 'process.entry')
    if len(entries) != 2:
        raise ValueError('expected parent and runner entry checkpoints')

    def stamp(stage):
        values = [int(m['monotonic_us']) for m in marks if m['stage'] == stage]
        if len(values) != 1:
            raise ValueError(f'expected one {stage} checkpoint, got {len(values)}')
        return values[0]

    boundaries = [started / 1000, entries[0], stamp('vm.spawn_begin'), entries[1],
                  stamp('runner.krun_enter'), stamp('runner.vmm_built'), ready / 1000]
    names = ('parent_load', 'parent_prepare', 'runner_load', 'runner_prepare',
             'vmm_build', 'guest_and_output')
    phases = {name: (b - a) / 1000 for name, a, b in zip(names, boundaries, boundaries[1:])}
    if any(v < 0 for v in phases.values()):
        raise ValueError('checkpoints are out of order')
    if abs(sum(phases.values()) - (ready - started) / 1e6) > 1e-6:
        raise ValueError('startup accounting does not close')
    spans = {
        'storage': ('session.storage_begin', 'session.storage_ready'),
        'record': ('storage.record_write_begin', 'storage.record_write_ready'),
        'overlay': ('storage.overlay_begin', 'storage.overlay_ready'),
        'agentctl': ('session.begin', 'session.agentctl_ready'),
        'ram': ('vm.ram_backing_begin', 'vm.ram_backing_ready'),
        'spec': ('vm.spec_write_begin', 'vm.spec_write_ready'),
        'attestation': ('runner.devices_configured', 'runner.attestation_ready'),
    }
    return dict(phases=phases, sub={k: (stamp(b) - stamp(a)) / 1000 for k, (a, b) in spans.items()})


def trial(args, case, round_id, diagnostic=False):
    name, backend, cpus, memory, firmware = case
    work = Path(tempfile.mkdtemp(prefix=f'{name}-{round_id}-', dir=args.output / 'trials'))
    (work / 'config').mkdir()
    env = {k: os.environ[k] for k in ('PATH', 'HOME', 'TMPDIR') if k in os.environ}
    env.update(PVISOR_RUN_HOME=str(work / 'runs'), XDG_CONFIG_HOME=str(work / 'config'))
    env['PVISOR_STARTUP_TIMING'] = '1' if diagnostic else '0'
    command = WORKLOAD.copy()
    if backend != 'direct':
        options = [] if backend == 'host' else [
            '--vm', '--rootfs', str(args.rootfs), '--vm-library-dir', str(firmware),
            '--cpu', str(cpus), '--memory', f'{memory}MiB',
        ]
        command = [str(args.binary), 'run', '--no-agent-defaults', '--overlaynet', 'off',
                   '--stdio', 'inherit', '--timeout', '10s', *options, '--', *WORKLOAD]
    ready, ended, stdout, stderr = [], [], [], []
    done = threading.Event()
    clock = lambda: time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    started = clock()
    process = subprocess.Popen(command, cwd=work, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)

    def read_stdout():
        for line in process.stdout:
            stdout.append(line)
            if line.strip() == MARKER:
                ready.append(clock())

    def wait():
        process.wait()
        ended.append(clock())
        done.set()

    readers = [threading.Thread(target=read_stdout),
               threading.Thread(target=lambda: stderr.append(process.stderr.read())),
               threading.Thread(target=wait)]
    for reader in readers:
        reader.start()
    if not done.wait(20):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        for reader in readers:
            reader.join(timeout=5)
        process.stdout.close()
        process.stderr.close()
        raise TimeoutError(f'trial exceeded 20s; retained at {work}')
    for reader in readers:
        reader.join()
    process.stdout.close()
    process.stderr.close()
    text, err = b''.join(stdout).decode(errors='replace'), b''.join(stderr).decode(errors='replace')
    (work / 'stdout.log').write_text(text)
    (work / 'stderr.log').write_text(err)
    if process.returncode != 0 or len(ready) != 1:
        raise RuntimeError(f'{name}: exit={process.returncode}, markers={len(ready)}; see {work}')
    bundle_dirs = [Path(line.removeprefix('Run Bundle: ')).parent for line in err.splitlines() if line.startswith('Run Bundle: ')]
    if backend != 'direct' and len(bundle_dirs) != 1:
        raise RuntimeError(f'{name}: expected one reported Run Bundle')
    isolation = None if backend == 'direct' else validate_bundle(bundle_dirs[0], backend)
    row = dict(case=name, backend=backend, cpus=cpus, memory_mib=memory, round=round_id,
               diagnostic=diagnostic, ready_ms=(ready[0] - started) / 1e6,
               completion_ms=(ended[0] - started) / 1e6, work=str(work),
               observed_isolation=isolation, exit=process.returncode)
    marks = [dict(item.split('=', 1) for item in line.split()[1:])
             for line in err.splitlines() if line.startswith('pvisor-startup ')]
    if diagnostic:
        row.update(checkpoints(marks, started, ready[0]))
        row['marks'] = marks
    elif marks:
        raise ValueError('diagnostic logging leaked into main matrix')
    return row


def distribution(values):
    return {f'p{p}': percentile(values, p) for p in (50, 95, 99)} | {'mean': statistics.fmean(values)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'rootfs', 'official', 'trimmed', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--samples', type=int, default=100)
    parser.add_argument('--warmups', type=int, default=5)
    parser.add_argument('--profile-samples', type=int, default=20)
    args = parser.parse_args()
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        parser.error('this matrix targets Apple Silicon/HVF')
    if args.samples < 1 or args.warmups < 0 or args.profile_samples < 0:
        parser.error('samples must be positive; warmups/profile-samples must be nonnegative')
    for name in ('binary', 'rootfs', 'official', 'trimmed', 'output'):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / 'trials').mkdir()
    cases = [('direct', 'direct', None, None, None), ('host', 'host', None, None, None)]
    for cpus, memory in SHAPES:
        cases.extend((f'{fw}-{cpus}cpu-{memory}', 'vm', cpus, memory, getattr(args, fw))
                     for fw in ('official', 'trimmed'))
    repo = Path(__file__).resolve().parents[2]
    files = [args.binary, args.official / 'libkrunfw.5.dylib', args.trimmed / 'libkrunfw.5.dylib',
             args.rootfs / 'bin/busybox', args.rootfs / 'etc/os-release', Path(__file__),
             Path(__file__).with_name('startup.py')]
    metadata = dict(schema='pvisor-vm-readiness/v1', protocol=dict(samples=args.samples,
                    warmups=args.warmups, profile_samples=args.profile_samples, shapes=SHAPES,
                    workload=WORKLOAD, seed=20261003, host_cache='warm; no eviction',
                    new_vm_per_trial=True), platform=platform.platform(),
                    source_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
                    source_status=subprocess.check_output(['git', 'status', '--porcelain'], cwd=repo, text=True),
                    load_before=os.getloadavg(), sha256={str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files})
    (args.output / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
    rng, rows = random.Random(20261003), []
    with (args.output / 'samples.jsonl').open('w') as log:
        for diagnostic, count, group in ((False, args.samples, cases),
                                       (True, args.profile_samples, [c for c in cases if c[1] == 'vm' and c[2] == 2])):
            if count == 0:
                continue
            for i in range(-args.warmups, count):
                order = group.copy()
                rng.shuffle(order)
                for case in order:
                    row = trial(args, case, i, diagnostic)
                    if i >= 0:
                        rows.append(row)
                        log.write(json.dumps(row) + '\n')
                        log.flush()
                print(f'{"profile" if diagnostic else "main"} round {i + 1}/{count}', flush=True)
    summary = {}
    for case in cases:
        group = [r for r in rows if r['case'] == case[0] and not r['diagnostic']]
        summary[case[0]] = {k: distribution([r[k] for r in group]) for k in ('ready_ms', 'completion_ms')}
    paired = {}
    for cpus, memory in SHAPES:
        off = [r for r in rows if r['case'] == f'official-{cpus}cpu-{memory}' and not r['diagnostic']]
        trim = [r for r in rows if r['case'] == f'trimmed-{cpus}cpu-{memory}' and not r['diagnostic']]
        delta = [a['ready_ms'] - b['ready_ms'] for a, b in zip(off, trim, strict=True)]
        boot = [statistics.median(rng.choices(delta, k=len(delta))) for _ in range(5000)]
        paired[f'{cpus}cpu-{memory}'] = dict(saved_p50_ms=statistics.median(delta),
            ci95=[percentile(boot, 2.5), percentile(boot, 97.5)], trimmed_faster=sum(d > 0 for d in delta))
    profiles = {}
    for case in cases:
        group = [r for r in rows if r['case'] == case[0] and r['diagnostic']]
        if group:
            profiles[case[0]] = {section: {k: distribution([r[section][k] for r in group])
                for k in group[0][section]} for section in ('phases', 'sub')}
            profiles[case[0]]['ready_ms'] = distribution([r['ready_ms'] for r in group])
    result = metadata | dict(summary=summary, paired=paired, profiles=profiles, samples=rows, load_after=os.getloadavg())
    (args.output / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(dict(summary=summary, paired=paired), indent=2))


if __name__ == '__main__':
    main()
