#!/usr/bin/env python3
"""Extended kernel cache over already enabled immutable physical metadata cache.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
With --profiles: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: decide whether owned Linux HOST API kernel caching reduces requests
beyond immutable physical caching while writable invalidation remains correct.
Conclusion sought: same-binary paired median changes with bootstrap 95% CI;
metadata writable vs legacy; KEEP_CACHE only within read-only conditions.
Design: native/legacy-writable/metadata-writable/metadata-readonly/data-readonly;
2048 byte-checked files, 32 half-deep branches, 3 warmups/30 shuffled samples.
All backing is private-namespace noatime tmpfs, identical across conditions;
old Btrfs/future-atime cohorts are unaccepted history and never pooled.
TTL workload waits 1.1s: legacy expires, extended 60s remains warm; not a 60s expiry test.
No journal/preimage/metrics/custom policy/exclusions; owner-only default_permissions.
Profile batches are independent, never used for timing conclusions. Failures
are retained, never zero timings; no slow-sample exclusions or cross-batch merge.
"""
import argparse
import csv
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import selectors
import shutil
import signal
import statistics
import sys
import subprocess
import time
import traceback

from publication import distribution

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
CONDITIONS = ('native', 'legacy-writable', 'metadata-writable',
              'metadata-readonly', 'metadata-and-data-readonly')
WORKLOADS = ('hot', 'ttl', 'readsearch', 'tools', 'whole-tools')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def run(command, cwd, log, timeout=300, env=None):
    with log.open('ab') as stream:
        stream.write((json.dumps(command) + '\n').encode()); stream.flush()
        subprocess.run(command, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT,
                       check=True, timeout=timeout, env=env)


def source_files():
    names = subprocess.check_output(['git', 'ls-files', '--cached', '--others',
                                     '--exclude-standard', '-z', 'crates', 'vendor',
                                     '.cargo', 'Cargo.toml', 'Cargo.lock'], cwd=ROOT).decode().split('\0')
    return sorted({p for p in names if p and (ROOT / p).is_file()})


def build(output, target):
    output.mkdir(parents=True, exist_ok=False)
    snapshot = output / 'source'
    snapshot.mkdir()
    inventory = {}
    for name in source_files():
        src = ROOT / name
        dst = snapshot / name
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        inventory[name] = sha(src)
    for name in ('kernel_cache_runner.py', 'kernel_cache_driver.rs',
                 'publication.py', 'test_kernel_cache_runner.py'):
        shutil.copy2(HERE / name, output / name)
    write_json(output / 'source-manifest.json', inventory)
    isolated = output / 'driver'
    isolated.mkdir()
    shutil.copy2(HERE / 'kernel_cache_driver.rs', isolated / 'main.rs')
    manifest = f'''[package]
name = "kernel-cache-driver"
version = "0.1.0"
edition = "2021"
[workspace]
[[bin]]
name = "kernel-cache-driver"
path = "main.rs"
[dependencies]
anyhow = "1"
serde_json = "1"
pvisor-overlayfs = {{ path = {json.dumps(str(snapshot / 'crates/pvisor-overlayfs'))}, default-features = false }}
pvisor-overlay-core = {{ path = {json.dumps(str(snapshot / 'crates/pvisor-overlay-core'))} }}
[patch.crates-io]
fuser = {{ path = {json.dumps(str(snapshot / 'vendor/fuser'))} }}
'''
    (isolated / 'Cargo.toml').write_text(manifest)
    # Preserve product dependency versions; Cargo trims unrelated workspace entries.
    shutil.copy2(snapshot / 'Cargo.lock', isolated / 'Cargo.lock')
    command = ['cargo', 'build', '--offline', '--release', '--manifest-path',
               str(isolated / 'Cargo.toml'), '--target-dir', str(target), '-j', '4']
    run(command, ROOT, output / 'build.log', timeout=600)
    binary = output / 'kernel-cache-driver'
    shutil.copy2(target / 'release/kernel-cache-driver', binary)
    receipt = {'profile_contract': derive_profile_contract(snapshot), 'command': command, 'cwd': str(ROOT), 'binary_sha256': sha(binary),
               'source_inventory_sha256': sha(output / 'source-manifest.json'),
               'source_count': len(inventory), 'manifest_sha256': sha(isolated / 'Cargo.toml'),
               'lock_sha256': sha(isolated / 'Cargo.lock'),
               'rustc': subprocess.check_output(['rustc', '-vV']).decode(),
               'cargo': subprocess.check_output(['cargo', '-V']).decode(),
               'harness': {name: sha(output / name) for name in
                           ('kernel_cache_runner.py', 'kernel_cache_driver.rs',
                            'publication.py', 'test_kernel_cache_runner.py')}}
    write_json(output / 'build-receipt.json', receipt)
    return receipt


