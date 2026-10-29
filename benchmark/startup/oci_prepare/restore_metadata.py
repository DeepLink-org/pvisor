"""Reference second-pass metadata restoration for the canonical experiment input."""

import os
import stat
import tarfile
from pathlib import Path


def apply_metadata(layer, root):
    count = 0
    with tarfile.open(layer, "r:") as archive:
        for m in archive:
            rel = Path(m.name)
            assert not rel.is_absolute() and ".." not in rel.parts, m.name
            p = root / rel
            # Do not follow symlinks. pVisor's existing lowerfs has no symlink xattr virtualization.
            if m.issym():
                assert p.is_symlink() and m.uid == os.getuid(), m.name
                os.chown(p, m.uid, m.gid, follow_symlinks=False)
                assert (p.lstat().st_uid, p.lstat().st_gid) == (m.uid, m.gid), m.name
                continue
            if not (m.isdir() or m.isfile() or m.islnk()):
                raise AssertionError(("Unsupported canonical OCI member", m.name, m.type))
            assert not p.is_symlink() and p.exists(), m.name
            kind = stat.S_IFDIR if m.isdir() else stat.S_IFREG
            mode = kind | m.mode
            # Host write access is required before user xattrs on read-only OCI entries.
            os.chmod(p, m.mode | (0o700 if m.isdir() else 0o600), follow_symlinks=False)
            os.setxattr(
                p,
                "user.containers.override_stat",
                f"{m.uid}:{m.gid}:{mode:o}".encode(),
                follow_symlinks=False,
            )
            # umoci deliberately ignores archived user.rootlesscontainers metadata.
            # Restore every archived xattr, keeping the existing exact audit unchanged.
            for key, value in m.pax_headers.items():
                if key.startswith("SCHILY.xattr."):
                    attr = key[len("SCHILY.xattr.") :]
                    assert attr != "user.containers.override_stat"
                    os.setxattr(
                        p, attr, value.encode("utf8", "surrogateescape"), follow_symlinks=False
                    )
            count += 1
    return count


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("layer", type=Path)
    parser.add_argument("rootfs", type=Path)
    args = parser.parse_args()
    print(apply_metadata(args.layer, args.rootfs))
