#!/usr/bin/env python3
"""Bounded, sequential SDK memory-scale engineering coordinator.

Benchmark: B-MEMORY-SCALE (benchmark/README.md#b-memory-scale), role engineering A/B.
Motivation: distinguish baseline sharing, dynamic KSM, COW isolation and offload.
Conclusion sought: checked 1/2/4-VM phase accounting and recovery evidence, not
production density, registration-byte savings or whole-machine RSS savings.
Design: fresh groups, randomized conditions per round, four cores/2 GiB/no swap,
64 MiB checked payloads, baseline/KSM advice off/on; fresh-live offload advice off.
Independent-inode/private-baseline controls are unsupported and remain unmeasured.

Run a separate --preflight output before a formal (at least five-round) run.
Optional --build-receipt verifies the supplied binary/source-manifest hashes and
retains the parent's build record; it does not independently reproduce the build.
No builds, cache eviction, global sysfs writes, pooled batches or tail statistics.
Worker accounting includes charged backing/cache and CPU at barriers; it does not
provide continuous peak timing or prove net host savings. KSM counters are global,
not attributable to this group; scanner unknown/off yields no merging claim.
"""

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import subprocess
import sys
import traceback
import uuid

REPO = Path(__file__).resolve().parents[2]
RESTORE_MODES = ('baseline', 'ksm')
MODES = (*RESTORE_MODES, 'raw', 'compressed')
PATTERNS = ('repeated', 'random-unique', 'random-shared')
MEMORY_MAX = 2 * 1024**3


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def save_json(path, value):
    path = Path(path)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def csv_choices(value, allowed):
    values = value.split(',')
    if not values or len(set(values)) != len(values) or any(v not in allowed for v in values):
        raise argparse.ArgumentTypeError('expected unique subset of ' + ','.join(allowed))
    return values


def concurrencies(value):
    return [int(v) for v in csv_choices(value, ('1', '2', '4'))]


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--example', type=Path, default=REPO / 'target/debug/examples/vm_memory_scale')
    p.add_argument('--build-receipt', type=Path,
                   help='parent build JSON; sibling source-manifest.json and executable hashes must match')
    for name in ('rootfs', 'firmware', 'output'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--scratch-root', type=Path, help='NEW short on-disk directory; default OUTPUT/w')
    p.add_argument('--concurrencies', type=concurrencies, default=[1, 2, 4])
    p.add_argument('--samples', type=int, default=5)
    p.add_argument('--warmups', type=int, default=0)
    p.add_argument('--modes', type=lambda s: csv_choices(s, MODES), default=list(MODES))
    p.add_argument('--patterns', type=lambda s: csv_choices(s, PATTERNS), default=list(PATTERNS))
    p.add_argument('--dedup', type=lambda s: csv_choices(s, ('off', 'on')), default=['off', 'on'],
                   help='baseline/KSM advice controls only; raw/compressed always use off')
    p.add_argument('--seed', type=int, default=20261006)
    p.add_argument('--settle-ms', type=int, default=500)
    p.add_argument('--ksm-wait-seconds', type=int, default=2)
    p.add_argument('--preflight', action='store_true', help='one round, no warmups; stop on first failure')
    return p


def cells(args):
    return [dict(vms=n, mode=mode, pattern=pattern, dedup=advice == 'on')
            for n in args.concurrencies for mode in args.modes for pattern in args.patterns
            for advice in (args.dedup if mode in RESTORE_MODES else ['off'])]


def worker_command(config):
    if config['vms'] not in (1, 2, 4):
        raise ValueError('VM count must be 1, 2 or 4; hard cap includes preparation')
    if config['mode'] not in (*MODES,'fresh','pool') or config['pattern'] not in PATTERNS:
        raise ValueError('unknown condition')
    if type(config['dedup']) is not bool or (config['mode'] not in (*RESTORE_MODES, 'fresh') and config['dedup']):
        raise ValueError('offload and dedup must be separate conditions')
    cmd = [config['example']]
    for name in ('rootfs', 'firmware', 'output', 'vms', 'mode', 'pattern', 'dedup',
                 'seed', 'settle_ms', 'ksm_wait_seconds'):
        value = config[name]
        cmd.extend(['--' + name.replace('_', '-'), str(value).lower() if isinstance(value, bool) else str(value)])
    for name in ('independent_inodes', 'cpus','memory_mib','pool_daemon','group_memory_max'):
        if name in config:
            value = config[name]
            cmd.extend(['--'+name.replace('_','-'), str(value).lower() if isinstance(value,bool) else str(value)])
    return cmd


def unit_command(unit, harness, config):
    return ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect', '--unit=' + unit,
            '--property=MemoryAccounting=yes', '--property=CPUAccounting=yes',
            '--property=MemoryMax=2147483648', '--property=MemorySwapMax=0',
            '--property=CPUQuota=400%', '--property=TasksMax=128',
            '--property=RuntimeMaxSec=200', '--property=TimeoutStopSec=10',
            '--property=KillMode=control-group', sys.executable, str(harness),
            '--internal-worker', str(config)]