def verify_build(directory):
    receipt = json.loads((directory / 'build-receipt.json').read_text())
    assert sha(directory / 'kernel-cache-driver') == receipt['binary_sha256']
    assert sha(directory / 'source-manifest.json') == receipt['source_inventory_sha256']
    for name, digest in json.loads((directory / 'source-manifest.json').read_text()).items():
        assert sha(directory / 'source' / name) == digest, name
    for name, digest in receipt['harness'].items():
        assert sha(directory / name) == digest, name
        assert sha(HERE / name) == digest, 'use the frozen harness or rebuild: ' + name
    assert sha(directory / 'driver/Cargo.toml') == receipt['manifest_sha256']
    assert sha(directory / 'driver/Cargo.lock') == receipt['lock_sha256']
    assert receipt['profile_contract'] == derive_profile_contract(directory / 'source')
    return receipt


def fixture_paths():
    for d in range(32):
        prefix = f'd{d:02}' if d < 16 else f'd{d:02}/a/b/c/e/f/g/h'
        for f in range(64):
            yield Path(f'{prefix}/f{f:02}.txt')


def fixture(root, log):
    root.mkdir()
    for p in fixture_paths():
        (root / p).parent.mkdir(parents=True, exist_ok=True)
        (root / p).write_bytes(f'needle {p}\n{"0123456789abcdef" * 32}\n'.encode())
    run(['git', 'init', '-q', str(root)], ROOT, log)
    run(['git', 'config', 'gc.auto', '0'], root, log)
    run(['git', 'config', 'maintenance.auto', 'false'], root, log)
    run(['git', 'add', '.'], root, log)
    run(['git', '-c', 'user.name=Benchmark', '-c', 'user.email=benchmark@example.invalid',
         '-c', 'commit.gpgsign=false', 'commit', '-qm', 'owned fixture'], root, log)
    # Genuine noatime backing is established and audited before fixture creation.


def inventory(root):
    result = {}
    for path in [root, *sorted(root.rglob('*'))]:
        st = path.lstat()
        result[str(path.relative_to(root))] = {
            'mode': st.st_mode, 'uid': st.st_uid, 'gid': st.st_gid,
            'dev': st.st_dev, 'ino': st.st_ino, 'nlink': st.st_nlink,
            'size': st.st_size, 'mtime_ns': st.st_mtime_ns,
            'ctime_ns': st.st_ctime_ns, 'atime_ns': st.st_atime_ns,
            'sha256': sha(path) if path.is_file() else None,
            'xattrs': {key: os.getxattr(path, key).hex() for key in os.listxattr(path)}}
    return result


def profile_records(path):
    records = [json.loads(line.split('pvisor-fs-profile ', 1)[1])
               for line in path.read_text().splitlines() if 'pvisor-fs-profile ' in line]
    latest = {}
    for record in records:
        key = (record['pid'], record['component'], record['instance'])
        latest[key] = record
    return {'records': records, 'instances': list(latest.values()),
            'missing_final': [list(key) for key, rec in latest.items() if not rec['final_record']]}


