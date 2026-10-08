#!/usr/bin/env python3
"""Benchmark: B-VCPU-IDLE-ENG (benchmark/README.md#b-vcpu-idle-eng), role engineering A/B.
Motivation: can real guest workloads expose waiting opportunities at acceptable cost?
Conclusion sought: backend-specific windows/Unknown and paired wall/CPU overhead, not offload gains.
Design: fresh 256 MiB VMs; sleep/busy/short-timer on 1/2 CPUs plus SMP one-busy;
seeded random off/on pairs, bounded samples, output validation, receipts and full failures.
M0 observe-only; no pause/offload, sysctl changes, M1/M2 or production density claims.
"""
import argparse
import hashlib
import json

import os
from pathlib import Path
import platform
import random
import shutil
import signal
import statistics
import subprocess
import time
import traceback

REPO = Path(__file__).resolve().parents[2]
CASES = [(w, c) for w in ('sleep', 'busy', 'short-timer') for c in (1, 2)] + [('smp-one-busy', 2)]
DIGEST = hashlib.sha256(bytes(range(256)) * 256).hexdigest()
STATES = {'Executing', 'HandlingExit', 'WaitingForEvent', 'HostDescheduled', 'ManualPaused', 'Stopped', 'Unknown'}


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def save(path, value):
    path = Path(path)
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')
    tmp.replace(path)


def source_inventory():
    names = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=REPO).decode().split('\0')
    selected = sorted({n for n in names if n and (n in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')
        or n.startswith(('crates/', 'scripts/', '.cargo/'))
        or n in ('benchmark/pvisor/vcpu_idle.py', 'benchmark/pvisor/vcpu_idle_plan.md',
                 'benchmark/pvisor/test_vcpu_idle.py'))})
    return {n: digest(REPO / n) for n in selected if (REPO / n).is_file()}


def build(output):
    output.mkdir(parents=True, exist_ok=False)
    sources = source_inventory()
    save(output / 'sources.json', sources)
    for name in sources:
        target = output / 'source' / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / name, target)
    command = ['cargo', 'build', '--locked', '-p', 'pvisor', '--example', 'vm_vcpu_observe', '--message-format=json-render-diagnostics']
    receipt = {'schema': 1, 'benchmark': 'B-VCPU-IDLE-ENG', 'command': command,
               'git_head': subprocess.check_output(['git', '--no-pager', 'rev-parse', 'HEAD'], cwd=REPO).decode().strip(),
               'rustc': subprocess.check_output(['rustc', '-Vv'], cwd=REPO).decode(),
               'environment': {k: v for k, v in os.environ.items() if k.startswith(('CARGO_', 'RUST', 'PVISOR_KRUNFW'))},
               'sources_sha256': digest(output / 'sources.json'), 'complete': False}
    (output / 'dirty.patch').write_bytes(subprocess.check_output(['git', '--no-pager', 'diff', 'HEAD'], cwd=REPO))
    save(output / 'build-receipt.json', receipt)
    with (output / 'build.log').open('wb') as log:
        result = subprocess.run(command, cwd=REPO, stdout=log, stderr=subprocess.STDOUT, timeout=600)
    if result.returncode:
        raise RuntimeError('build failed; see build.log')
    if sources != source_inventory():
        raise ValueError('sources changed during build')
    artifacts = []
    for line in (output / 'build.log').read_text().splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get('reason') == 'compiler-artifact' and event['target']['name'] == 'vm_vcpu_observe' and event.get('executable'):
            artifacts.append(event['executable'])
    if len(artifacts) != 1:
        raise ValueError('expected one Cargo example artifact')
    shutil.copy2(artifacts[0], output / 'vm_vcpu_observe')
    if platform.system() == 'Darwin':
        signing = ['codesign', '--force', '--sign', '-', '--entitlements', str(REPO/'crates/pvisor/macos-hypervisor.entitlements'), str(output/'vm_vcpu_observe')]
        subprocess.run(signing, check=True, timeout=30)
        subprocess.run(['codesign', '--verify', '--strict', str(output/'vm_vcpu_observe')], check=True, timeout=30)
        receipt['signing_command'] = signing
    receipt['sdk'] = json.loads(subprocess.check_output([str(output/'vm_vcpu_observe'), '--describe'], timeout=10))
    receipt.update(binary_sha256=digest(output / 'vm_vcpu_observe'), complete=True)
    save(output / 'build-receipt.json', receipt)