def raw_field(accounting, name):
    value = accounting.get('counters', {}).get(name, {}).get('raw')
    if not isinstance(value, str):
        raise ValueError('missing accounting: ' + name)
    return value.strip()


def numbers(raw):
    return {line.split()[0]: int(line.split()[1]) for line in raw.splitlines() if line.strip()}


def scanner(accounting):
    value = accounting.get('ksm', {}).get('run', {}).get('raw')
    if value is None:
        return 'unknown'
    if value.strip() not in ('0', '1', '2'):
        raise ValueError('invalid KSM scanner state')
    return value.strip()


def validate_accounting(accounting, group, memory_max=MEMORY_MAX):
    if accounting.get('cgroup') != group:
        raise ValueError('accounting escaped the owned cgroup')
    if raw_field(accounting, 'memory.max') != str(memory_max) or raw_field(accounting, 'memory.swap.max') != '0':
        raise ValueError('incorrect memory/swap budget')
    quota, period = map(int, raw_field(accounting, 'cpu.max').split())
    if period <= 0 or quota != 4 * period:
        raise ValueError('incorrect four-core budget')
    for field in ('memory.current', 'memory.peak', 'memory.swap.current', 'pids.current'):
        if int(raw_field(accounting, field)) < 0:
            raise ValueError('negative resource accounting')
    if int(raw_field(accounting, 'memory.swap.current')) != 0:
        raise ValueError('swap used')
    stat = numbers(raw_field(accounting, 'memory.stat'))
    cpu = numbers(raw_field(accounting, 'cpu.stat'))
    if not {'anon', 'file', 'kernel'} <= stat.keys() or 'usage_usec' not in cpu:
        raise ValueError('incomplete physical memory/CPU accounting')
    for field in ('memory.events', 'memory.events.local'):
        events = numbers(raw_field(accounting, field))
        if not {'oom', 'oom_kill'} <= events.keys() or any(events.get(k, 0) for k in ('oom', 'oom_kill', 'oom_group_kill')):
            raise ValueError('OOM or missing OOM evidence')
    for field in ('memory.pressure', 'cpu.pressure'):
        if not raw_field(accounting, field):
            raise ValueError('missing PSI accounting')
    if accounting.get('process_errors') != [] or not isinstance(accounting.get('processes'), list):
        raise ValueError('incomplete group process inventory')
    # /proc access gaps remain explicit observations, never substituted with zeros.
    for process in accounting['processes']:
        if not isinstance(process.get('smaps'), dict) or not isinstance(process.get('smaps_totals_bytes'), dict):
            raise ValueError('missing smaps evidence (raw or explicit error required)')
        if not ({'raw', 'error'} & process['smaps'].keys()):
            raise ValueError('silent smaps gap')
    return cpu['usage_usec']


