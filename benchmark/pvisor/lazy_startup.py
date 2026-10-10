#!/usr/bin/env python3
"""Benchmark: B-LAZY-STARTUP (benchmark/README.md#b-lazy-startup), role user-facing.
Motivation: quantify disposable shell or Python numerical environment startup with lazy client reads.
Conclusion sought: cold/warm ready, completion and transferred response bytes;
service preparation is separate, not a claim of registry-free cold startup.
Design: pinned amd64 manifest, Distribution registry and real pvisor-cache TCP,
loopback with no injected latency, 2 CPUs / 2 GiB, shuffled paired rounds.
Failures are retained and invalidate the campaign; no slow-sample exclusion.

Current NumPy rerun (prepare NEW static binaries independently, finish builds/tests
before preflight/sampling; never replace target/release or the host listener):
  sg docker -c 'python3 benchmark/pvisor/lazy_startup.py --isolate-host \
    --workload numpy-script --binary-dir PATH_TO_NEW_STATIC_BINARIES \
    --output benchmark/pvisor/.data/lazy-numpy-current-preflight --samples 1 --warmups 0'
Use a different new output for --samples 30 --warmups 3 after preflight succeeds.
Optional --prepared-store benchmark/pvisor/.data/lazy-numpy-local-20261007/service-store
copies supported records with symlinks and records cached-service preparation;
without it, full upstream preparation is the default. Add --registry-source-store
with the retained service-store path to publish validated original compressed OCI
blobs to a fresh Distribution registry without Docker Hub. No rootfs reconstruction
or network fallback is allowed for that registry source. Combine both store options
for a cached-preparation rerun. The registry image must already be in Docker.
Registry publication and cached service preparation remain separately timed.
--registry-copy-timeout defaults to 180 seconds; registry-mirror.stdout/.stderr and
phase.json update during the copy, with heartbeat messages in namespace.stdout. --build-receipt retains independent build
evidence without claiming that a receipt alone verifies source/binary correspondence.
Isolation requires Linux user/mount/PID namespaces, KVM, CPU 0-3, Docker socket
/run/docker.sock, skopeo, openssl, and free ports 15000/15443/15444/15445. The supervisor
must have primary Docker GID before unshare; root UID/GID map to the launching UID/GID.
The private root recursively binds /run, /var and /sys in addition to the existing
plan; /sys preserves CPU topology for lscpu after pivot_root.
Evidence must be host-visible outside private /tmp. Registry uses its daemon-owned
anonymous storage volume, never a private-/tmp bind source; TLS certificates stay
in host-visible output and are read by the Python proxy, not a daemon bind mount.
Current lazy flags IMAGE_V2, INDEX_PAGES and BRIDGE_V2 are explicitly 1. Data from Read
is file content; Data from Metadata is metadata, not file content. Final report is
read-only with a digest, frozen source/binaries and namespace/teardown receipts.
No current performance claim follows from runner tests or historical cohorts.
"""
import argparse
import fcntl
import grp
import sys
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import random
import selectors
import stat
import shlex
import shutil
import signal
import socket
import socketserver
import ssl
import statistics
import struct
import subprocess
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
SEED = 20261007
MARKER = b'LAZY_READY ubuntu 26.04\n'
WORKLOAD = '. /etc/os-release; test "$ID" = ubuntu && test "$VERSION_ID" = 26.04 || exit 41; printf "LAZY_READY %s %s\\n" "$ID" "$VERSION_ID"'


TORCH_SOURCE = 'docker.io/determinedai/pytorch-cpu@sha256:875cbd3391016a74c42cfb0b3712d3b70f5b803a04b80eebd7f2a46b9d53d18d'
TORCH_MARKER = b'LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240\n'
TORCH_WORKLOAD = '''import sys
import torch
assert sys.version_info[:3] == (3, 10, 14), sys.version
assert torch.__version__ == "2.0.1+cpu", torch.__version__
assert torch.version.cuda is None, torch.version.cuda
torch.set_num_threads(1)
torch.set_num_interop_threads(1)
x = torch.arange(16, dtype=torch.int64, device="cpu")
assert x.square().sum().item() == 1240
print("LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240", flush=True)
'''


NUMPY_SOURCE = 'docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159'
NUMPY_MARKER = b'LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680\n'
NUMPY_WORKLOAD = '''import sys
import numpy as np
assert sys.version_info[:3] == (3, 13, 14), sys.version
assert np.__version__ == "2.5.2", np.__version__
x = np.arange(16, dtype=np.int64).reshape(4, 4)
assert int(np.square(x).sum()) == 1240
assert int((x @ x.T).sum()) == 3680
print("LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680", flush=True)
'''


def workload(name):
    if name == 'ubuntu-shell':
        return ['/bin/sh', '-c', WORKLOAD], MARKER
    if name == 'torch-import':
        # Explicit environment and interpreter: do not depend on image ENV support.
        return ['/usr/bin/env', 'OMP_NUM_THREADS=1', 'MKL_NUM_THREADS=1',
                'OPENBLAS_NUM_THREADS=1', 'PYTHONHASHSEED=0',
                '/opt/conda/bin/python', '-B', '-u', '-c', TORCH_WORKLOAD], TORCH_MARKER
    if name == 'numpy-script':
        return ['/usr/bin/env', 'OMP_NUM_THREADS=1', 'MKL_NUM_THREADS=1',
                'OPENBLAS_NUM_THREADS=1', 'PYTHONHASHSEED=0',
                '/usr/local/bin/python', '-B', '-u', '-c', NUMPY_WORKLOAD], NUMPY_MARKER
    raise ValueError(f'unknown workload: {name}')


def blob_bytes(manifest):
    # Repeated empty layers are transferred once by digest, not once per descriptor.
    descriptors = [manifest['config'], *manifest['layers']]
    return sum({item['digest']: item['size'] for item in descriptors}.values())


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


NAMESPACE_CHILD = False
CURRENT_LAZY_ENV = dict(PVISOR_LAZY_IMAGE_V2='1', PVISOR_LAZY_INDEX_PAGES='1',
                        PVISOR_LAZY_BRIDGE_V2='1')


def docker(*args):
    argv = ['docker', *map(str, args)]
    # sg must precede unshare: the child maps the launching Docker GID to root.
    return ['docker', '--host', 'unix:///run/docker.sock', *map(str, args)] if NAMESPACE_CHILD else ['sg', 'docker', '-c', shlex.join(argv)]


