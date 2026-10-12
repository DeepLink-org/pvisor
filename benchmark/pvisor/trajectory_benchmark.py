#!/usr/bin/env python3
"""Run one fixed-trajectory condition, retaining every attempt.

Benchmark: B-TRAJECTORY-ENG (benchmark/README.md#b-trajectory-eng), role engineering A/B.
Motivation: test real tools across same-VM pause/offload/resume barriers.
Conclusion sought: continuity, artifacts, and charged service resource costs.
Design: one new root upper/raw backing per sample, 2 vCPU/2 GiB guest,
4 CPU/4 GiB/zero-swap service, external 100 ms sampler. No public speed claims.
"""
import argparse
import csv
import json
import os
import re
import shutil
import subprocess
import sys
import time
import traceback
import uuid
from pathlib import Path

from prepare_replay_rootfs import digest


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def counters(path):
    return {key: int(value) for key, value in (line.split() for line in path.read_text().splitlines())}


def comparable_output(data, step):
    """Frozen allowlist: only SymPy test seeds, timing and summary decoration."""
    text = data.decode('utf-8', errors='strict')
    if step in (7, 19, 29, 33, 39):
        text = re.sub(r'(?m)^(random seed:\s*)\d+', r'\1<seed>', text)
        text = re.sub(r'PYTHONHASHSEED=\d+', 'PYTHONHASHSEED=<seed>', text)
        text = re.sub(r'\bin \d+(?:\.\d+)? seconds\b', 'in <elapsed> seconds', text)
        text = '\n'.join(line.strip('= ').rstrip() if 'tests finished:' in line else line.rstrip()
                         for line in text.splitlines()) + '\n'
    return text


def historical_output(actual, expected, step, preflight_warnings):
    """Two explicitly audited fixture differences; never ignore other warnings."""
    if comparable_output(actual, step) == comparable_output(expected, step):
        return 'equal-after-test-seed-and-time-normalization'
    if step == 5 and expected == b'bash: line 1: pytest: command not found\n' and actual == b'/bin/bash: line 1: pytest: command not found\n':
        return 'absolute-bash-path-in-error'
    warning = ('/testbed/sympy/solvers/diophantine.py:3188: SyntaxWarning: "is" with a literal. Did you mean "=="?\n'
               "  if feasible is 1:  # it's prime and k == 2\n")
    if step == 6 and not actual and expected == warning.encode() and '\n'.join(preflight_warnings) + '\n' == warning:
        return 'exact-first-import-warning-observed-in-preflight'
    raise ValueError('unexplained historical stdout difference at step %d' % step)


def sample(group):
    stat = counters(group / 'memory.stat')
    return dict(host_monotonic_ns=time.monotonic_ns(), memory_current=int((group / 'memory.current').read_text()),
                memory_peak=int((group / 'memory.peak').read_text()),
                swap_current=int((group / 'memory.swap.current').read_text()),
                anon=stat['anon'], file=stat['file'], kernel=stat['kernel'],
                cpu=counters(group / 'cpu.stat'), events=counters(group / 'memory.events'),
                io=(group / 'io.stat').read_text() if (group / 'io.stat').exists() else None,
                io_accounting='cgroup' if (group / 'io.stat').exists() else 'unavailable: controller not delegated',
                pids=(group / 'cgroup.procs').read_text().split())


def service(config_path):
    config = json.loads(config_path.read_text())
    root = config_path.parent
    group = Path('/sys/fs/cgroup') / Path('/proc/self/cgroup').read_text().strip().split('::', 1)[1].lstrip('/')
    assert (group / 'memory.max').read_text().strip() == '4294967296'
    assert (group / 'memory.swap.max').read_text().strip() == '0'
    quota, period = map(int, (group / 'cpu.max').read_text().split())
    assert quota / period == 4
    write(root / 'service-start.json', dict(cgroup=str(group), pid=os.getpid(),
          affinity=sorted(os.sched_getaffinity(0)), sample=sample(group)))
    env = os.environ.copy()
    for name in list(env):
        if name.startswith('PVISOR_'):
            env.pop(name)
    env.update(PVISOR_FS_PROFILE='0', PVISOR_STARTUP_TIMING='0',
               PVISOR_RUN_HOME=str(root / 'run-home'), XDG_CONFIG_HOME=str(root / 'config-home'))
    if config.get('enomem_workaround', 'auto') != 'auto':
        env['PVISOR_KRUN_ENOMEM_WORKAROUND'] = config['enomem_workaround']
    with (root / 'sdk.stdout').open('wb') as stdout, (root / 'sdk.stderr').open('wb') as stderr:
        command = [config['binary'], str(config_path)]
        if config.get('apparmor_profile'):
            command = ['aa-exec', '-p', config['apparmor_profile'], '--'] + command
        result = subprocess.run(command, stdout=stdout, stderr=stderr, env=env)
    final = sample(group)
    write(root / 'service-final.json', dict(returncode=result.returncode, sample=final,
          remaining_other_pids=[pid for pid in final['pids'] if int(pid) != os.getpid()]))
    return result.returncode or bool([pid for pid in final['pids'] if int(pid) != os.getpid()])


