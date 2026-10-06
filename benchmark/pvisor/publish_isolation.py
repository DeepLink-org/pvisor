#!/usr/bin/env python3
"""Audit complete B-ISOLATION fixtures and publish a correctness matrix."""

import argparse
import json
import random
from pathlib import Path
from types import SimpleNamespace

from publication import write_csv
from publish_density import verify_harness
from reference_baselines import digest, verified_build_receipt
from v1.common import Context
from v1.oci import verify_prepared

BACKENDS = ('native', 'host', 'staged', 'safe', 'vm', 'container', 'podman')
POSITIVES = ('inside-read', 'inside-write', 'inside-socket')
NEGATIVES = ('absolute-read', 'symlink-read', 'proc-root-read',
             'traversal-read', 'absolute-write', 'symlink-write', 'unix-socket')
HOST_FIELDS = ('host-outside-mutated', 'host-lower-mutated', 'workspace-staged')


def validate_observations(backend, value, affinity):
    fields = (*POSITIVES, *NEGATIVES, 'lower-alias-write', *HOST_FIELDS)
    if any(type(value.get(key)) is not bool for key in fields):
        raise ValueError('missing or non-boolean isolation observation')
    if not all(value[key] for key in POSITIVES):
        raise ValueError('inside-view positive control failed')
    if value.get('cpu_affinity') != affinity:
        raise ValueError('payload CPU affinity differs')
    exposed = backend in ('native', 'host')
    if any(value[key] != exposed for key in NEGATIVES):
        raise ValueError('outside-view boundary or native/host control failed')
    if value['host-outside-mutated'] != exposed:
        raise ValueError('outside host mutation differs from boundary')
    staged = backend in ('staged', 'safe', 'vm')
    if value['workspace-staged'] != staged or staged and value['host-lower-mutated']:
        raise ValueError('workspace staging or alias protection failed')
    if exposed and not value['host-lower-mutated']:
        raise ValueError('native/host alias control failed')