def isolation_helpers():
    # Avoid importing a second copy when this entry point runs as __main__.
    sys.modules.setdefault('lazy_startup', sys.modules[__name__])
    import lazy_image_v2
    return lazy_image_v2


def docker_bind_plan(base):
    plan = dict(binds=list(base['binds']), symlinks=list(base['symlinks']))
    for name in ('/run', '/var', '/sys'):
        path = Path(name)
        canonical = path.resolve(strict=True)
        if not canonical.is_dir() or canonical == Path('/') or canonical.is_relative_to('/tmp'):
            raise RuntimeError(f'unsafe Docker bind source: {path}')
        if not any(canonical.is_relative_to(Path(parent)) for parent in plan['binds']):
            plan['binds'].append(str(canonical))
        if path.is_symlink():
            plan['symlinks'].append(dict(path=name, target=os.readlink(path)))
    return plan


def namespace_command(args):
    command = ['unshare', '--user', '--map-root-user', '--mount', '--pid', '--fork',
               '--kill-child=KILL', '--mount-proc', '--propagation', 'private',
               sys.executable, str(args.output / 'frozen/source/benchmark/pvisor/lazy_startup.py'),
               '--isolate-host', '--namespace-child', '--output', str(args.output),
               '--binary-dir', str(args.output / 'frozen/bin'), '--workload', args.workload,
               '--source', args.source, '--registry-image', args.registry_image,
               '--samples', str(args.samples), '--warmups', str(args.warmups)]
    if args.prepared_store:
        command += ['--prepared-store', str(args.prepared_store)]
    if args.registry_source_store:
        command += ['--registry-source-store', str(args.registry_source_store)]
    command += ['--registry-copy-timeout', str(args.registry_copy_timeout)]
    return command


def enter_campaign_namespace(output):
    helpers = isolation_helpers()
    parent = json.loads((output / 'containment-parent.json').read_text())
    identities = helpers.namespaces()
    if any(identities[name] == parent['namespaces'][name] for name in identities):
        raise RuntimeError('namespace isolation missing')
    receipt = dict(namespaces=identities, pid=os.getpid(), uid=os.getuid(), gid=os.getgid(),
                   uid_map=Path('/proc/self/uid_map').read_text(),
                   gid_map=Path('/proc/self/gid_map').read_text(), kvm_open=False,
                   host_listener_action='none; private tmpfs /tmp and owned pivot_root only')
    helpers.save(output / 'containment-child.json', receipt)
    subprocess.run(['mount', '-t', 'tmpfs', '-o', 'mode=1777,nosuid,nodev', 'tmpfs', '/tmp'],
                   check=True, timeout=10)
    mountinfo = Path('/proc/self/mountinfo').read_text()
    tmp_mounts = [line for line in mountinfo.splitlines() if line.split()[4] == '/tmp']
    if not tmp_mounts or ' - tmpfs ' not in tmp_mounts[-1]:
        raise RuntimeError('private /tmp is not tmpfs')
    with open('/dev/kvm', 'rb+', buffering=0) as kvm:
        version = fcntl.ioctl(kvm.fileno(), 0xAE00)
    if version != 12:
        raise RuntimeError('unexpected KVM API version')
    original = helpers.chroot_bind_plan
    # Extend only this process's plan; all root construction, recursive binds,
    # ownership checks and root switching still use the proven implementation.
    helpers.chroot_bind_plan = lambda: docker_bind_plan(original())
    try:
        chroot = helpers.enter_private_chroot(output, Path.cwd())
    finally:
        helpers.chroot_bind_plan = original
    receipt.update(kvm_open=True, kvm_api_version=version, tmp_mount=tmp_mounts[-1],
                   chroot=chroot)
    helpers.save(output / 'containment-child.json', receipt)
    os.sched_setaffinity(0, {2, 3})
    return chroot


def isolated_campaign(args):
    helpers = isolation_helpers()
    docker_gid = grp.getgrnam('docker').gr_gid
    if os.getgid() != docker_gid:
        raise RuntimeError('Run the entire script via sg docker -c before unshare; '
                           'the primary Docker GID must be mapped into the namespace')
    if args.firmware or args.cache_binary:
        raise ValueError('--isolate-host requires both static binaries in --binary-dir; '
                         'no firmware/cache-only override')
    args.output = args.output.resolve()
    args.binary_dir = args.binary_dir.resolve()
    if args.prepared_store:
        args.prepared_store = args.prepared_store.resolve()
        if (not args.prepared_store.is_dir() or args.prepared_store.is_relative_to('/tmp')
                or args.output.is_relative_to(args.prepared_store)):
            raise ValueError('prepared store must exist and must not contain output')
    # Docker daemon mount sources and retained evidence must survive private /tmp.
    binds = docker_bind_plan(helpers.chroot_bind_plan())['binds']
    if args.registry_source_store:
        args.registry_source_store = args.registry_source_store.resolve()
        if (not args.registry_source_store.is_dir()
                or args.registry_source_store.is_relative_to('/tmp')
                or args.output.is_relative_to(args.registry_source_store)
                or not any(args.registry_source_store.is_relative_to(Path(p)) for p in binds)):
            raise ValueError('registry source store must exist in a preserved host path outside private /tmp; output must be separate')
    if args.output.is_relative_to('/tmp') or not any(args.output.is_relative_to(Path(p)) for p in binds):
        raise ValueError('output must be a host-visible path outside private /tmp (prefer repository .data)')
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output
    report = dict(benchmark='B-LAZY-STARTUP', role='user-facing', status='failed', rows=[], failures=[])
    helpers.save(output / 'containment-parent.json', dict(namespaces=helpers.namespaces(),
                 pid=os.getpid(), uid=os.getuid(), gid=os.getgid(), docker_gid=docker_gid,
                 host_listener_action='none'))
    try:
        provenance = helpers.freeze(output, args.binary_dir)
        provenance['harness_sha256'] = provenance['hashes']['frozen/source/benchmark/pvisor/lazy_startup.py']
        if args.build_receipt:
            destination = output / 'build-receipt'
            shutil.copy2(args.build_receipt.resolve(), destination)
            provenance['independent_build_receipt_sha256'] = sha(destination)
            provenance['hashes']['build-receipt'] = sha(destination)
        helpers.save(output / 'campaign-provenance.json', provenance)
        report['provenance'] = provenance
        print(f'namespace campaign logs: {output / "namespace.stdout"}; live phase: {output / "phase.json"}', flush=True)
        receipt = helpers.supervise(namespace_command(args), output, timeout=args.supervisor_timeout)
        helpers.save(output / 'containment-supervisor.json', receipt)
        result = output / 'child-result.json'
        if result.exists():
            report.update(json.loads(result.read_text()))
        elif (output / 'progress.json').exists():
            report.update(json.loads((output / 'progress.json').read_text()))
            report['status'] = 'failed'
        report['containment'] = receipt
        if receipt['timed_out'] or receipt['exit_code'] != 0 or not receipt['teardown_verified']:
            raise RuntimeError('namespace child failed/timed out or teardown could not be verified')
        helpers.verify_frozen(output, provenance)
        validate_campaign_rows(report['rows'], args.samples, args.warmups)
        registry_receipt = report.get('preparation', {}).get('registry_source', {})
        if registry_receipt.get('mode') == 'cached-oci':
            verify_registry_archive(output, registry_receipt)
        report['summary'] = summarize(report['rows'])
    except Exception as error:
        report['status'] = 'failed'
        report['failures'].append(dict(error=repr(error), at=time.time()))
        report.pop('summary', None)
    finally:
        try:
            report['docker_cleanup'] = cleanup_owned_docker(output, report.get('manifest_digest'))
        except Exception as error:
            report['status'] = 'failed'
            report['failures'].append(dict(error='Docker cleanup failed: ' + repr(error), at=time.time()))
            report.pop('summary', None)
    path = output / 'report.json'
    with path.open('x') as stream:
        stream.write(json.dumps(report, indent=2, sort_keys=True) + '\n')
    path.chmod(0o444)
    (output / 'report.sha256').write_text(sha(path) + '  report.json\n')
    print(f'{report["status"]}: {path}', flush=True)
    return 0 if report['status'] in ('passed', 'smoke-only') and not report['failures'] else 1


