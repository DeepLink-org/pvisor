#!/usr/bin/env python3
"""Prepare an offline OCI layout and command-only guest input for B-TRAJECTORY-ENG."""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tarfile
from pathlib import Path, PurePosixPath


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def inventory(root):
    rows = []
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in sorted(dirs + files):
            path = Path(parent) / name
            stat = path.lstat()
            row = dict(path=str(path.relative_to(root)), mode=stat.st_mode,
                       uid=stat.st_uid, gid=stat.st_gid)
            if not path.is_symlink():
                try:
                    row['virtual_identity'] = os.getxattr(path, 'user.containers.override_stat').decode()
                except OSError:
                    pass
            if path.is_symlink():
                row['link'] = os.readlink(path)
            elif path.is_file():
                row.update(size=stat.st_size, sha256=digest(path))
            rows.append(row)
    return sorted(rows, key=lambda row: row['path'])


def restore_virtual_identity(layout, rootfs):
    """Decorate umoci's bytes with OCI identities understood by pVisor.

    Rootless umoci intentionally maps owners to the calling uid. Do not extract
    any tar content here: umoci remains the authority for links/whiteouts/data.
    Replay only metadata deletions/updates and apply to surviving native entries.
    """
    descriptor, = json.loads((layout / 'index.json').read_text())['manifests']
    blobs = layout / 'blobs/sha256'
    manifest = json.loads((blobs / descriptor['digest'].split(':')[1]).read_text())
    identities = {}
    for layer in manifest['layers']:
        members = []
        with tarfile.open(blobs / layer['digest'].split(':')[1], mode='r|*') as archive:
            for member in archive:
                relative = PurePosixPath(member.name)
                if relative.is_absolute() or '..' in relative.parts:
                    raise ValueError('invalid OCI metadata path')
                members.append((relative, member.uid, member.gid, member.mode, member.isdir()))
        # Whiteouts affect lower layers, regardless of ordering inside this tar.
        for relative, *_ in members:
            if not relative.name.startswith('.wh.'):
                continue
            opaque = relative.name == '.wh..wh..opq'
            victim = relative.parent if opaque else relative.with_name(relative.name[4:])
            for old in list(identities):
                if PurePosixPath(old).is_relative_to(victim) and (not opaque or old != str(victim)):
                    identities.pop(old)
        for relative, uid, gid, mode, directory in members:
            if relative.name.startswith('.wh.'):
                continue
            name = str(relative)
            if not directory and identities.get(name, (0, 0, 0, False))[3]:
                for old in list(identities):
                    if old != name and PurePosixPath(old).is_relative_to(relative):
                        identities.pop(old)
            identities[name] = (uid, gid, mode, directory)
    applied, symlinks = 0, 0
    for name, (uid, gid, mode, _) in identities.items():
        path = rootfs / name
        if path.is_symlink():
            symlinks += 1  # Linux disallows user xattrs on symlinks; targets retain identity.
            continue
        if not path.exists():
            continue
        if not path.resolve().is_relative_to(rootfs.resolve()):
            raise ValueError('OCI identity escapes rootfs')
        physical_mode = path.stat().st_mode & 0o7777
        try:
            if not physical_mode & 0o200:
                path.chmod(physical_mode | 0o200)
            os.setxattr(path, 'user.containers.override_stat', ('%d:%d:%o' % (uid, gid, mode)).encode())
        finally:
            if not physical_mode & 0o200:
                path.chmod(physical_mode)
        applied += 1
    os.setxattr(rootfs, 'user.containers.override_stat', b'0:0:755')
    return dict(applied=applied, symlink_owners_not_virtualized=symlinks,
                source='verified OCI metadata; bytes/links/whiteouts extracted only by umoci')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sample', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--umoci', type=Path, required=True)
    args = parser.parse_args()
    sample, output = args.sample.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    audit = json.loads((sample / 'audit.json').read_text())
    fixture_path = Path(__file__).parent / 'fixtures/sympy_15599_replay.json'
    fixture = json.loads(fixture_path.read_text())
    if audit['instance_id'] != fixture['instance_id'] or audit['base_commit'] != fixture['base_commit']:
        raise ValueError('unexpected trajectory task')
    if digest(sample / 'steps.jsonl') != fixture['steps_sha256'] or digest(sample / 'recorded.patch') != fixture['recorded_patch_sha256']:
        raise ValueError('fixed fixture input mismatch')
    for name, expected in audit['checksums'].items():
        if digest(sample / name) != expected:
            raise ValueError('sample checksum mismatch: ' + name)
    layout = sample / 'environment/image.oci'
    index = json.loads((layout / 'index.json').read_text())
    descriptor, = index['manifests']
    expected_digest = 'sha256:068c2c597f9dffc4344f276d9a9d1e0557b9e32b15b8ff4c4ffba76995ce6853'
    if descriptor['digest'] != expected_digest:
        raise ValueError('unexpected OCI digest')
    for blob in (layout / 'blobs/sha256').iterdir():
        if digest(blob) != blob.name:
            raise ValueError('OCI blob checksum mismatch: ' + str(blob))
    tag = descriptor['annotations']['org.opencontainers.image.ref.name']
    unpack_command = [str(args.umoci.resolve()), 'unpack', '--rootless', '--image',
               str(layout) + ':' + tag, str(output / 'bundle')]
    with (output / 'unpack.log').open('wb') as log:
        subprocess.run(unpack_command, stdout=log, stderr=subprocess.STDOUT, check=True)
    rootfs = output / 'bundle/rootfs'
    identity_receipt = restore_virtual_identity(layout, rootfs)
    for name in ('replay-input', 'replay-output', 'replay-control'):
        (rootfs / name).mkdir()
    steps = [json.loads(line) for line in (sample / 'steps.jsonl').read_text().splitlines()]
    if [step['step'] for step in steps] != list(range(1, 45)):
        raise ValueError('expected 44 ordered commands')
    commands = []
    for step in steps:
        command = step['arguments']['command']
        subprocess.run(['/bin/bash', '-n', '-c', command], check=True)
        commands.append(dict(step=step['step'], tool_call_id=step['tool_call_id'],
                             command=command, terminal=step['step'] == 44,
                             command_sha256=hashlib.sha256(command.encode()).hexdigest()))
    input_dir = rootfs / 'replay-input'
    (input_dir / 'commands.jsonl').write_text(''.join(json.dumps(row) + '\n' for row in commands))
    if digest(input_dir / 'commands.jsonl') != fixture['commands_sha256']:
        raise ValueError('command-only fixture mismatch')
    worker = Path(__file__).parent / 'fixtures/trajectory_worker.py'
    shutil.copyfile(worker, input_dir / 'trajectory_worker.py')
    image_env = dict(value.split('=', 1) for value in json.loads(
        (sample / 'environment/image-config.json').read_text())['config']['Env'])
    image_env.update(json.loads((sample / 'agent-config.original.json').read_text())['environment']['env'])
    image_env['HOME'] = '/root'
    receipt = dict(schema='pvisor-trajectory-prepared/v1', image_digest=expected_digest,
                   rootfs=str(rootfs), sample=str(sample), environment=image_env,
                   input_hashes=audit['checksums'], commands_sha256=digest(input_dir / 'commands.jsonl'),
                   worker_sha256=digest(worker), umoci_sha256=digest(args.umoci),
                   fixture_sha256=digest(fixture_path), preparer_sha256=digest(__file__),
                   unpack_command=unpack_command,
                   ownership=identity_receipt)
    # No chmod of the image tree: preserving its original modes matters to fidelity.
    (output / 'rootfs-inventory.json').write_text(json.dumps(inventory(rootfs), indent=2) + '\n')
    receipt['inventory_sha256'] = digest(output / 'rootfs-inventory.json')
    (output / 'prepared.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt))


if __name__ == '__main__':
    main()
