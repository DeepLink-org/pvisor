"""Shared lazy image v2 contracts for active JSON benchmarks.

Extracted from the retired runners; no historical campaign entry point.
"""

import ctypes
import hashlib
import json
import os
import shutil
import socket
import socketserver
import stat
import struct
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import lazy_startup

MAX_FRAME = 1024 * 1024


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def exact(stream, length, clean_eof=False):
    data = bytearray()
    while len(data) < length:
        block = stream.recv(length - len(data))
        if not block:
            if clean_eof and not data:
                return None
            raise EOFError("partial cache frame/body")
        data.extend(block)
    return bytes(data)


def frame(stream, clean_eof=False):
    prefix = exact(stream, 4, clean_eof)
    if prefix is None:
        return None
    length = struct.unpack("!I", prefix)[0]
    if not 0 < length <= MAX_FRAME:
        raise ValueError("invalid cache JSON frame length")
    body = exact(stream, length)
    return prefix + body, json.loads(body)


class Counts:
    """Drain in-flight exchanges, not idle persistent sockets."""

    def __init__(self):
        self.condition = threading.Condition()
        self.active = 0
        self.connections = 0
        self.rows = []
        self.errors = []

    def snapshot(self, timeout=5):
        deadline = time.monotonic() + timeout
        with self.condition:
            while self.active:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("proxy requests did not drain within 5 seconds")
                self.condition.wait(remaining)
            return dict(
                rows=list(self.rows), errors=list(self.errors), connections=self.connections
            )


class CacheProxy(socketserver.BaseRequestHandler):
    """Preserve exact response bytes; delimit raw Data bodies before next frame."""

    def handle(self):
        counts = self.server.counts
        with counts.condition:
            counts.connections += 1
            connection_id = counts.connections
        upstream = None
        active = False
        version = None
        try:
            self.request.settimeout(5)
            self.request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            while True:
                # Read one byte before marking active: idle v2 sockets must not
                # delay a snapshot. A partially received request *is* active.
                try:
                    first = self.request.recv(1)
                except socket.timeout:
                    return
                if not first:
                    return
                started = time.perf_counter()
                with counts.condition:
                    counts.active += 1
                active = True
                prefix = first + exact(self.request, 3)
                size = struct.unpack("!I", prefix)[0]
                if not 0 < size <= MAX_FRAME:
                    raise ValueError("invalid request frame length")
                raw_request = exact(self.request, size)
                envelope = json.loads(raw_request)
                current = envelope["version"]
                if current not in (1, 2) or (version is not None and version != current):
                    raise ValueError("invalid or changing protocol version")
                version = current
                request = envelope["request"]
                if upstream is None:
                    upstream = socket.create_connection(
                        ("127.0.0.1", self.server.upstream), timeout=90
                    )
                    upstream.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                upstream.sendall(prefix + raw_request)
                wire, response = frame(upstream)
                length = response.get("length", 0) if response["status"] == "data" else 0
                if type(length) is not int or not 0 <= length <= MAX_FRAME:
                    raise ValueError("invalid Data.length")
                body = exact(upstream, length)
                body_hash = "sha256:" + hashlib.sha256(body).hexdigest()
                if response["status"] == "data" and response.get("sha256") != body_hash:
                    raise ValueError("server body hash mismatch")
                # Never synthesize/re-serialize a response or forward an incomplete body.
                self.request.sendall(wire + body)
                with counts.condition:
                    counts.rows.append(
                        dict(
                            connection_id=connection_id,
                            version=version,
                            op=request["op"],
                            path=request.get("path"),
                            request=request,
                            response=response,
                            content_bytes=length if request["op"] == "read" else 0,
                            metadata_bytes=length if request["op"] == "metadata" else 0,
                            response_bytes=len(wire) + length,
                            server_body_sha256=response.get("sha256"),
                            forwarded_body_sha256=body_hash,
                            elapsed_ms=(time.perf_counter() - started) * 1000,
                        )
                    )
                    counts.active -= 1
                    active = False
                    counts.condition.notify_all()
                if version == 1:
                    return
        except Exception as error:
            with counts.condition:
                counts.errors.append(dict(connection_id=connection_id, error=repr(error)))
        finally:
            if upstream is not None:
                upstream.close()
            if active:
                with counts.condition:
                    counts.active -= 1
                    counts.condition.notify_all()


class TCPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def prepared_store_manifest(root):
    """Hash only lstat-regular files; record links without traversing targets."""
    manifest = {}
    pending = [root]
    while pending:
        directory = pending.pop()
        for path in sorted(directory.iterdir()):
            mode = path.lstat().st_mode
            name = str(path.relative_to(root))
            if stat.S_ISLNK(mode):
                manifest[name] = dict(kind="symlink", target=os.readlink(path))
            elif stat.S_ISDIR(mode):
                pending.append(path)
            elif stat.S_ISREG(mode):
                manifest[name] = dict(kind="regular", sha256=lazy_startup.sha(path))
            else:
                raise RuntimeError(f"unsupported prepared-store entry: {name}")
    return manifest


def chroot_bind_plan():
    """Bind canonical directories once while preserving top-level host aliases."""
    sources = set()
    aliases = []
    for name in ("/home", "/usr", "/etc", "/dev", "/proc", "/bin", "/sbin", "/lib", "/lib64"):
        path = Path(name)
        if name == "/lib64" and not path.exists() and not path.is_symlink():
            continue
        canonical = path.resolve(strict=True)
        if not canonical.is_dir() or canonical == Path("/") or canonical.is_relative_to("/tmp"):
            raise RuntimeError(f"unsafe chroot bind source: {path} -> {canonical}")
        sources.add(canonical)
        if path.is_symlink():
            aliases.append(dict(path=name, target=os.readlink(path)))
    binds = []
    for source in sorted(sources, key=lambda path: (len(path.parts), str(path))):
        if not any(source.is_relative_to(Path(parent)) for parent in binds):
            binds.append(str(source))
    return dict(binds=binds, symlinks=aliases)


def copy_launch_binaries(frozen_dir, private_root):
    directory = private_root / "tmp/lazy-binaries"
    directory.mkdir(mode=0o700)
    receipt = {}
    for name in ("pvisor", "pvisor-daemon"):
        source = frozen_dir / name
        if not stat.S_ISREG(source.lstat().st_mode):
            raise RuntimeError(f"frozen binary is not a regular file: {source}")
        expected = lazy_startup.sha(source)
        destination = directory / name
        shutil.copyfile(source, destination)
        destination.chmod(0o700)
        copied = lazy_startup.sha(destination)
        if copied != expected or lazy_startup.sha(source) != expected:
            raise RuntimeError(f"launch binary copy hash mismatch: {name}")
        receipt[name] = dict(
            frozen_path=str(source),
            launch_path="/tmp/lazy-binaries/" + name,
            frozen_sha256=expected,
            launch_sha256=copied,
        )
    return receipt


def validate_launch_tree(root):
    for relative, mode in (("", 0o700), ("tmp", 0o1777), ("tmp/lazy-binaries", 0o700)):
        path = root / relative
        metadata = path.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != mode
        ):
            raise RuntimeError(f"unsafe private launch ancestor: {path}")
    for name in ("pvisor", "pvisor-daemon"):
        path = root / "tmp/lazy-binaries" / name
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise RuntimeError(f"unsafe private launch executable: {path}")


def verify_launch_binaries(binaries, root=Path("/")):
    for name, record in binaries.items():
        path = root / record["launch_path"].lstrip("/")
        if (
            lazy_startup.sha(path) != record["frozen_sha256"]
            or lazy_startup.sha(Path(record["frozen_path"])) != record["frozen_sha256"]
        ):
            raise RuntimeError(f"launch/frozen binary changed: {name}")


class ContainmentEnvironmentGap(RuntimeError):
    """The host cannot provide the required root switch; no insecure fallback."""


def perform_pivot_root(private_root):
    old_root = private_root / ".old-root"
    utility = shutil.which("pivot_root")
    try:
        if utility:
            subprocess.run([utility, str(private_root), str(old_root)], check=True, timeout=10)
            return "pivot_root utility: " + utility
        libc = ctypes.CDLL(None, use_errno=True)
        pivot = getattr(libc, "pivot_root", None)
        if pivot is None:
            raise ContainmentEnvironmentGap(
                "pivot_root environment gap: neither utility nor libc function available"
            )
        pivot.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
        pivot.restype = ctypes.c_int
        if pivot(os.fsencode(private_root), os.fsencode(old_root)) != 0:
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code))
        return "libc pivot_root"
    except (OSError, subprocess.SubprocessError) as error:
        raise ContainmentEnvironmentGap(f"pivot_root environment gap: {error}") from error