def verify(config, prepared, reference):
    root = Path(config['output'])
    driver = json.loads((root / 'driver-ok.json').read_text())
    steps_root = root / 'stage/upper/replay-output'
    steps = [json.loads(path.read_text()) for path in sorted(steps_root.glob('step-*.json'))]
    result = dict(driver=driver, step_count=len(steps))
    if config['kind'] != 'replay' or config['scenario'] != 'normal':
        return result
    if len(steps) != 44:
        raise ValueError('not all 44 tools completed')
    history = [json.loads(line) for line in (Path(prepared['sample']) / 'steps.jsonl').read_text().splitlines()]
    boot = json.loads((root / 'stage/upper/replay-control/boot.json').read_text())
    observations = {}
    for expected in history[:43]:
        step = expected['step']
        observations[str(step)] = historical_output(
            (steps_root / ('tool-%04d.txt' % step)).read_bytes(),
            expected['expected_observation']['raw_output'].encode(), step,
            boot['preflight']['import_warnings'])
    result['historical_stdout_review'] = observations
    mismatches = []
    for actual, expected in zip(steps, history):
        expected_code = expected['expected_observation'].get('returncode')
        if expected['step'] == 44:
            expected_code = 0
        if actual['step'] != expected['step'] or actual['returncode'] != expected_code or actual['timed_out']:
            mismatches.append(dict(step=actual['step'], expected_code=expected_code, actual_code=actual['returncode']))
    result['historical_exit_differences'] = mismatches
    result['patch_sha256'] = digest(steps_root / 'final.patch')
    result['historical_patch_equal'] = result['patch_sha256'] == prepared['input_hashes']['recorded.patch']
    write(root.parent / 'comparison.json', result)
    if mismatches or not result['historical_patch_equal']:
        raise ValueError('historical replay mismatch; see comparison.json')
    if reference:
        baseline = reference / 'vm/stage/upper/replay-output'
        differences = []
        for actual in steps:
            expected = json.loads((baseline / ('step-%04d.json' % actual['step'])).read_text())
            for key in ('returncode', 'command_sha256', 'source_sha256'):
                if actual[key] != expected[key]:
                    differences.append(dict(step=actual['step'], field=key))
            name = 'tool-%04d.txt' % actual['step']
            if comparable_output((steps_root / name).read_bytes(), actual['step']) != comparable_output((baseline / name).read_bytes(), actual['step']):
                differences.append(dict(step=actual['step'], field='normalized_stdout'))
        for filename in ('final.patch', 'files.json'):
            if (steps_root / filename).read_bytes() != (baseline / filename).read_bytes():
                differences.append(dict(file=filename))
        result['reference_differences'] = differences
        result['raw_output_different_steps'] = [actual['step'] for actual in steps if
            (steps_root / ('tool-%04d.txt' % actual['step'])).read_bytes() !=
            (baseline / ('tool-%04d.txt' % actual['step'])).read_bytes()]
        write(root.parent / 'comparison.json', result)
        if differences:
            raise ValueError('resident reference mismatch')
    with (root.parent / 'steps.jsonl').open('w') as stream:
        for row in steps:
            stream.write(json.dumps(row) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--service', type=Path)
    parser.add_argument('--prepared', type=Path)
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--firmware', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--kind', choices=['preflight', 'handshake', 'replay'], default='replay')
    parser.add_argument('--mode', choices=['resident', 'pause', 'offload'], default='resident')
    parser.add_argument('--scenario', choices=['normal', 'wrong-token', 'no-go', 'cancel'], default='normal')
    parser.add_argument('--barriers', default='1,17,39')
    parser.add_argument('--wait-seconds', type=float, default=5)
    parser.add_argument('--reference', type=Path)
    parser.add_argument('--row-output', action='store_true')
    parser.add_argument('--apparmor-profile', help='Existing administrator-installed profile; never installs or changes policy')
    parser.add_argument('--enomem-workaround', choices=['auto', '0', '1'], default='auto',
                        help='Explicit SDK compatibility override; recorded for every sample')
    args = parser.parse_args()
    if args.service:
        return service(args.service)
    for key in ('prepared', 'binary', 'firmware', 'output'):
        if getattr(args, key) is None:
            parser.error('--' + key + ' required')
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    prepared = json.loads(args.prepared.read_text())
    token = uuid.uuid4().hex
    affinity = sorted(os.sched_getaffinity(0))[:4]
    config = dict(rootfs=prepared['rootfs'], firmware=str(args.firmware.resolve()), output=str(root / 'vm'),
                  binary=str(args.binary.resolve()), environment=prepared['environment'],
                  commands_sha256=prepared['commands_sha256'], run_token=token,
                  mode=args.mode, kind=args.kind, scenario=args.scenario,
                  barriers=list(map(int, args.barriers.split(','))) if args.barriers else [], wait_seconds=args.wait_seconds,
                  benchmark_id='B-TRAJECTORY-ENG', binary_sha256=digest(args.binary),
                  firmware_sha256=digest(args.firmware / 'libkrunfw.so.5'),
                  prepared_sha256=digest(args.prepared), cpu_affinity=affinity,
                  measurement_scope='entire owned service cgroup; external observer; shared lower cache charge may be outside',
                  reference=str(args.reference.resolve()) if args.reference else None,
                  apparmor_profile=args.apparmor_profile, enomem_workaround=args.enomem_workaround)
    write(root / 'config.json', config)
    harness = root / 'harness'
    harness.mkdir()
    for name in ('trajectory_benchmark.py', 'prepare_replay_rootfs.py'):
        shutil.copyfile(Path(__file__).with_name(name), harness / name)
    repo = Path(__file__).resolve().parents[2]
    source = repo / 'crates/pvisor/examples/vm_trajectory_replay.rs'
    shutil.copyfile(source, harness / source.name)
    shutil.copyfile(Path(prepared['rootfs']) / 'replay-input/trajectory_worker.py', harness / 'trajectory_worker.py')
    write(root / 'harness-sha256.json', {p.name: digest(p) for p in harness.iterdir()})
    unit = 'pvisor-traj-' + token[:12]
    command = ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect', '--unit=' + unit,
               '--property=MemoryAccounting=yes', '--property=CPUAccounting=yes', '--property=IOAccounting=yes',
               '--property=MemoryMax=4G', '--property=MemorySwapMax=0', '--property=CPUQuota=400%',
               '--property=CPUAffinity=' + ' '.join(map(str, affinity)), '--property=TasksMax=256',
               '--property=RuntimeMaxSec=3900', '--property=TimeoutStopSec=30', '--property=KillMode=control-group',
               sys.executable, str(harness / 'trajectory_benchmark.py'), '--service', str(root / 'config.json')]
    write(root / 'launch.json', dict(unit=unit, command=command, host_kernel=os.uname().release,
          started_monotonic_ns=time.monotonic_ns()))
    observations, failures = [], []
    with (root / 'launcher.log').open('wb') as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
        group = None
        deadline = time.monotonic() + 3960
        with (root / 'memory.jsonl').open('w') as memory:
            try:
                while process.poll() is None:
                    if time.monotonic() > deadline:
                        raise TimeoutError('overall sample deadline')
                    if group is None and (root / 'service-start.json').exists():
                        group = Path(json.loads((root / 'service-start.json').read_text())['cgroup'])
                    if group is not None:
                        try:
                            row = sample(group)
                        except FileNotFoundError:
                            pass  # systemd may collect an already exited service
                        else:
                            observations.append(row)
                            memory.write(json.dumps(row) + '\n')
                            memory.flush()
                    time.sleep(.1)
            except BaseException as error:
                failures.append(repr(error))
                subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True, timeout=45)
            finally:
                process.wait(timeout=45)
    result = dict(schema='pvisor-trajectory-result/v1', correctness='failed', mode=args.mode, kind=args.kind,
                  scenario=args.scenario, launcher_exit_code=process.returncode, failures=failures)
    try:
        if process.returncode:
            raise RuntimeError('service failed; see sdk.stderr and launcher.log')
        final = json.loads((root / 'service-final.json').read_text())
        if final['remaining_other_pids']:
            raise RuntimeError('service leaked descendant processes')
        observations.insert(0, json.loads((root / 'service-start.json').read_text())['sample'])
        observations.append(final['sample'])
        if any(row['events'].get('oom', 0) or row['events'].get('oom_kill', 0) for row in observations):
            raise RuntimeError('OOM occurred')
        result['verification'] = verify(config, prepared, args.reference.resolve() if args.reference else None)
        result['correctness'] = 'passed'
    except BaseException as error:
        result['failures'].append(repr(error))
        result['traceback'] = traceback.format_exc()
    observations.sort(key=lambda row: row['host_monotonic_ns'])
    with (root / 'memory.csv').open('w') as stream:
        fields = ['host_monotonic_ns', 'memory_current', 'memory_peak', 'swap_current', 'anon', 'file', 'kernel']
        writer = csv.DictWriter(stream, fieldnames=fields, extrasaction='ignore')
        writer.writeheader()
        writer.writerows(observations)
    if observations:
        result['peak_bytes'] = max(row['memory_peak'] for row in observations)
        result['service_gb_seconds'] = sum((b['host_monotonic_ns'] - a['host_monotonic_ns']) / 1e9 *
            (a['memory_current'] + b['memory_current']) / 2 / 1e9 for a, b in zip(observations, observations[1:]))
    result['service_state'] = subprocess.run(['systemctl', '--user', 'show', unit,
        '--property=LoadState', '--property=ActiveState'], capture_output=True, text=True).stdout
    if 'ActiveState=inactive' not in result['service_state'] and 'ActiveState=failed' not in result['service_state']:
        result['correctness'] = 'failed'
        result['failures'].append('service not quiescent')
    write(root / 'result.json', result)
    if args.row_output and result['correctness'] == 'passed':
        print(json.dumps({'N': 1, 'Passed': 1, 'Tools': result['verification']['step_count'],
                          'Peak MiB': result.get('peak_bytes', 0) / 1024**2,
                          'Service GB s': result.get('service_gb_seconds', 0)}))
    else:
        print(json.dumps(result))
    return 0 if result['correctness'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
