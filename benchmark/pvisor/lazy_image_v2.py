#!/usr/bin/env python3
"""Benchmark: B-LAZY-ENG (benchmark/README.md#b-lazy-eng), role engineering A/B.
Motivation: evaluate bounded metadata prefetch, persistent cache connections,
client index pages, and legacy versus persistent private bridge RPC.
Conclusion sought: same-binary NumPy cold/warm ready, completion, request,
connection, file content and metadata differences with paired bootstrap uncertainty.
Design: shuffled paired V1-compatible/V2, V2 RPC/client pages, or bridge RPC/pages in independent
cohorts, initial excluded round + 3 warmups,
30 samples, CPU 0/1, 2 vCPU, 2048 MiB, loopback cache on CPU 2/3. Private
user/mount/PID namespaces and tmpfs /tmp contain the Host listener. Any failure
invalidates the batch; no speed exclusions, Docker, TLS registry, or WAN claims.
"""
import argparse
import ctypes
import fcntl
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import signal
import socket
import socketserver
import stat
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time

import lazy_startup
from publication import distribution, percentile
from reference_baselines import validate_bundle_execution

ROOT = lazy_startup.ROOT
SEED = 20261008
VARIANTS = ('v1', 'v2')
COMPARISONS = {
    'v1-v2': {
        'v1': dict(PVISOR_LAZY_IMAGE_V2='0', PVISOR_LAZY_INDEX_PAGES='0'),
        'v2': dict(PVISOR_LAZY_IMAGE_V2='1', PVISOR_LAZY_INDEX_PAGES='0'),
    },
    'v2-index': {
        'rpc': dict(PVISOR_LAZY_IMAGE_V2='1', PVISOR_LAZY_INDEX_PAGES='0'),
        'pages': dict(PVISOR_LAZY_IMAGE_V2='1', PVISOR_LAZY_INDEX_PAGES='1'),
    },
}
# Pin historical controls to legacy bridge semantics, never an ambient default.
for _variants in COMPARISONS.values():
    for _env in _variants.values():
        _env['PVISOR_LAZY_BRIDGE_V2'] = '0'
for _comparison, _pages in (('bridge-rpc', '0'), ('bridge-pages', '1')):
    COMPARISONS[_comparison] = {
        label: dict(PVISOR_LAZY_IMAGE_V2='1', PVISOR_LAZY_INDEX_PAGES=_pages,
                    PVISOR_LAZY_BRIDGE_V2=mode)
        for label, mode in (('bridge_v1', '0'), ('bridge_v2', '1'))
    }
BRIDGE_WORDS = ('schema', 'accepted_connections', 'v1_frames', 'v2_frames',
                'forwarded_stat', 'forwarded_list', 'forwarded_read', 'forwarded_metadata',
                'queue_wait_ns', 'upstream_execution_ns', 'rejected_connections', 'queue_full',
                'active_connections', 'active_workers', 'queued_requests', 'reserved')
BRIDGE_METRICS = ('bridge_connections', 'bridge_requests') + tuple(
    'bridge_' + name for name in BRIDGE_WORDS[2:12])
METRICS = ('ready_ms', 'completion_ms', 'requests', 'connections', 'content_bytes',
           'metadata_bytes', 'response_bytes')
MAX_FRAME = 1024 * 1024


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def exact(stream, length, clean_eof=False):
    data = bytearray()
    while len(data) < length:
        block = stream.recv(length - len(data))
        if not block:
            if clean_eof and not data:
                return None
            raise EOFError('partial cache frame/body')
        data.extend(block)
    return bytes(data)


def frame(stream, clean_eof=False):
    prefix = exact(stream, 4, clean_eof)
    if prefix is None:
        return None
    length = struct.unpack('!I', prefix)[0]
    if not 0 < length <= MAX_FRAME:
        raise ValueError('invalid cache JSON frame length')
    body = exact(stream, length)
    return prefix + body, json.loads(body)


class Counts:
    """Drain in-flight exchanges, not idle persistent sockets."""
    def __init__(self):
        self.condition = threading.Condition()
        self.active = 0
        self.connections = 0
        self.rows = []
        self.errors = []

    def snapshot(self, timeout=5):
        deadline = time.monotonic() + timeout
        with self.condition:
            while self.active:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('proxy requests did not drain within 5 seconds')
                self.condition.wait(remaining)
            return dict(rows=list(self.rows), errors=list(self.errors), connections=self.connections)


class CacheProxy(socketserver.BaseRequestHandler):
    """Preserve exact response bytes; delimit raw Data bodies before next frame."""
    def handle(self):
        counts = self.server.counts
        with counts.condition:
            counts.connections += 1
            connection_id = counts.connections
        upstream = None
        active = False
        version = None
        try:
            self.request.settimeout(5)
            self.request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            while True:
                # Read one byte before marking active: idle v2 sockets must not
                # delay a snapshot. A partially received request *is* active.
                try:
                    first = self.request.recv(1)
                except socket.timeout:
                    return
                if not first:
                    return
                started = time.perf_counter()
                with counts.condition:
                    counts.active += 1
                active = True
                prefix = first + exact(self.request, 3)
                size = struct.unpack('!I', prefix)[0]
                if not 0 < size <= MAX_FRAME:
                    raise ValueError('invalid request frame length')
                raw_request = exact(self.request, size)
                envelope = json.loads(raw_request)
                current = envelope['version']
                if current not in (1, 2) or (version is not None and version != current):
                    raise ValueError('invalid or changing protocol version')
                version = current
                request = envelope['request']
                if upstream is None:
                    upstream = socket.create_connection(('127.0.0.1', self.server.upstream), timeout=90)
                    upstream.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                upstream.sendall(prefix + raw_request)
                wire, response = frame(upstream)
                length = response.get('length', 0) if response['status'] == 'data' else 0
                if type(length) is not int or not 0 <= length <= MAX_FRAME:
                    raise ValueError('invalid Data.length')
                body = exact(upstream, length)
                body_hash = 'sha256:' + hashlib.sha256(body).hexdigest()
                if response['status'] == 'data' and response.get('sha256') != body_hash:
                    raise ValueError('server body hash mismatch')
                # Never synthesize/re-serialize a response or forward an incomplete body.
                self.request.sendall(wire + body)
                with counts.condition:
                    counts.rows.append(dict(connection_id=connection_id, version=version,
                                            op=request['op'], path=request.get('path'),
                                            request=request, response=response,
                                            content_bytes=length if request['op'] == 'read' else 0,
                                            metadata_bytes=length if request['op'] == 'metadata' else 0,
                                            response_bytes=len(wire) + length,
                                            server_body_sha256=response.get('sha256'),
                                            forwarded_body_sha256=body_hash,
                                            elapsed_ms=(time.perf_counter() - started) * 1000))
                    counts.active -= 1
                    active = False
                    counts.condition.notify_all()
                if version == 1:
                    return
        except Exception as error:
            with counts.condition:
                counts.errors.append(dict(connection_id=connection_id, error=repr(error)))
        finally:
            if upstream is not None:
                upstream.close()
            if active:
                with counts.condition:
                    counts.active -= 1
                    counts.condition.notify_all()


class TCPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def create_bridge_metrics_dir(folder):
    directory = folder / 'bridge-metrics'
    directory.mkdir(mode=0o700)
    directory.chmod(0o700)
    return directory


def parse_bridge_stats(data):
    """Stable post-teardown ABI; SIGKILL may leave nonzero gauges."""
    if len(data) != 128:
        raise ValueError('bridge stats must contain 16 native-endian u64 words')
    result = dict(zip(BRIDGE_WORDS, struct.unpack('=16Q', data)))
    if result['schema'] != 1 or result['reserved'] != 0:
        raise ValueError('unsupported bridge stats schema/reserved word')
    return result


def reconcile_bridge(stats, requests):
    # Host preparation and initial root loads share connections with Prepare;
    # none of that connection's requests passed through the private bridge.
    host_connections = {r['connection_id'] for r in requests if r['op'] == 'prepare'}
    forwarded = Counter(r['op'] for r in requests
                        if r['connection_id'] not in host_connections
                        and r['op'] in ('stat', 'list', 'read', 'metadata'))
    for op in ('stat', 'list', 'read', 'metadata'):
        if stats['forwarded_' + op] != forwarded[op]:
            raise ValueError('bridge/proxy forwarded operation mismatch: ' + op)
    if sum(forwarded.values()) > stats['v1_frames'] + stats['v2_frames']:
        raise ValueError('forwarded operations exceed accepted bridge frames')
    return dict(host_prelaunch_connections=sorted(host_connections),
                forwarded_operations=dict(forwarded))


def collect_bridge_metrics(output, rows, teardown_verified, required=False):
    """Only called by the outer parent after the namespace has no remaining users."""
    if not teardown_verified:
        raise ValueError('bridge stats require verified namespace teardown')
    totals = Counter()
    for row in rows:
        folder = output / f'trial-{row["trial"]}-{row["variant"]}' / row['cache']
        directory = folder / 'bridge-metrics'
        metadata = directory.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise ValueError('unsafe bridge metrics directory')
        files = sorted(directory.iterdir())
        if not files and required:
            raise ValueError('missing bridge stats (including zero-request warm launches)')
        snapshots = []
        for path in files:
            metadata = path.lstat()
            if (path.suffix != '.stats' or not stat.S_ISREG(metadata.st_mode)
                    or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o600):
                raise ValueError('unsafe bridge stats file')
            snapshots.append(dict(file=str(path.relative_to(output)), **parse_bridge_stats(path.read_bytes())))
        if not snapshots:
            continue
        stats = {name: sum(s[name] for s in snapshots) for name in BRIDGE_WORDS[1:]}
        requests = json.loads((folder / 'requests.json').read_text())['rows']
        # The bridge cohorts pin IMAGE_V2=1, so host root loads use the
        # Prepare connection. Historical V1 has unpooled host connections and
        # cannot use that attribution rule; retain its telemetry without it.
        reconciliation = (reconcile_bridge(stats, requests) if required else
                          dict(status='not checked outside bridge comparisons'))
        if row['variant'] == 'bridge_v1' and stats['v2_frames']:
            raise ValueError('legacy bridge accepted V2 frames')
        # A denied initial V2 probe is an accepted connection, not a V2 frame.
        row.update(bridge_connections=stats['accepted_connections'],
                   bridge_requests=stats['v1_frames'] + stats['v2_frames'],
                   bridge_stats=snapshots, bridge_reconciliation=reconciliation,
                   bridge_telemetry_validated=True)
        row.update({'bridge_' + name: stats[name] for name in BRIDGE_WORDS[2:12]})
        totals.update({metric: row[metric] for metric in BRIDGE_METRICS})
    return dict(totals)


