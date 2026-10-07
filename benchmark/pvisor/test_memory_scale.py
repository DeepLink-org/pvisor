"""Coordinator tests only: no KVM, sysfs writes or real systemd services."""
import argparse
import copy
import json
from pathlib import Path
import subprocess

import pytest

import memory_scale as scale


def valid(mode='baseline', n=2, scanner='0'):
    config = dict(example='/frozen/worker', example_sha256='b' * 64, rootfs='/rootfs',
                  firmware='/firmware', output='/short/t0', vms=n, mode=mode,
                  pattern='repeated', dedup=mode in ('baseline', 'ksm'), seed=7, settle_ms=500,
                  ksm_wait_seconds=2, cgroup='/sys/fs/cgroup/owned.service')
    accounting = dict(cgroup=config['cgroup'], process_errors=[], processes=[dict(
        smaps={'raw': 'Pss: 12 kB\nKSM: 0 kB\n'}, smaps_totals_bytes={'Pss': 12288})],
        counters={name: {'raw': value} for name, value in {
            'memory.max': '2147483648', 'memory.swap.max': '0', 'cpu.max': '400000 100000',
            'memory.current': '100', 'memory.peak': '200', 'memory.swap.current': '0',
            'pids.current': '2', 'memory.stat': 'anon 50\nfile 40\nkernel 10\n',
            'memory.events': 'oom 0\noom_kill 0\n', 'memory.events.local': 'oom 0\noom_kill 0\n',
            'cpu.stat': 'usage_usec 100\n', 'memory.pressure': 'some avg10=0 total=0',
            'cpu.pressure': 'some avg10=0 total=0',
        }.items()}, ksm={'run': {'error': 'not readable'} if scanner is None else {'raw': scanner + '\n'}})
    restored_mode = mode in ('baseline', 'ksm')
    write_mode = restored_mode or mode == 'fresh'
    expected = {(0, 0): 'a' * 64}
    expected.update({(i, p): f'{i * 100 + p:064x}' for i in range(1, n + 1)
                     for p in ((0, 25, 100) if write_mode else (0,))})
    names = ['ready', 'cow25', 'cow100', 'after_exit'] if write_mode else [
        'ready', 'offloaded0', 'resumed0', 'offloaded1', 'resumed1', 'after_exit']
    if mode == 'ksm':
        names = ['ready', 'dynamic_private_before_wait', 'dynamic_private_after_wait',
                 'cow25', 'cow100', 'after_exit']
    report = dict(schema='pvisor-memory-scale/v1', correctness='passed',
                  conditions={k: v for k, v in config.items() if k not in ('example', 'example_sha256', 'cgroup')},
                  profile=dict(memory_mib=256, cpus=1, payload_bytes=64 * 1024**2,
                               page_bytes=4096, max_live_vms=4, deadline_seconds=180),
                  source={'binary_sha256': config['example_sha256']}, cleanup={'all_reaped': True},
                  expected_digests=[dict(instance=i, percent=p, digest=d) for (i, p), d in expected.items()],
                  before=copy.deepcopy(accounting), after=copy.deepcopy(accounting),
                  ksm_scan_window=dict(seconds=2, deadline_is_not_product_failure=True,
                                       before=copy.deepcopy(accounting), after=copy.deepcopy(accounting)),
                  phases=[], checks=[], guests=[])
    report['conditions']['private_baselines'] = False
    for index, name in enumerate(names):
        ids = list(range(2 if name == 'after_exit' else 1, n + 1))
        report['phases'].append(dict(name=name, elapsed_ms=index, paused=True,
                                     offloaded=name.startswith('offloaded'), instances=ids,
                                     heartbeat=[100] * len(ids), heartbeat_stable=True,
                                     accounting=copy.deepcopy(accounting)))

    def check(name, evidence):
        report['checks'].append(dict(name=name, passed=True, evidence=evidence))

    heartbeat = {i: 10 for i in range(n + 1)}

    def phase(name):
        row = next(p for p in report['phases'] if p['name'] == name)
        row['heartbeat'] = [heartbeat[i] for i in row['instances']]
        if not row['offloaded']:
            for i in row['instances']:
                before = heartbeat[i]
                heartbeat[i] += 1
                check('resume_heartbeat_progress', dict(phase=name, instance=i,
                                                       before=before, after=heartbeat[i]))

    def ack(token, op, i, p=0, oracle=None, restored=None, advance=True):
        slot = i if restored is None else restored
        heartbeat[slot] += int(advance)
        return dict(token=token, op=op, instance=i, percent=p,
                    digest=expected[(i if oracle is None else oracle, p)],
                    heartbeat=heartbeat[slot], read_ms=1)

    check('group_budget', {})
    if restored_mode:
        check('producer_payload', ack('producer-ready', 'read', 0))
        report['checkpoint'] = dict(snapshot_id='c' * 64, store='/checkpoint', source_run_id='run0',
                                    source_attempt_id='attempt0', created_at_unix_ms=1, ram_storage='raw')
        receipt = dict(request_id='memory-scale-baseline', checkpoint=report['checkpoint'])
        check('producer_reaped_before_restore', receipt)
        report['producer_final_heartbeat'] = heartbeat[0]
    for i in range(1, n + 1):
        if restored_mode:
            heartbeat[i] = heartbeat[0]
            check('restored_original_and_heartbeat', ack(f'original-{i}', 'read', 0, oracle=0, restored=i))
        check('ready_full_digest', ack(f'prepare-{i}', 'prepare', i))
    phase('ready')
    if mode == 'ksm':
        report['dynamic_ksm_scan_window'] = dict(seconds=2, deadline_is_not_product_failure=True,
                                                merging_required=False, before=copy.deepcopy(accounting),
                                                after=copy.deepcopy(accounting))
        for i in range(1, n + 1):
            check('dynamic_private_full_digest', ack(f'duplicate-{i}', 'duplicate', i))
        phase('dynamic_private_before_wait')
        for i in range(1, n + 1):
            check('dynamic_private_after_wait_full_digest', ack(f'dynamic-readback-{i}', 'read', i))
        phase('dynamic_private_after_wait')
    if restored_mode:
        for i in range(1, n + 1):
            check('private_offload_rejected_and_healthy', dict(instance=i, error='restored private COW RAM',
                   ack=ack(f'rejected-offload-{i}', 'read', i)))
    if write_mode:
        for p in (25, 100):
            for index in range(n):
                i = index + 1
                check('cow_full_digest', ack(f'cow{p}-{i}', 'mutate', i, p))
                for peer in range(index + 2, n + 1):
                    check('peer_write_isolation', ack(f'isolation-{p}-{index}-{peer}', 'read', peer, 0 if p == 25 else 25))
            phase(f'cow{p}')
    else:
        for cycle in range(2):
            for i in range(1, n + 1):
                check('offload_receipt', dict(instance=i, cycle=cycle, elapsed_ms=1,
                                             memory={'backed_bytes': 256 * 1024**2}))
            phase(f'offloaded{cycle}')
            for i in range(1, n + 1):
                parked = heartbeat[i]
                readback = ack(f'readback-{cycle}-{i}', 'read', i, advance=False)
                heartbeat[i] += 1
                check('offload_resume_full_digest_and_heartbeat', dict(cycle=cycle, elapsed_ms=1,
                      ack=readback, parked_heartbeat=parked, resumed_heartbeat=heartbeat[i]))
            phase(f'resumed{cycle}')
    check('one_vm_cancelled_and_reaped', {'instance': 1})
    p = 100 if write_mode else 0
    for i in range(2, n + 1):
        check('survivor_full_digest', ack(f'survivor-{i}', 'read', i, p))
    phase('after_exit')
    for i in range(2, n + 1):
        check('orderly_exit_digest', ack(f'exit-{i}', 'exit', i, p))
    check('cleanup_all_reaped', {'all_reaped': True})
    for i in ([0] if restored_mode else []) + list(range(1, n + 1)):
        result = dict(run_id=f'run{i}', attempt_id=f'attempt{i}',
                      started_at_unix_ms=1 if i == 0 else 3, finished_at_unix_ms=2 if i == 0 else 10,
                      state='hibernated' if i == 0 else 'cancelled' if i == 1 else 'completed')
        if i == 0:
            result['value'] = receipt
        elif i >= 2:
            result['exit_code'] = 0
        report['guests'].append(dict(instance=i, result=result))
    return report, config