def collect_profiles(output, expected):
    profiles = {}
    for stage, condition in expected:
        path = stage / 'stderr.log'
        parsed = profile_records(path)
        components = [record['component'] for record in parsed['instances']]
        wanted = expected_components(condition)
        finals = Counter((r['pid'], r['component'], r['instance'])
                         for r in parsed['records'] if r['final_record'])
        if Counter(components) != Counter(wanted) or parsed['missing_final'] or any(n != 1 for n in finals.values()):
            raise ValueError(f'incomplete profile coverage for {stage}: {components}')
        profiles[str(stage.relative_to(output))] = parsed
    return profiles


class Worker:
    def __init__(self, binary, condition, lower, stage, affinity, profile):
        self.stage = stage
        self.condition = condition
        stage.mkdir()
        self.view_stage = stage.parent / 'backing' / stage.name
        self.view_stage.mkdir()
        self.stderr = (stage / 'stderr.log').open('wb')
        self.log = (stage / 'protocol.jsonl').open('w')
        self.started = time.monotonic()
        env = dict(os.environ)
        for key in list(env):
            if key.startswith('PVISOR_'):
                del env[key]
        env['PVISOR_FS_PROFILE'] = '1' if profile else '0'
        env['PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE'] = '0'
        env['GIT_OPTIONAL_LOCKS'] = '0'
        command = ['taskset', '-c', affinity, str(binary), condition, str(lower), str(self.view_stage)]
        write_json(stage / 'launch.json', {'command': command, 'env': {k: v for k, v in env.items()
                    if k.startswith('PVISOR_') or k == 'GIT_OPTIONAL_LOCKS'}})
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=self.stderr, env=env, start_new_session=True)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)
        self.ready = self.receive()
        assert self.ready['event'] == 'ready'
        paths = [lower, self.view_stage] if condition == 'native' else [lower, self.view_stage / 'upper', self.view_stage / 'work']
        self.backing_proof = [prove_backing(p) for p in paths]
        write_json(stage / 'backing.json', self.backing_proof)
        ACTIVE_WORKERS.append(self)

    def receive(self):
        deadline = time.monotonic() + 90
        while not self.selector.select(0.05):
            supervise_workers(self)
            if time.monotonic() >= deadline:
                raise TimeoutError('worker response timeout: ' + str(self.stage))
        supervise_workers(self)
        line = self.proc.stdout.readline()
        self.log.write(line.decode()); self.log.flush()
        if not line:
            raise RuntimeError('worker exited: ' + str(self.stage / 'stderr.log'))
        return json.loads(line)

    def request(self, command):
        self.proc.stdin.write((command + '\n').encode()); self.proc.stdin.flush()
        return self.receive()

    def close(self):
        self.closing = True
        result = self.request('stop')
        self.proc.wait(timeout=15)
        assert self.proc.returncode == 0
        result['process_lifetime_ms'] = (time.monotonic() - self.started) * 1000
        self.stderr.close(); self.log.close(); self.selector.close()
        if mounted(self.view_stage / 'mnt'):
            raise RuntimeError('normal unmount left a mount: ' + str(self.stage))
        return result

    def abort(self):
        if self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL); self.proc.wait(timeout=10)
        self.stderr.close(); self.log.close(); self.selector.close()
        # Only this instance's exact generated mountpoint. Never sudo/lazy unmount.
        mount = self.view_stage / 'mnt'
        if mounted(mount):
            helper = shutil.which('fusermount3') or shutil.which('fusermount')
            if helper:
                run([helper, '-u', str(mount)], ROOT, self.stage / 'cleanup.log', timeout=15)
            if mounted(mount):
                raise RuntimeError('owned mount remains; preserve stage: ' + str(mount))


def order(seed, rounds, workloads=WORKLOADS):
    rng = random.Random(seed)
    plan = []
    for round_id in range(rounds):
        cells = [(c, w) for c in CONDITIONS for w in workloads]
        rng.shuffle(cells)
        plan.append(cells)
    return plan


def paired_ci(off, on, seed=4207):
    assert len(off) == len(on) and off
    rng = random.Random(seed)
    draws = []
    for _ in range(5000):
        indices = [rng.randrange(len(off)) for _ in off]
        draws.append(100 * (statistics.median([on[i] for i in indices]) /
                           statistics.median([off[i] for i in indices]) - 1))
    draws.sort()
    return {'percent_change': 100 * (statistics.median(on) / statistics.median(off) - 1),
            'ci95_percent': [draws[124], draws[4874]], 'bootstrap': '5000 paired round resamples'}