def verify_build(receipt_path):
    base = receipt_path.parent
    receipt = json.loads(receipt_path.read_text())
    if receipt.get('schema') != 1 or not receipt.get('complete') or receipt.get('benchmark') != 'B-VCPU-IDLE-ENG':
        raise ValueError('incomplete/foreign build receipt')
    if digest(base / 'sources.json') != receipt['sources_sha256']:
        raise ValueError('source manifest mismatch')
    sources = json.loads((base / 'sources.json').read_text())
    if sources != source_inventory():
        raise ValueError('current source differs from build; build a fresh receipt')
    for name, sha in sources.items():
        if digest(base / 'source' / name) != sha:
            raise ValueError('frozen source mismatch: ' + name)
    binary = base / 'vm_vcpu_observe'
    if digest(binary) != receipt['binary_sha256']:
        raise ValueError('binary receipt mismatch')
    sdk = json.loads(subprocess.check_output([str(binary.resolve()), '--describe'], timeout=10))
    if sdk != receipt['sdk']:
        raise ValueError('SDK/kernel identity mismatch')
    return binary.resolve()


def inventory(root):
    rows = {}
    for p in sorted(root.rglob('*')):
        name = str(p.relative_to(root))
        if p.is_symlink():
            rows[name] = {'link': os.readlink(p)}
        elif p.is_file():
            rows[name] = {'sha256': digest(p), 'mode': p.stat().st_mode & 0o7777, 'size': p.stat().st_size}
        elif p.is_dir():
            rows[name] = {'directory': True, 'mode': p.stat().st_mode & 0o7777}
        else:
            raise ValueError('special input file unsupported: ' + str(p))
    return rows


def schedule(rounds, seed):
    rng = random.Random(seed)
    plan = []
    for round_id in range(rounds):
        cases = CASES.copy()
        rng.shuffle(cases)
        for workload, cpus in cases:
            order = [False, True]
            rng.shuffle(order)
            for enabled in order:
                plan.append(dict(round=round_id, workload=workload, cpus=cpus, observer=enabled))
    return plan


def execute(command, directory, timeout, env):
    start = time.monotonic()
    with (directory / 'stdout.log').open('wb') as stdout, (directory / 'stderr.log').open('wb') as stderr:
        p = subprocess.Popen(command, stdout=stdout, stderr=stderr, env=env, start_new_session=True)
        timed_out = False
        try:
            while True:
                pid, status, usage = os.wait4(p.pid, os.WNOHANG)
                if pid:
                    break
                if time.monotonic() - start > timeout:
                    timed_out = True
                    os.killpg(p.pid, signal.SIGKILL)
                    _, status, usage = os.wait4(p.pid, 0)
                    break
                time.sleep(.01)
        except BaseException:
            os.killpg(p.pid, signal.SIGKILL)
            os.wait4(p.pid, 0)
            p.returncode = -signal.SIGKILL
            raise
        p.returncode = os.waitstatus_to_exitcode(status)
    return dict(returncode=p.returncode, timeout=timed_out, wall_s=time.monotonic()-start,
                cpu_s=usage.ru_utime+usage.ru_stime, maxrss_native=usage.ru_maxrss)