def cleanup_owned_docker(output, digest):
    # Docker resources outlive namespace PID 1. Never target host processes,
    # unrelated containers, shared tags or globally prune the daemon's store.
    name = registry_name_for(output)
    def local_docker(*args):
        return docker('--host', 'unix:///run/docker.sock', *args)
    listed = subprocess.run(local_docker('ps', '-aq', '--filter', 'name=^/' + name + '(-|$)'),
                            capture_output=True, timeout=20)
    if listed.returncode:
        raise RuntimeError('cannot audit owned Docker containers: ' + listed.stderr.decode(errors='replace'))
    logs = subprocess.run(local_docker('logs', name), capture_output=True, timeout=20)
    (output / 'registry-parent.log').write_bytes(logs.stdout + logs.stderr)
    removed = []
    for container in listed.stdout.decode().split():
        result = subprocess.run(local_docker('rm', '-f', '-v', container), capture_output=True, timeout=30)
        if result.returncode:
            raise RuntimeError('owned Docker container removal failed')
        removed.append(container)
    remaining = subprocess.run(local_docker('ps', '-aq', '--filter', 'name=^/' + name + '(-|$)'),
                               capture_output=True, timeout=20)
    if remaining.returncode or remaining.stdout.strip():
        raise RuntimeError('owned Docker containers remain or cleanup audit failed')
    if digest:
        subprocess.run(local_docker('image', 'rm', '127.0.0.1:15443/bench/workload@' + digest),
                       capture_output=True, timeout=30)
    return dict(name_prefix=name, removed_containers=removed, remaining_containers=[],
                container_teardown_verified=True, global_prune=False)


def registry_name_for(output):
    return 'pvisor-lazy-bench-' + hashlib.sha256(str(output).encode()).hexdigest()[:16]


def validate_campaign_rows(rows, samples, warmups):
    expected = {(variant, state, trial) for variant in ('docker', 'lazy')
                for state in ('cold', 'warm') for trial in range(-warmups - 1, samples)}
    observed = [(row['variant'], row['cache'], row['trial']) for row in rows]
    if len(observed) != len(expected) or set(observed) != expected:
        raise ValueError('incomplete/duplicate campaign including initial round and warmups')
    if any(row['correctness'] != 'passed' for row in rows):
        raise ValueError('incorrect campaign sample including excluded rounds')


def validate_prepared_record(prepared, digest):
    record = json.loads(prepared)
    if (record.get('status') != 'prepared' or record.get('digest') != digest
            or record.get('architecture') != 'amd64' or not record.get('image_handle')):
        raise RuntimeError('supported prepare CLI returned a mismatching digest/platform/handle')
    return record


def phase(output, name, **details):
    record = dict(phase=name, at=time.time(), **details)
    temporary = output / 'phase.json.tmp'
    temporary.write_text(json.dumps(record, indent=2) + '\n')
    temporary.replace(output / 'phase.json')
    print(json.dumps(record), flush=True)


def checked_live(argv, output, name, timeout):
    """Stream preparation logs to evidence while bounding our own process group."""
    started = time.monotonic()
    (output / (name + '.command.json')).write_text(json.dumps(argv))
    phase(output, name, status='running', timeout_seconds=timeout,
          stdout=str(output / (name + '.stdout')), stderr=str(output / (name + '.stderr')))
    with (output / (name + '.stdout')).open('wb') as stdout, (output / (name + '.stderr')).open('wb') as stderr:
        process = subprocess.Popen(argv, stdout=stdout, stderr=stderr, start_new_session=True)
        heartbeat = started
        try:
            while process.poll() is None:
                now = time.monotonic()
                if now - started >= timeout:
                    raise TimeoutError(f'{name} exceeded {timeout}s; see retained live stdout/stderr')
                if now - heartbeat >= 5:
                    phase(output, name, status='running', elapsed_seconds=now - started,
                          timeout_seconds=timeout)
                    heartbeat = now
                time.sleep(min(.1, timeout - (now - started)))
        except BaseException:
            isolation_helpers().kill_group(process)
            phase(output, name, status='interrupted', elapsed_seconds=time.monotonic() - started)
            raise
    elapsed = (time.monotonic() - started) * 1000
    phase(output, name, status='passed' if process.returncode == 0 else 'failed', elapsed_ms=elapsed)
    if process.returncode:
        error = (output / (name + '.stderr')).read_text(errors='replace')[-2000:]
        raise RuntimeError(f'{name} failed ({process.returncode}): {error}')
    return elapsed


