#!/usr/bin/env python3
"""Benchmark: B-LAZY-STARTUP (benchmark/README.md#b-lazy-startup), role user-facing.
Motivation: quantify disposable shell or Python numerical environment startup with lazy client reads.
Conclusion sought: cold/warm ready, completion and transferred response bytes;
service preparation is separate, not a claim of registry-free cold startup.
Design: pinned amd64 manifest, Distribution registry and real pvisor-cache TCP,
loopback with no injected latency, 2 CPUs / 2 GiB, shuffled paired rounds.
Failures are retained and invalidate the campaign; no slow-sample exclusion.
"""
import argparse
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import random
import selectors
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


def docker(*args):
    return ['sg', 'docker', '-c', shlex.join(['docker', *map(str, args)])]


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
    def handle(self):
        counts = self.server.counts
        with counts.lock:
            counts.active += 1
        upstream = None
        data = bytearray()
        try:
            upstream = socket.create_connection(('127.0.0.1', self.server.upstream), timeout=900)
            self.request.settimeout(900)
            prefix = exact(self.request, 4)
            request = exact(self.request, struct.unpack('!I', prefix)[0])
            upstream.sendall(prefix + request)
            while block := upstream.recv(65536):
                self.request.sendall(block)
                data.extend(block)
            header_size = struct.unpack('!I', data[:4])[0]
            response = json.loads(data[4:4 + header_size])
            request = json.loads(request)['request']
            with counts.lock:
                counts.rows.append(dict(op=request['op'], response=response['status'], response_wire_bytes=len(data), content_bytes=len(data) - 4 - header_size))
        finally:
            if upstream:
                upstream.close()
            with counts.lock:
                counts.active -= 1


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
            result[f'{variant}-{state}'] = dict(n=len(selected), **{field: distribution([r[field] for r in selected]) for field in ('ready_ms', 'completion_ms', 'response_bytes', 'content_bytes')})
        rng = random.Random(SEED)
        paired = list(zip(groups['docker'], groups['lazy'], strict=True))
        differences = []
        for _ in range(5000):
            sample = rng.choices(paired, k=len(paired))
            differences.append(statistics.median([a['ready_ms'] for a, b in sample]) - statistics.median([b['ready_ms'] for a, b in sample]))
        result[f'docker-minus-lazy-{state}'] = dict(ready_median_difference_ms=statistics.median([a['ready_ms'] for a, b in paired]) - statistics.median([b['ready_ms'] for a, b in paired]), ci95_ms=[percentile(differences, .025), percentile(differences, .975)])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
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
    workload_argv, marker = workload(args.workload)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    firmware = args.firmware.resolve() if args.firmware else None
    binary = args.binary_dir.resolve() / 'pvisor'
    cache_binary = args.cache_binary.resolve() if args.cache_binary else args.binary_dir.resolve() / 'pvisor-cache'
    report = dict(benchmark='B-LAZY-STARTUP', seed=SEED, workload=args.workload, workload_argv=workload_argv, marker=marker.decode(), source=args.source, registry_image=args.registry_image, samples=args.samples, warmups=args.warmups, rows=[], failures=[], preparation={}, protocol=dict(network='local loopback HTTPS registry / authenticated unencrypted TCP cache; no latency or bandwidth injection; warm host page cache. Count HTTP bodies versus cache response frames, excluding TLS/TCP/IP overhead', budget='2 CPUs (0,1) and 2 GiB; Docker hard limit versus pVisor guest RAM, not equivalent enclosing VM limit', cache='Docker image removed before each cold pair; cold validity requires complete registry blob responses. Lazy fresh XDG cache per pair, reused warm; new guest/stage/workspace each launch', service_preparation='Registry mirror and cache preparation separate. Cache imports identical digest from upstream Docker Hub because OCI client requires public-trusted HTTPS; not from insecure local registry.', binary_source_relationship='pre-existing release artifacts; build-time source relationship unverified; current source identity recorded separately', invalidation='any failed correctness, missing cold transfer, warm content transfer or timeout invalidates campaign; configured resource controls are not independently audited; no performance outliers dropped'))
    report['provenance'] = dict(binary_sha256=sha(binary), cache_binary_sha256=sha(cache_binary), harness_sha256=sha(__file__), firmware_sha256={p.name: sha(p) for p in firmware.iterdir() if p.is_file()} if firmware else 'embedded in hashed pvisor binary; separate bundle hash unavailable', kernel=os.uname().release, head=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip(), status=subprocess.check_output(['git', '--no-optional-locks', 'status', '--short'], cwd=ROOT).decode())
    shutil.copy2(__file__, output / 'harness.py')
    (output / 'source-dirty.patch').write_bytes(subprocess.check_output(['git', '--no-pager', 'diff', '--binary'], cwd=ROOT))
    files = subprocess.check_output(['git', 'ls-files', 'crates', 'vendor', 'Cargo.toml', 'Cargo.lock', '.cargo'], cwd=ROOT).decode().splitlines()
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', 'crates', 'vendor', '.cargo'], cwd=ROOT).decode().splitlines()
    (output / 'source-manifest.json').write_text(json.dumps({name: sha(ROOT / name) for name in sorted(set(files + untracked)) if (ROOT / name).is_file()}, indent=2))
    def save():
        (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    registry_name = 'pvisor-lazy-bench-' + str(os.getpid())
    registry_counts, cache_counts = Counts(), Counts()
    registry_proxy = cache_proxy = cache_process = None
    registry_started = False
    image = None
    service_log = None
    env = os.environ.copy()
    for name in list(env):
        if name.startswith('PVISOR_'):
            env.pop(name)
    env.update(PVISOR_CACHE_SERVER='tcp://127.0.0.1:15444', PVISOR_CACHE_TOKEN='local-benchmark-only', PVISOR_STARTUP_TIMING='0', PVISOR_FS_PROFILE='0')
    try:
        checked(docker('info'), output, 'docker-info')
        checked(docker('run', '-d', '--name', registry_name, '--cpuset-cpus', '2,3', '-p', '127.0.0.1:15000:5000', args.registry_image), output, 'registry-start')
        registry_started = True
        for _ in range(100):
            try:
                urllib.request.urlopen('http://127.0.0.1:15000/v2/', timeout=1).close()
                break
            except OSError:
                time.sleep(.1)
        else:
            raise RuntimeError('registry not ready')
        _, elapsed = checked(['skopeo', 'copy', '--override-arch', 'amd64', '--preserve-digests', '--dest-tls-verify=false', 'docker://' + args.source, 'docker://127.0.0.1:15000/bench/workload:latest'], output, 'registry-mirror', timeout=900)
        report['preparation']['registry_mirror_ms'] = elapsed
        manifest = urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:15000/v2/bench/workload/manifests/latest', headers={'Accept': 'application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json'}), timeout=10).read()
        (output / 'image-manifest.json').write_bytes(manifest)
        manifest_json = json.loads(manifest)
        digest = 'sha256:' + hashlib.sha256(manifest).hexdigest()
        if 'layers' not in manifest_json:
            raise RuntimeError('mirror did not select one architecture')
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
        prepare_env = env | dict(XDG_CACHE_HOME=str(output / 'prepare-cache'))
        prepared, elapsed = checked([str(cache_binary), 'prepare', args.source], output, 'cache-prepare', env=prepare_env, timeout=900)
        report['preparation']['upstream_cache_prepare_ms'] = elapsed
        if digest.encode() not in prepared:
            raise RuntimeError(f'cache/registry manifest mismatch: {prepared!r} != {digest}')
        report['prepared_handle'] = prepared.decode()
        checked(['lscpu'], output, 'lscpu')
        save()
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
                    before = len(counts.snapshot())
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
                    requests = counts.snapshot()[before:]
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
                        response_bytes = sum(r['response_wire_bytes'] for r in requests)
                        if state == 'cold' and content <= 0:
                            raise RuntimeError('cold lazy run fetched no image content')
                        if state == 'warm' and content != 0:
                            raise RuntimeError('warm lazy run fetched image content')
                        bundles = list((folder / 'stage').rglob('run-bundle.json'))
                        if len(bundles) != 1:
                            raise RuntimeError(f'expected one Run Bundle, got {bundles}')
                        from reference_baselines import validate_bundle_execution
                        validate_bundle_execution(json.loads(bundles[0].read_text()), 'pvisor-vm')
                    report['rows'].append(dict(variant=variant, cache=state, trial=trial, **row, content_bytes=content, response_bytes=response_bytes, requests=len(requests), correctness='passed'))
                    save()
            print(f'round {trial}: ' + ', '.join(f'{r["variant"]}/{r["cache"]}={r["ready_ms"]:.1f} ms' for r in report['rows'][-4:]), flush=True)
        if sha(binary) != report['provenance']['binary_sha256'] or sha(cache_binary) != report['provenance']['cache_binary_sha256']:
            raise RuntimeError('artifacts changed during campaign')
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


if __name__ == '__main__':
    main()