def validate_report(report, config):
    """Reject incomplete successful-looking reports; return observations, not savings."""
    worker_command(config)
    if report.get('schema') != 'pvisor-memory-scale/v1' or report.get('correctness') != 'passed':
        raise ValueError('missing successful integrity proof')
    conditions = report.get('conditions', {})
    memory_max = config.get('group_memory_max',MEMORY_MAX)
    if memory_max not in (MEMORY_MAX,2*MEMORY_MAX) or conditions.get('group_memory_max',MEMORY_MAX)!=memory_max:
        raise ValueError('group memory budget condition mismatch')
    for name in ('rootfs', 'firmware', 'output', 'vms', 'mode', 'pattern', 'dedup', 'seed', 'settle_ms', 'ksm_wait_seconds'):
        if conditions.get(name) != config[name]:
            raise ValueError('wrong condition: ' + name)
    if conditions.get('private_baselines') is not False:
        raise ValueError('private baseline unsupported')
    profile = report.get('profile', {})
    for name, value in dict(memory_mib=config.get('memory_mib',256), cpus=config.get('cpus',1), payload_bytes=64 * 1024**2,
                            page_bytes=4096, max_live_vms=4, deadline_seconds=180).items():
        if profile.get(name) != value:
            raise ValueError('wrong VM profile: ' + name)
    if conditions.get('independent_inodes',False) != config.get('independent_inodes',False):
        raise ValueError('wrong independent-inode condition')
    if config.get('independent_inodes'):
        evidence=[check.get('evidence',{}) for check in report.get('checks',[])
                  if check.get('name')=='independent_ram_inode' and check.get('passed') is True]
        if (len(evidence)!=config['vms'] or len({(item['device'],item['inode']) for item in evidence})!=config['vms']
                or [item['instance'] for item in evidence]!=list(range(1,config['vms']+1))
                or any(item['bytes']<config.get('memory_mib',256)*1024**2 for item in evidence)):
            raise ValueError('missing independent RAM inode proof')
    if report.get('source', {}).get('binary_sha256') != config['example_sha256']:
        raise ValueError('worker binary receipt mismatch')
    if config['mode']=='pool':
        if report.get('pool_cleanup',{}).get('reaped') is not True:
            raise ValueError('daemon pool not reaped')
        if report.get('pool_daemon_sha256') != digest(Path(config['pool_daemon'])):
            raise ValueError('daemon pool binary mismatch')
        if not report.get('pool_observations'):
            raise ValueError('missing actual pool observations')
    if config['mode']=='fresh' and config['dedup']:
        guests=report.get('guests',[])
        accepted=[re.search(r'dedup advice installation: accepted_bytes=(\d+)',
                           g.get('result',{}).get('output',{}).get('stderr','')) for g in guests]
        ready=next((p for p in report.get('phases',[]) if p['name']=='ready'),{})
        eligible=sum(any(line.startswith('VmFlags:') and 'mg' in line.split()[1:]
                         for line in process.get('smaps',{}).get('raw','').splitlines())
                     for process in ready.get('accounting',{}).get('processes',[]))
        if len(accepted)!=config['vms'] or not all(a and int(a.group(1))>0 for a in accepted) or eligible<config['vms']:
            raise ValueError('fresh KSM requires accepted advice and mergeable RAM in every VM')
    if report.get('cleanup', {}).get('all_reaped') is not True:
        raise ValueError('missing terminal reaping fence')
    n, restored_mode = config['vms'], config['mode'] in RESTORE_MODES
    dynamic_ksm = config['mode'] == 'ksm'
    write_mode = restored_mode or config['mode'] in ('fresh','pool')
    phases = report.get('phases', [])
    if dynamic_ksm:
        names = ['ready', 'dynamic_private_before_wait', 'dynamic_private_after_wait',
                 'cow25', 'cow100', 'after_exit']
    elif write_mode:
        names = ['ready', 'cow25', 'cow100', 'after_exit']
    else:
        names = ['ready', 'offloaded0', 'resumed0', 'offloaded1', 'resumed1', 'after_exit']
    if [p.get('name') for p in phases] != names:
        raise ValueError('missing or reordered barrier phases')
    prior_time = -1
    for phase in phases:
        ids = list(range(2 if phase['name'] == 'after_exit' else 1, n + 1))
        if phase.get('instances') != ids or phase.get('heartbeat_stable') is not True or phase.get('paused') is not True:
            raise ValueError('incomplete simultaneous barrier')
        if phase.get('offloaded') != phase['name'].startswith('offloaded'):
            raise ValueError('wrong offload phase')
        beats = phase.get('heartbeat', [])
        if len(beats) != len(ids) or any(type(b) is not int or b <= 0 for b in beats):
            raise ValueError('missing barrier heartbeat')
        elapsed = phase.get('elapsed_ms', -1)
        if elapsed < prior_time:
            raise ValueError('nonmonotonic phase time')
        prior_time = elapsed
    window = report.get('ksm_scan_window', {})
    if window.get('seconds') != config['ksm_wait_seconds'] or window.get('deadline_is_not_product_failure') is not True:
        raise ValueError('missing bounded KSM scan window')
    snapshots = [report.get('before', {}), window.get('before', {}), window.get('after', {})]
    if dynamic_ksm:
        dynamic_window = report.get('dynamic_ksm_scan_window', {})
        if (dynamic_window.get('seconds') != config['ksm_wait_seconds']
                or dynamic_window.get('deadline_is_not_product_failure') is not True
                or dynamic_window.get('merging_required') is not False):
            raise ValueError('missing bounded dynamic KSM scan window (merging must remain optional)')
        # Rust resumes the before-wait barrier, samples the scan window, then
        # checks readback and pauses the group at the after-wait barrier.
        snapshots += [p.get('accounting', {}) for p in phases[:2]]
        snapshots += [dynamic_window.get('before', {}), dynamic_window.get('after', {})]
        snapshots += [p.get('accounting', {}) for p in phases[2:]]
    else:
        snapshots += [p.get('accounting', {}) for p in phases]
    snapshots += [report.get('after', {})]
    usage = [validate_accounting(a, config['cgroup'], config.get('group_memory_max',MEMORY_MAX)) for a in snapshots]
    if usage != sorted(usage):
        raise ValueError('nonmonotonic group CPU')
    states = [scanner(a) for a in snapshots]
    if len(set(states)) != 1:
        raise ValueError('KSM scanner state changed between phases')
    checks = report.get('checks', [])
    if not checks or any(c.get('passed') is not True for c in checks):
        raise ValueError('failed or missing correctness checks')
    by_name = {}
    for check in checks:
        by_name.setdefault(check['name'], []).append(check.get('evidence'))

    def count(name, total):
        if len(by_name.get(name, [])) != total:
            raise ValueError('missing/duplicate proof: ' + name)
        return by_name.get(name, [])

    def strict_progress(evidence, before_field, after_field):
        before, after = evidence.get(before_field), evidence.get(after_field)
        if (type(before) is not int or type(after) is not int
                or not 0 < before < after < 2**64):
            raise ValueError('missing or non-advancing strict resume heartbeat proof')
        return before, after

    progress_phases = [(phase, instance, heartbeat)
                       for phase in phases if not phase['offloaded']
                       for instance, heartbeat in zip(phase['instances'], phase['heartbeat'])]
    progress = count('resume_heartbeat_progress', len(progress_phases))
    for evidence, (phase, instance, heartbeat) in zip(progress, progress_phases):
        before, _ = strict_progress(evidence, 'before', 'after')
        if (evidence.get('phase') != phase['name'] or evidence.get('instance') != instance
                or before != heartbeat):
            raise ValueError('resume heartbeat proof does not match frozen phase/instance')

    count('group_budget', 1)
    count('cleanup_all_reaped', 1)
    if count('one_vm_cancelled_and_reaped', 1)[0].get('instance') != 1:
        raise ValueError('wrong cancellation order')
    expected = {(e['instance'], e['percent']): e['digest'] for e in report.get('expected_digests', [])}
    oracle_keys = {(0, 0)} | {(i, percent) for i in range(1, n + 1)
                                     for percent in ((0, 25, 100) if write_mode else (0,))}
    if set(expected) != oracle_keys or len(report['expected_digests']) != len(expected) or any(len(v) != 64 or any(c not in '0123456789abcdef' for c in v) for v in expected.values()):
        raise ValueError('incomplete full-payload oracle')

    def acks(name, specifications):
        values = count(name, len(specifications))
        for ack, (token, op, instance, percent, oracle) in zip(values, specifications):
            if any(ack.get(k) != v for k, v in dict(token=token, op=op, instance=instance, percent=percent).items()):
                raise ValueError('wrong fresh guest proof: ' + name)
            if (ack.get('digest') != expected.get((oracle, percent))
                    or type(ack.get('heartbeat')) is not int or not 0 < ack['heartbeat'] < 2**64
                    or ack.get('read_ms', -1) < 0):
                raise ValueError('invalid digest/heartbeat/read proof: ' + name)

    acks('ready_full_digest', [(f'prepare-{i}', 'prepare', i, 0, i) for i in range(1, n + 1)])
    if restored_mode:
        acks('producer_payload', [('producer-ready', 'read', 0, 0, 0)])
        receipt = count('producer_reaped_before_restore', 1)[0]
        if receipt.get('request_id') != 'memory-scale-baseline' or receipt.get('checkpoint') != report.get('checkpoint') or not report.get('checkpoint'):
            raise ValueError('missing fresh snapshot/producer receipt')
        acks('restored_original_and_heartbeat', [(f'original-{i}', 'read', 0, 0, 0) for i in range(1, n + 1)])
        if dynamic_ksm:
            acks('dynamic_private_full_digest', [(f'duplicate-{i}', 'duplicate', i, 0, i)
                                                 for i in range(1, n + 1)])
            acks('dynamic_private_after_wait_full_digest', [(f'dynamic-readback-{i}', 'read', i, 0, i)
                                                            for i in range(1, n + 1)])
        rejected = count('private_offload_rejected_and_healthy', n)
        for i, evidence in enumerate(rejected, 1):
            if evidence.get('instance') != i or 'restored private COW RAM' not in evidence.get('error', ''):
                raise ValueError('private offload rejection missing')
        by_name['rejected_ack'] = [e['ack'] for e in rejected]
        acks('rejected_ack', [(f'rejected-offload-{i}', 'read', i, 0, i) for i in range(1, n + 1)])
    if write_mode:
        acks('cow_full_digest', [(f'cow{p}-{i}', 'mutate', i, p, i) for p in (25, 100) for i in range(1, n + 1)])
        acks('peer_write_isolation', [(f'isolation-{p}-{index}-{i}', 'read', i, 0 if p == 25 else 25, i)
                                     for p in (25, 100) for index in range(n) for i in range(index + 2, n + 1)])
    else:
        receipts = count('offload_receipt', 2 * n)
        resumes = count('offload_resume_full_digest_and_heartbeat', 2 * n)
        for index, evidence in enumerate(receipts):
            if evidence.get('cycle') != index // n or evidence.get('instance') != index % n + 1 or evidence.get('memory', {}).get('backed_bytes', 0) < 256 * 1024**2 or evidence.get('elapsed_ms', -1) < 0:
                raise ValueError('incomplete two-cycle RAM offload')
        for index, evidence in enumerate(resumes):
            cycle, instance = index // n, index % n + 1
            if evidence.get('cycle') != cycle or evidence.get('elapsed_ms', -1) < 0:
                raise ValueError('incomplete two-cycle resume')
            parked, resumed = strict_progress(evidence, 'parked_heartbeat', 'resumed_heartbeat')
            offloaded_phase = phases[1 + 2 * cycle]
            ack_heartbeat = evidence.get('ack', {}).get('heartbeat')
            if (parked != offloaded_phase['heartbeat'][instance - 1]
                    or type(ack_heartbeat) is not int or not parked <= ack_heartbeat <= resumed):
                raise ValueError('offload resume heartbeat does not match parked phase/readback')
        by_name['resume_ack'] = [e['ack'] for e in resumes]
        acks('resume_ack', [(f'readback-{cycle}-{i}', 'read', i, 0, i) for cycle in range(2) for i in range(1, n + 1)])
    percent = 100 if write_mode else 0
    acks('survivor_full_digest', [(f'survivor-{i}', 'read', i, percent, i) for i in range(2, n + 1)])
    acks('orderly_exit_digest', [(f'exit-{i}', 'exit', i, percent, i) for i in range(2, n + 1)])
    guests = report.get('guests', [])
    if [g.get('instance') for g in guests] != ([0] if restored_mode else []) + list(range(1, n + 1)):
        raise ValueError('missing producer/guest terminal results')
    identities = set()
    for guest in guests:
        i, result = guest['instance'], guest.get('result', {})
        identity = (result.get('run_id'), result.get('attempt_id'))
        if not all(identity) or identity in identities or result.get('failure') is not None:
            raise ValueError('missing/duplicate fresh Run/Attempt or terminal failure')
        identities.add(identity)
        started, finished = result.get('started_at_unix_ms', 0), result.get('finished_at_unix_ms', 0)
        if started <= 0 or finished < started:
            raise ValueError('missing terminal guest timestamps')
        if restored_mode and i != 0 and started < guests[0]['result']['finished_at_unix_ms']:
            raise ValueError('producer overlapped restored instances; preparation exceeds cap')
        state = 'hibernated' if i == 0 else ('cancelled' if i == 1 else 'completed')
        if result.get('state') != state or (i >= 2 and result.get('exit_code') != 0):
            raise ValueError('wrong terminal guest state')
        if i == 0:
            checkpoint = report['checkpoint']
            if (result.get('value') != receipt or checkpoint.get('source_run_id') != identity[0]
                    or checkpoint.get('source_attempt_id') != identity[1]
                    or checkpoint.get('ram_storage') != 'raw'
                    or checkpoint.get('created_at_unix_ms', 0) <= 0
                    or not Path(checkpoint.get('store', '')).is_absolute()
                    or len(checkpoint.get('snapshot_id', '')) != 64
                    or result.get('exit_code') is not None):
                raise ValueError('fresh snapshot not bound to reaped producer')
            if report.get('producer_final_heartbeat', 0) < by_name['producer_payload'][0]['heartbeat']:
                raise ValueError('missing/regressed saved producer heartbeat')
    # Command acks can complete in the sampled heartbeat iteration. Restore
    # readiness and resume progress are separate strict gates, not ack timing.
    last_heartbeat = {}
    for check in checks:
        evidence = check.get('evidence', {})
        if check['name'] == 'resume_heartbeat_progress':
            instance = evidence['instance']
            if evidence['before'] < last_heartbeat.get(instance, 0):
                raise ValueError('frozen phase heartbeat regressed')
            last_heartbeat[instance] = evidence['after']
            continue
        ack = evidence.get('ack', evidence) if isinstance(evidence, dict) else {}
        if 'token' not in ack:
            continue
        instance = ack['instance']
        if check['name'] == 'restored_original_and_heartbeat':
            instance = int(ack['token'].split('-')[-1])
            if ack['heartbeat'] <= report['producer_final_heartbeat']:
                raise ValueError('restored guest heartbeat failed strict readiness')
            last_heartbeat[instance] = report['producer_final_heartbeat']
        if ack['heartbeat'] < last_heartbeat.get(instance, 0):
            raise ValueError('guest heartbeat regressed')
        if check['name'] == 'offload_resume_full_digest_and_heartbeat':
            if evidence['parked_heartbeat'] < last_heartbeat.get(instance, 0):
                raise ValueError('offload parked heartbeat regressed')
            last_heartbeat[instance] = evidence['resumed_heartbeat']
        else:
            last_heartbeat[instance] = ack['heartbeat']
    return dict(scanner_state=states[0], ksm_attribution='none: global counters are not group savings',
                merging_claim=False, accounting='worker barrier snapshots; continuous monitor not enabled')