def validate_snapshot(s, cpus, enabled):
    if s['enabled'] is not enabled or s['hypervisor'] not in ('Kvm', 'Hvf'):
        raise ValueError('observer/backend mismatch')
    vs = s['vcpus']
    if len(vs) != cpus or [v['id'] for v in vs] != list(range(cpus)):
        raise ValueError('incomplete vCPU topology')
    for v in vs:
        if v['state'] not in STATES or not 0 <= v['since_ns'] <= s['sampled_at_ns']:
            raise ValueError('invalid state/time')
        if v['state'] in ('ManualPaused', 'HostDescheduled', 'Stopped') or not v['online']:
            raise ValueError('unexpected control/scheduling state during experiment')
        if s['hypervisor']=='Kvm' and v['state']=='WaitingForEvent':
            raise ValueError('KVM M0 cannot report an exact waiting state')
        if not 0 <= v['wait_exits'] <= v['wait_entries'] or not 0 <= v['completed_wait_ns'] <= s['sampled_at_ns']:
            raise ValueError('invalid wait counters')
    all_waiting = all(v['online'] and v['state'] == 'WaitingForEvent' for v in vs)
    if s['all_waiting'] != all_waiting:
        raise ValueError('invalid aggregation')
    if all_waiting:
        if s['hypervisor'] == 'Kvm' or s['rejection'] != 'WakeDeadlineUnavailable' or not isinstance(s['all_waiting_since_ns'], int):
            raise ValueError('invalid waiting opportunity')
    elif s['all_waiting_since_ns'] is not None:
        raise ValueError('inactive window has start')
    expected = 'Disabled' if not enabled else ('Unknown' if any(v['state']=='Unknown' for v in vs) else ('WakeDeadlineUnavailable' if all_waiting else 'NotAllWaiting'))
    if s['rejection'] != expected or (enabled and s['session'] <= 0):
        raise ValueError('session/rejection inconsistent with current states')
    if all_waiting and not 0 <= s['all_waiting_since_ns'] <= s['sampled_at_ns']:
        raise ValueError('invalid window start')
    if not 0 <= s['completed_all_waiting_ns'] <= s['sampled_at_ns']:
        raise ValueError('invalid window cumulative time')
    if s['rejection'] not in ('Disabled', 'TopologyIncomplete', 'Unknown', 'NotAllWaiting', 'WakeDeadlineUnavailable'):
        raise ValueError('unknown rejection')


def validate_trial(directory, cell, seconds, interval):
    guest = json.loads((directory / 'guest.json').read_text())
    observer = json.loads((directory / 'observer.json').read_text())
    initial = json.loads((directory / 'initial.json').read_text())
    validate_snapshot(initial, cell['cpus'], cell['observer'])
    if guest['mode'] != cell['workload'] or guest['cpus'] != cell['cpus'] or guest['start_method'] != 'fork' or guest['elapsed_ns'] < seconds*10**9:
        raise ValueError('guest workload/elapsed mismatch')
    if len(guest['workers']) != cell['cpus']:
        raise ValueError('missing guest worker')
    for cpu, row in enumerate(guest['workers']):
        if row['cpu'] != cpu or row['affinity'] != [cpu] or row['iterations'] <= 0 or row['digest'] != DIGEST or row['elapsed_ns'] < seconds*10**9:
            raise ValueError('guest output/affinity/digest mismatch')
        busy = cell['workload']=='busy' or (cell['workload']=='smp-one-busy' and cpu==0)
        if row['busy'] is not busy or row['cpu_ns'] < 0 or row['finished_ns']-row['started_ns'] < seconds*10**9:
            raise ValueError('guest compute/time evidence mismatch')
    overlap = min(w['finished_ns'] for w in guest['workers']) - max(w['started_ns'] for w in guest['workers'])
    if overlap < seconds*10**9//2:
        raise ValueError('workers did not overlap for half the planned workload')
    if 'vcpu-guest-ok' not in (directory / 'stdout.log').read_text():
        raise ValueError('missing guest shutdown success')
    if (directory / 'observer-error.txt').exists():
        raise ValueError('observer reported error')
    rows = [json.loads(line) for line in (directory / 'samples.jsonl').read_text().splitlines()]
    limit = (seconds+60)*1000//interval+2
    if observer != {'observer': cell['observer'], 'samples': len(rows), 'limit': limit, 'complete': True}:
        raise ValueError('incomplete observer receipt')
    if not 0 <= len(rows) <= limit or bool(rows) != cell['observer']:
        raise ValueError('sample bound/off samples violation')
    last = initial
    reasons, states, epochs = {}, {}, set()
    for row in rows:
        if not isinstance(row['sample_call_and_encode_ns'], int) or row['sample_call_and_encode_ns'] < 0:
            raise ValueError('invalid sampling cost')
        s = row['snapshot']
        validate_snapshot(s, cell['cpus'], True)
        for k in ('session', 'topology_generation', 'hypervisor'):
            if s[k] != initial[k]:
                raise ValueError('observation identity changed')
        for k in ('sampled_at_ns', 'sequence', 'idle_epoch', 'completed_all_waiting_ns'):
            if s[k] < last[k]:
                raise ValueError('counter regression: '+k)
        for v, before in zip(s['vcpus'], last['vcpus']):
            for k in ('sequence', 'transitions', 'wait_entries', 'wait_exits', 'completed_wait_ns'):
                if v[k] < before[k]:
                    raise ValueError('vCPU counter regression')
            states[v['state']] = states.get(v['state'], 0) + 1
        reasons[s['rejection']] = reasons.get(s['rejection'], 0)+1
        if s['all_waiting']:
            epochs.add(s['idle_epoch'])
        last = s
    # Counter includes windows shorter than sampler resolution; current window is separate.
    active = (last['sampled_at_ns']-last['all_waiting_since_ns']) if last['all_waiting'] else 0
    return dict(backend=initial['hypervisor'], samples=len(rows), state_counts=states,
                rejection_counts=reasons, sampled_wait_epochs=len(epochs), idle_epoch=last['idle_epoch'],
                completed_all_waiting_ns=last['completed_all_waiting_ns'], active_all_waiting_ns=active,
                sample_call_and_encode_ns=[r['sample_call_and_encode_ns'] for r in rows], guest=guest)