@pytest.mark.parametrize('value', ['3', '5', '8', '0', '-1', '1,5', '1,1', '', '01'])
def test_concurrency_hard_cap(value):
    with pytest.raises(argparse.ArgumentTypeError):
        scale.concurrencies(value)


def test_cli_defaults_and_dedup_separation():
    args = scale.parser().parse_args(['--rootfs', '/r', '--firmware', '/f', '--output', '/o'])
    assert args.concurrencies == [1, 2, 4]
    assert args.samples == 5 and args.warmups == 0
    matrix = scale.cells(args)
    assert args.modes == ['baseline', 'ksm', 'raw', 'compressed']
    assert len(matrix) == 54
    assert all(not c['dedup'] for c in matrix if c['mode'] in ('raw', 'compressed'))
    for mode in ('baseline', 'ksm'):
        assert {c['dedup'] for c in matrix if c['mode'] == mode} == {False, True}
    args.dedup = ['on']
    assert all(not c['dedup'] for c in scale.cells(args) if c['mode'] in ('raw', 'compressed'))


@pytest.mark.parametrize('mode', scale.MODES)
@pytest.mark.parametrize('n', [1, 2, 4])
def test_exact_worker_schema_accepted(mode, n):
    report, config = valid(mode, n)
    assert scale.validate_report(report, config)['merging_claim'] is False
    cmd = scale.worker_command(config)
    assert cmd[cmd.index('--dedup') + 1] == ('true' if mode in ('baseline', 'ksm') else 'false')
    assert '--private-baselines' not in cmd
    assert cmd[cmd.index('--vms') + 1] == str(n)
    progress = [c for c in report['checks'] if c['name'] == 'resume_heartbeat_progress']
    assert len(progress) == (6 if mode == 'ksm' else 4) * n - 1
    offload_resumes = [c['evidence'] for c in report['checks']
                      if c['name'] == 'offload_resume_full_digest_and_heartbeat']
    assert len(offload_resumes) == (0 if mode in ('baseline', 'ksm') else 2 * n)
    assert all(e['ack']['heartbeat'] == e['parked_heartbeat'] < e['resumed_heartbeat']
               for e in offload_resumes)