def inventory(root):
    entries = []
    for path in [root] + sorted(root.rglob('*')):
        row = dict(path=str(path.relative_to(root)), mode=path.lstat().st_mode)
        if path.is_symlink():
            row.update(kind='symlink', target=os.readlink(path))
            if path.is_file():
                row['resolved_sha256'] = digest(path)
        elif path.is_file():
            row.update(kind='file', bytes=path.stat().st_size, sha256=digest(path))
        elif path.is_dir():
            row['kind'] = 'directory'
        else:
            raise ValueError('unsupported input type: ' + str(path))
        entries.append(row)
    return entries


def git(*arguments):
    return subprocess.run(['git', '--no-pager', *arguments], cwd=REPO, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30).stdout


def freeze_build_receipt(path, binary, output):
    """Verify retained bytes against a supplied build record, not its source claims."""
    destination = output / 'build'
    destination.mkdir()
    shutil.copy2(path, destination / 'build-receipt.json')
    shutil.copy2(path.parent / 'source-manifest.json', destination / 'source-manifest.json')
    receipt = json.loads((destination / 'build-receipt.json').read_text())
    if not isinstance(receipt, dict):
        raise ValueError('build receipt must be a JSON object')
    if receipt.get('example_sha256') != digest(binary):
        raise ValueError('build receipt binary digest mismatch')
    if receipt.get('source_manifest_sha256') != digest(destination / 'source-manifest.json'):
        raise ValueError('build receipt source manifest digest mismatch')
    identity = receipt.get('source_identity')
    if (not isinstance(identity, dict) or not isinstance(identity.get('head'), str)
            or not identity['head'].strip() or type(identity.get('dirty')) is not bool):
        raise ValueError('build receipt requires source_identity head and boolean dirty')
    command = receipt.get('build_command')
    if not ((isinstance(command, str) and command.strip())
            or (isinstance(command, list) and command
                and all(isinstance(part, str) and part.strip() for part in command))):
        raise ValueError('build receipt requires a nonempty build_command string or argument list')
    return dict(status='verified supplied receipt', receipt=receipt,
                receipt_path=str(destination / 'build-receipt.json'),
                receipt_sha256=digest(destination / 'build-receipt.json'),
                source_manifest_path=str(destination / 'source-manifest.json'),
                source_manifest_sha256=digest(destination / 'source-manifest.json'),
                verification='binary and manifest hashes verified; build/source identity are parent assertions, not independently reproduced')