def summarize(rows, samples, warmups, variants=VARIANTS):
    if len(variants) != 2 or len(set(variants)) != 2:
        raise ValueError('two distinct comparison variants required')
    baseline, candidate = variants
    bridge = tuple(variants) == ('bridge_v1', 'bridge_v2')
    metrics = METRICS + BRIDGE_METRICS if bridge else METRICS
    delta_label = f'delta_{candidate}_minus_{baseline}'
    expected = {(trial, variant, cache) for trial in range(-warmups - 1, samples)
                for variant in variants for cache in ('cold', 'warm')}
    indexed = {}
    for row in rows:
        key = (row['trial'], row['variant'], row['cache'])
        if key in indexed or key not in expected:
            raise ValueError('duplicate or unexpected pair member')
        if row.get('correctness') != 'passed':
            raise ValueError('wrong correctness')
        if row.get('proxy_errors') or not row.get('bundle_validated'):
            raise ValueError('unverified request or Run Bundle')
        if bridge and not row.get('bridge_telemetry_validated'):
            raise ValueError('unverified bridge telemetry')
        for metric in metrics:
            # Older V1/V2 reports predate separate metadata accounting.
            value = row.get(metric, 0) if metric == 'metadata_bytes' else row.get(metric)
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
                raise ValueError('invalid metric')
            if bridge and metric in BRIDGE_METRICS and type(value) is not int:
                raise ValueError('bridge counters must be integers')
        if bridge:
            if row['bridge_requests'] != row['bridge_v1_frames'] + row['bridge_v2_frames']:
                raise ValueError('bridge frame total mismatch')
            if sum(row['bridge_forwarded_' + op] for op in ('stat', 'list', 'read', 'metadata')) > row['bridge_requests']:
                raise ValueError('bridge forwarding exceeds frames')
            if row['variant'] == 'bridge_v1' and row['bridge_v2_frames']:
                raise ValueError('legacy bridge accepted V2 frames')
        if row['completion_ms'] < row['ready_ms']:
            raise ValueError('completion precedes ready')
        if (row['cache'] == 'cold' and row['content_bytes'] <= 0) or (row['cache'] == 'warm' and row['content_bytes'] != 0):
            raise ValueError('wrong cold/warm content')
        indexed[key] = row
    if set(indexed) != expected:
        raise ValueError('incomplete paired cohort (including excluded rounds)')
    result = {}
    for cache in ('cold', 'warm'):
        result[cache] = {}
        for metric in metrics:
            # Sort by trial before shared-index paired resampling.
            a = [indexed[t, baseline, cache].get(metric, 0) for t in range(samples)]
            b = [indexed[t, candidate, cache].get(metric, 0) for t in range(samples)]
            rng = random.Random(SEED)
            bootstrap = []
            for _ in range(5000):
                indices = [rng.randrange(samples) for _ in range(samples)]
                bootstrap.append(statistics.median([b[i] for i in indices]) -
                                 statistics.median([a[i] for i in indices]))
            lo, hi = percentile(bootstrap, 2.5), percentile(bootstrap, 97.5)
            distributions = {variant: distribution(values) for variant, values in zip(variants, (a, b))}
            for value in distributions.values():
                if value['distribution'] == 'separated-clusters':
                    value.update(low_fraction=value['low_n'] / samples, high_fraction=value['high_n'] / samples)
            result[cache][metric] = dict(**distributions, **{delta_label: statistics.median(b) - statistics.median(a)},
                                        ci95=[lo, hi], bootstrap_resamples=5000,
                                        conclusion='no detected difference' if lo <= 0 <= hi else 'detected difference',
                                        estimand='difference of marginal medians; paired trial resampling, not cluster ranking')
    return result


def namespaces():
    return {name: os.readlink('/proc/self/ns/' + name) for name in ('user', 'mnt', 'pid')}


def namespace_command(args):
    return ['unshare', '--user', '--map-root-user', '--mount', '--pid', '--fork',
            '--kill-child=KILL', '--mount-proc', '--propagation', 'private',
            sys.executable, str(args.output / 'frozen/source/benchmark/pvisor/lazy_image_v2.py'), '--namespace-child',
            '--binary-dir', str(args.binary_dir), '--prepared-store', str(args.prepared_store),
            '--output', str(args.output), '--samples', str(args.samples), '--warmups', str(args.warmups),
            '--comparison', args.comparison]


def process_starttime(path):
    """Kernel /proc stat field 22; comm may itself contain spaces and ')'."""
    record = (path / 'stat').read_text()
    prefix, separator, fields = record.rpartition(')')
    if not separator or prefix.split(' ', 1)[0] != path.name:
        raise RuntimeError(f'invalid process identity for {path.name}')
    try:
        starttime = int(fields.split()[19])
    except (IndexError, ValueError) as error:
        raise RuntimeError(f'invalid process starttime for {path.name}') from error
    if starttime < 0:
        raise RuntimeError(f'invalid process starttime for {path.name}')
    return starttime


def capture_inaccessible_processes():
    """Capture only stable, already-live same-UID identities before unshare."""
    captured = []
    for path in Path('/proc').iterdir():
        if not path.name.isdigit():
            continue
        try:
            if path.stat().st_uid != os.getuid():
                continue
            starttime = process_starttime(path)
            try:
                os.readlink(path / 'ns/mnt')
            except PermissionError:
                if process_starttime(path) == starttime:
                    captured.append(dict(pid=int(path.name), starttime=starttime,
                        reason='same-UID namespace inaccessible before namespace creation; '
                               'stable kernel starttime proves preexisting unrelated process'))
        except FileNotFoundError:
            continue
        except PermissionError as error:
            raise RuntimeError(f'cannot capture preexisting process identity {path.name}') from error
    return sorted(captured, key=lambda row: row['pid'])


def namespace_users(identity, preexisting_inaccessible=None, exclusions=None):
    users = []
    baseline = {row['pid']: row for row in (preexisting_inaccessible or [])}
    for path in Path('/proc').iterdir():
        if not path.name.isdigit():
            continue
        try:
            # --map-root-user maps exactly the launching host UID. Namespace
            # descendants cannot switch to another host UID; unrelated root or
            # other-user processes need not grant ptrace/readlink permission.
            if path.stat().st_uid != os.getuid():
                continue
            starttime = process_starttime(path)
            try:
                namespace = os.readlink(path / 'ns/mnt')
            except PermissionError as error:
                previous = baseline.get(int(path.name))
                if (previous is None or previous['starttime'] != starttime
                        or process_starttime(path) != starttime):
                    raise RuntimeError(f'cannot audit namespace user {path.name}: '
                                       'new, reused, or changed inaccessible identity') from error
                if exclusions is not None and previous not in exclusions:
                    exclusions.append(dict(previous))
                continue
            if process_starttime(path) != starttime:
                raise RuntimeError(f'process identity changed during namespace audit: {path.name}')
            if namespace == identity:
                users.append(int(path.name))
        except FileNotFoundError:
            continue
        except PermissionError as error:
            # Unknown/new inaccessible processes still invalidate teardown.
            raise RuntimeError(f'cannot audit namespace user {path.name}') from error
    return sorted(users)


