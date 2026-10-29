"""Independent complete extraction audit after the usable/exit timestamps, never before boot."""

import hashlib
import os
import stat
import tarfile
from pathlib import Path


def audit(layer, root):
    files = links = directories = 0
    with tarfile.open(layer, "r:") as archive:
        for member in archive:
            rel = Path(member.name)
            assert not rel.is_absolute() and ".." not in rel.parts
            p = root / rel
            s = p.lstat()
            if member.issym():
                assert p.is_symlink() and os.readlink(p) == member.linkname, member.name
                assert (s.st_uid, s.st_gid) == (member.uid, member.gid), (
                    "Symlink ownership mismatch",
                    member.name,
                )
                links += 1
                continue
            override = os.getxattr(p, "user.containers.override_stat").decode().split(":")
            assert (int(override[0]), int(override[1]), stat.S_IMODE(int(override[2], 8))) == (
                member.uid,
                member.gid,
                member.mode,
            ), member.name
            for key, value in member.pax_headers.items():
                if key.startswith("SCHILY.xattr."):
                    assert os.getxattr(p, key[len("SCHILY.xattr.") :]) == value.encode(
                        "utf-8", "surrogateescape"
                    ), member.name
            if member.isdir():
                assert stat.S_ISDIR(s.st_mode)
                directories += 1
            elif member.islnk():
                q = root / member.linkname
                assert (s.st_dev, s.st_ino) == (q.stat().st_dev, q.stat().st_ino)
                links += 1
            else:
                assert member.isfile() and stat.S_ISREG(s.st_mode) and s.st_size == member.size
                with archive.extractfile(member) as a, p.open("rb") as b:
                    assert (
                        hashlib.file_digest(a, "sha256").hexdigest()
                        == hashlib.file_digest(b, "sha256").hexdigest()
                    ), member.name
                files += 1
    return dict(
        passed=True,
        files=files,
        links=links,
        directories=directories,
        payloads_verified=True,
        symlink_owner_actual_match=True,
        excluded_from_primary_external_validation=True,
    )


if __name__ == "__main__":
    import argparse
    import json

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("layer", type=Path)
    parser.add_argument("rootfs", type=Path)
    args = parser.parse_args()
    print(json.dumps(audit(args.layer, args.rootfs), indent=2))