def freeze(args):
    """Keep current source separate from the optionally supplied build-time record."""
    root = args.output
    (root / 'bin').mkdir()
    binary = root / 'bin/vm_memory_scale'
    shutil.copy2(args.example, binary)
    build_receipt = getattr(args, 'build_receipt', None)
    build_provenance = (freeze_build_receipt(build_receipt, binary, root) if build_receipt
                        else dict(status='unverified: no build receipt supplied'))
    (root / 'harness').mkdir()
    harness = root / 'harness/memory_scale.py'
    shutil.copy2(Path(__file__), harness)
    test = Path(__file__).with_name('test_memory_scale.py')
    if test.exists():
        shutil.copy2(test, root / 'harness/test_memory_scale.py')
    paths = git('ls-files', '-z', '--cached', '--others', '--exclude-standard').decode().split('\0')
    selected = sorted({p for p in paths if p and (p.startswith(('crates/', 'vendor/', '.cargo/'))
                      or p in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'))})
    source = []
    for relative in selected:
        path = REPO / relative
        if not path.exists() and not path.is_symlink():
            source.append(dict(path=relative, deleted=True))
            continue
        destination = root / 'source' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination, follow_symlinks=False)
        row = dict(path=relative, mode=path.lstat().st_mode)
        if path.is_symlink():
            row['symlink'] = os.readlink(path)
        else:
            row.update(sha256=digest(destination), bytes=destination.stat().st_size)
        source.append(row)
    example_source = 'crates/pvisor/examples/vm_memory_scale.rs'
    if not any(row['path'] == example_source for row in source):
        raise ValueError('current Rust example missing from source manifest')
    save_json(root / 'source-manifest.json', source)
    (root / 'source-dirty.patch').write_bytes(git('diff', '--binary', 'HEAD'))
    status = git('status', '--porcelain=v1', '--untracked-files=all').decode()
    (root / 'source-status.txt').write_text(status)
    save_json(root / 'rootfs-manifest.json', inventory(args.rootfs))
    save_json(root / 'firmware-manifest.json', inventory(args.firmware))
    receipt = dict(example_sha256=digest(binary), example_source_sha256=digest(root / 'source' / example_source),
                   source_manifest_sha256=digest(root / 'source-manifest.json'),
                   source_head=git('rev-parse', 'HEAD').decode().strip(), dirty_source=bool(status),
                   source_status=status, dirty_patch_sha256=digest(root / 'source-dirty.patch'),
                   binary_source_relationship=build_provenance['status'],
                   build_provenance=build_provenance,
                   dependency_scope='repository crates/vendor/Cargo files/.cargo; external registry sources not frozen',
                   rootfs_manifest_sha256=digest(root / 'rootfs-manifest.json'),
                   firmware_manifest_sha256=digest(root / 'firmware-manifest.json'),
                   harness_sha256={p.name: digest(p) for p in sorted((root / 'harness').glob('*.py'))})
    save_json(root / 'source-receipt.json', receipt)
    return binary, harness, receipt


def host_preflight():
    """Read-only host observations; the worker remains the capability/budget gate."""
    observations = dict(cgroup_v2=Path('/sys/fs/cgroup/cgroup.controllers').exists(), devices={})
    for device in ('/dev/kvm', '/dev/fuse'):
        observations['devices'][device] = dict(exists=Path(device).exists(),
                                               read_write_access=os.access(device, os.R_OK | os.W_OK))
    observations['ksm'] = {}
    for name in ('run', 'pages_to_scan', 'sleep_millisecs', 'full_scans'):
        try:
            observations['ksm'][name] = dict(raw=(Path('/sys/kernel/mm/ksm') / name).read_text())
        except OSError as error:
            observations['ksm'][name] = dict(error=str(error))
    observations['policy'] = 'no host writes; unavailable capabilities fail the retained worker sample'
    return observations


def current_group():
    relative = next(line[3:] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    return str(Path('/sys/fs/cgroup') / relative.lstrip('/'))


def internal(config):
    result = dict(correctness='failed', command=worker_command(config))
    config = config | dict(cgroup=current_group())
    result['cgroup'] = config['cgroup']
    env = os.environ.copy()
    env.update(PVISOR_FS_PROFILE='0', PVISOR_STARTUP_TIMING='0')
    env.pop('PVISOR_TEST_ALLOW_NO_USERNS', None)
    try:
        group = Path(config['cgroup'])
        if (group / 'pids.max').read_text().strip() != '128':
            raise ValueError('TasksMax=128 not installed')
        with Path(config['stdout']).open('wb') as stdout, Path(config['stderr']).open('wb') as stderr:
            completed = subprocess.run(result['command'], stdout=stdout, stderr=stderr, env=env, timeout=185)
        result['returncode'] = completed.returncode
        raw = json.loads((Path(config['output']) / 'raw.json').read_text())
        result['observations'] = validate_report(raw, config)
        if completed.returncode:
            raise ValueError('worker exited nonzero')
        result['correctness'] = 'passed'
    except Exception as error:
        result.update(error=str(error), traceback=traceback.format_exc())
    finally:
        save_json(config['result'], result)
    return result


def settle_unit(unit, logs):
    """Only touch our UUID-owned unit, and never wait indefinitely for cleanup."""
    try:
        stopped = subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True, timeout=15)
        (logs / 'stop.stdout').write_bytes(stopped.stdout)
        (logs / 'stop.stderr').write_bytes(stopped.stderr)
        shown = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ActiveState',
                                '--property=LoadState'], capture_output=True, timeout=10)
        (logs / 'unit-final.txt').write_bytes(shown.stdout + shown.stderr)
        return b'ActiveState=inactive' in shown.stdout or b'ActiveState=failed' in shown.stdout or b'LoadState=not-found' in shown.stdout
    except (OSError, subprocess.TimeoutExpired) as error:
        (logs / 'cleanup-error.txt').write_text(str(error))
        return False


