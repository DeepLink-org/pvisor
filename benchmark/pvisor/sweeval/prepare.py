"""Prepare pinned OCI rootfs inputs; every attempt gets a new evidence directory."""
import argparse
import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

from config import load_config
from rootfs import restore_virtual_identity


def command(argv, directory, name):
    try:
        result = subprocess.run(argv, capture_output=True, timeout=1800)
    except subprocess.TimeoutExpired as exc:
        (directory / (name + '.stdout')).write_bytes(exc.stdout or b'')
        (directory / (name + '.stderr')).write_bytes(exc.stderr or b'')
        raise
    (directory / (name + '.stdout')).write_bytes(result.stdout)
    (directory / (name + '.stderr')).write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(name + ' failed; inspect retained stderr')
    return result.stdout


def pin_image(image, digest):
    """Replace a tag/digest without removing a registry's port."""
    name = image.split('@', 1)[0]
    if ':' in name.rsplit('/', 1)[-1]:
        name = name.rsplit(':', 1)[0]
    return name + '@' + digest


def prepare(config, entry, attempt):
    from sweeval.task.task import Task

    task = Task(Path(config['task_dir']) / entry['directory'])
    source_image = task.config.environment.docker_image
    image = entry.get('image', source_image)
    record = json.loads(command(['skopeo', 'inspect', '--override-arch', 'amd64',
                                 '--override-os', 'linux', 'docker://' + image], attempt, 'inspect'))
    pinned = pin_image(image, record['Digest'])
    layout = attempt / 'oci'
    command(['skopeo', 'copy', '--override-arch', 'amd64', '--override-os', 'linux',
             'docker://' + pinned, 'oci:' + str(layout) + ':task'], attempt, 'copy')
    bundle = attempt / 'bundle'
    command([config['umoci'], 'unpack', '--rootless', '--image', str(layout) + ':task',
             str(bundle)], attempt, 'unpack')
    identities = restore_virtual_identity(layout, bundle / 'rootfs')
    desc, = json.loads((layout / 'index.json').read_text())['manifests']
    blobs = layout / 'blobs/sha256'
    manifest = json.loads((blobs / desc['digest'].split(':')[1]).read_text())
    image_config = json.loads((blobs / manifest['config']['digest'].split(':')[1]).read_text())['config']
    result = dict(source_image=source_image, resolved_image=image, image_digest=record['Digest'], rootfs=str(bundle / 'rootfs'),
                  workdir=entry.get('workdir') or image_config.get('WorkingDir') or '/',
                  environment={**dict(v.split('=', 1) for v in (image_config.get('Env') or [])),
                               **entry.get('environment', {})},
                  python=entry.get('python', '/usr/bin/python3'), identities=identities,
                  base_commit=entry.get('base_commit'),
                  preparer_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    target = Path(entry['prepared'])
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('x') as stream:
        json.dump(result, stream, indent=2)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--config', type=Path, required=True)
    args = parser.parse_args()
    config = load_config(args.config)
    failures = []
    for entry in config['tasks']:
        if Path(entry['prepared']).exists():
            print(entry['name'] + ': already prepared')
            continue
        attempt = Path(config['inputs']) / (entry['name'] + '-' + str(time.time_ns()))
        if not attempt.resolve().is_relative_to('/data'):
            raise ValueError('preparation data must be under /data')
        attempt.mkdir(parents=True, mode=0o700)
        try:
            prepare(config, entry, attempt)
        except Exception as exc:
            failure = dict(task=entry['name'], error_type=type(exc).__name__, error=str(exc))
            (attempt / 'failure.json').write_text(json.dumps(failure))
            failures.append(failure)
            print(json.dumps(failure))
    if failures:
        raise SystemExit(1)


if __name__ == '__main__':
    os.umask(0o077)
    main()