@pytest.mark.parametrize('mode', scale.MODES)
@pytest.mark.parametrize('mutation', ['missing', 'duplicate', 'equal', 'regressed', 'no_before',
                                       'no_after', 'boolean', 'phase', 'instance', 'frozen'])
def test_phase_resume_progress_requires_exact_strict_proofs(mode, mutation):
    report, config = valid(mode)
    check = next(c for c in report['checks'] if c['name'] == 'resume_heartbeat_progress')
    evidence = check['evidence']
    if mutation == 'missing':
        report['checks'].remove(check)
    elif mutation == 'duplicate':
        report['checks'].append(copy.deepcopy(check))
    elif mutation == 'equal':
        evidence['after'] = evidence['before']
    elif mutation == 'regressed':
        evidence['after'] = evidence['before'] - 1
    elif mutation == 'no_before':
        del evidence['before']
    elif mutation == 'no_after':
        del evidence['after']
    elif mutation == 'boolean':
        evidence['before'] = True
    elif mutation == 'phase':
        evidence['phase'] = 'after_exit'
    elif mutation == 'instance':
        evidence['instance'] = 99
    else:
        evidence['before'] -= 1
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


@pytest.mark.parametrize('mode', ['raw', 'compressed'])
@pytest.mark.parametrize('mutation', ['missing', 'duplicate', 'equal', 'regressed', 'no_parked',
                                       'no_resumed', 'boolean', 'parked', 'ack_above_resumed'])