def run_attempt(record, config, harness, logs):
    cfg = logs / 'config.json'
    save_json(cfg, config)
    cmd = unit_command(record['unit'], harness, cfg)
    record.update(command=cmd, worker_command=worker_command(config), config=str(cfg), status='failed')
    try:
        with (logs / 'service.stdout').open('wb') as stdout, (logs / 'service.stderr').open('wb') as stderr:
            completed = subprocess.run(cmd, stdout=stdout, stderr=stderr, timeout=220)
        record['returncode'] = completed.returncode
        result = json.loads(Path(config['result']).read_text())
        record['result'] = result
        if completed.returncode or result.get('correctness') != 'passed':
            raise ValueError('service/worker failed; inspect retained logs and raw.json')
        # Independently validate the persisted worker bytes, not just wrapper status.
        raw = json.loads((Path(config['output']) / 'raw.json').read_text())
        record['observations'] = validate_report(raw, config | dict(cgroup=result['cgroup']))
        record['status'] = 'successful'
    except Exception as error:
        record.update(error=str(error), deadline=isinstance(error, subprocess.TimeoutExpired))
    finally:
        record['unit_quiescent'] = settle_unit(record['unit'], logs)
        raw_path = Path(config['output']) / 'raw.json'
        if raw_path.exists():
            shutil.copy2(raw_path, logs / 'raw.json')
            record['raw_report'] = str(logs / 'raw.json')
        result_path = Path(config['result'])
        if result_path.exists():
            record['result_path'] = str(result_path)
        if not record['unit_quiescent']:
            record.update(status='failed', error='owned unit cleanup not proven; stop sweep to preserve cap')
    return record


