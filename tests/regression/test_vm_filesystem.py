#!/usr/bin/env python3
"""Guest filesystem checks; select tests.regression.test_vm_filesystem inside a VM."""

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
import unittest
from contextlib import contextmanager
from pathlib import Path


class VmFilesystemTests(unittest.TestCase):
    def assertErrno(self, code, operation):
        with self.assertRaises(OSError) as failure:
            operation()
        self.assertEqual(failure.exception.errno, code, str(failure.exception))

    @contextmanager
    def guest_fs(self, mount_dir):
        if not mount_dir:
            self.skipTest("requires PVISOR_TEST_GUEST_FS_DIRS inside a Linux VM")
        if sys.platform != "linux" or os.geteuid() != 0:
            self.fail("guest filesystem checks require Linux and root inside the VM")
        with tempfile.TemporaryDirectory(prefix="pvisor-fs-", dir=mount_dir) as directory:
            yield Path(directory).absolute()

    def test_read_write(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                f = guest_fs_dir / "file"
                f.write_bytes(b"hello")
                with f.open("ab") as stream:
                    stream.write(b" world")
                    stream.flush()
                    os.fsync(stream.fileno())
                self.assertEqual(f.read_bytes(), b"hello world")
                with f.open("r+b") as stream:
                    stream.seek(6)
                    stream.write(b"VM")
                    stream.truncate(8)
                self.assertEqual(f.read_bytes(), b"hello VM")

    def test_sparse_mmap(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                with (guest_fs_dir / "file").open("w+b", buffering=0) as stream:
                    stream.truncate(1024 * 1024)
                    self.assertEqual(stream.read(4096), bytes(4096))
                    with mmap.mmap(stream.fileno(), 0) as mapping:
                        mapping[4096:4100] = b"test"
                        mapping.flush()
                    stream.seek(4096)
                    self.assertEqual(stream.read(4), b"test")

    def test_permissions(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                f = guest_fs_dir / "file"
                f.touch()
                f.chmod(0o640)
                os.chown(f, 1234, 1235)
                os.utime(f, ns=(1_700_000_000_123456789, 1_700_000_001_987654321))
                info = f.stat()
                self.assertEqual(stat.S_IMODE(info.st_mode), 0o640)
                self.assertEqual((info.st_uid, info.st_gid), (1234, 1235))
                self.assertEqual(info.st_mtime_ns, 1_700_000_001_987654321)

    def test_symlinks(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                (guest_fs_dir / "file").write_text("data")
                (guest_fs_dir / "link").symlink_to("file")
                self.assertEqual(os.readlink(guest_fs_dir / "link"), "file")
                self.assertEqual((guest_fs_dir / "link").read_text(), "data")
                (guest_fs_dir / "file").unlink()
                self.assertTrue((guest_fs_dir / "link").is_symlink())
                self.assertErrno(errno.ENOENT, lambda: (guest_fs_dir / "link").read_bytes())
                (guest_fs_dir / "loop").symlink_to("loop")
                self.assertErrno(errno.ELOOP, lambda: (guest_fs_dir / "loop").read_bytes())

    def test_hardlinks(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                (guest_fs_dir / "file").write_bytes(b"first")
                os.link(guest_fs_dir / "file", guest_fs_dir / "alias")
                self.assertTrue(
                    os.path.samefile(guest_fs_dir / "file", guest_fs_dir / "alias"),
                    "hard links have different inode numbers",
                )
                self.assertEqual((guest_fs_dir / "alias").stat().st_nlink, 2)
                (guest_fs_dir / "alias").write_bytes(b"second")
                self.assertEqual((guest_fs_dir / "file").read_bytes(), b"second")
                (guest_fs_dir / "file").unlink()
                self.assertEqual((guest_fs_dir / "alias").read_bytes(), b"second")

    def test_open_unlinked(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                f = guest_fs_dir / "file"
                with f.open("w+b", buffering=0) as stream:
                    stream.write(b"original")
                    f.unlink()
                    self.assertEqual(os.fstat(stream.fileno()).st_size, 8)
                    stream.seek(0)
                    self.assertEqual(stream.read(), b"original")
                    stream.write(b"!")
                    os.fsync(stream.fileno())
                    os.ftruncate(stream.fileno(), 3)
                    os.fchmod(stream.fileno(), 0o600)
                    self.assertEqual(os.fstat(stream.fileno()).st_size, 3)

    def test_atomic_replace(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                f = guest_fs_dir / "file"
                f.write_bytes(b"old")
                with f.open("rb") as old:
                    (guest_fs_dir / "new").write_bytes(b"replacement")
                    os.replace(guest_fs_dir / "new", f)
                    self.assertEqual(old.read(), b"old")
                    self.assertEqual(os.fstat(old.fileno()).st_size, 3)
                    self.assertEqual(f.read_bytes(), b"replacement")

    def test_directories(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                (guest_fs_dir / "tree/child").mkdir(parents=True)
                for i in range(200):
                    (guest_fs_dir / "tree/child" / str(i)).touch()
                self.assertEqual(len(list((guest_fs_dir / "tree/child").iterdir())), 200)
                self.assertErrno(errno.ENOTEMPTY, lambda: (guest_fs_dir / "tree").rmdir())
                (guest_fs_dir / "tree").rename(guest_fs_dir / "moved")
                fd = os.open(guest_fs_dir / "moved/child", os.O_RDONLY | os.O_DIRECTORY)
                try:
                    self.assertTrue(stat.S_ISDIR(os.fstat(fd).st_mode))
                    os.fsync(fd)
                finally:
                    os.close(fd)
                shutil.rmtree(guest_fs_dir / "moved")
                self.assertFalse((guest_fs_dir / "moved").exists())

    def test_names_errors(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                for name in ["空 格.txt", ".hidden", "a" * 255]:
                    (guest_fs_dir / name).write_text(name)
                    self.assertEqual((guest_fs_dir / name).read_text(), name)
                self.assertErrno(errno.ENAMETOOLONG, lambda: (guest_fs_dir / ("b" * 256)).touch())
                self.assertErrno(
                    errno.EEXIST,
                    lambda: os.open(guest_fs_dir / ".hidden", os.O_CREAT | os.O_EXCL),
                )
                self.assertErrno(errno.ENOTDIR, lambda: (guest_fs_dir / ".hidden/child").stat())

    def test_locks(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                with (guest_fs_dir / "lock").open("w") as stream:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    code = "import fcntl,sys; f=open(sys.argv[1],'w'); fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)"
                    result = subprocess.run(
                        [sys.executable, "-c", code, str(guest_fs_dir / "lock")],
                        capture_output=True,
                    )
                    self.assertNotEqual(
                        result.returncode, 0, "second process acquired exclusive lock"
                    )
                    fcntl.flock(stream, fcntl.LOCK_UN)

    def test_archive(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                with tarfile.open(guest_fs_dir / "data.tar", "w") as archive:
                    member = tarfile.TarInfo("sub/file")
                    member.size = 4
                    member.mode = 0o640
                    archive.addfile(member, io.BytesIO(b"data"))
                (guest_fs_dir / "out").mkdir()
                subprocess.run(
                    [
                        "tar",
                        "xf",
                        str(guest_fs_dir / "data.tar"),
                        "-C",
                        str(guest_fs_dir / "out"),
                    ],
                    check=True,
                )
                self.assertEqual((guest_fs_dir / "out/sub/file").read_bytes(), b"data")
                self.assertEqual(
                    stat.S_IMODE((guest_fs_dir / "out/sub/file").stat().st_mode), 0o640
                )

    def test_sqlite_transactions(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                with sqlite3.connect(guest_fs_dir / "db") as db:
                    self.assertEqual(db.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal")
                    db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)")
                    db.executemany(
                        "INSERT INTO items(value) VALUES (?)", [(str(i),) for i in range(1000)]
                    )
                    db.commit()
                    db.execute("DELETE FROM items")
                    db.rollback()
                    self.assertEqual(db.execute("SELECT count(*) FROM items").fetchone()[0], 1000)
                    self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")

    def test_fifo_socket(self):
        for mount_dir in os.environ.get("PVISOR_TEST_GUEST_FS_DIRS", "").split(os.pathsep):
            with self.subTest(mount_dir=mount_dir), self.guest_fs(mount_dir) as guest_fs_dir:
                os.mkfifo(guest_fs_dir / "fifo")
                fd = os.open(guest_fs_dir / "fifo", os.O_RDWR | os.O_NONBLOCK)
                try:
                    os.write(fd, b"data")
                    self.assertEqual(os.read(fd, 4), b"data")
                finally:
                    os.close(fd)
                with socket.socket(socket.AF_UNIX) as server:
                    server.bind(str(guest_fs_dir / "socket"))
                    self.assertTrue(stat.S_ISSOCK((guest_fs_dir / "socket").stat().st_mode))
