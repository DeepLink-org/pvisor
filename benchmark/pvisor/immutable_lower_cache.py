#!/usr/bin/env python3
"""Owned immutable physical metadata cache experiment.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
With --profiles: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: decide whether an explicit lifetime-owned immutable lower materially
reduces repeated host FUSE metadata/open/read without hiding upper mutations.
Conclusion sought: same-binary cache-off/on paired median difference with 95% CI,
separate hot/TTL-expiry, mount/lifecycle costs and validated upper/lower state.
Design: native/mutable/immutable-cache-off/on; 2048 full-byte-checked files in
32 branches (half nested seven levels); same owned lower; 3 warmups/30 seeded
shuffled rounds per workload, persistent real mounts, no journal/review cost.
Profile batches are separate; no TTL, KEEP_CACHE or permission changes. Failures
are retained, never zero timings; no slow-sample exclusions or cross-batch merge.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import selectors
import shutil
import signal
import statistics
import subprocess
import time
import traceback

from publication import distribution

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
CONDITIONS = ('native', 'mutable', 'immutable-cache-off', 'immutable-cache-on')
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
    for name in ('immutable_lower_cache.py', 'immutable_lower_cache_driver.rs',
                 'publication.py', 'test_immutable_lower_cache.py'):
        shutil.copy2(HERE / name, output / name)
    write_json(output / 'source-manifest.json', inventory)
    isolated = output / 'driver'
    isolated.mkdir()
    shutil.copy2(HERE / 'immutable_lower_cache_driver.rs', isolated / 'main.rs')
    manifest = f'''[package]
name = "immutable-lower-cache-driver"
version = "0.1.0"
edition = "2021"
[workspace]
[[bin]]
name = "immutable-lower-cache-driver"
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
    binary = output / 'immutable-lower-cache-driver'
    shutil.copy2(target / 'release/immutable-lower-cache-driver', binary)
    receipt = {'command': command, 'cwd': str(ROOT), 'binary_sha256': sha(binary),
               'source_inventory_sha256': sha(output / 'source-manifest.json'),
               'source_count': len(inventory), 'manifest_sha256': sha(isolated / 'Cargo.toml'),
               'lock_sha256': sha(isolated / 'Cargo.lock'),
               'rustc': subprocess.check_output(['rustc', '-vV']).decode(),
               'cargo': subprocess.check_output(['cargo', '-V']).decode(),
               'harness': {name: sha(output / name) for name in
                           ('immutable_lower_cache.py', 'immutable_lower_cache_driver.rs',
                            'publication.py', 'test_immutable_lower_cache.py')}}
    write_json(output / 'build-receipt.json', receipt)
    return receipt


def verify_build(directory):
    receipt = json.loads((directory / 'build-receipt.json').read_text())
    assert sha(directory / 'immutable-lower-cache-driver') == receipt['binary_sha256']
    assert sha(directory / 'source-manifest.json') == receipt['source_inventory_sha256']
    for name, digest in json.loads((directory / 'source-manifest.json').read_text()).items():
        assert sha(directory / 'source' / name) == digest, name
    for name, digest in receipt['harness'].items():
        assert sha(directory / name) == digest, name
        assert sha(HERE / name) == digest, 'use the frozen harness or rebuild: ' + name
    assert sha(directory / 'driver/Cargo.toml') == receipt['manifest_sha256']
    assert sha(directory / 'driver/Cargo.lock') == receipt['lock_sha256']
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
    # Prevent normal relatime read access from modifying physical atime during
    # the owned-lower lifetime. No mount option or global policy is altered.
    future = time.time_ns() + 86_400_000_000_000
    for path in [*root.rglob('*'), root]:
        st = path.stat()
        os.utime(path, ns=(future, st.st_mtime_ns))


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
        wanted = [] if condition == 'native' else ['host-fuse', 'overlay-core']
        if sorted(components) != wanted or parsed['missing_final']:
            raise ValueError(f'incomplete profile coverage for {stage}: {components}')
        profiles[str(stage.relative_to(output))] = parsed
    return profiles


class Worker:
    def __init__(self, binary, condition, lower, stage, affinity, profile):
        self.stage = stage
        self.condition = condition
        stage.mkdir()
        self.stderr = (stage / 'stderr.log').open('wb')
        self.log = (stage / 'protocol.jsonl').open('w')
        self.started = time.monotonic()
        env = dict(os.environ)
        for key in list(env):
            if key.startswith('PVISOR_'):
                del env[key]
        env['PVISOR_FS_PROFILE'] = '1' if profile else '0'
        env['PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE'] = '1' if condition == 'immutable-cache-off' else '0'
        env['GIT_OPTIONAL_LOCKS'] = '0'
        command = ['taskset', '-c', affinity, str(binary), condition, str(lower), str(stage)]
        write_json(stage / 'launch.json', {'command': command, 'env': {k: v for k, v in env.items()
                    if k.startswith('PVISOR_') or k == 'GIT_OPTIONAL_LOCKS'}})
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=self.stderr, env=env, start_new_session=True)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)
        self.ready = self.receive()
        assert self.ready['event'] == 'ready'

    def receive(self):
        if not self.selector.select(90):
            raise TimeoutError('worker response timeout: ' + str(self.stage))
        line = self.proc.stdout.readline()
        self.log.write(line.decode()); self.log.flush()
        if not line:
            raise RuntimeError('worker exited: ' + str(self.stage / 'stderr.log'))
        return json.loads(line)

    def request(self, command):
        self.proc.stdin.write((command + '\n').encode()); self.proc.stdin.flush()
        return self.receive()

    def close(self):
        result = self.request('stop')
        self.proc.wait(timeout=15)
        assert self.proc.returncode == 0
        result['process_lifetime_ms'] = (time.monotonic() - self.started) * 1000
        self.stderr.close(); self.log.close(); self.selector.close()
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
        mount = self.stage / 'mnt'
        if os.path.ismount(mount):
            helper = shutil.which('fusermount3') or shutil.which('fusermount')
            if helper:
                run([helper, '-u', str(mount)], ROOT, self.stage / 'cleanup.log', timeout=15)
            if os.path.ismount(mount):
                raise RuntimeError('owned mount remains; preserve stage: ' + str(mount))


def order(seed, rounds):
    rng = random.Random(seed)
    plan = []
    for round_id in range(rounds):
        cells = [(c, w) for c in CONDITIONS for w in WORKLOADS]
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
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    report = {'benchmark': 'B-FS-DIAG' if args.profiles else 'B-FS-ENG',
              'role': 'diagnostic' if args.profiles else 'engineering A/B',
              'arguments': {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
              'host': {'uname': platform.uname()._asdict(), 'cpuinfo': Path('/proc/cpuinfo').read_text(),
                       'affinity': sorted(os.sched_getaffinity(0)),
                       'mountinfo': Path('/proc/self/mountinfo').read_text(),
                       'fuse': str(Path('/dev/fuse').stat()),
                       'git': subprocess.check_output(['git', '--version']).decode(),
                       'rg': subprocess.check_output(['rg', '--version']).decode()},
              'rows': [], 'failures': [], 'lifecycles': {},
              'exclusion_rule': 'no slow-sample exclusion; any error or inventory change fails cohort',
              'scope': 'host FUSE only; no journal/preimage/review; no VM/OCI speedup claim'}
    workers = {}
    lower = out / 'lower'
    before = None
    try:
        directory = args.build_receipt.resolve().parent
        report['build'] = verify_build(directory)
        binary = out / 'driver'; shutil.copy2(directory / 'immutable-lower-cache-driver', binary)
        fixture(lower, out / 'fixture.log')
        before = inventory(lower); write_json(out / 'input-before.json', before)
        report['input_sha256'] = sha(out / 'input-before.json')
        native = out / 'native-input'; shutil.copytree(lower, native)
        plan = order(args.seed, args.warmups + args.samples)
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
            assert any(str(out / condition / 'mnt') in line and ' - fuse' in line
                       for line in active_mountinfo.splitlines()), 'not a real FUSE mount'
        for index, cells in enumerate(plan):
            for condition, workload in cells:
                if workload == 'whole-tools':
                    stage = out / f'whole-{index:03}-{condition}'
                    transient = Worker.__new__(Worker)
                    workers[f'whole-{index:03}-{condition}'] = transient
                    start = time.monotonic()
                    transient.__init__(binary, condition, native if condition == 'native' else lower,
                                       stage, args.affinity, args.profiles)
                    task = transient.request('tools')
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
                report.setdefault('cache_comparison', {})[workload] = paired_ci(
                    values['immutable-cache-off'], values['immutable-cache-on'], args.seed)
        report['state'] = 'passed'
    except Exception:
        report['state'] = 'failed'
        report['failures'].append(traceback.format_exc())
    finally:
        for worker in workers.values():
            if hasattr(worker, 'proc') and worker.proc.poll() is None:
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
        write_json(out / 'report.json', report)
    print(json.dumps({'state': report['state'], 'output': str(out),
                      'failures': report['failures'], 'summary': report.get('summary'),
                      'cache_comparison': report.get('cache_comparison')}, indent=2))
    return 0 if report['state'] == 'passed' else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--build', action='store_true')
    parser.add_argument('--target-dir', type=Path, default=ROOT / 'target')
    parser.add_argument('--build-receipt', type=Path)
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--seed', type=int, default=4207)
    parser.add_argument('--affinity', default=','.join(map(str, sorted(os.sched_getaffinity(0))[:2])))
    parser.add_argument('--profiles', action='store_true')
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
    return experiment(args)


if __name__ == '__main__':
    raise SystemExit(main())
