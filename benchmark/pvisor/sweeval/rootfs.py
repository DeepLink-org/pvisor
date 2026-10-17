"""OCI identity helper reused from the previous trajectory rootfs preparer."""
import json
import os
import tarfile
from pathlib import PurePosixPath


def apply_layer_identities(identities, members):
    """Apply whiteouts to the previous layer in O(paths * path depth).

    A whiteout removes its target and descendants; an opaque directory removes
    only descendants. Apply all deletions before adding this layer's entries.
    """
    deleted, opaque = set(), set()
    for relative, *_ in members:
        if relative.name == '.wh..wh..opq':
            opaque.add(str(relative.parent))
        elif relative.name.startswith('.wh.'):
            deleted.add(str(relative.with_name(relative.name[4:])))
    removed_subtrees = deleted | opaque
    if removed_subtrees:
        for old in list(identities):
            relative = PurePosixPath(old)
            ancestors = {str(parent) for parent in relative.parents}
            if old in deleted or ancestors.intersection(removed_subtrees):
                identities.pop(old)
    for relative, uid, gid, mode, directory in members:
        if relative.name.startswith('.wh.'):
            continue
        name = str(relative)
        if not directory and identities.get(name, (0, 0, 0, False))[3]:
            prefix = name + '/'
            for old in list(identities):
                if old.startswith(prefix):
                    identities.pop(old)
        identities[name] = (uid, gid, mode, directory)


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
        apply_layer_identities(identities, members)
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