def audit(path):
    path = Path(path).resolve()
    root = path.parent
    report = json.loads(path.read_text())
    args = report['cli_arguments']
    expected = {(backend, trial) for backend in BACKENDS for trial in range(3)}
    keys = [(row['backend'], row['trial']) for row in report['rows']]
    if (report.get('benchmark_id') != 'B-ISOLATION' or args['suites'] != 'isolation'
            or int(args['samples']) < 3 or args['warmups'] != 0
            or report['isolation_protocol']['repetitions'] != 3
            or report.get('prepared_inputs_unchanged') is not True
            or report['capabilities'] or len(keys) != len(set(keys)) or set(keys) != expected):
        raise ValueError('requires complete seven-mode, three-repetition isolation evidence')
    verify_harness(report, root)
    for name in ('v1/common.py', 'v1/isolation_worker.py'):
        if digest(root / 'harness' / name) != digest(Path(__file__).parent / name):
            raise ValueError('publication command or fixture contract differs from retained harness')
    receipt = verified_build_receipt(root / 'build-receipt.json', root / 'bin/pvisor')
    if receipt != report['binary_build'] or receipt['pvisor_sha256'] != report['binary_sha256']:
        raise ValueError('isolation binary/build identity differs')
    if digest(root / 'input-manifest.json') != report['input_manifest_sha256']:
        raise ValueError('prepared input receipt differs')
    manifest = json.loads((root / 'input-manifest.json').read_text())
    verify_prepared(Path(report['prepared_rootfs']), manifest)
    if (manifest['podman_image'] != report['podman_image']
            or digest('/usr/bin/python3') != manifest['host_python_sha256']
            or digest('/usr/bin/git') != manifest['host_git_sha256']
            or digest(root / 'firmware/libkrunfw.so.5') != report['firmware_sha256']):
        raise ValueError('tool, image or firmware identity differs')
    affinity = sorted(map(int, args['cpu_affinity'].split(',')))
    if len(affinity) != 2 or len(set(affinity)) != 2:
        raise ValueError('requires two distinct CPU affinities')
    ctx = object.__new__(Context)
    ctx.args = SimpleNamespace(vm_memory=args['vm_memory'])
    ctx.metadata = report
    ctx.binary = root / 'bin/pvisor'
    ctx.rootfs = Path(report['prepared_rootfs'])
    ctx.firmware = root / 'firmware'
    ctx.image = report['podman_image']
    ctx.podman_options = report['podman_options']
    sources = {}
    identities = set()
    rng = random.Random(int(args['seed']))
    ordered = []
    for trial in range(3):
        backends = list(BACKENDS)
        rng.shuffle(backends)
        ordered.extend((backend, trial) for backend in backends)
    directories = sorted((root / 'trials').iterdir())
    if len(directories) != 21:
        raise ValueError('missing or extra trial evidence')
    by_key = {(row['backend'], row['trial']): row for row in report['rows']}
    for ordinal, (directory, key) in enumerate(zip(directories, ordered), 1):
        backend, trial = key
        row = by_key[key]
        work, stage, outside = (directory / name for name in ('workspace', 'stage', 'outside'))
        if (directory.name != f'{ordinal:05d}-isolation-{backend}'
                or Path(row['logs']).resolve() != directory or row['correctness'] != 'passed'):
            raise ValueError('row/order does not match retained condition')
        commands = list((directory / 'commands').glob('*/command.json'))
        if len(commands) != 1:
            raise ValueError('missing unique retained command')
        details = json.loads(commands[0].read_text())
        argv = details['argv']
        if (details['cwd'] != str(work) or details['exit_code'] != 0
                or details.get('timed_out') or details['wall_ms'] != row['wall_ms']):
            raise ValueError('retained invocation differs from successful row')
        socket_path = argv[-2]
        if not socket_path.startswith('/tmp/pv-iso-') or not socket_path.endswith('/outside/socket'):
            raise ValueError('unexpected outside socket fixture')
        if backend == 'safe':
            mount = argv[argv.index('--mount') + 1]
            if not mount.endswith(':read') or not mount.startswith('/'):
                raise ValueError('safe toolchain mount is not read-only')
            ctx.toolchain = Path(mount[:-5])
        payload = ['/usr/bin/python3', 'worker.py', str(outside), str(work),
                   socket_path, '0,1' if backend == 'vm' else args['cpu_affinity']]
        expected_argv = ['taskset', '--cpu-list', args['cpu_affinity'],
                         *ctx.command(backend, work, stage, payload)]
        if argv != expected_argv:
            raise ValueError('invocation differs from declared isolation mode')
        retained = [commands[0], commands[0].parent / 'command.stdout',
                    commands[0].parent / 'command.stderr', work / 'worker.py',
                    work / 'inside', outside / 'secret']
        if digest(work / 'worker.py') != digest(root / 'harness/v1/isolation_worker.py'):
            raise ValueError('fixture worker changed')
        if (work / 'inside').read_bytes() != b'benchmark-inside' or (outside / 'secret').read_bytes() != b'benchmark-secret':
            raise ValueError('original fixture bytes changed')
        if backend in ('native', 'podman'):
            stdout = (commands[0].parent / 'command.stdout').read_text()
        else:
            bundles = list((directory / 'runs').glob('*/run-bundle.json'))
            if (stage / 'run-bundle.json').is_file():
                bundles.append(stage / 'run-bundle.json')
            if len(bundles) != 1:
                raise ValueError('missing unique Run Bundle')
            bundle = json.loads(bundles[0].read_text())
            run = bundle['run']
            isolation = 'virtual_machine' if backend == 'vm' else 'container' if backend == 'container' else 'rootless_process'
            identity = (run['run_id'], run['attempt_id'])
            if (not all(identity) or identity in identities or run['state'] != 'completed'
                    or run['exit_code'] != 0 or run['executor']['isolation'] != isolation
                    or row['observed_isolation'] != isolation or row['safety'] != bundle['safety']):
                raise ValueError('Run lacks independent completed isolation evidence')
            identities.add(identity)
            ctx.validate_bundle(backend, directory / 'runs', stage)
            stdout = run['output']['stdout']
            retained.append(bundles[0])
        value = json.loads(stdout.strip().splitlines()[-1])
        for name in ('written', 'symlink-written'):
            p = outside / name
            if p.exists():
                if p.read_bytes() != b'benchmark-write':
                    raise ValueError('outside write bytes differ')
                retained.append(p)
        value['host-outside-mutated'] = any((outside / name).exists() for name in ('written', 'symlink-written'))
        value['host-lower-mutated'] = (work / 'alias-written').exists()
        value['workspace-staged'] = not (work / 'staged-marker').exists()
        if value != row['observations']:
            raise ValueError('report observations differ from output/final host state')
        validate_observations(backend, value, [0, 1] if backend == 'vm' else affinity)
        marker = (stage / 'upper' if value['workspace-staged'] else work) / 'staged-marker'
        if marker.read_bytes() != b'benchmark-write':
            raise ValueError('workspace write missing or corrupted')
        retained.append(marker)
        if value['host-lower-mutated']:
            alias = work / 'alias-written'
            if alias.read_bytes() != b'benchmark-write':
                raise ValueError('host alias write bytes differ')
            retained.append(alias)
        for p in retained:
            if not p.resolve().is_relative_to(root):
                raise ValueError('retained evidence escapes cohort')
            sources[str(p.relative_to(root))] = digest(p)
    record = dict(state='passed', conditions=21, failed=0, report_sha256=digest(path),
                  script_sha256=digest(Path(__file__)), retained_evidence_sha256=sources,
                  scope='retained path/socket fixtures, positive controls, commands, Bundles and host/stage bytes; not an escape audit')
    (root / 'isolation-publication-audit.json').write_text(json.dumps(record, indent=2) + '\n')
    return report, receipt


def publish(path, output):
    path = Path(path).resolve()
    report, receipt = audit(path)
    identity = dict(cohort=path.parent.name, recorded_at=report['recorded_at'],
                    report_sha256=digest(path), binary_sha256=receipt['pvisor_sha256'],
                    source_manifest_sha256=receipt['source_manifest_sha256'],
                    audit_sha256=digest(path.parent / 'isolation-publication-audit.json'))
    rows = []
    for backend in BACKENDS:
        selected = [row for row in report['rows'] if row['backend'] == backend]
        for behavior in (*POSITIVES, *NEGATIVES, 'lower-alias-write', *HOST_FIELDS):
            rows.append(dict(backend=backend, behavior=behavior, planned=3, passed=3, failed=0,
                             observed_true=sum(row['observations'][behavior] for row in selected),
                             repetitions='three fresh correctness fixtures; no latency statistics', **identity))
    provenance = [dict(field=k, value=json.dumps(v, sort_keys=True) if isinstance(v, (dict, list)) else v)
                  for k, v in {**identity, 'platform': report['platform'], 'cpu': report['cpu'],
                               'cpu_affinity': report['cli_arguments']['cpu_affinity'],
                               'vm_memory': report['cli_arguments']['vm_memory'],
                               'host_memory_limit': 'no benchmark-specific cap; correctness only',
                               'input_manifest_sha256': report['input_manifest_sha256'],
                               'podman_image': report['podman_image'], 'protocol': report['isolation_protocol']}.items()]
    output.mkdir(parents=True, exist_ok=True)
    write_csv(output / 'isolation-tests.csv', rows)
    write_csv(output / 'isolation-provenance.csv', provenance)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    publish(args.report, args.output)