def digest_hex(digest):
    if (not isinstance(digest, str) or not digest.startswith('sha256:')
            or len(digest) != 71 or any(c not in '0123456789abcdef' for c in digest[7:])):
        raise ValueError(f'expected canonical SHA-256 digest: {digest!r}')
    return digest[7:]


def verify_oci_blob(path, digest, size):
    expected = digest_hex(digest)
    if type(size) is not int or size < 0:
        raise ValueError('invalid OCI descriptor size')
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f'OCI blob must be a regular file: {path}')
    hasher = hashlib.sha256()
    length = 0
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            length += len(block)
            hasher.update(block)
    if length != size or hasher.hexdigest() != expected:
        raise ValueError(f'OCI blob length/hash mismatch: {path}')


def archive_registry_oci(store, output, source):
    """Package original compressed blobs, never reconstruct from prepared rootfs."""
    store = store.resolve(strict=True)
    digest = source.split('@', 1)[1]
    hex_digest = digest_hex(digest)
    manifest_path = store / 'metadata/manifests-v1' / (hex_digest + '.json')
    if not stat.S_ISREG(manifest_path.lstat().st_mode):
        raise ValueError('retained manifest must be a regular file')
    manifest_bytes = manifest_path.read_bytes()
    if hashlib.sha256(manifest_bytes).hexdigest() != hex_digest:
        raise ValueError('retained manifest hash differs from pinned source')
    manifest = json.loads(manifest_bytes)
    if (manifest.get('schemaVersion') != 2
            or manifest.get('mediaType') != 'application/vnd.oci.image.manifest.v1+json'
            or not isinstance(manifest.get('layers'), list)):
        raise ValueError('expected a schema-2 single-platform OCI image manifest')
    config = manifest['config']
    if config.get('mediaType') != 'application/vnd.oci.image.config.v1+json':
        raise ValueError('expected OCI image config')
    layer_types = {'application/vnd.oci.image.layer.v1.tar+gzip',
                   'application/vnd.oci.image.layer.v1.tar+zstd'}
    if any(layer.get('mediaType') not in layer_types for layer in manifest['layers']):
        raise ValueError('expected original compressed OCI layers')
    descriptors = {}
    for descriptor in [config, *manifest['layers']]:
        blob_digest = descriptor['digest']
        blob_hex = digest_hex(blob_digest)
        size = descriptor['size']
        if blob_digest in descriptors and descriptors[blob_digest]['size'] != size:
            raise ValueError('inconsistent duplicate OCI descriptor sizes')
        path = store / 'blobs/sha256' / blob_hex
        verify_oci_blob(path, blob_digest, size)
        descriptors[blob_digest] = dict(path=path, size=size)
    config_path = descriptors[config['digest']]['path']
    platform = json.loads(config_path.read_bytes())
    if platform.get('os') != 'linux' or platform.get('architecture') != 'amd64':
        raise ValueError('retained OCI config is not linux/amd64')
    layout = output / 'archive/registry-oci'
    blobs = layout / 'blobs/sha256'
    blobs.mkdir(parents=True, exist_ok=False)
    records = {}
    for blob_digest, descriptor in descriptors.items():
        destination = blobs / digest_hex(blob_digest)
        # Independent read-only copies: chmod/write can never affect retained
        # originals through a hardlink. Hash again to reject concurrent mutation.
        shutil.copyfile(descriptor['path'], destination)
        verify_oci_blob(destination, blob_digest, descriptor['size'])
        destination.chmod(0o444)
        records[blob_digest] = dict(size=descriptor['size'], source_path=str(descriptor['path']),
                                   archive_path=str(destination.relative_to(output)))
    archived_manifest = blobs / hex_digest
    archived_manifest.write_bytes(manifest_bytes)
    archived_manifest.chmod(0o444)
    for name, value in (
        ('oci-layout', dict(imageLayoutVersion='1.0.0')),
        ('index.json', dict(schemaVersion=2, mediaType='application/vnd.oci.image.index.v1+json',
            manifests=[dict(mediaType=manifest['mediaType'], digest=digest, size=len(manifest_bytes),
                            platform=dict(os='linux', architecture='amd64'),
                            annotations={'org.opencontainers.image.ref.name': 'workload'})])),
    ):
        path = layout / name
        path.write_text(json.dumps(value, indent=2) + '\n')
        path.chmod(0o444)
    archive_files = {str(path.relative_to(output)): dict(digest='sha256:' + sha(path), size=path.stat().st_size)
                     for path in layout.rglob('*') if path.is_file()}
    receipt = dict(mode='cached-oci', source_store=str(store), layout=str(layout.relative_to(output)),
                   archive_files=archive_files,
                   manifest_source_path=str(manifest_path), manifest_digest=digest,
                   manifest_size=len(manifest_bytes), manifest_media_type=manifest['mediaType'],
                   platform=dict(os='linux', architecture='amd64'), blobs=records,
                   original_files_modified=False, retention='independent read-only copies; no rootfs reconstruction')
    (output / 'registry-source.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return 'oci:' + str(layout) + ':workload', receipt


def verify_registry_archive(output, receipt):
    for name, descriptor in receipt['archive_files'].items():
        path = Path(name)
        if path.is_absolute() or '..' in path.parts or not path.is_relative_to('archive/registry-oci'):
            raise ValueError('invalid archived OCI evidence path')
        verify_oci_blob(output / path, descriptor['digest'], descriptor['size'])


def registry_copy_command(source):
    return ['skopeo', 'copy', '--override-arch', 'amd64', '--preserve-digests',
            '--dest-tls-verify=false', source, 'docker://127.0.0.1:15000/bench/workload:latest']


def checked(argv, output, name, env=None, timeout=180):
    started = time.perf_counter()
    process = subprocess.run(argv, env=env, capture_output=True, timeout=timeout)
    (output / (name + '.stdout')).write_bytes(process.stdout)
    (output / (name + '.stderr')).write_bytes(process.stderr)
    (output / (name + '.command.json')).write_text(json.dumps(argv))
    if process.returncode:
        raise RuntimeError(f'{name} failed ({process.returncode}): {process.stderr.decode(errors="replace")[-2000:]}')
    return process.stdout, (time.perf_counter() - started) * 1000


class Counts:
    def __init__(self):
        self.lock = threading.Lock()
        self.rows = []
        self.active = 0

    def snapshot(self):
        deadline = time.monotonic() + 5
        while True:
            with self.lock:
                if not self.active:
                    return list(self.rows)
            if time.monotonic() > deadline:
                raise RuntimeError('proxy connections failed to drain')
            time.sleep(.005)


class RegistryProxy(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *args):
        pass

    def do_HEAD(self):
        self.forward()

    def do_GET(self):
        self.forward()

    def forward(self):
        counts = self.server.counts
        with counts.lock:
            counts.active += 1
        size = 0
        connection = http.client.HTTPConnection('127.0.0.1', self.server.upstream, timeout=90)
        try:
            connection.request(self.command, self.path, headers={k: v for k, v in self.headers.items() if k.lower() not in ('host', 'connection')})
            response = connection.getresponse()
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() not in ('connection', 'transfer-encoding'):
                    self.send_header(key, value)
            self.send_header('Connection', 'close')
            self.end_headers()
            if self.command != 'HEAD':
                while data := response.read(65536):
                    self.wfile.write(data)
                    size += len(data)
            with counts.lock:
                counts.rows.append(dict(method=self.command, path=self.path, status=response.status, response_body_bytes=size))
        finally:
            connection.close()
            self.close_connection = True
            with counts.lock:
                counts.active -= 1


class CacheProxy(socketserver.BaseRequestHandler):
    """Use the shared framed proxy for retry-safe V1/V2 and payload accounting."""
    def handle(self):
        return isolation_helpers().CacheProxy.handle(self)



def exact(stream, length):
    data = bytearray()
    while len(data) < length:
        block = stream.recv(length - len(data))
        if not block:
            raise EOFError('incomplete frame')
        data.extend(block)
    return data


class TCPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def serve(server):
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def timed(argv, cwd, env, folder, timeout=120, marker=MARKER):
    start = time.perf_counter_ns()
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
    selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
    logs = dict(stdout=bytearray(), stderr=bytearray())
    ready = None
    try:
        while selector.get_map():
            if (time.perf_counter_ns() - start) / 1e9 > timeout:
                raise TimeoutError('startup timeout')
            for key, _ in selector.select(.1):
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                logs[key.data].extend(data)
                if ready is None and marker in logs['stdout']:
                    ready = (time.perf_counter_ns() - start) / 1e6
        code = process.wait(timeout=5)
        completion = (time.perf_counter_ns() - start) / 1e6
        if code or ready is None or bytes(logs['stdout']).count(marker) != 1:
            raise RuntimeError(f'workload failed code={code}, ready={ready}: {logs["stderr"].decode(errors="replace")[-1800:]}')
        return dict(ready_ms=ready, completion_ms=completion)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        selector.close()
        process.stdout.close()
        process.stderr.close()
        for name, data in logs.items():
            (folder / (name + '.log')).write_bytes(data)
        (folder / 'command.json').write_text(json.dumps(dict(argv=argv, exit=process.returncode), indent=2))


def percentile(values, quantile):
    values = sorted(values)
    index = (len(values) - 1) * quantile
    lo = int(index)
    return values[lo] + (values[min(lo + 1, len(values) - 1)] - values[lo]) * (index - lo)


def summarize(rows):
    from publication import distribution
    formal = [row for row in rows if row['trial'] >= 0]
    identities = [(row['variant'], row['cache'], row['trial']) for row in formal]
    if len(identities) != len(set(identities)) or not formal:
        raise ValueError('duplicate or empty formal cohort')
    if any(row['correctness'] != 'passed' for row in formal):
        raise ValueError('incorrect sample')
    trial_sets = [{row['trial'] for row in formal if row['variant'] == variant and row['cache'] == state} for variant in ('docker', 'lazy') for state in ('cold', 'warm')]
    if any(trials != set(range(len(trial_sets[0]))) for trials in trial_sets):
        raise ValueError('incomplete paired cohort')
    result = {}
    for state in ('cold', 'warm'):
        groups = {variant: [r for r in rows if r['variant'] == variant and r['cache'] == state and r['trial'] >= 0] for variant in ('docker', 'lazy')}
        for variant, selected in groups.items():
            result[f'{variant}-{state}'] = dict(n=len(selected), **{field: distribution([r[field] for r in selected]) for field in ('ready_ms', 'completion_ms', 'response_bytes', 'content_bytes', *(['metadata_bytes'] if all('metadata_bytes' in r for r in selected) else []))})
        rng = random.Random(SEED)
        paired = list(zip(sorted(groups['docker'], key=lambda r: r['trial']),
                                  sorted(groups['lazy'], key=lambda r: r['trial']), strict=True))
        differences = []
        for _ in range(5000):
            sample = rng.choices(paired, k=len(paired))
            differences.append(statistics.median([a['ready_ms'] for a, b in sample]) - statistics.median([b['ready_ms'] for a, b in sample]))
        result[f'docker-minus-lazy-{state}'] = dict(ready_median_difference_ms=statistics.median([a['ready_ms'] for a, b in paired]) - statistics.median([b['ready_ms'] for a, b in paired]), ci95_ms=[percentile(differences, .025), percentile(differences, .975)])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--isolate-host', action='store_true', help='Current static binaries in supervised private namespaces; run entire script via sg docker -c')
    parser.add_argument('--namespace-child', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--registry-source-store', type=Path, help='Offline registry source: validate original compressed OCI blobs and pinned manifest from a retained supported image store; no network fallback')
    parser.add_argument('--registry-copy-timeout', type=int, default=180, help='Bound skopeo registry publication; live logs and phase receipts retained (seconds)')
    parser.add_argument('--prepared-store', type=Path, help='Optional supported service store; symlink-preserving copy and cached prepare, NOT fresh preparation')
    parser.add_argument('--build-receipt', type=Path, help='Retain independently produced build evidence verbatim; does not by itself verify source/binary relationship')
    parser.add_argument('--supervisor-timeout', type=int, default=3600, help='Whole isolated campaign timeout, including preparation (seconds)')
    parser.add_argument('--analyze', type=Path, help='Write separate distribution-aware analysis of retained report; no measurements')
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--warmups', type=int, default=3)
    parser.add_argument('--firmware', type=Path, help='External bundle for dynamic GNU builds only; omit for embedded musl firmware')
    parser.add_argument('--binary-dir', type=Path, default=ROOT / 'target/release', help='Directory containing frozen pvisor and pvisor-cache executables')
    parser.add_argument('--cache-binary', type=Path, help='Override only cache executable; keep launcher compatible with existing Host listener')
    parser.add_argument('--workload', choices=('ubuntu-shell', 'torch-import', 'numpy-script'), default='ubuntu-shell')
    parser.add_argument('--source', help='Pinned upstream image; default depends on workload')
    parser.add_argument('--registry-image', default='registry@sha256:ddf754342cfc8acc51a56d5d0ab6af06826461864460636d8bd5c546dab2a7b8')
    args = parser.parse_args()
    if args.analyze:
        retained = json.loads(args.analyze.read_text())
        if retained.get('failures') or retained.get('status') not in ('passed', 'smoke-only'):
            parser.error('cannot analyze failed/incomplete cohort')
        analysis = dict(report_sha256=sha(args.analyze), analysis_harness_sha256=sha(__file__), summary=summarize(retained['rows']), note='Separated-cluster detection uses publication.py. Bootstrap estimand remains difference of marginal medians, not cluster ranking or a causal estimate.')
        path = args.analyze.parent / 'analysis.json'
        path.write_text(json.dumps(analysis, indent=2) + '\n')
        print(json.dumps(analysis, indent=2))
        return
    if args.output is None:
        parser.error('--output required for measurements')
    if args.samples < 1 or args.warmups < 0:
        parser.error('samples must be positive; warmups nonnegative')
    args.source = args.source or {'torch-import': TORCH_SOURCE, 'numpy-script': NUMPY_SOURCE, 'ubuntu-shell': 'docker.io/library/ubuntu@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7'}[args.workload]
    if '@sha256:' not in args.source:
        parser.error('source must be pinned by digest')
    if args.supervisor_timeout <= 0 or args.registry_copy_timeout <= 0:
        parser.error('supervisor and registry copy timeouts must be positive')
    if args.namespace_child and not args.isolate_host:
        parser.error('namespace child requires isolation')
    if args.isolate_host and not args.namespace_child:
        return isolated_campaign(args)
    workload_argv, marker = workload(args.workload)
    output = args.output.resolve()
    if not args.namespace_child:
        output.mkdir(parents=True, exist_ok=False)
    global NAMESPACE_CHILD
    NAMESPACE_CHILD = args.namespace_child
    chroot = enter_campaign_namespace(output) if args.namespace_child else None
    firmware = args.firmware.resolve() if args.firmware else None
    binary = args.binary_dir.resolve() / 'pvisor'
    cache_binary = args.cache_binary.resolve() if args.cache_binary else args.binary_dir.resolve() / 'pvisor-cache'
    if chroot:
        binary = Path(chroot['binaries']['pvisor']['launch_path'])
        cache_binary = Path(chroot['binaries']['pvisor-cache']['launch_path'])
    report = dict(benchmark='B-LAZY-STARTUP', seed=SEED, workload=args.workload, workload_argv=workload_argv, marker=marker.decode(), source=args.source, registry_image=args.registry_image, samples=args.samples, warmups=args.warmups, rows=[], failures=[], preparation={}, protocol=dict(network='local loopback HTTPS registry / authenticated unencrypted TCP cache; no latency or bandwidth injection; warm host page cache. Count HTTP bodies versus cache response frames, excluding TLS/TCP/IP overhead', budget='2 CPUs (0,1) and 2 GiB; Docker hard limit versus pVisor guest RAM, not equivalent enclosing VM limit', cache='Docker image removed before each cold pair; cold validity requires complete registry blob responses. Lazy fresh XDG cache per pair, reused warm; new guest/stage/workspace each launch', service_preparation='Registry mirror and cache preparation separate. Cache imports identical digest from upstream Docker Hub because OCI client requires public-trusted HTTPS; not from insecure local registry.', binary_source_relationship='pre-existing release artifacts; build-time source relationship unverified; current source identity recorded separately', invalidation='any failed correctness, missing cold transfer, warm content transfer or timeout invalidates campaign; configured resource controls are not independently audited; no performance outliers dropped'))
    if chroot:
        report['provenance'] = json.loads((output / 'campaign-provenance.json').read_text())
        report['chroot'] = chroot
        report['lazy_env'] = CURRENT_LAZY_ENV
        report['accounting'] = 'content_bytes: Read Data only; metadata_bytes: Metadata Data only (Docker zero); response_bytes: HTTP response bodies for Docker, JSON frames plus all raw Data for cache; excludes transport overhead'
        report['protocol']['docker_daemon'] = 'local unix:///run/docker.sock; daemon/containerd not constrained to client two-CPU budget'
        report['protocol']['binary_source_relationship'] = report['provenance']['binary_source_relationship']
        report['protocol']['invalidation'] += '; namespace teardown and frozen bytes audited by outer parent'
    else:
        report['provenance'] = dict(binary_sha256=sha(binary), cache_binary_sha256=sha(cache_binary), harness_sha256=sha(__file__), firmware_sha256={p.name: sha(p) for p in firmware.iterdir() if p.is_file()} if firmware else 'embedded in hashed pvisor binary; separate bundle hash unavailable', kernel=os.uname().release, head=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip(), status=subprocess.check_output(['git', '--no-optional-locks', 'status', '--short'], cwd=ROOT).decode())
    if not chroot:
        shutil.copy2(__file__, output / 'harness.py')
        (output / 'source-dirty.patch').write_bytes(subprocess.check_output(['git', '--no-pager', 'diff', '--binary'], cwd=ROOT))
        files = subprocess.check_output(['git', 'ls-files', 'crates', 'vendor', 'Cargo.toml', 'Cargo.lock', '.cargo'], cwd=ROOT).decode().splitlines()
        untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', 'crates', 'vendor', '.cargo'], cwd=ROOT).decode().splitlines()
        (output / 'source-manifest.json').write_text(json.dumps({name: sha(ROOT / name) for name in sorted(set(files + untracked)) if (ROOT / name).is_file()}, indent=2))
    def save():
        (output / ('progress.json' if chroot else 'report.json')).write_text(json.dumps(report, indent=2) + '\n')
    save()
    registry_name = registry_name_for(output)
    registry_counts, cache_counts = Counts(), isolation_helpers().Counts()
    registry_proxy = cache_proxy = cache_process = None
    registry_started = False
    image = None
    service_log = None
    env = os.environ.copy()
    for name in list(env):
        if name.startswith('PVISOR_'):
            env.pop(name)
    if chroot:
        env.update(CURRENT_LAZY_ENV, HOME=str(output / 'private-home'),
                   XDG_RUNTIME_DIR='/tmp', TMPDIR='/tmp')
        (output / 'private-home').mkdir()
    env.update(PVISOR_CACHE_SERVER='tcp://127.0.0.1:15444', PVISOR_CACHE_TOKEN='local-benchmark-only', PVISOR_STARTUP_TIMING='0', PVISOR_FS_PROFILE='0')
    try:
        phase(output, 'preparation', registry_source='cached-oci' if args.registry_source_store else 'network-source')
        if args.registry_source_store:
            report['preparation']['registry_source'] = dict(mode='cached-oci', source_store=str(args.registry_source_store))
            save()
            phase(output, 'validate-and-archive-registry-oci', status='running')
            started = time.perf_counter()
            registry_source, receipt = archive_registry_oci(args.registry_source_store, output, args.source)
            report['preparation']['registry_oci_archive_ms'] = (time.perf_counter() - started) * 1000
            report['preparation']['registry_source'] = receipt
        else:
            registry_source = 'docker://' + args.source
            report['preparation']['registry_source'] = dict(mode='network-source', source=registry_source)
        report['protocol']['service_preparation'] = ('Registry publication from ' + report['preparation']['registry_source']['mode'] +
            ('; cache uses copied supported store and cached prepare' if args.prepared_store else
             '; separate full upstream cache preparation; cache OCI import requires public-trusted HTTPS'))
        save()
        if args.prepared_store:
            helpers = isolation_helpers()
            started = time.perf_counter()
            source_manifest = helpers.prepared_store_manifest(args.prepared_store.resolve())
            helpers.copy_prepared_store(args.prepared_store.resolve(), output / 'service-store')
            report['preparation'].update(mode='cached-service-store', store_copy_ms=(time.perf_counter() - started) * 1000)
            store_manifest = helpers.prepared_store_manifest(output / 'service-store')
            if not store_manifest or store_manifest != source_manifest:
                raise RuntimeError('empty or changed prepared store')
            helpers.save(output / 'prepared-store-manifest.json', store_manifest)
        else:
            report['preparation']['mode'] = 'full-upstream-prepare'
        # Fail on occupied ports, never stop or overwrite a listener.
        for port in (15000, 15443, 15444, 15445):
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', port))
        checked(docker('info'), output, 'docker-info')
        checked(docker('run', '-d', *(['--pull=never'] if args.registry_source_store else []), '--name', registry_name, '--cpuset-cpus', '2,3', '-p', '127.0.0.1:15000:5000', args.registry_image), output, 'registry-start')
        registry_started = True
        for _ in range(100):
            try:
                urllib.request.urlopen('http://127.0.0.1:15000/v2/', timeout=1).close()
                break
            except OSError:
                time.sleep(.1)
        else:
            raise RuntimeError('registry not ready')
        elapsed = checked_live(registry_copy_command(registry_source), output, 'registry-mirror',
                               timeout=args.registry_copy_timeout)
        report['preparation']['registry_mirror_ms'] = elapsed
        if args.registry_source_store:
            verify_registry_archive(output, receipt)
        save()
        manifest = urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:15000/v2/bench/workload/manifests/latest', headers={'Accept': 'application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json'}), timeout=10).read()
        (output / 'image-manifest.json').write_bytes(manifest)
        manifest_json = json.loads(manifest)
        digest = 'sha256:' + hashlib.sha256(manifest).hexdigest()
        if 'layers' not in manifest_json:
            raise RuntimeError('mirror did not select one architecture')
        if digest != args.source.split('@', 1)[1]:
            raise RuntimeError('registry manifest differs from pinned source digest')
        report['manifest_digest'] = digest
        report['compressed_layer_bytes'] = sum(layer['size'] for layer in manifest_json['layers'])
        report['blob_bytes'] = blob_bytes(manifest_json)
        image = '127.0.0.1:15443/bench/workload@' + digest
        checked(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(output / 'registry-key.pem'), '-out', str(output / 'registry-cert.pem'), '-days', '1', '-subj', '/CN=127.0.0.1', '-addext', 'subjectAltName=IP:127.0.0.1'], output, 'registry-certificate')
        os.chmod(output / 'registry-key.pem', 0o600)
        registry_proxy = http.server.ThreadingHTTPServer(('127.0.0.1', 15443), RegistryProxy)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(output / 'registry-cert.pem', output / 'registry-key.pem')
        registry_proxy.socket = tls.wrap_socket(registry_proxy.socket, server_side=True)
        registry_proxy.upstream, registry_proxy.counts = 15000, registry_counts
        serve(registry_proxy)
        cache_proxy = TCPServer(('127.0.0.1', 15444), CacheProxy)
        cache_proxy.upstream, cache_proxy.counts = 15445, cache_counts
        serve(cache_proxy)
        service_log = (output / 'cache-service.log').open('wb')
        cache_process = subprocess.Popen(['taskset', '-c', '2,3', str(cache_binary), 'serve', '--listen', 'tcp://127.0.0.1:15445', '--image-store', str(output / 'service-store')], env=env, stdout=service_log, stderr=subprocess.STDOUT, start_new_session=True)
        for _ in range(100):
            if cache_process.poll() is not None:
                raise RuntimeError('cache service exited')
            try:
                with socket.create_connection(('127.0.0.1', 15445), timeout=1):
                    break
            except OSError:
                time.sleep(.1)
        else:
            raise RuntimeError('cache service not ready')
        prepare_env = env | dict(XDG_CACHE_HOME=str(output / 'prepare-cache'))
        phase(output, 'cached-service-prepare' if args.prepared_store else 'upstream-cache-prepare', status='running')
        prepared, elapsed = checked([str(cache_binary), 'prepare', args.source], output, 'cache-prepare', env=prepare_env, timeout=90 if args.prepared_store else 900)
        report['preparation']['cached_service_prepare_ms' if args.prepared_store else 'upstream_cache_prepare_ms'] = elapsed
        validate_prepared_record(prepared, digest)
        preparation_counts = cache_counts.snapshot()
        (output / 'preparation-requests.json').write_text(json.dumps(preparation_counts, indent=2))
        if preparation_counts['errors']:
            raise RuntimeError('proxy error during preparation')
        report['prepared_handle'] = prepared.decode()
        checked(['lscpu'], output, 'lscpu')
        save()
        phase(output, 'client-campaign', status='running')
        rng = random.Random(SEED)
        for trial in range(-args.warmups - 1, args.samples):
            variants = ['docker', 'lazy']
            rng.shuffle(variants)
            for variant in variants:
                pair = output / f'trial-{trial}-{variant}'
                pair.mkdir()
                client_cache = pair / 'client-cache'
                if variant == 'docker':
                    removed = subprocess.run(docker('image', 'rm', image), capture_output=True)
                    (pair / 'image-removal.log').write_bytes(removed.stdout + removed.stderr)
                    inspect = subprocess.run(docker('image', 'inspect', image), capture_output=True)
                    if inspect.returncode == 0:
                        raise RuntimeError('Docker cold image remains present')
                for state in ('cold', 'warm'):
                    folder = pair / state
                    workspace = folder / 'workspace'
                    workspace.mkdir(parents=True)
                    local_env = env | dict(XDG_CACHE_HOME=str(client_cache), XDG_CONFIG_HOME=str(folder / 'config'), PVISOR_RUN_HOME=str(folder / 'runs'), PVISOR_IMAGE_STORE=str(folder / 'store'))
                    counts = registry_counts if variant == 'docker' else cache_counts
                    snapshot = counts.snapshot()
                    before = len(snapshot if variant == 'docker' else snapshot['rows'])
                    name = f'{registry_name}-{trial}-{state}'
                    if variant == 'docker':
                        argv = docker('run', '--rm', '--name', name, '--cpuset-cpus', '0,1', '--cpus', '2', '--memory', '2g', '--memory-swap', '2g', '--network', 'none', '--pull=' + ('missing' if state == 'cold' else 'never'), image, *workload_argv)
                    else:
                        argv = ['taskset', '-c', '0,1', str(binary), 'run', '--vm', '--rootfs', 'image=' + args.source, '--no-agent-defaults', '--overlaynet', 'off', '--stdio', 'inherit', '--stage', str(folder / 'stage'), '--cpu', '2', '--memory', '2048MiB', *(['--vm-library-dir', str(firmware)] if firmware else []), '--timeout', '90s', '--', *workload_argv]
                    try:
                        row = timed(argv, workspace, local_env, folder, marker=marker)
                    except Exception:
                        if variant == 'docker':
                            subprocess.run(docker('rm', '-f', name), capture_output=True, timeout=20)
                        raise
                    after = counts.snapshot()
                    if variant == 'lazy' and after['errors']:
                        raise RuntimeError('cache proxy error invalidates campaign')
                    requests = (after if variant == 'docker' else after['rows'])[before:]
                    (folder / 'requests.json').write_text(json.dumps(requests, indent=2))
                    if variant == 'docker':
                        content = sum(r['response_body_bytes'] for r in requests if '/blobs/' in r['path'] and r['status'] == 200)
                        response_bytes = sum(r['response_body_bytes'] for r in requests)
                        if state == 'cold' and content != report['blob_bytes']:
                            raise RuntimeError(f'cold Docker transfer mismatch: {content} != {report["blob_bytes"]}')
                        if state == 'warm' and requests:
                            raise RuntimeError('warm Docker fetched registry data')
                    else:
                        content = sum(r['content_bytes'] for r in requests)
                        response_bytes = sum(r['response_bytes'] for r in requests)
                        if state == 'cold' and content <= 0:
                            raise RuntimeError('cold lazy run fetched no image content')
                        if state == 'warm' and content != 0:
                            raise RuntimeError('warm lazy run fetched image content')
                        bundles = list((folder / 'stage').rglob('run-bundle.json'))
                        if len(bundles) != 1:
                            raise RuntimeError(f'expected one Run Bundle, got {bundles}')
                        from reference_baselines import validate_bundle_execution
                        validate_bundle_execution(json.loads(bundles[0].read_text()), 'pvisor-vm')
                    report['rows'].append(dict(variant=variant, cache=state, trial=trial, **row, content_bytes=content, metadata_bytes=sum(r['metadata_bytes'] for r in requests) if variant == 'lazy' else 0, response_bytes=response_bytes, requests=len(requests), correctness='passed'))
                    save()
            print(f'round {trial}: ' + ', '.join(f'{r["variant"]}/{r["cache"]}={r["ready_ms"]:.1f} ms' for r in report['rows'][-4:]), flush=True)
        if sha(binary) != report['provenance']['binary_sha256'] or sha(cache_binary) != report['provenance']['cache_binary_sha256']:
            raise RuntimeError('artifacts changed during campaign')
        if chroot:
            helpers = isolation_helpers()
            helpers.validate_launch_tree(Path('/'))
            helpers.verify_launch_binaries(chroot['binaries'])
        validate_campaign_rows(report['rows'], args.samples, args.warmups)
        if args.registry_source_store:
            verify_registry_archive(output, receipt)
        report['summary'] = summarize(report['rows'])
        report['status'] = 'passed' if args.samples >= 30 else 'smoke-only'
        save()
        print(json.dumps(report['summary'], indent=2), flush=True)
    except Exception as error:
        report['failures'].append(dict(error=repr(error), at=time.time()))
        report['status'] = 'failed'
        save()
        raise
    finally:
        if cache_process and cache_process.poll() is None:
            os.killpg(cache_process.pid, signal.SIGTERM)
            try:
                cache_process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(cache_process.pid, signal.SIGKILL)
                cache_process.wait()
        if service_log:
            service_log.close()
        for server in (cache_proxy, registry_proxy):
            if server:
                server.shutdown()
                server.server_close()
        if image:
            subprocess.run(docker('image', 'rm', image), capture_output=True, timeout=30)
        if registry_started:
            logs = subprocess.run(docker('logs', registry_name), capture_output=True, timeout=20)
            (output / 'registry.log').write_bytes(logs.stdout + logs.stderr)
            subprocess.run(docker('rm', '-f', '-v', registry_name), capture_output=True, timeout=30)
        try:
            final_counts = cache_counts.snapshot()
            (output / 'all-proxy-requests.json').write_text(json.dumps(final_counts, indent=2))
            if final_counts['errors']:
                raise RuntimeError('proxy errors at final drain')
        except Exception as error:
            report['status'] = 'failed'
            report['failures'].append(dict(error=repr(error)))
            report.pop('summary', None)
        if chroot:
            (output / 'child-result.json').write_text(json.dumps(report, indent=2) + '\n')
        else:
            save()
    return 0 if report['status'] in ('passed', 'smoke-only') and not report['failures'] else 1


if __name__ == '__main__':
    sys.exit(main())