def experiment(args):
    out = args.output.resolve()
    assert args.namespace_child and os.getpid() == 1, 'must run as supervised private PID namespace init'
    ACTIVE_WORKERS.clear()
    noatime = setup_backing(out)
    report = {'benchmark': 'B-FS-DIAG' if args.profiles else 'B-FS-ENG',
              'role': 'diagnostic' if args.profiles else 'engineering A/B',
              'arguments': {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
              'host': {'uname': platform.uname()._asdict(), 'cpuinfo': Path('/proc/cpuinfo').read_text(),
                       'affinity': sorted(os.sched_getaffinity(0)),
                       'mountinfo': Path('/proc/self/mountinfo').read_text(),
                       'fuse': str(Path('/dev/fuse').stat()),
                       'git': subprocess.check_output(['git', '--version']).decode(),
                       'rg': subprocess.check_output(['rg', '--version']).decode()},
              'rows': [], 'failures': [], 'lifecycles': {}, 'backing': noatime,
              'acceptance': 'noatime-tmpfs-v1',
              'exclusion_rule': 'no slow-sample exclusion; any error or inventory change fails cohort',
              'scope': 'Linux HOST API only; not pvisor run/VM/review guarantees; no journal/preimage/metrics/custom policy/exclusions',
              'profile_expectation': {'components': expected_components('metadata-writable'),
                  'derivation': 'core.rs build_for_layout + fs.rs from_core each create one Arc Profile; cache.rs notifier threads create none; no journal profiles',
                  'sources': ['crates/pvisor-overlay-core/src/core.rs', 'crates/pvisor-overlay-core/src/profile.rs',
                              'crates/pvisor-overlayfs/src/fs.rs', 'crates/pvisor-overlayfs/src/cache.rs']}}
    workers = {}
    report['interference_checks'] = []
    lower = out / 'backing/lower'
    before = None
    try:
        check_interference(report, 'start')
        directory = args.build_receipt.resolve().parent
        report['build'] = verify_build(directory)
        binary = out / 'driver'; shutil.copy2(directory / 'kernel-cache-driver', binary)
        fixture(lower, out / 'fixture.log')
        before = inventory(lower); write_json(out / 'input-before.json', before)
        report['input_sha256'] = sha(out / 'input-before.json')
        native = out / 'backing/native-input'; shutil.copytree(lower, native)
        plan = order(args.seed, args.warmups + args.samples,
                     WORKLOADS + ('prime-only',) if args.profiles else WORKLOADS)
        write_json(out / 'order.json', plan)
        # Constructor failures must still leave a handle for cleanup.
        for condition in CONDITIONS:
            worker = Worker.__new__(Worker)
            workers[condition] = worker
            worker.__init__(binary, condition, native if condition == 'native' else lower,
                            out / condition, args.affinity, args.profiles)
        active_mountinfo = Path('/proc/self/mountinfo').read_text()
        (out / 'mountinfo-active.txt').write_text(active_mountinfo)
        for condition in CONDITIONS[1:]:
            assert any(str(out / 'backing' / condition / 'mnt') in line and ' - fuse' in line
                       for line in active_mountinfo.splitlines()), 'not a real FUSE mount'
        for index, cells in enumerate(plan):
            for condition, workload in cells:
                check_interference(report, f'{index}:{condition}:{workload}:before')
                if workload == 'whole-tools' or args.profiles:
                    stage = out / f'case-{index:03}-{workload}-{condition}'
                    transient = Worker.__new__(Worker)
                    workers[stage.name] = transient
                    start = time.monotonic()
                    transient.__init__(binary, condition, native if condition == 'native' else lower,
                                       stage, args.affinity, args.profiles)
                    if workload in ('hot', 'ttl', 'readsearch', 'prime-only'):
                        transient.request('hot')
                    if workload == 'ttl':
                        time.sleep(1.1)
                    task = (dict(operation_ms=0) if workload == 'prime-only' else
                            transient.request('tools' if workload == 'whole-tools' else workload))
                    stopped = transient.close()
                    result = {'event': 'result', 'workload': workload,
                              'correctness': 'passed', 'operation_ms': (time.monotonic() - start) * 1000,
                              'tools_ms': task['operation_ms'], 'mount_ms': transient.ready['mount_ms'],
                              'unmount_ms': stopped['unmount_ms']}
                else:
                    # Immediate same-view priming makes hot independent of shuffled
                    # coordinator idle time. TTL begins only after that priming ends.
                    if workload in ('hot', 'ttl', 'readsearch'):
                        workers[condition].request('hot')
                    if workload == 'ttl':
                        time.sleep(1.1)  # outside operation timer; every condition same wait
                    start = time.monotonic()
                    result = workers[condition].request(workload)
                assert result['event'] == 'result' and result['correctness'] == 'passed'
                result.update(condition=condition, round=index - args.warmups,
                              warmup=index < args.warmups,
                              coordinator_command_ms=(time.monotonic() - start) * 1000)
                check_interference(report, f'{index}:{condition}:{workload}:after')
                report['rows'].append(result)
                write_json(out / 'report.json', report)
        for condition in CONDITIONS:
            worker = workers[condition]
            report['lifecycles'][condition] = {'ready': worker.ready,
                                             'correctness': worker.request('correct'),
                                             'stop': worker.close()}
        after = inventory(lower); write_json(out / 'input-after.json', after)
        assert before == after, 'immutable physical lower changed'
        verify_build(directory)
        assert sha(binary) == report['build']['binary_sha256']
        if args.profiles:
            report['profiles'] = collect_profiles(
                out, [(worker.stage, worker.condition) for worker in workers.values()])
            export_counters(out, report)
        else:
            report['summary'] = []
            for workload in WORKLOADS:
                values = {}
                for condition in CONDITIONS:
                    values[condition] = [r['operation_ms'] for r in report['rows']
                        if not r['warmup'] and r['condition'] == condition and r['workload'] == workload]
                    assert len(values[condition]) == args.samples
                    report['summary'].append({'condition': condition, 'workload': workload,
                                              **distribution(values[condition])})
                for baseline, candidate in COMPARISONS:
                    report.setdefault('cache_comparison', {}).setdefault(workload, {})[
                        baseline + ':' + candidate] = paired_ci(values[baseline], values[candidate], args.seed)
        report['state'] = 'passed'
    except Exception:
        report['state'] = 'failed'
        report['failures'].append(traceback.format_exc())
    finally:
        for worker in workers.values():
            if hasattr(worker, 'proc') and (worker.proc.poll() is None or mounted(worker.view_stage / 'mnt')):
                try:
                    worker.abort()
                except Exception:
                    report['failures'].append('cleanup: ' + traceback.format_exc())
                    report['state'] = 'failed'
        if before is not None:
            try:
                after = inventory(lower); write_json(out / 'input-after.json', after)
                if before != after:
                    report['failures'].append('lower inventory changed'); report['state'] = 'failed'
            except Exception:
                report['failures'].append(traceback.format_exc()); report['state'] = 'failed'
        report['mount_cleanup'] = {str(worker.view_stage / 'mnt'): not mounted(worker.view_stage / 'mnt')
                                   for worker in workers.values() if hasattr(worker, 'view_stage')}
        report['backing_proofs'] = {worker.stage.name: worker.backing_proof for worker in workers.values()
                                    if hasattr(worker, 'backing_proof')}
        report['backing_after'] = prove_backing(out / 'backing')
        if not all(report['mount_cleanup'].values()):
            report['state'] = 'failed'; report['failures'].append('owned mount remains after cleanup')
        if all(report['mount_cleanup'].values()):
            try:
                run(['tar', '--xattrs', '--acls', '--numeric-owner', '-cpf', str(out / 'backing.tar'),
                     '-C', str(out / 'backing'), '.'], ROOT, out / 'retention.log', timeout=90)
                report['backing_archive_sha256'] = sha(out / 'backing.tar')
                run(['umount', str(out / 'backing')], ROOT, out / 'retention.log', timeout=15)
                report['backing_detached'] = not mounted(out / 'backing')
                assert report['backing_detached']
            except Exception:
                report['state'] = 'failed'; report['failures'].append('backing retention/cleanup: ' + traceback.format_exc())
        write_json(out / 'report.json', report)
    if report['state'] == 'passed' and not args.profiles:
        export_csv(out, report)
    print(json.dumps({'state': report['state'], 'output': str(out),
                      'failures': report['failures'], 'summary': report.get('summary'),
                      'cache_comparison': report.get('cache_comparison')}, indent=2))
    return 0 if report['state'] == 'passed' else 1


ACTIVE_WORKERS = []


def supervise_workers(current=None):
    for worker in ACTIVE_WORKERS:
        if worker is not current and not getattr(worker, 'closing', False) and worker.proc.poll() is not None:
            raise RuntimeError('contained server exited; stop all users: ' + str(worker.stage))


def prove_backing(path):
    path = os.path.abspath(path)
    candidates = []
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        fields = line.split(); mountpoint = fields[4]
        if path == mountpoint or path.startswith(mountpoint.rstrip('/') + '/'):
            candidates.append((len(mountpoint), line, fields))
    _, line, fields = max(candidates)
    separator = fields.index('-')
    assert fields[separator + 1] == 'tmpfs' and 'noatime' in fields[5].split(','), line
    assert not any(f.startswith(('shared:', 'master:')) for f in fields[6:separator]), line
    return dict(path=path, mountinfo=line, filesystem='tmpfs', noatime=True,
                device=fields[2], stat_dev=os.stat(path).st_dev)


def setup_backing(out):
    root = out / 'backing'
    root.mkdir()
    command = ['mount', '-t', 'tmpfs', '-o', 'noatime,nosuid,nodev,mode=0700,size=512m', 'tmpfs', str(root)]
    run(command, ROOT, out / 'namespace.log', timeout=15)
    proof = prove_backing(root)
    # Past atime forces a relatime backing to update, unlike the rejected future-atime fixture.
    directory = root / 'atime-proof'; directory.mkdir()
    file = directory / 'file'; file.write_bytes(b'noatime backing proof')
    for path in (directory, file):
        os.utime(path, ns=(1_000_000_000, 2_000_000_000))
    before = {str(p.name): p.stat().st_atime_ns for p in (directory, file)}
    for _ in range(20):
        assert file.read_bytes() == b'noatime backing proof'
        list(directory.iterdir())
    after = {str(p.name): p.stat().st_atime_ns for p in (directory, file)}
    assert before == after
    proof.update(command=command, probe_atime_before=before, probe_atime_after=after,
                 namespace={k: os.readlink('/proc/self/ns/' + k) for k in ('user', 'mnt', 'pid')},
                 uid_map=Path('/proc/self/uid_map').read_text(), pid=os.getpid())
    write_json(out / 'noatime-proof.json', proof)
    return proof


def namespace_run(args):
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    command = ['unshare', '--user', '--map-root-user', '--mount', '--pid', '--fork',
               '--kill-child=KILL', '--mount-proc', '--propagation', 'private',
               sys.executable, str(Path(__file__).resolve()), *sys.argv[1:], '--namespace-child']
    receipt = dict(command=command, outer_namespace={k: os.readlink('/proc/self/ns/' + k)
                   for k in ('user', 'mnt', 'pid')}, host_checks=[],
                   contract='private user/mount/PID namespaces; coordinator is PID1; all drivers and tool users are descendants; no view exports; unexpected server exit stops cohort; init death kills all namespace users; unshare death kills init; outer supervisor enforces 1800s deadline',
                   timeout_seconds=1800)
    state = 'failed'
    with (out / 'supervisor.log').open('wb') as log:
        proc = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        receipt['outer_pid'] = proc.pid
        started = time.monotonic()
        try:
            while proc.poll() is None:
                check_interference({'interference_checks': receipt['host_checks']}, 'outer-host')
                if time.monotonic() - started > 1800:
                    raise TimeoutError('contained namespace lifetime exceeded')
                time.sleep(0.25)
            assert proc.returncode == 0, 'namespace failed; see supervisor.log'
            report = json.loads((out / 'report.json').read_text())
            assert report['state'] == 'passed' and report['backing_detached']
            assert all(report['mount_cleanup'].values())
            assert all(report['backing']['namespace'][k] != receipt['outer_namespace'][k] for k in ('user', 'mnt', 'pid'))
            state = 'passed'
        except BaseException:
            receipt['failure'] = traceback.format_exc()
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=15)
            receipt.update(state=state, returncode=proc.returncode,
                           duration_seconds=time.monotonic() - started,
                           host_mounts=[line for line in Path('/proc/self/mountinfo').read_text().splitlines()
                                        if str(out / 'backing') in line])
            receipt['surviving_users'] = []
            if (out / 'noatime-proof.json').exists():
                namespace = json.loads((out / 'noatime-proof.json').read_text())['namespace']['pid']
                for pid in Path('/proc').iterdir():
                    if pid.name.isdigit():
                        try:
                            if os.readlink(pid / 'ns/pid') == namespace:
                                receipt['surviving_users'].append(pid.name)
                        except (FileNotFoundError, PermissionError, ProcessLookupError):
                            pass
            if receipt['host_mounts'] or receipt['surviving_users']:
                receipt['state'] = state = 'failed'
            write_json(out / 'containment-receipt.json', receipt)
            if (out / 'report.json').exists() and state != 'passed':
                report = json.loads((out / 'report.json').read_text())
                report['state'] = 'failed'; report['failures'].append(receipt.get('failure', 'containment failure'))
                write_json(out / 'report.json', report)
    print(json.dumps(dict(state=state, output=str(out), failure=receipt.get('failure')), indent=2))
    return 0 if state == 'passed' else 1