def enter_private_chroot(output, cwd):
    """All construction precedes binds; never chmod/chown a bound host tree."""
    plan = chroot_bind_plan()
    for path in (output, cwd):
        if (
            not path.is_absolute()
            or not path.is_dir()
            or not any(path.is_relative_to(Path(source)) for source in plan["binds"])
        ):
            raise RuntimeError(f"chroot cannot preserve evidence/workspace path: {path}")
    evidence_identity = (output.stat().st_dev, output.stat().st_ino)
    private_root = Path(tempfile.mkdtemp(prefix="lazy-owned-root-", dir="/tmp"))
    (private_root / ".old-root").mkdir(mode=0o700)
    (private_root / "tmp").mkdir(mode=0o1777)
    (private_root / "tmp").chmod(0o1777)
    # Prepare every mountpoint and alias before binding, so directory creation
    # can never write through /home, /usr, /etc, /dev or /proc into the host.
    for source in plan["binds"]:
        (private_root / source.lstrip("/")).mkdir(parents=True, mode=0o700)
    for alias in plan["symlinks"]:
        destination = private_root / alias["path"].lstrip("/")
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(alias["target"])
    binaries = copy_launch_binaries(output / "frozen/bin", private_root)
    validate_launch_tree(private_root)
    receipt = dict(
        host_private_root=str(private_root),
        chroot_root="/",
        root_mode="0700",
        tmp_mode="1777",
        launch_directory_mode="0700",
        uid=os.getuid(),
        cwd=str(cwd),
        symlinks=plan["symlinks"],
        binds=[],
        binaries=binaries,
        root_switch="pivot_root",
        root_self_bind=False,
        pivot_root_method=None,
        entered=False,
        old_root_detached=False,
        evidence_path_unchanged=False,
        host_ownership_changes="none; only private tmpfs paths created/chmodded",
    )
    receipt_path = output / "containment-chroot.json"
    save(receipt_path, receipt)
    # Bind the fresh tree to itself before adding any submounts. It must be a
    # mountpoint for pivot_root; there are no locked children to omit here.
    subprocess.run(
        ["mount", "--bind", str(private_root), str(private_root)], check=True, timeout=10
    )
    receipt["root_self_bind"] = True
    save(receipt_path, receipt)
    for source in plan["binds"]:
        target = private_root / source.lstrip("/")
        # Recursive binds preserve locked submounts (notably /dev and /proc)
        # that a nonrecursive bind cannot omit in a user namespace. Propagation
        # is already private from unshare, so these mounts remain contained.
        subprocess.run(["mount", "--rbind", source, str(target)], check=True, timeout=10)
        receipt["binds"].append(
            dict(source=source, host_mountpoint=str(target), chroot_path=source, recursive=True)
        )
        save(receipt_path, receipt)
    os.chdir(private_root)
    try:
        receipt["pivot_root_method"] = perform_pivot_root(private_root)
        # pivot_root switches the mount namespace's root as well as fs root;
        # chroot alone would prevent libkrun from creating its user namespace.
        os.chdir("/")
        receipt["entered"] = True
        save(receipt_path, receipt)
        try:
            subprocess.run(["umount", "-l", "/.old-root"], check=True, timeout=10)
        except (OSError, subprocess.SubprocessError) as error:
            raise ContainmentEnvironmentGap(
                f"pivot_root old-root detach environment gap: {error}"
            ) from error
        receipt["old_root_detached"] = True
    except ContainmentEnvironmentGap as error:
        receipt["environment_gap"] = str(error)
        save(receipt_path, receipt)
        raise
    os.chdir(cwd)
    validate_launch_tree(Path("/"))
    verify_launch_binaries(binaries)
    if (output.stat().st_dev, output.stat().st_ino) != evidence_identity:
        raise RuntimeError("chroot changed evidence directory identity")
    receipt["evidence_path_unchanged"] = True
    receipt["mountinfo_after_pivot_root"] = Path("/proc/self/mountinfo").read_text()
    save(receipt_path, receipt)
    return receipt