def kill_group(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=10)


def supervise(command, output, timeout=900):
    """unshare's kill-child kills PID 1 even if timed() made new sessions."""
    preexisting = capture_inaccessible_processes()
    save(output / 'containment-preexisting.json', dict(audited_host_uid=os.getuid(),
         capture='before namespace subprocess creation', inaccessible_processes=preexisting))
    with (output / 'namespace.stdout').open('wb') as stdout, (output / 'namespace.stderr').open('wb') as stderr:
        process = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
        timed_out = False
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        finally:
            # Also kill the original group after normal child exit; never target
            # any process by executable name or any host listener.
            kill_group(process)
        receipt_path = output / 'containment-child.json'
        receipt = json.loads(receipt_path.read_text()) if receipt_path.exists() else {}
        identity = receipt.get('namespaces', {}).get('mnt')
        leftovers = None
        audit_error = None
        exclusions = []
        if identity:
            deadline = time.monotonic() + 5
            try:
                while True:
                    leftovers = namespace_users(identity, preexisting, exclusions)
                    if not leftovers or time.monotonic() >= deadline:
                        break
                    time.sleep(.05)
            except Exception as error:
                audit_error = repr(error)
        return dict(command=command, supervisor_pid=process.pid, exit_code=process.returncode,
                    timed_out=timed_out, timeout_seconds=timeout, kill_group_attempted=True,
                    namespace_identity=identity, audited_host_uid=os.getuid(),
                    leftover_namespace_users=leftovers, audit_error=audit_error,
                    preexisting_inaccessible_processes=preexisting, audit_exclusions=exclusions,
                    teardown_verified=identity is not None and leftovers == [] and audit_error is None)


def assert_static(path):
    data = path.read_bytes()
    if data[:6] != b'\x7fELF\x02\x01':
        raise ValueError(f'{path}: expected little-endian ELF64 release binary')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    if size < 56 or phoff + size * count > len(data):
        raise ValueError('invalid ELF program headers')
    if any(struct.unpack_from('<I', data, phoff + i * size)[0] == 3 for i in range(count)):
        raise ValueError(f'{path}: dynamic interpreter present; static release binary required')


def freeze(output, binary_dir):
    """Freeze working-tree bytes, including dirty/untracked source, not just HEAD."""
    frozen = output / 'frozen'
    frozen.mkdir()
    binaries = frozen / 'bin'
    binaries.mkdir()
    for name in ('pvisor', 'pvisor-cache'):
        source = binary_dir / name
        assert_static(source)
        shutil.copy2(source, binaries / name)
    source_root = frozen / 'source'
    source_root.mkdir()
    scopes = ['crates', 'vendor', 'fw', 'scripts', '.cargo', 'Cargo.toml', 'Cargo.lock', 'build.rs',
              'benchmark/pvisor']
    files = set()
    for options in ([], ['--others', '--exclude-standard']):
        files.update(subprocess.check_output(['git', '--no-pager', 'ls-files', '-z', *options, '--', *scopes], cwd=ROOT).decode().split('\0'))
    manifest = {}
    for name in sorted(files - {''}):
        source = ROOT / name
        if source.is_file():
            destination = source_root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
            manifest[name] = lazy_startup.sha(destination)
    for name, command in (
        ('source-dirty.patch', ['git', '--no-pager', 'diff', '--binary', 'HEAD']),
        ('source-status.txt', ['git', '--no-pager', '--no-optional-locks', 'status', '--short']),
        ('source-head.txt', ['git', '--no-pager', 'rev-parse', 'HEAD']),
    ):
        (frozen / name).write_bytes(subprocess.check_output(command, cwd=ROOT))
    save(frozen / 'source-manifest.json', manifest)
    hashes = {str(path.relative_to(output)): lazy_startup.sha(path) for path in frozen.rglob('*') if path.is_file()}
    save(output / 'frozen-manifest.json', hashes)
    return dict(hashes=hashes, kernel=os.uname().release,
                binary_sha256=hashes['frozen/bin/pvisor'],
                cache_binary_sha256=hashes['frozen/bin/pvisor-cache'],
                harness_sha256=hashes['frozen/source/benchmark/pvisor/lazy_image_v2.py'],
                source_manifest_sha256=hashes['frozen/source-manifest.json'],
                dirty_patch_sha256=hashes['frozen/source-dirty.patch'],
                binary_source_relationship='preexisting artifacts; build-time source relationship unverified',
                firmware='embedded in static binary; no external firmware override')


def verify_frozen(output, provenance):
    if any(lazy_startup.sha(output / name) != digest for name, digest in provenance['hashes'].items()):
        raise RuntimeError('frozen artifacts/source/harness changed')


def clean_env():
    env = {key: value for key, value in os.environ.items() if not key.startswith('PVISOR_')}
    env.update(PVISOR_CACHE_TOKEN='local-lazy-v2-benchmark', PVISOR_STARTUP_TIMING='0', PVISOR_FS_PROFILE='0')
    return env


def copy_prepared_store(source, destination):
    # Image-root symlinks describe the guest tree, not paths to copy on the host.
    shutil.copytree(source, destination, symlinks=True)


