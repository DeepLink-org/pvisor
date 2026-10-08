#!/usr/bin/env python3
"""Guest filesystem checks: pytest tests/test_vm_filesystem.py --guest-fs-dir /var/tmp --guest-fs-dir ."""

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


def expect_errno(code, operation):
    try:
        operation()
    except OSError as error:
        assert error.errno == code, (error.errno, code, str(error))
    else:
        raise AssertionError(f"expected errno {code}")


def test_read_write(guest_fs_dir):
    f = guest_fs_dir / "file"
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


def test_sparse_mmap(guest_fs_dir):
    with (guest_fs_dir / "file").open("w+b", buffering=0) as stream:
        stream.truncate(1024 * 1024)
        assert stream.read(4096) == bytes(4096)
        with mmap.mmap(stream.fileno(), 0) as mapping:
            mapping[4096:4100] = b"test"
            mapping.flush()
        stream.seek(4096)
        assert stream.read(4) == b"test"


def test_permissions(guest_fs_dir):
    f = guest_fs_dir / "file"
    f.touch()
    f.chmod(0o640)
    os.chown(f, 1234, 1235)
    os.utime(f, ns=(1_700_000_000_123456789, 1_700_000_001_987654321))
    info = f.stat()
    assert stat.S_IMODE(info.st_mode) == 0o640
    assert (info.st_uid, info.st_gid) == (1234, 1235)
    assert info.st_mtime_ns == 1_700_000_001_987654321


def test_symlinks(guest_fs_dir):
    (guest_fs_dir / "file").write_text("data")
    (guest_fs_dir / "link").symlink_to("file")
    assert os.readlink(guest_fs_dir / "link") == "file"
    assert (guest_fs_dir / "link").read_text() == "data"
    (guest_fs_dir / "file").unlink()
    assert (guest_fs_dir / "link").is_symlink()
    expect_errno(errno.ENOENT, lambda: (guest_fs_dir / "link").read_bytes())
    (guest_fs_dir / "loop").symlink_to("loop")
    expect_errno(errno.ELOOP, lambda: (guest_fs_dir / "loop").read_bytes())


def test_hardlinks(guest_fs_dir):
    (guest_fs_dir / "file").write_bytes(b"first")
    os.link(guest_fs_dir / "file", guest_fs_dir / "alias")
    assert os.path.samefile(guest_fs_dir / "file", guest_fs_dir / "alias"), (
        "hard links have different inode numbers"
    )
    assert (guest_fs_dir / "alias").stat().st_nlink == 2
    (guest_fs_dir / "alias").write_bytes(b"second")
    assert (guest_fs_dir / "file").read_bytes() == b"second"
    (guest_fs_dir / "file").unlink()
    assert (guest_fs_dir / "alias").read_bytes() == b"second"


def test_open_unlinked(guest_fs_dir):
    f = guest_fs_dir / "file"
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


def test_atomic_replace(guest_fs_dir):
    f = guest_fs_dir / "file"
    f.write_bytes(b"old")
    with f.open("rb") as old:
        (guest_fs_dir / "new").write_bytes(b"replacement")
        os.replace(guest_fs_dir / "new", f)
        assert old.read() == b"old"
        assert os.fstat(old.fileno()).st_size == 3
        assert f.read_bytes() == b"replacement"


def test_directories(guest_fs_dir):
    (guest_fs_dir / "tree/child").mkdir(parents=True)
    for i in range(200):
        (guest_fs_dir / "tree/child" / str(i)).touch()
    assert len(list((guest_fs_dir / "tree/child").iterdir())) == 200
    expect_errno(errno.ENOTEMPTY, lambda: (guest_fs_dir / "tree").rmdir())
    (guest_fs_dir / "tree").rename(guest_fs_dir / "moved")
    fd = os.open(guest_fs_dir / "moved/child", os.O_RDONLY | os.O_DIRECTORY)
    try:
        assert stat.S_ISDIR(os.fstat(fd).st_mode)
        os.fsync(fd)
    finally:
        os.close(fd)
    shutil.rmtree(guest_fs_dir / "moved")
    assert not (guest_fs_dir / "moved").exists()


def test_names_errors(guest_fs_dir):
    for name in ["空 格.txt", ".hidden", "a" * 255]:
        (guest_fs_dir / name).write_text(name)
        assert (guest_fs_dir / name).read_text() == name
    expect_errno(errno.ENAMETOOLONG, lambda: (guest_fs_dir / ("b" * 256)).touch())
    expect_errno(errno.EEXIST, lambda: os.open(guest_fs_dir / ".hidden", os.O_CREAT | os.O_EXCL))
    expect_errno(errno.ENOTDIR, lambda: (guest_fs_dir / ".hidden/child").stat())


def test_locks(guest_fs_dir):
    with (guest_fs_dir / "lock").open("w") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        code = (
            "import fcntl,sys; f=open(sys.argv[1],'w'); fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)"
        )
        result = subprocess.run(
            [sys.executable, "-c", code, str(guest_fs_dir / "lock")], capture_output=True
        )
        assert result.returncode != 0, "second process acquired exclusive lock"
        fcntl.flock(stream, fcntl.LOCK_UN)


def test_archive(guest_fs_dir):
    with tarfile.open(guest_fs_dir / "data.tar", "w") as archive:
        member = tarfile.TarInfo("sub/file")
        member.size = 4
        member.mode = 0o640
        archive.addfile(member, io.BytesIO(b"data"))
    (guest_fs_dir / "out").mkdir()
    subprocess.run(
        ["tar", "xf", str(guest_fs_dir / "data.tar"), "-C", str(guest_fs_dir / "out")], check=True
    )
    assert (guest_fs_dir / "out/sub/file").read_bytes() == b"data"
    assert stat.S_IMODE((guest_fs_dir / "out/sub/file").stat().st_mode) == 0o640


def test_sqlite_transactions(guest_fs_dir):
    with sqlite3.connect(guest_fs_dir / "db") as db:
        assert db.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
        db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)")
        db.executemany("INSERT INTO items(value) VALUES (?)", [(str(i),) for i in range(1000)])
        db.commit()
        db.execute("DELETE FROM items")
        db.rollback()
        assert db.execute("SELECT count(*) FROM items").fetchone()[0] == 1000
        assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"


def test_fifo_socket(guest_fs_dir):
    os.mkfifo(guest_fs_dir / "fifo")
    fd = os.open(guest_fs_dir / "fifo", os.O_RDWR | os.O_NONBLOCK)
    try:
        os.write(fd, b"data")
        assert os.read(fd, 4) == b"data"
    finally:
        os.close(fd)
    with socket.socket(socket.AF_UNIX) as server:
        server.bind(str(guest_fs_dir / "socket"))
        assert stat.S_ISSOCK((guest_fs_dir / "socket").stat().st_mode)