def mounted(path):
    # stat-based ismount can return false for a disconnected FUSE connection.
    # Read kernel namespace state instead of asking the dead filesystem.
    encoded = os.path.abspath(path).replace('\\', '\\134').replace(' ', '\\040').replace('\t', '\\011').replace('\n', '\\012')
    return any(line.split()[4] == encoded for line in Path('/proc/self/mountinfo').read_text().splitlines())


def check_interference(report, phase):
    processes = subprocess.check_output(['ps', '-eo', 'pid=,comm=,args='], text=True)
    suspects = []
    for line in processes.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) != 3:
            continue
        pid, comm, command = fields
        if comm in ('cargo', 'rustc', 'cargo-nextest', 'pytest') or (
                comm.startswith('python') and (' -m pytest' in command or ' -m unittest' in command)):
            suspects.append(line)
    report['interference_checks'].append(dict(phase=phase, monotonic=time.monotonic(), suspects=suspects))
    if suspects:
        raise RuntimeError('concurrent build/test detected: ' + repr(suspects))


COMPARISONS = (('legacy-writable', 'metadata-writable'),
               ('metadata-readonly', 'metadata-and-data-readonly'))


def derive_profile_contract(snapshot):
    sources = ('crates/pvisor-overlay-core/src/core.rs',
               'crates/pvisor-overlayfs/src/fs.rs',
               'crates/pvisor-overlayfs/src/mount.rs',
               'crates/pvisor-overlayfs/src/cache.rs')
    evidence = {}
    counts = Counter()
    for name in sources:
        path = snapshot / name
        text = re.split(r'#\[cfg\(test\)\]\s*mod\s+\w+\s*\{', path.read_text(), maxsplit=1)[0]
        constructors = re.findall(r'Profile::from_env\("([^"\n]+)"\)', text)
        counts.update(constructors)
        evidence[name] = dict(sha256=sha(path), constructors=constructors)
    if counts != Counter({'overlay-core': 1, 'host-fuse': 1}):
        raise ValueError('profile construction changed; review lifecycle derivation: ' + repr(counts))
    return dict(expected_components=list(counts.elements()), sources=evidence,
                lifecycle='one mounted core + one adapter; clones share Arc; notifier threads create no profiles; journal disabled')