def test_offload_resume_requires_strict_bound_heartbeat_proofs(mode, mutation):
    report, config = valid(mode)
    check = next(c for c in report['checks'] if c['name'] == 'offload_resume_full_digest_and_heartbeat')
    evidence = check['evidence']
    if mutation == 'missing':
        report['checks'].remove(check)
    elif mutation == 'duplicate':
        report['checks'].append(copy.deepcopy(check))
    elif mutation == 'equal':
        evidence['resumed_heartbeat'] = evidence['parked_heartbeat']
    elif mutation == 'regressed':
        evidence['resumed_heartbeat'] = evidence['parked_heartbeat'] - 1
    elif mutation == 'no_parked':
        del evidence['parked_heartbeat']
    elif mutation == 'no_resumed':
        del evidence['resumed_heartbeat']
    elif mutation == 'boolean':
        evidence['parked_heartbeat'] = True
    elif mutation == 'parked':
        evidence['parked_heartbeat'] -= 1
    else:
        evidence['ack']['heartbeat'] = evidence['resumed_heartbeat'] + 1
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


@pytest.mark.parametrize('mode', ['baseline', 'ksm'])
def test_same_iteration_command_ack_is_valid_but_restore_readiness_stays_strict(mode):
    report, config = valid(mode)
    original = next(c['evidence'] for c in report['checks']
                    if c['name'] == 'restored_original_and_heartbeat')
    ready = next(c['evidence'] for c in report['checks'] if c['name'] == 'ready_full_digest')
    ready['heartbeat'] = original['heartbeat']
    scale.validate_report(report, config)
    original['heartbeat'] = report['producer_final_heartbeat']
    with pytest.raises(ValueError, match='strict readiness'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('mode', ['baseline', 'ksm'])
def test_command_ack_can_equal_separately_proven_resume_progress(mode):
    report, config = valid(mode)
    phase = 'dynamic_private_after_wait' if mode == 'ksm' else 'ready'
    progressed = next(c['evidence'] for c in report['checks'] if c['name'] == 'resume_heartbeat_progress'
                      and c['evidence']['phase'] == phase and c['evidence']['instance'] == 1)
    rejected = next(c['evidence']['ack'] for c in report['checks']
                    if c['name'] == 'private_offload_rejected_and_healthy')
    rejected['heartbeat'] = progressed['after']
    scale.validate_report(report, config)
    rejected['heartbeat'] -= 1
    with pytest.raises(ValueError, match='regressed'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('mode', ['raw', 'compressed'])
def test_mutually_exclusive_advice(mode):
    _, config = valid(mode)
    config['dedup'] = True
    with pytest.raises(ValueError, match='separate'):
        scale.worker_command(config)


@pytest.mark.parametrize('mutation', ['phase', 'checks', 'cleanup', 'oracle', 'guest', 'binary',
                                       'barrier', 'snapshot', 'heartbeat', 'condition', 'offload'])
def test_incomplete_reports_rejected(mutation):
    report, config = valid('raw' if mutation == 'offload' else 'baseline')
    if mutation == 'phase':
        report['phases'].pop()
    elif mutation == 'checks':
        report['checks'] = [c for c in report['checks'] if c['name'] != 'ready_full_digest']
    elif mutation == 'cleanup':
        report['cleanup']['all_reaped'] = False
    elif mutation == 'oracle':
        report['expected_digests'].pop()
    elif mutation == 'guest':
        report['guests'][0]['result']['state'] = 'running'
    elif mutation == 'binary':
        report['source']['binary_sha256'] = 'wrong'
    elif mutation == 'barrier':
        report['phases'][0]['heartbeat_stable'] = False
    elif mutation == 'snapshot':
        report['checkpoint']['source_run_id'] = 'another-run'
    elif mutation == 'heartbeat':
        next(c for c in report['checks'] if c['name'] == 'ready_full_digest')['evidence']['heartbeat'] = 1
    elif mutation == 'condition':
        report['conditions']['seed'] += 1
    else:
        report['checks'] = [c for c in report['checks'] if c['name'] != 'offload_receipt']
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


@pytest.mark.parametrize('field,value', [
    ('memory.max', 'max'), ('memory.swap.max', '1'), ('cpu.max', '200000 100000'),
    ('memory.swap.current', '1'), ('memory.events', 'oom 1\noom_kill 0'),
    ('memory.events.local', 'oom 0\noom_kill 1'), ('memory.stat', 'anon 1\nfile 2'),
    ('cpu.stat', ''), ('memory.pressure', ''),
])
def test_per_phase_budget_and_accounting(field, value):
    report, config = valid()
    report['phases'][1]['accounting']['counters'][field]['raw'] = value
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


def test_same_group_and_cpu_monotonicity():
    report, config = valid()
    report['phases'][0]['accounting']['cgroup'] = '/escaped'
    with pytest.raises(ValueError, match='escaped'):
        scale.validate_report(report, config)
    report, config = valid()
    report['after']['counters']['cpu.stat']['raw'] = 'usage_usec 1'
    with pytest.raises(ValueError, match='CPU'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('state', [None, '0', '1', '2'])
def test_scanner_observations_never_claim_group_savings(state):
    report, config = valid(scanner=state)
    observation = scale.validate_report(report, config)
    assert observation['scanner_state'] == ('unknown' if state is None else state)
    assert observation['merging_claim'] is False
    assert 'global' in observation['ksm_attribution']


def test_scanner_phase_consistency():
    report, config = valid()
    report['phases'][1]['accounting']['ksm']['run']['raw'] = '1'
    with pytest.raises(ValueError, match='scanner state changed'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('advice', [False, True])
@pytest.mark.parametrize('n', [1, 2, 4])
def test_ksm_mode_is_a_restored_advice_pair(advice, n):
    args = scale.parser().parse_args(['--rootfs', '/r', '--firmware', '/f', '--output', '/o',
                                      '--modes', 'ksm'])
    assert len(scale.cells(args)) == 18
    report, config = valid('ksm', n)
    report['conditions']['dedup'] = config['dedup'] = advice
    scale.validate_report(report, config)
    cmd = scale.worker_command(config)
    assert cmd[cmd.index('--mode') + 1] == 'ksm'
    assert cmd[cmd.index('--dedup') + 1] == str(advice).lower()
    assert [g['instance'] for g in report['guests']] == list(range(n + 1))


@pytest.mark.parametrize('mutation', ['window', 'seconds', 'deadline', 'merging', 'before', 'after',
                                       'phase', 'duplicate', 'readback', 'token', 'op', 'digest'])
def test_incomplete_dynamic_ksm_evidence_rejected(mutation):
    report, config = valid('ksm')
    window = report['dynamic_ksm_scan_window']
    if mutation == 'window':
        del report['dynamic_ksm_scan_window']
    elif mutation == 'seconds':
        window['seconds'] += 1
    elif mutation == 'deadline':
        window['deadline_is_not_product_failure'] = False
    elif mutation == 'merging':
        window['merging_required'] = True
    elif mutation in ('before', 'after'):
        del window[mutation]
    elif mutation == 'phase':
        report['phases'].pop(1)
    elif mutation in ('duplicate', 'readback'):
        name = 'dynamic_private_full_digest' if mutation == 'duplicate' else 'dynamic_private_after_wait_full_digest'
        report['checks'].remove(next(c for c in report['checks'] if c['name'] == name))
    else:
        ack = next(c for c in report['checks'] if c['name'] == 'dynamic_private_full_digest')['evidence']
        ack[mutation] = {'token': 'wrong-token', 'op': 'read', 'digest': 'f' * 64}[mutation]
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


def test_dynamic_ksm_cpu_accounting_uses_actual_temporal_order():
    report, config = valid('ksm')
    initial = report['ksm_scan_window']
    dynamic = report['dynamic_ksm_scan_window']
    phases = report['phases']
    ordered = [report['before'], initial['before'], initial['after'],
               phases[0]['accounting'], phases[1]['accounting'], dynamic['before'], dynamic['after'],
               phases[2]['accounting'], phases[3]['accounting'], phases[4]['accounting'],
               phases[5]['accounting'], report['after']]
    for i, snapshot in enumerate(ordered):
        snapshot['counters']['cpu.stat']['raw'] = f'usage_usec {100 + i}'
    scale.validate_report(report, config)
    # Each window endpoint must be checked against its surrounding barriers.
    dynamic['before']['counters']['cpu.stat']['raw'] = 'usage_usec 103'
    with pytest.raises(ValueError, match='nonmonotonic group CPU'):
        scale.validate_report(report, config)
    dynamic['before']['counters']['cpu.stat']['raw'] = 'usage_usec 105'
    dynamic['after']['counters']['cpu.stat']['raw'] = 'usage_usec 108'
    with pytest.raises(ValueError, match='nonmonotonic group CPU'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('endpoint', ['before', 'after'])
@pytest.mark.parametrize('mutation', ['scanner', 'group', 'oom', 'budget'])
def test_dynamic_ksm_window_accounting_is_mandatory(endpoint, mutation):
    report, config = valid('ksm')
    accounting = report['dynamic_ksm_scan_window'][endpoint]
    if mutation == 'scanner':
        accounting['ksm']['run']['raw'] = '1'
    elif mutation == 'group':
        accounting['cgroup'] = '/escaped'
    elif mutation == 'oom':
        accounting['counters']['memory.events']['raw'] = 'oom 0\noom_kill 1'
    else:
        accounting['counters']['cpu.max']['raw'] = '200000 100000'
    with pytest.raises(ValueError):
        scale.validate_report(report, config)


@pytest.mark.parametrize('mode', ['baseline', 'ksm'])
@pytest.mark.parametrize('state', [None, '0', '1', '2'])
def test_restored_modes_do_not_require_merging(mode, state):
    report, config = valid(mode, scanner=state)
    assert scale.validate_report(report, config)['merging_claim'] is False


def test_owned_units_are_bounded_no_watcher():
    cmd = scale.unit_command('pvisor-memory-scale-owned.service', Path('/harness'), Path('/config'))
    for flag in ('--property=CPUQuota=400%', '--property=MemoryMax=2147483648',
                 '--property=MemorySwapMax=0', '--property=TasksMax=128',
                 '--property=RuntimeMaxSec=200', '--property=TimeoutStopSec=10',
                 '--property=KillMode=control-group'):
        assert flag in cmd
    assert '--user' in cmd and '--wait' in cmd
    assert not hasattr(scale, 'threading')


def test_deadline_retains_partial_raw_and_stops_owned_unit(tmp_path, monkeypatch):
    report, config = valid()
    config.update(output=str(tmp_path / 'worker'), stdout=str(tmp_path / 'worker.stdout'),
                  stderr=str(tmp_path / 'worker.stderr'), result=str(tmp_path / 'result.json'))
    Path(config['output']).mkdir()
    partial = {'correctness': 'failed', 'phases': []}
    scale.save_json(Path(config['output']) / 'raw.json', partial)
    calls = []

    def run(cmd, **kwargs):
        calls.append((cmd, kwargs))
        assert kwargs['timeout'] <= 220
        if cmd[0] == 'systemd-run':
            kwargs['stderr'].write(b'partial service stderr\n')
            raise subprocess.TimeoutExpired(cmd, 220)
        return subprocess.CompletedProcess(cmd, 0, b'ActiveState=inactive\n', b'')

    monkeypatch.setattr(scale.subprocess, 'run', run)
    record = {'unit': 'pvisor-memory-scale-owned.service'}
    scale.run_attempt(record, config, Path('/harness'), tmp_path)
    assert record['status'] == 'failed' and record['deadline']
    assert record['unit_quiescent']
    assert (tmp_path / 'service.stderr').read_bytes() == b'partial service stderr\n'
    assert json.loads((tmp_path / 'raw.json').read_text()) == partial
    assert any(c[0][:3] == ['systemctl', '--user', 'stop'] for c in calls)


def test_cleanup_timeout_is_bounded(tmp_path, monkeypatch):
    def run(cmd, **kwargs):
        assert kwargs['timeout'] == 15
        raise subprocess.TimeoutExpired(cmd, 15)
    monkeypatch.setattr(scale.subprocess, 'run', run)
    assert scale.settle_unit('pvisor-memory-scale-owned.service', tmp_path) is False


def test_input_manifest_hashes_symlink_target(tmp_path):
    (tmp_path / 'file').write_bytes(b'firmware')
    (tmp_path / 'link').symlink_to('file')
    manifest = scale.inventory(tmp_path)
    link = next(r for r in manifest if r['path'] == 'link')
    assert link['target'] == 'file'
    assert link['resolved_sha256'] == scale.digest(tmp_path / 'file')


def test_counts_keep_missing_and_warmups_separate():
    report = dict(cells=[{'id': 0}, {'id': 1}], attempts=[
        dict(cell=0, status='successful', warmup=True), dict(cell=0, status='failed', warmup=False),
        dict(cell=1, status='unmeasured', warmup=False)])
    scale.counts(report)
    assert report['counts'] == dict(planned=3, attempted=2, successful=1, failed=1, unmeasured=1)
    assert report['cells'][1]['counts']['unmeasured'] == 1


@pytest.mark.parametrize('build_status', ['unverified: no build receipt supplied', 'verified supplied receipt'])
def test_failed_preflight_leaves_remaining_conditions_unmeasured(tmp_path, monkeypatch, build_status):
    example = tmp_path / 'example'
    example.write_text('unused')
    example.chmod(0o755)
    rootfs, firmware = tmp_path / 'rootfs', tmp_path / 'firmware'
    rootfs.mkdir()
    firmware.mkdir()
    output = tmp_path / 'report'
    monkeypatch.setattr(scale.shutil, 'which', lambda name: '/bin/' + name)
    monkeypatch.setattr(scale, 'freeze', lambda args: (example, Path('/harness'),
                         {'example_sha256': 'b' * 64, 'build_provenance': {'status': build_status}}))
    observed = []

    def attempt(record, config, harness, logs):
        observed.append(config)
        record.update(status='failed', unit_quiescent=True, error='fake failed preflight')

    monkeypatch.setattr(scale, 'run_attempt', attempt)
    # /tmp pytest paths may exceed the socket limit; no real scratch is used here.
    monkeypatch.setattr(scale.os, 'fsencode', lambda value: b'/short/t0')
    assert scale.main(['--example', str(example), '--rootfs', str(rootfs), '--firmware', str(firmware),
                       '--output', str(output), '--preflight']) == 1
    report = json.loads((output / 'report.json').read_text())
    assert len(observed) == 1
    assert report['counts'] == dict(planned=54, attempted=1, failed=1, successful=0, unmeasured=53)
    assert report['arguments']['samples'] == 1 and report['arguments']['warmups'] == 0
    assert report['benchmark_id'] == 'B-MEMORY-SCALE'
    assert report['protocol']['build_provenance'] == build_status
    assert report['attempts'][0]['provenance']['build_provenance']['status'] == build_status


def test_producer_must_exit_before_four_restores():
    report, config = valid(n=4)
    report['guests'][1]['result']['started_at_unix_ms'] = 1
    with pytest.raises(ValueError, match='producer overlapped'):
        scale.validate_report(report, config)


@pytest.mark.parametrize('supplied_build', [False, True])
def test_freeze_retains_dirty_source_cargo_inputs_and_harness(tmp_path, monkeypatch, supplied_build):
    repo = tmp_path / 'repo'
    rust = repo / 'crates/pvisor/examples/vm_memory_scale.rs'
    rust.parent.mkdir(parents=True)
    rust.write_text('// uncommitted current worker\n')
    (repo / 'Cargo.toml').write_text('[workspace]\n')
    (repo / 'Cargo.lock').write_text('# current dependencies\n')
    output = tmp_path / 'output'
    output.mkdir()
    binary = tmp_path / 'worker'
    binary.write_bytes(b'frozen executable')
    rootfs, firmware = tmp_path / 'rootfs', tmp_path / 'firmware'
    rootfs.mkdir()
    firmware.mkdir()
    (firmware / 'libkrunfw.so.5').write_bytes(b'firmware')
    args = argparse.Namespace(output=output, example=binary, rootfs=rootfs, firmware=firmware,
                              build_receipt=None)
    if supplied_build:
        args.build_receipt = build_receipt_fixture(tmp_path, binary)
    monkeypatch.setattr(scale, 'REPO', repo)

    def git(*args):
        if args[0] == 'ls-files':
            return b'Cargo.toml\0Cargo.lock\0crates/pvisor/examples/vm_memory_scale.rs\0'
        if args[0] == 'diff':
            return b'dirty patch'
        if args[0] == 'status':
            return b'?? crates/pvisor/examples/vm_memory_scale.rs\n'
        return b'head-identity\n'

    monkeypatch.setattr(scale, 'git', git)
    frozen, harness, receipt = scale.freeze(args)
    assert frozen.read_bytes() == binary.read_bytes()
    assert harness.read_bytes() == Path(scale.__file__).read_bytes()
    assert receipt['dirty_source']
    assert receipt['example_source_sha256'] == scale.digest(rust)
    assert receipt['example_sha256'] == scale.digest(frozen)
    if supplied_build:
        assert receipt['binary_source_relationship'] == 'verified supplied receipt'
        assert (output / 'build/build-receipt.json').read_bytes() == args.build_receipt.read_bytes()
        assert (output / 'build/source-manifest.json').read_bytes() == (args.build_receipt.parent / 'source-manifest.json').read_bytes()
        assert receipt['build_provenance']['receipt']['source_identity']['head'] == 'parent-build-head'
        assert receipt['source_head'] == 'head-identity'
        assert receipt['build_provenance']['source_manifest_sha256'] != receipt['source_manifest_sha256']
    else:
        assert receipt['binary_source_relationship'].startswith('unverified')
        assert not (output / 'build').exists()
    manifest = json.loads((output / 'source-manifest.json').read_text())
    assert {r['path'] for r in manifest} == {'Cargo.toml', 'Cargo.lock', 'crates/pvisor/examples/vm_memory_scale.rs'}
    assert (output / 'source-dirty.patch').read_bytes() == b'dirty patch'


def build_receipt_fixture(tmp_path, binary):
    inputs = tmp_path / 'build-inputs'
    inputs.mkdir()
    scale.save_json(inputs / 'source-manifest.json', [{'path': 'build-time-source.rs', 'sha256': 'a' * 64}])
    path = inputs / 'receipt.json'
    scale.save_json(path, dict(example_sha256=scale.digest(binary),
                              source_manifest_sha256=scale.digest(inputs / 'source-manifest.json'),
                              source_identity=dict(head='parent-build-head', dirty=True),
                              build_command=['cargo', 'build', '--example', 'vm_memory_scale']))
    return path


@pytest.mark.parametrize('mismatch', ['binary', 'manifest'])
def test_build_receipt_rejects_mismatched_bytes(tmp_path, mismatch):
    binary = tmp_path / 'example'
    binary.write_bytes(b'parent built binary')
    path = build_receipt_fixture(tmp_path, binary)
    if mismatch == 'binary':
        binary.write_bytes(b'different binary')
    else:
        (path.parent / 'source-manifest.json').write_text('[]\n')
    output = tmp_path / 'output'
    output.mkdir()
    with pytest.raises(ValueError, match='digest mismatch'):
        scale.freeze_build_receipt(path, binary, output)


@pytest.mark.parametrize('field', ['source_identity', 'build_command'])
def test_build_receipt_requires_build_metadata(tmp_path, field):
    binary = tmp_path / 'example'
    binary.write_bytes(b'parent built binary')
    path = build_receipt_fixture(tmp_path, binary)
    receipt = json.loads(path.read_text())
    del receipt[field]
    scale.save_json(path, receipt)
    output = tmp_path / 'output'
    output.mkdir()
    with pytest.raises(ValueError, match='requires'):
        scale.freeze_build_receipt(path, binary, output)


def test_build_receipt_cli_option():
    args = scale.parser().parse_args(['--rootfs', '/r', '--firmware', '/f', '--output', '/o',
                                      '--build-receipt', '/parent/receipt.json'])
    assert args.build_receipt == Path('/parent/receipt.json')


def test_host_preflight_reads_only_and_keeps_unknown_scanner(monkeypatch):
    def read(path, *args, **kwargs):
        raise PermissionError('no sysfs access')
    def write(*args, **kwargs):
        pytest.fail('host preflight must not write')
    monkeypatch.setattr(Path, 'read_text', read)
    monkeypatch.setattr(Path, 'write_text', write)
    result = scale.host_preflight()
    assert 'error' in result['ksm']['run']
    assert 'no host writes' in result['policy']


def test_fresh_independent_group_requires_write_isolation_without_snapshot_producer():
    report, config = valid('fresh', 4, scanner='1')
    config['dedup'] = report['conditions']['dedup'] = True
    assert scale.validate_report(report, config)['merging_claim'] is False
    assert all(g['instance'] != 0 for g in report['guests'])
    report['checks'] = [c for c in report['checks'] if c['name'] != 'peer_write_isolation']
    with pytest.raises(ValueError):
        scale.validate_report(report, config)
