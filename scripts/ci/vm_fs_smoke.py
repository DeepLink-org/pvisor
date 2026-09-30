#!/usr/bin/env python3
"""Run inside a Linux pVisor VM: python3 scripts/ci/vm_fs_smoke.py [directory ...]."""

import errno
import fcntl
import io
import mmap
import os
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import tarfile
import tempfile
import traceback
from pathlib import Path


def expect_errno(code, operation):
    try:
        operation()
    except OSError as error:
        assert error.errno == code, (error.errno, code, str(error))
    else:
        raise AssertionError(f"expected errno {code}")


def read_write(p):
    f = p / "file"
    f.write_bytes(b"hello")
    with f.open("ab") as stream:
        stream.write(b" world")
        stream.flush()
        os.fsync(stream.fileno())
    assert f.read_bytes() == b"hello world"
    with f.open("r+b") as stream:
        stream.seek(6)
        stream.write(b"VM")
        stream.truncate(8)
    assert f.read_bytes() == b"hello VM"


def sparse_mmap(p):
    with (p / "file").open("w+b", buffering=0) as stream:
        stream.truncate(1024 * 1024)
        assert stream.read(4096) == bytes(4096)
        with mmap.mmap(stream.fileno(), 0) as mapping:
            mapping[4096:4100] = b"test"
            mapping.flush()
        stream.seek(4096)
        assert stream.read(4) == b"test"


def permissions(p):
    f = p / "file"
    f.touch()
    f.chmod(0o640)
    os.chown(f, 1234, 1235)
    os.utime(f, ns=(1_700_000_000_123456789, 1_700_000_001_987654321))
    info = f.stat()
    assert stat.S_IMODE(info.st_mode) == 0o640
    assert (info.st_uid, info.st_gid) == (1234, 1235)
    assert info.st_mtime_ns == 1_700_000_001_987654321


def symlinks(p):
    (p / "file").write_text("data")
    (p / "link").symlink_to("file")
    assert os.readlink(p / "link") == "file"
    assert (p / "link").read_text() == "data"
    (p / "file").unlink()
    assert (p / "link").is_symlink()
    expect_errno(errno.ENOENT, lambda: (p / "link").read_bytes())
    (p / "loop").symlink_to("loop")
    expect_errno(errno.ELOOP, lambda: (p / "loop").read_bytes())


def hardlinks(p):
    (p / "file").write_bytes(b"first")
    os.link(p / "file", p / "alias")
    assert os.path.samefile(p / "file", p / "alias"), "hard links have different inode numbers"
    assert (p / "alias").stat().st_nlink == 2
    (p / "alias").write_bytes(b"second")
    assert (p / "file").read_bytes() == b"second"
    (p / "file").unlink()
    assert (p / "alias").read_bytes() == b"second"


def open_unlinked(p):
    f = p / "file"
    with f.open("w+b", buffering=0) as stream:
        stream.write(b"original")
        f.unlink()
        assert os.fstat(stream.fileno()).st_size == 8
        stream.seek(0)
        assert stream.read() == b"original"
        stream.write(b"!")
        os.fsync(stream.fileno())
        os.ftruncate(stream.fileno(), 3)
        os.fchmod(stream.fileno(), 0o600)
        assert os.fstat(stream.fileno()).st_size == 3


def atomic_replace(p):
    f = p / "file"
    f.write_bytes(b"old")
    with f.open("rb") as old:
        (p / "new").write_bytes(b"replacement")
        os.replace(p / "new", f)
        assert old.read() == b"old"
        assert os.fstat(old.fileno()).st_size == 3
        assert f.read_bytes() == b"replacement"


def directories(p):
    (p / "tree/child").mkdir(parents=True)
    for i in range(200):
        (p / "tree/child" / str(i)).touch()
    assert len(list((p / "tree/child").iterdir())) == 200
    expect_errno(errno.ENOTEMPTY, lambda: (p / "tree").rmdir())
    (p / "tree").rename(p / "moved")
    fd = os.open(p / "moved/child", os.O_RDONLY | os.O_DIRECTORY)
    try:
        assert stat.S_ISDIR(os.fstat(fd).st_mode)
        os.fsync(fd)
    finally:
        os.close(fd)
    shutil.rmtree(p / "moved")
    assert not (p / "moved").exists()


def names_errors(p):
    for name in ["空 格.txt", ".hidden", "a" * 255]:
        (p / name).write_text(name)
        assert (p / name).read_text() == name
    expect_errno(errno.ENAMETOOLONG, lambda: (p / ("b" * 256)).touch())
    expect_errno(errno.EEXIST, lambda: os.open(p / ".hidden", os.O_CREAT | os.O_EXCL))
    expect_errno(errno.ENOTDIR, lambda: (p / ".hidden/child").stat())


def locks(p):
    with (p / "lock").open("w") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        code = (
            "import fcntl,sys; f=open(sys.argv[1],'w'); fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)"
        )
        result = subprocess.run([sys.executable, "-c", code, str(p / "lock")], capture_output=True)
        assert result.returncode != 0, "second process acquired exclusive lock"
        fcntl.flock(stream, fcntl.LOCK_UN)


def archive(p):
    with tarfile.open(p / "data.tar", "w") as archive:
        member = tarfile.TarInfo("sub/file")
        member.size = 4
        member.mode = 0o640
        archive.addfile(member, io.BytesIO(b"data"))
    (p / "out").mkdir()
    subprocess.run(["tar", "xf", str(p / "data.tar"), "-C", str(p / "out")], check=True)
    assert (p / "out/sub/file").read_bytes() == b"data"
    assert stat.S_IMODE((p / "out/sub/file").stat().st_mode) == 0o640


def sqlite_transactions(p):
    with sqlite3.connect(p / "db") as db:
        assert db.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
        db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)")
        db.executemany("INSERT INTO items(value) VALUES (?)", [(str(i),) for i in range(1000)])
        db.commit()
        db.execute("DELETE FROM items")
        db.rollback()
        assert db.execute("SELECT count(*) FROM items").fetchone()[0] == 1000
        assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"


def fifo_socket(p):
    os.mkfifo(p / "fifo")
    fd = os.open(p / "fifo", os.O_RDWR | os.O_NONBLOCK)
    try:
        os.write(fd, b"data")
        assert os.read(fd, 4) == b"data"
    finally:
        os.close(fd)
    with socket.socket(socket.AF_UNIX) as server:
        server.bind(str(p / "socket"))
        assert stat.S_ISSOCK((p / "socket").stat().st_mode)


checks = [
    read_write,
    sparse_mmap,
    permissions,
    symlinks,
    hardlinks,
    open_unlinked,
    atomic_replace,
    directories,
    names_errors,
    locks,
    archive,
    fifo_socket,
    sqlite_transactions,
]
failures = []
for base in sys.argv[1:] or ["/var/tmp", "."]:
    for check in checks:
        try:
            with tempfile.TemporaryDirectory(prefix="pvisor-fs-", dir=base) as directory:
                check(Path(directory).absolute())
            print(f"PASS {base}: {check.__name__}", flush=True)
        except Exception as error:
            traceback.print_exc()
            failures.append((base, check.__name__, repr(error)))
            print(f"FAIL {base}: {check.__name__}: {error!r}", flush=True)
print(f"{len(checks) * len(sys.argv[1:] or ['/var/tmp', '.'])} checks; {len(failures)} failures")
sys.exit(bool(failures))