def paired_summary(trials, seed):
    rng = random.Random(seed)
    result = []
    for workload, cpus in CASES:
        pairs = {}
        for t in trials:
            c = t['cell']
            if (c['workload'], c['cpus']) == (workload, cpus):
                pairs.setdefault(c['round'], {})[c['observer']] = t
        valid = [p for p in pairs.values() if len(p) == 2 and all(t['status'] == 'valid' for t in p.values())]
        row = dict(workload=workload, cpus=cpus, valid_pairs=len(valid), invalid_pairs=len(pairs)-len(valid))
        for metric in ('wall_s', 'cpu_s'):
            delta = [p[True]['execution'][metric]-p[False]['execution'][metric] for p in valid]
            if delta:
                boot = sorted(statistics.median(rng.choices(delta, k=len(delta))) for _ in range(2000))
                row[metric] = dict(median_on_minus_off=statistics.median(delta), ci95=[boot[49], boot[1949]],
                                   note='descriptive; n=1 cannot estimate uncertainty' if len(delta)==1 else 'paired bootstrap')
        result.append(row)
    return result


def run(a):
    a.output.mkdir(parents=True, exist_ok=False)
    report = dict(benchmark='B-VCPU-IDLE-ENG', stage='M0', complete=False, trials=[],
                  offload_benefit='unmeasured', M1='not implemented', M2='not implemented',
                  host=platform.uname()._asdict(), affinity=sorted(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else 'unavailable',
                  config=vars(a) | {'output': str(a.output)})
    report['config'] = {k: str(v) if isinstance(v, Path) else v for k,v in report['config'].items()}
    try:
        binary = verify_build(a.build_receipt)
        rootfs, firmware, init = a.rootfs.resolve(strict=True), a.firmware.resolve(strict=True), a.init.resolve(strict=True)
        if a.output.resolve().is_relative_to(rootfs):
            raise ValueError('output must be outside input rootfs')
        sdk = json.loads(subprocess.check_output([str(binary), '--describe'], timeout=10))
        firmware_file = firmware / sdk['firmware_name']
        if sdk['embedded_kernel'] is None and not firmware_file.is_file():
            raise ValueError('selected firmware directory lacks SDK firmware: '+str(firmware_file))
        firmware_sha = digest(firmware_file) if firmware_file.is_file() else None
        report['sdk'] = sdk
        report['firmware_file_sha256'] = firmware_sha
        inputs = {'rootfs': inventory(rootfs), 'firmware': inventory(firmware), 'init_sha256': digest(init)}
        save(a.output / 'inputs.json', inputs)
        shutil.copytree(a.build_receipt.parent, a.output / 'build')
        binary = a.output.resolve() / 'build/vm_vcpu_observe'
        plan = schedule(a.pairs, a.seed)
        save(a.output / 'schedule.json', plan)
        env = os.environ.copy()
        env['LD_LIBRARY_PATH' if platform.system() == 'Linux' else 'DYLD_LIBRARY_PATH'] = str(firmware)
        # The library loader must not silently choose an ambient alternate firmware.
        for i, cell in enumerate(plan):
            d = a.output.resolve() / f't{i:04d}'
            d.mkdir()
            trial = dict(cell=cell, status='failed', directory=d.name)
            report['trials'].append(trial)
            try:
                shutil.copytree(rootfs, d / 'rootfs', symlinks=True)
                command = [str(binary), '--rootfs', str(d / 'rootfs'), '--init', str(init), '--output', str(d)]
                for key, value in cell.items():
                    if key != 'round':
                        command += ['--'+key, str(value).lower() if isinstance(value, bool) else str(value)]
                command += ['--seconds', str(a.seconds), '--interval-ms', str(a.interval_ms), '--python', a.python]
                save(d / 'command.json', command)
                trial['execution'] = execute(command, d, a.timeout, env)
                if trial['execution']['returncode'] or trial['execution']['timeout']:
                    raise ValueError('VM nonzero exit or timeout')
                trial['observation'] = validate_trial(d, cell, a.seconds, a.interval_ms)
                if platform.system() == 'Linux' and sdk['embedded_kernel'] is None:
                    maps = (d/'ready-maps.txt').read_text()
                    if str(firmware_file.resolve()) not in maps:
                        raise ValueError('requested dynamic firmware absent from real VM maps')
                trial['status'] = 'valid'
            except Exception:
                trial['error'] = traceback.format_exc()
            save(a.output / 'report.json', report)
        if inputs != {'rootfs': inventory(rootfs), 'firmware': inventory(firmware), 'init_sha256': digest(init)}:
            raise ValueError('input assets changed during experiment; cohort invalid')
        if firmware_sha != (digest(firmware_file) if firmware_file.is_file() else None):
            raise ValueError('firmware symlink target changed; cohort invalid')
        verify_build(a.output / 'build/build-receipt.json')
        report['summary'] = paired_summary(report['trials'], a.seed)
        report['complete'] = all(t['status'] == 'valid' for t in report['trials'])
    except BaseException:
        report['error'] = traceback.format_exc()
    finally:
        report['valid_trials'] = sum(t['status']=='valid' for t in report['trials'])
        report['failed_trials'] = sum(t['status']!='valid' for t in report['trials'])
        save(a.output / 'report.json', report)
    return 0 if report['complete'] else 1


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--build', action='store_true', help='build and freeze only; never starts a VM')
    p.add_argument('--output', type=Path, required=True)
    for name in ('build-receipt', 'rootfs', 'firmware', 'init'):
        p.add_argument('--'+name, type=Path)
    p.add_argument('--python', default='/usr/bin/python3', help='absolute Python 3 path inside guest')
    p.add_argument('--pairs', type=int, default=5)
    p.add_argument('--seed', type=int, default=20261007)
    p.add_argument('--seconds', type=int, default=3)
    p.add_argument('--interval-ms', type=int, default=10)
    p.add_argument('--timeout', type=int, default=90)
    a = p.parse_args()
    if a.build:
        build(a.output.resolve())
        return 0
    if any(getattr(a, n) is None for n in ('build_receipt', 'rootfs', 'firmware', 'init')):
        p.error('measurement requires --build-receipt --rootfs --firmware --init')
    if not (1 <= a.pairs <= 100 and 1 <= a.seconds <= 30 and 1 <= a.interval_ms <= 1000 and a.seconds+60 <= a.timeout <= 180):
        p.error('bounds: pairs 1..100, seconds 1..30, interval 1..1000 ms, timeout seconds+60..180')
    if not a.python.startswith('/'):
        p.error('--python must be an absolute guest path')
    return run(a)


if __name__ == '__main__':
    raise SystemExit(main())