def prepared_store_manifest(root):
    """Hash only lstat-regular files; record links without traversing targets."""
    manifest = {}
    pending = [root]
    while pending:
        directory = pending.pop()
        for path in sorted(directory.iterdir()):
            mode = path.lstat().st_mode
            name = str(path.relative_to(root))
            if stat.S_ISLNK(mode):
                manifest[name] = dict(kind='symlink', target=os.readlink(path))
            elif stat.S_ISDIR(mode):
                pending.append(path)
            elif stat.S_ISREG(mode):
                manifest[name] = dict(kind='regular', sha256=lazy_startup.sha(path))
            else:
                raise RuntimeError(f'unsupported prepared-store entry: {name}')
    return manifest


def chroot_bind_plan():
    """Bind canonical directories once while preserving top-level host aliases."""
    sources = set()
    aliases = []
    for name in ('/home', '/usr', '/etc', '/dev', '/proc', '/bin', '/sbin', '/lib', '/lib64'):
        path = Path(name)
        if name == '/lib64' and not path.exists() and not path.is_symlink():
            continue
        canonical = path.resolve(strict=True)
        if not canonical.is_dir() or canonical == Path('/') or canonical.is_relative_to('/tmp'):
            raise RuntimeError(f'unsafe chroot bind source: {path} -> {canonical}')
        sources.add(canonical)
        if path.is_symlink():
            aliases.append(dict(path=name, target=os.readlink(path)))
    binds = []
    for source in sorted(sources, key=lambda path: (len(path.parts), str(path))):
        if not any(source.is_relative_to(Path(parent)) for parent in binds):
            binds.append(str(source))
    return dict(binds=binds, symlinks=aliases)


def copy_launch_binaries(frozen_dir, private_root):
    directory = private_root / 'tmp/lazy-binaries'
    directory.mkdir(mode=0o700)
    receipt = {}
    for name in ('pvisor', 'pvisor-cache'):
        source = frozen_dir / name
        if not stat.S_ISREG(source.lstat().st_mode):
            raise RuntimeError(f'frozen binary is not a regular file: {source}')
        expected = lazy_startup.sha(source)
        destination = directory / name
        shutil.copyfile(source, destination)
        destination.chmod(0o700)
        copied = lazy_startup.sha(destination)
        if copied != expected or lazy_startup.sha(source) != expected:
            raise RuntimeError(f'launch binary copy hash mismatch: {name}')
        receipt[name] = dict(frozen_path=str(source), launch_path='/tmp/lazy-binaries/' + name,
                             frozen_sha256=expected, launch_sha256=copied)
    return receipt


def validate_launch_tree(root):
    for relative, mode in (('', 0o700), ('tmp', 0o1777), ('tmp/lazy-binaries', 0o700)):
        path = root / relative
        metadata = path.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != mode):
            raise RuntimeError(f'unsafe private launch ancestor: {path}')
    for name in ('pvisor', 'pvisor-cache'):
        path = root / 'tmp/lazy-binaries' / name
        metadata = path.lstat()
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != 0o700):
            raise RuntimeError(f'unsafe private launch executable: {path}')


def verify_launch_binaries(binaries, root=Path('/')):
    for name, record in binaries.items():
        path = root / record['launch_path'].lstrip('/')
        if (lazy_startup.sha(path) != record['frozen_sha256']
                or lazy_startup.sha(Path(record['frozen_path'])) != record['frozen_sha256']):
            raise RuntimeError(f'launch/frozen binary changed: {name}')


class ContainmentEnvironmentGap(RuntimeError):
    """The host cannot provide the required root switch; no insecure fallback."""


def perform_pivot_root(private_root):
    old_root = private_root / '.old-root'
    utility = shutil.which('pivot_root')
    try:
        if utility:
            subprocess.run([utility, str(private_root), str(old_root)], check=True, timeout=10)
            return 'pivot_root utility: ' + utility
        libc = ctypes.CDLL(None, use_errno=True)
        pivot = getattr(libc, 'pivot_root', None)
        if pivot is None:
            raise ContainmentEnvironmentGap('pivot_root environment gap: neither utility nor libc function available')
        pivot.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
        pivot.restype = ctypes.c_int
        if pivot(os.fsencode(private_root), os.fsencode(old_root)) != 0:
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code))
        return 'libc pivot_root'
    except (OSError, subprocess.SubprocessError) as error:
        raise ContainmentEnvironmentGap(f'pivot_root environment gap: {error}') from error


