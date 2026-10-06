"""Input inventories for B-STARTUP/B-FS-TOOLS/B-AGENT-TASK helpers.

No measurements are performed here. Hashing belongs before formal warmups,
outside task timers. Legacy arrays have no directory metadata; new arrays
include it. Neither scope proves that a separately loaded OCI image has the
same contents: its immutable ID/import and live capability checks are separate.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat


ASSETS = ('vmlinux', 'bzImage', 'kernel.config', 'agent-env.ext4',
          'agent-env.tar', 'tool-identities.json', 'assets.json')
REQUIRED = {*ASSETS, 'rootfs/bench/reference_workload.py', 'rootfs/bench/affinity'}


def relative_path(value):
    if not isinstance(value, str):
        raise ValueError('input path is not a string')
    path = PurePosixPath(value)
    if path.is_absolute() or '..' in path.parts or str(path) != value or value == '.':
        raise ValueError('input path is not canonical and relative')
    return path


def identity(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
            info.st_ctime_ns, info.st_mode)


def fingerprint(path, before):
    sha = hashlib.sha256()
    with path.open('rb') as stream:
        if identity(os.fstat(stream.fileno())) != identity(before):
            raise ValueError(f'input changed before opening: {path}')
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            sha.update(block)
        if identity(os.fstat(stream.fileno())) != identity(before):
            raise ValueError(f'input changed during hashing: {path}')
    if identity(path.lstat()) != identity(before):
        raise ValueError(f'input replaced during hashing: {path}')
    return sha.hexdigest()


def inventory(root, directories):
    base = root / 'rootfs'
    if base.is_symlink() or not base.is_dir():
        raise ValueError('reference rootfs must be an actual directory')
    nodes = set()
    for path in [base, *base.rglob('*')]:
        if path.is_symlink() or not path.is_dir() or directories:
            nodes.add(path.relative_to(root).as_posix())
    return nodes | set(ASSETS)


def create_reference_manifest(root):
    root = Path(root).resolve()
    destination = root / 'input-manifest.json'
    if destination.exists():
        raise ValueError('do not replace an existing frozen input manifest')
    entries = []
    for relative in sorted(inventory(root, directories=True)):
        path = root / relative
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            entry = dict(path=relative, symlink=os.readlink(path))
        elif stat.S_ISDIR(info.st_mode):
            entry = dict(path=relative, directory=True, mode=stat.S_IMODE(info.st_mode))
        elif stat.S_ISREG(info.st_mode):
            entry = dict(path=relative, bytes=info.st_size, mode=stat.S_IMODE(info.st_mode),
                         sha256=fingerprint(path, info))
        else:
            raise ValueError(f'unsupported input node: {relative}')
        entries.append(entry)
    destination.write_text(json.dumps(entries, indent=2) + '\n')
    return destination


def verify_reference_inputs(root):
    root = Path(root).resolve()
    manifest = root / 'input-manifest.json'
    raw = manifest.read_bytes()
    entries = json.loads(raw)
    if not isinstance(entries, list) or not entries:
        raise ValueError('reference input manifest must be a nonempty array')
    known = set()
    directories = False
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError('invalid input manifest entry')
        relative = relative_path(entry.get('path')).as_posix()
        if relative in known:
            raise ValueError('duplicate input path')
        if relative not in ASSETS and not (relative == 'rootfs' or relative.startswith('rootfs/')):
            raise ValueError('input entry outside the declared environment')
        known.add(relative)
        directories |= entry.get('directory') is True
    if not REQUIRED <= known:
        raise ValueError('reference manifest is missing required workload/assets')
    actual = inventory(root, directories)
    if actual != known:
        raise ValueError(f'input inventory differs: missing={sorted(known-actual)[:5]}, extra={sorted(actual-known)[:5]}')
    counts = dict(files=0, symlinks=0, directories=0, bytes=0)
    observed_identities = {}
    for entry in entries:
        relative = entry['path']
        path = root / relative
        if not path.parent.resolve().is_relative_to(root):
            raise ValueError('input parent resolved outside assets')
        info = path.lstat()
        observed_identities[path] = identity(info)
        keys = set(entry)
        if keys == {'path', 'symlink'}:
            if not stat.S_ISLNK(info.st_mode) or os.readlink(path) != entry['symlink']:
                raise ValueError(f'input symlink differs: {relative}')
            counts['symlinks'] += 1
        elif keys == {'path', 'directory', 'mode'} and entry['directory'] is True:
            if (type(entry['mode']) is not int or not stat.S_ISDIR(info.st_mode)
                    or stat.S_IMODE(info.st_mode) != entry['mode']):
                raise ValueError(f'input directory differs: {relative}')
            counts['directories'] += 1
        elif keys in ({'path', 'bytes', 'mode', 'sha256'}, {'path', 'bytes', 'sha256'}):
            if not stat.S_ISREG(info.st_mode) or type(entry['bytes']) is not int or info.st_size != entry['bytes']:
                raise ValueError(f'input file type/size differs: {relative}')
            if relative.startswith('rootfs/') and 'mode' not in entry:
                raise ValueError('rootfs file is missing its permission identity')
            if 'mode' in entry and (type(entry['mode']) is not int or stat.S_IMODE(info.st_mode) != entry['mode']):
                raise ValueError(f'input file permissions differ: {relative}')
            if fingerprint(path, info) != entry['sha256']:
                raise ValueError(f'input file content differs: {relative}')
            counts['files'] += 1
            counts['bytes'] += info.st_size
        else:
            raise ValueError('unsupported or incomplete input manifest entry')
    if inventory(root, directories) != known or manifest.read_bytes() != raw:
        raise ValueError('input inventory/manifest changed during verification')
    if any(identity(path.lstat()) != observed for path, observed in observed_identities.items()):
        raise ValueError('input identity changed after its individual hash/check')
    return dict(input_manifest_sha256=hashlib.sha256(raw).hexdigest(), **counts,
                directory_metadata_coverage='complete' if directories else 'not recorded by legacy manifest',
                scope=('complete rootfs inventory including directory modes' if directories else
                       'complete non-directory rootfs inventory; directory metadata not recorded') +
                      '; declared asset bytes/modes/symlinks; OCI image and host interpreter verification are separate')