def expected_components(condition):
    # Frozen source: core.rs build_for_layout creates one profile, fs.rs
    # from_core creates one. Profile clones share Arc<State>. cache.rs creates
    # three threads but no profile; no journal means no preimage-log instance.
    return [] if condition == 'native' else ['overlay-core', 'host-fuse']


def export_counters(out, report):
    # Only actual FUSE callback spans count; helper spans (directory_snapshot,
    # reclaim_inode) and inclusive core spans must not be added as requests.
    callbacks = {'forget', 'lookup', 'getattr', 'setattr', 'readlink', 'mknod',
                 'mkdir', 'unlink', 'rmdir', 'symlink', 'rename', 'link', 'open',
                 'read', 'write', 'flush', 'release', 'fsync', 'opendir',
                 'readdir', 'readdirplus', 'releasedir', 'fsyncdir', 'statfs',
                 'setxattr', 'getxattr', 'listxattr', 'removexattr', 'access',
                 'create', 'lseek'}
    rows = []
    for stage, profile in report['profiles'].items():
        for instance in profile['instances']:
            for metric, value in instance['measurements'].items():
                rows.append(dict(batch=out.name, binary_sha256=report['build']['binary_sha256'],
                    stage=stage, pid=instance['pid'], component=instance['component'],
                    instance=instance['instance'], final_record=instance['final_record'],
                    metric=metric, calls=value['calls'], units=value['units'],
                    fuse_callback=instance['component'] == 'host-fuse' and metric in callbacks))
    with (out / 'counters.csv').open('w') as stream:
        writer = csv.DictWriter(stream, fieldnames=['batch', 'binary_sha256', 'stage', 'pid',
            'component', 'instance', 'final_record', 'metric', 'calls', 'units', 'fuse_callback'])
        writer.writeheader(); writer.writerows(rows)