def counts(report):
    rows = report['attempts']
    report['counts'] = dict(planned=len(rows), attempted=sum(r['status'] != 'unmeasured' for r in rows),
                           successful=sum(r['status'] == 'successful' for r in rows),
                           failed=sum(r['status'] == 'failed' for r in rows),
                           unmeasured=sum(r['status'] == 'unmeasured' for r in rows))
    for cell in report['cells']:
        matching = [r for r in rows if r['cell'] == cell['id']]
        cell['counts'] = {key: sum(r['status'] == key for r in matching)
                          for key in ('successful', 'failed', 'unmeasured')}
        cell['counts'].update(planned=len(matching), attempted=sum(r['status'] != 'unmeasured' for r in matching))
        cell['measured_counts'] = {key: sum(not r['warmup'] and r['status'] == key for r in matching)
                                   for key in ('successful', 'failed', 'unmeasured')}
        cell['warmup_counts'] = {key: sum(r['warmup'] and r['status'] == key for r in matching)
                                 for key in ('successful', 'failed', 'unmeasured')}


def main(argv=None):
    p = parser()
    args = p.parse_args(argv)
    if args.samples < 1 or args.warmups < 0 or not 0 <= args.seed < 2**64:
        p.error('samples >=1, warmups >=0 and a u64 seed required')
    if not 0 <= args.settle_ms <= 5000 or not 0 <= args.ksm_wait_seconds <= 60:
        p.error('settle-ms must be 0..5000 and ksm-wait-seconds 0..60')
    if args.preflight:
        args.samples, args.warmups = 1, 0
    elif args.samples < 5:
        p.error('formal runs require at least five samples; use --preflight in a separate output')
    for name in ('example', 'rootfs', 'firmware', 'output'):
        setattr(args, name, getattr(args, name).resolve())
    if args.build_receipt is not None:
        args.build_receipt = args.build_receipt.resolve()
    if not args.example.is_file() or not os.access(args.example, os.X_OK):
        p.error('example must be an existing executable; build before measuring')
    if not args.rootfs.is_dir() or not args.firmware.is_dir():
        p.error('rootfs and firmware must be existing directories')
    if args.output.exists():
        p.error('output must be NEW')
    scratch = (args.scratch_root or args.output / 'w').resolve()
    if scratch.exists():
        p.error('scratch-root must be NEW')
    for generated in (args.output, scratch):
        if any(generated == source or source in generated.parents for source in (args.rootfs, args.firmware)):
            p.error('output/scratch-root must not modify the input trees')
    if scratch == args.output or scratch in args.output.parents:
        p.error('scratch-root must not contain the report output')
    matrix = cells(args)
    total = len(matrix) * (args.samples + args.warmups)
    if len(os.fsencode(str(scratch / f't{total - 1}'))) > 70:
        p.error('canonical worker output exceeds 70 bytes; choose a short --output or --scratch-root on disk')
    if not sys.platform.startswith('linux') or os.uname().machine != 'x86_64':
        p.error('requires Linux x86-64')
    if not shutil.which('systemd-run') or not shutil.which('systemctl'):
        p.error('systemd user services required')
    args.output.mkdir(parents=True)
    scratch.mkdir(parents=True)
    batch = uuid.uuid4().hex
    report = dict(schema='pvisor-memory-scale-coordinator/v1', benchmark_id='B-MEMORY-SCALE', role='engineering A/B',
                  batch=batch, recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                  arguments={k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
                  host=dict(kernel=os.uname().release, machine=os.uname().machine,
                            cpu_affinity=sorted(os.sched_getaffinity(0))),
                  budget=dict(cpu_cores=4, memory_max=MEMORY_MAX, swap_max=0, tasks_max=128, runtime_seconds=200),
                  protocol=dict(order='sequential fresh groups; seeded shuffle each paired round',
                                accounting='worker barrier snapshots including backing/cache; no continuous watcher',
                                exclusions='none; all failures retained; deadline not a product-failure claim',
                                source='current source receipt is separate from supplied build-time provenance',
                                build_provenance='unverified: no verified supplied receipt yet',
                                private_baselines='unsupported/unmeasured: no independent captured inode control',
                                ksm='read-only global scanner observations; no attribution from global counters',
                                analysis='no pooled batches, savings estimates or tail latency statistics'),
                  cells=[dict(id=i, condition=cell) for i, cell in enumerate(matrix)], attempts=[])
    rng = random.Random(args.seed)
    for round_index in range(-args.warmups, args.samples):
        order = list(range(len(matrix)))
        rng.shuffle(order)
        for cell_id in order:
            index = len(report['attempts'])
            report['attempts'].append(dict(id=index, cell=cell_id, round=round_index,
                                          warmup=round_index < 0, status='unmeasured',
                                          unit=f'pvisor-memory-scale-{batch}-{index}.service',
                                          worker_output=str(scratch / f't{index}')))
    def save():
        counts(report)
        save_json(args.output / 'report.json', report)
    save()
    try:
        report['host_preflight'] = host_preflight()
        save()
        binary, harness, receipt = freeze(args)
        report['provenance'] = receipt
        report['protocol']['build_provenance'] = receipt.get('build_provenance', {}).get(
            'status', 'unverified: no build receipt supplied')
        for cell in report['cells']:
            cell['provenance_receipt'] = str(args.output / 'source-receipt.json')
        save()
        for record in report['attempts']:
            logs = args.output / 'attempts' / str(record['id'])
            logs.mkdir(parents=True)
            config = matrix[record['cell']] | dict(example=str(binary), example_sha256=receipt['example_sha256'],
                         rootfs=str(args.rootfs), firmware=str(args.firmware), output=record['worker_output'],
                         seed=args.seed, settle_ms=args.settle_ms, ksm_wait_seconds=args.ksm_wait_seconds,
                         stdout=str(logs / 'worker.stdout'), stderr=str(logs / 'worker.stderr'),
                         result=str(logs / 'result.json'))
            record['status'] = 'attempting'
            record['provenance'] = receipt
            save()
            run_attempt(record, config, harness, logs)
            save()
            if not record['unit_quiescent'] or (args.preflight and record['status'] != 'successful'):
                report['stopped'] = 'failed preflight or unproven owned-unit cleanup; remaining conditions unmeasured'
                break
    except Exception as error:
        report.update(error=str(error), traceback=traceback.format_exc())
        for record in report['attempts']:
            if record['status'] == 'attempting':
                record.update(status='failed', error=str(error))
    finally:
        save()
    return 1 if report.get('error') or report['counts']['failed'] or report['counts']['unmeasured'] else 0


if __name__ == '__main__':
    if sys.argv[1:2] == ['--internal-worker']:
        configuration = json.loads(Path(sys.argv[2]).read_text())
        outcome = internal(configuration)
        raise SystemExit(0 if outcome['correctness'] == 'passed' else 1)
    raise SystemExit(main())