def enter_private_chroot(output, cwd):
    """All construction precedes binds; never chmod/chown a bound host tree."""
    plan = chroot_bind_plan()
    for path in (output, cwd):
        if not path.is_absolute() or not path.is_dir() or not any(
                path.is_relative_to(Path(source)) for source in plan['binds']):
            raise RuntimeError(f'chroot cannot preserve evidence/workspace path: {path}')
    evidence_identity = (output.stat().st_dev, output.stat().st_ino)
    private_root = Path(tempfile.mkdtemp(prefix='lazy-owned-root-', dir='/tmp'))
    (private_root / '.old-root').mkdir(mode=0o700)
    (private_root / 'tmp').mkdir(mode=0o1777)
    (private_root / 'tmp').chmod(0o1777)
    # Prepare every mountpoint and alias before binding, so directory creation
    # can never write through /home, /usr, /etc, /dev or /proc into the host.
    for source in plan['binds']:
        (private_root / source.lstrip('/')).mkdir(parents=True, mode=0o700)
    for alias in plan['symlinks']:
        destination = private_root / alias['path'].lstrip('/')
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(alias['target'])
    binaries = copy_launch_binaries(output / 'frozen/bin', private_root)
    validate_launch_tree(private_root)
    receipt = dict(host_private_root=str(private_root), chroot_root='/', root_mode='0700',
                   tmp_mode='1777', launch_directory_mode='0700', uid=os.getuid(),
                   cwd=str(cwd), symlinks=plan['symlinks'], binds=[], binaries=binaries,
                   root_switch='pivot_root', root_self_bind=False, pivot_root_method=None,
                   entered=False, old_root_detached=False, evidence_path_unchanged=False,
                   host_ownership_changes='none; only private tmpfs paths created/chmodded')
    receipt_path = output / 'containment-chroot.json'
    save(receipt_path, receipt)
    # Bind the fresh tree to itself before adding any submounts. It must be a
    # mountpoint for pivot_root; there are no locked children to omit here.
    subprocess.run(['mount', '--bind', str(private_root), str(private_root)], check=True, timeout=10)
    receipt['root_self_bind'] = True
    save(receipt_path, receipt)
    for source in plan['binds']:
        target = private_root / source.lstrip('/')
        # Recursive binds preserve locked submounts (notably /dev and /proc)
        # that a nonrecursive bind cannot omit in a user namespace. Propagation
        # is already private from unshare, so these mounts remain contained.
        subprocess.run(['mount', '--rbind', source, str(target)], check=True, timeout=10)
        receipt['binds'].append(dict(source=source, host_mountpoint=str(target), chroot_path=source,
                                     recursive=True))
        save(receipt_path, receipt)
    os.chdir(private_root)
    try:
        receipt['pivot_root_method'] = perform_pivot_root(private_root)
        # pivot_root switches the mount namespace's root as well as fs root;
        # chroot alone would prevent libkrun from creating its user namespace.
        os.chdir('/')
        receipt['entered'] = True
        save(receipt_path, receipt)
        try:
            subprocess.run(['umount', '-l', '/.old-root'], check=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as error:
            raise ContainmentEnvironmentGap(f'pivot_root old-root detach environment gap: {error}') from error
        receipt['old_root_detached'] = True
    except ContainmentEnvironmentGap as error:
        receipt['environment_gap'] = str(error)
        save(receipt_path, receipt)
        raise
    os.chdir(cwd)
    validate_launch_tree(Path('/'))
    verify_launch_binaries(binaries)
    if (output.stat().st_dev, output.stat().st_ino) != evidence_identity:
        raise RuntimeError('chroot changed evidence directory identity')
    receipt['evidence_path_unchanged'] = True
    receipt['mountinfo_after_pivot_root'] = Path('/proc/self/mountinfo').read_text()
    save(receipt_path, receipt)
    return receipt


def child(args):
    output = args.output
    variant_envs = COMPARISONS[args.comparison]
    report = dict(comparison=args.comparison, variant_envs=variant_envs,
                  rows=[], failures=[], status='running', preparation={})
    server = service = None
    service_log = None
    counts = Counts()
    try:
        parent = json.loads((output / 'containment-parent.json').read_text())
        identities = namespaces()
        if any(identities[name] == parent['namespaces'][name] for name in identities):
            raise RuntimeError('namespace isolation missing')
        save(output / 'containment-child.json', dict(namespaces=identities, pid=os.getpid(),
                                                    uid=os.getuid(), kvm_open=False))
        subprocess.run(['mount', '-t', 'tmpfs', '-o', 'mode=1777,nosuid,nodev', 'tmpfs', '/tmp'], check=True, timeout=10)
        mountinfo = Path('/proc/self/mountinfo').read_text()
        tmp_mounts = [line for line in mountinfo.splitlines() if line.split()[4] == '/tmp']
        if not tmp_mounts or ' - tmpfs ' not in tmp_mounts[-1]:
            raise RuntimeError('private /tmp is not tmpfs')
        with open('/dev/kvm', 'rb+', buffering=0) as kvm:
            api_version = fcntl.ioctl(kvm.fileno(), 0xAE00)  # KVM_GET_API_VERSION
        if api_version != 12:
            raise RuntimeError('unexpected KVM API version')
        save(output / 'containment-child.json', dict(namespaces=identities, pid=os.getpid(),
             uid=os.getuid(), kvm_open=True, kvm_api_version=api_version,
             tmp_mount=tmp_mounts[-1], mountinfo=mountinfo,
             host_listener_action='none; private tmpfs /tmp and owned pivot_root only'))
        chroot_receipt = enter_private_chroot(output, Path.cwd())
        report['chroot'] = chroot_receipt
        containment = json.loads((output / 'containment-child.json').read_text())
        containment['chroot'] = chroot_receipt
        save(output / 'containment-child.json', containment)
        os.sched_setaffinity(0, {2, 3})
        # Existing supported records are copied verbatim BEFORE starting serve.
        started = time.perf_counter()
        copy_prepared_store(args.prepared_store, output / 'service-store')
        report['preparation']['store_copy_ms'] = (time.perf_counter() - started) * 1000
        store_manifest = prepared_store_manifest(output / 'service-store')
        if not store_manifest:
            raise RuntimeError('empty prepared store')
        save(output / 'prepared-store-manifest.json', store_manifest)
        env = clean_env() | dict(HOME=str(output / 'private-home'),
            XDG_CACHE_HOME=str(output / 'service-client-cache'),
            XDG_CONFIG_HOME=str(output / 'service-config'), XDG_RUNTIME_DIR='/tmp', TMPDIR='/tmp')
        (output / 'private-home').mkdir()
        cache_binary = Path(chroot_receipt['binaries']['pvisor-cache']['launch_path'])
        binary = Path(chroot_receipt['binaries']['pvisor']['launch_path'])
        # Bind ephemeral loopback ports without touching any existing listener.
        reservation = socket.socket()
        reservation.bind(('127.0.0.1', 0))
        upstream_port = reservation.getsockname()[1]
        reservation.close()
        service_log = (output / 'cache-service.log').open('wb')
        service = subprocess.Popen(['taskset', '-c', '2,3', str(cache_binary), 'serve', '--listen',
                                    f'tcp://127.0.0.1:{upstream_port}', '--image-store', str(output / 'service-store')],
                                   env=env, stdout=service_log, stderr=subprocess.STDOUT)
        server = TCPServer(('127.0.0.1', 0), CacheProxy)
        server.counts, server.upstream = counts, upstream_port
        lazy_startup.serve(server)
        env['PVISOR_CACHE_SERVER'] = f'tcp://127.0.0.1:{server.server_address[1]}'
        for _ in range(100):
            if service.poll() is not None:
                raise RuntimeError('cache service exited')
            try:
                with socket.create_connection(('127.0.0.1', upstream_port), timeout=.1):
                    break
            except OSError:
                time.sleep(.1)
        else:
            raise TimeoutError('cache service not ready')
        prepared, elapsed = lazy_startup.checked(
            [str(cache_binary), 'prepare', lazy_startup.NUMPY_SOURCE], output, 'cached-service-prepare',
            env=env | dict(XDG_CACHE_HOME=str(output / 'prepare-cache')), timeout=90)
        report['preparation']['cached_service_prepare_ms'] = elapsed
        report['preparation']['prepared_stdout'] = prepared.decode()
        digest = lazy_startup.NUMPY_SOURCE.split('@')[1]
        prepared_record = json.loads(prepared)
        if (prepared_record.get('status') != 'prepared' or prepared_record.get('digest') != digest
                or prepared_record.get('architecture') != 'amd64' or not prepared_record.get('image_handle')):
            raise RuntimeError('supported prepare CLI returned a mismatching digest/platform/handle')
        prep_counts = counts.snapshot()
        save(output / 'preparation-requests.json', prep_counts)
        if prep_counts['errors']:
            raise RuntimeError('proxy error during service preparation')
        workload, marker = lazy_startup.workload('numpy-script')
        rng = random.Random(SEED)
        for trial in range(-args.warmups - 1, args.samples):
            variants = list(variant_envs)
            rng.shuffle(variants)
            for variant in variants:
                pair = output / f'trial-{trial}-{variant}'
                pair.mkdir()
                cache = pair / 'client-cache'
                for state in ('cold', 'warm'):
                    folder = pair / state
                    workspace = folder / 'workspace'
                    workspace.mkdir(parents=True)
                    metrics_dir = create_bridge_metrics_dir(folder)
                    local_env = env | variant_envs[variant] | dict(
                        PVISOR_LAZY_BRIDGE_METRICS_DIR=str(metrics_dir), XDG_CACHE_HOME=str(cache), XDG_CONFIG_HOME=str(folder / 'config'),
                        XDG_RUNTIME_DIR='/tmp', TMPDIR='/tmp', PVISOR_RUN_HOME=str(folder / 'runs'),
                        PVISOR_IMAGE_STORE=str(folder / 'store'))
                    before = counts.snapshot()
                    argv = ['taskset', '-c', '0,1', str(binary), 'run', '--vm', '--rootfs',
                            'image=' + lazy_startup.NUMPY_SOURCE, '--no-agent-defaults', '--overlaynet', 'off',
                            '--stdio', 'inherit', '--stage', str(folder / 'stage'), '--cpu', '2',
                            '--memory', '2048MiB', '--timeout', '90s', '--', *workload]
                    try:
                        timing = lazy_startup.timed(argv, workspace, local_env, folder, timeout=90, marker=marker)
                    finally:
                        try:
                            after = counts.snapshot(timeout=5)
                        except Exception:
                            with counts.condition:
                                after = dict(rows=list(counts.rows), errors=list(counts.errors),
                                             connections=counts.connections, drain_failed=True)
                            save(folder / 'requests.json', after)
                            raise
                        requests = after['rows'][len(before['rows']):]
                        save(folder / 'requests.json', dict(rows=requests, errors=after['errors'],
                                                          connections=after['connections'] - before['connections']))
                    if after['errors']:
                        raise RuntimeError('cache proxy error; invalid batch')
                    bundles = list((folder / 'stage').rglob('run-bundle.json'))
                    if len(bundles) != 1:
                        raise RuntimeError('missing or duplicate Run Bundle')
                    validate_bundle_execution(json.loads(bundles[0].read_text()), 'pvisor-vm')
                    content = sum(r['content_bytes'] for r in requests)
                    if (state == 'cold' and content <= 0) or (state == 'warm' and content != 0):
                        raise RuntimeError('invalid cold/warm content transfer')
                    report['rows'].append(dict(trial=trial, variant=variant, cache=state, **timing,
                        content_bytes=content, metadata_bytes=sum(r['metadata_bytes'] for r in requests),
                        response_bytes=sum(r['response_bytes'] for r in requests),
                        requests=len(requests), connections=after['connections'] - before['connections'],
                        operations=dict(Counter(r['op'] for r in requests)),
                        correctness='passed', bundle_validated=True, proxy_errors=[]))
                    save(output / 'progress.json', report)
            print(f'completed paired round {trial}', flush=True)
        if service.poll() is not None:
            raise RuntimeError('cache service exited during campaign')
        validate_launch_tree(Path('/'))
        verify_launch_binaries(chroot_receipt['binaries'])
        # Bridge mmap counters are not stable until the outer supervisor has
        # reaped PID 1 and audited that no namespace users remain.
        if not args.comparison.startswith('bridge-'):
            report['summary'] = summarize(report['rows'], args.samples, args.warmups, tuple(variant_envs))
        report['status'] = 'passed' if args.samples >= 30 else 'smoke-only'
    except Exception as error:
        report['failures'].append(repr(error))
        report['status'] = 'failed'
        if isinstance(error, ContainmentEnvironmentGap):
            report['environment_gaps'] = [str(error)]
    finally:
        if server:
            server.shutdown()
            server.server_close()
        if service and service.poll() is None:
            service.terminate()
            try:
                service.wait(timeout=5)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait(timeout=5)
        if service_log:
            service_log.close()
        try:
            final_counts = counts.snapshot()
            save(output / 'all-proxy-requests.json', final_counts)
            if final_counts['errors']:
                report['status'] = 'failed'
                report['failures'].append('proxy errors at final drain')
        except Exception as error:
            report['status'] = 'failed'
            report['failures'].append(repr(error))
            with counts.condition:
                save(output / 'all-proxy-requests.json', dict(rows=list(counts.rows),
                     errors=list(counts.errors), connections=counts.connections, drain_failed=True))
        if report['status'] == 'failed':
            report.pop('summary', None)
        save(output / 'child-result.json', report)
    return 0 if report['status'] in ('passed', 'smoke-only') else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary-dir', type=Path, required=True)
    parser.add_argument('--prepared-store', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True, help='new evidence directory (never reused)')
    parser.add_argument('--comparison', choices=tuple(COMPARISONS), default='v1-v2',
                        help='v1-v2: prefetch/socket reuse; v2-index: client pages (both pin legacy bridge=0); bridge-rpc/bridge-pages: legacy versus persistent bridge with pages=0/1 respectively')
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--namespace-child', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error('positive samples and nonnegative warmups required')
    for name in ('binary_dir', 'prepared_store', 'output'):
        setattr(args, name, getattr(args, name).resolve())
    if args.namespace_child:
        return child(args)
    if not args.prepared_store.is_dir():
        parser.error('--prepared-store must exist')
    if args.output == args.prepared_store or args.output.is_relative_to(args.prepared_store):
        parser.error('output must not be within prepared store')
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output
    report = dict(benchmark='B-LAZY-ENG', role='engineering A/B', source=lazy_startup.NUMPY_SOURCE,
                  workload='numpy-script', comparison=args.comparison,
                  variant_envs=COMPARISONS[args.comparison], samples=args.samples, warmups=args.warmups,
                  initial_excluded_round=True, seed=SEED, status='failed', rows=[], failures=[],
                  protocol=dict(client_cpu=[0, 1], service_and_proxy_cpu=[2, 3], vcpu=2,
                  guest_ram_mib=2048, launch_timeout_seconds=90, supervisor_timeout_seconds=900,
                  network='loopback TCP; no Docker, TLS registry, artificial delay, or WAN claim',
                  cache='fresh XDG cache per variant/trial pair; cold then warm; fresh stage/workspace each launch',
                  comparison={
                      'v1-v2': 'IMAGE_V2=0/1, INDEX_PAGES=0 both, BRIDGE_V2=0 both (historical control)',
                         'v2-index': 'IMAGE_V2=1 both, INDEX_PAGES=0/1, BRIDGE_V2=0 both (prior control)',
                         'bridge-rpc': 'IMAGE_V2=1 both, INDEX_PAGES=0 both, BRIDGE_V2=0/1',
                         'bridge-pages': 'IMAGE_V2=1 both, INDEX_PAGES=1 both, BRIDGE_V2=0/1'}[args.comparison],
                  bridge_telemetry=dict(abi='schema 1; 16 native-endian u64 words; 128 bytes',
                      words=BRIDGE_WORDS, directory_mode='0700; same user; new per launch',
                      file_mode='0600; bridge-created .stats mmap survives _exit/SIGKILL',
                      collection='outer parent after reaping and verified namespace teardown',
                      gauges='retained per file; nonzero after SIGKILL is allowed, not an invalidation',
                      totals='all launches, including initial round/warmups; timing counters in ns'),
                  accounting='bridge_*: runner to Unix bridge, post-namespace-teardown mmap totals (all launches including excluded rounds); requests/connections: separate upstream TCP proxy counts including host Prepare/root loads; bridge forwarded operations reconcile after removing Prepare connections; content_bytes: Read Data only; metadata_bytes: Metadata Data only; response_bytes: JSON frames plus all raw Data bytes; excludes TCP/IP overhead; pings counted and op-separated',
                  exclusions='initial round and configured warmups only; any failure invalidates batch; no speed exclusions',
                  limits='warm host page cache; guest RAM is not enclosing memory limit; no concurrency/throughput claim'))
    save(output / 'containment-parent.json', dict(namespaces=namespaces(), pid=os.getpid(), uid=os.getuid()))
    try:
        report['provenance'] = freeze(output, args.binary_dir)
        receipt = supervise(namespace_command(args), output)
        save(output / 'containment-supervisor.json', receipt)
        report['containment'] = receipt
        result = output / 'child-result.json'
        if result.exists():
            report.update(json.loads(result.read_text()))
        elif (output / 'progress.json').exists():
            report.update(json.loads((output / 'progress.json').read_text()))
            report['status'] = 'failed'
        if receipt['timed_out'] or receipt['exit_code'] != 0 or not receipt['teardown_verified']:
            raise RuntimeError('namespace child failed, timed out, or teardown could not be verified')
        verify_frozen(output, report['provenance'])
        report['bridge_totals'] = collect_bridge_metrics(
            output, report['rows'], receipt['teardown_verified'],
            required=args.comparison.startswith('bridge-'))
        report['summary'] = summarize(report['rows'], args.samples, args.warmups,
                                      tuple(COMPARISONS[args.comparison]))
        report['containment'] = receipt
    except Exception as error:
        report['status'] = 'failed'
        report['failures'].append(repr(error))
        report.pop('summary', None)
    # One immutable final report; progress and child evidence are separate files.
    path = output / 'report.json'
    with path.open('x') as stream:
        stream.write(json.dumps(report, indent=2, sort_keys=True) + '\n')
    path.chmod(0o444)
    (output / 'report.sha256').write_text(lazy_startup.sha(path) + '  report.json\n')
    print(f'{report["status"]}: {path}', flush=True)
    return 0 if report['status'] in ('passed', 'smoke-only') and not report['failures'] else 1


if __name__ == '__main__':
    sys.exit(main())