def export_csv(out, report):
    fields = ['batch', 'binary_sha256', 'input_sha256', 'workload', 'condition',
              'baseline', 'samples', 'distribution_json', 'percent_change', 'ci95_percent_json']
    with (out / 'summary.csv').open('w') as stream:
        writer = csv.DictWriter(stream, fieldnames=fields); writer.writeheader()
        for row in report['summary']:
            baseline = next((b for b, c in COMPARISONS if c == row['condition']), '')
            comparison = report['cache_comparison'][row['workload']].get(baseline + ':' + row['condition'], {})
            writer.writerow(dict(batch=out.name, binary_sha256=report['build']['binary_sha256'],
                input_sha256=report['input_sha256'], workload=row['workload'], condition=row['condition'],
                baseline=baseline, samples=report['arguments']['samples'],
                distribution_json=json.dumps({k: v for k, v in row.items() if k not in ('condition', 'workload')}),
                percent_change=comparison.get('percent_change', ''),
                ci95_percent_json=json.dumps(comparison.get('ci95_percent', []))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--build', action='store_true')
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'benchmark/.data/kernel-cache-target')
    parser.add_argument('--build-receipt', type=Path)
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--seed', type=int, default=4207)
    parser.add_argument('--affinity', default=','.join(map(str, sorted(os.sched_getaffinity(0))[:2])))
    parser.add_argument('--profiles', action='store_true')
    parser.add_argument('--namespace-child', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if '.data' not in args.output.resolve().parts:
        parser.error('output must be a NEW .data directory')
    if args.samples < 1 or args.warmups < 0:
        parser.error('invalid rounds')
    if args.build:
        build(args.output.resolve(), args.target_dir.resolve())
        print(args.output.resolve() / 'build-receipt.json')
        return 0
    if not args.build_receipt:
        parser.error('--build-receipt required when not building')
    return experiment(args) if args.namespace_child else namespace_run(args)


if __name__ == '__main__':
    raise SystemExit(main())
