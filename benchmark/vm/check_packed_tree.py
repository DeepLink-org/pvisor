"""B-VM-GUEST-ENG: untimed Linux/KVM checks for the fixed-layout publication fixture."""

import argparse
import contextlib
import os
import resource
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def verify(ready):
    assert stat.S_ISDIR(ready.lstat().st_mode)
    assert {p.name for p in ready.iterdir()} == {f"part-{i}" for i in range(8)}
    for i in range(8):
        part = ready / f"part-{i}"
        st = part.lstat()
        assert stat.S_ISREG(st.st_mode) and st.st_size == 512 * 1024
        expected = b"".join(bytes([(31 * i + j) % 256]) * 1024 for j in range(512))
        assert part.read_bytes() == expected


@contextlib.contextmanager
def observed(argv, **options):
    with tempfile.TemporaryFile() as log, subprocess.Popen(
        argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, **options
    ) as p:
        try:
            yield p
        except BaseException:
            log.seek(0)
            shutil.copyfileobj(log, sys.stderr.buffer)
            raise
        finally:
            p.kill()
            p.wait(timeout=10)


def phase(p, name):
    expected = (name + "\n").encode()
    received = b""
    deadline = time.monotonic() + 20
    while len(received) < len(expected):
        assert select.select([p.stdout], [], [], max(0, deadline - time.monotonic()))[0], name
        data = os.read(p.stdout.fileno(), len(expected) - len(received))
        assert data, (name, p.poll())
        received += data
    assert received == expected


def resume(p):
    p.stdin.write(b"\n")
    p.stdin.flush()


def importer_checks(probe, pack, top):
    cmd = [probe, "import", str(pack.fileno())]
    opts = {"pass_fds": (pack.fileno(),)}
    for stop in ("prepared", "renamed"):
        dest = top / stop
        dest.mkdir()
        with observed([*cmd, "observe"], cwd=dest, **opts) as p:
            phase(p, "prepared")
            assert not os.path.lexists(dest / "ready")
            if stop == "renamed":
                resume(p)
                phase(p, "renamed")
        verify(dest / (".pending" if stop == "prepared" else "ready"))
        assert not os.path.lexists(dest / ("ready" if stop == "prepared" else ".pending"))

    def small_file_limit():
        resource.setrlimit(resource.RLIMIT_FSIZE, (65536, 65536))

    for fault in ("conflict", "short-write", "extra-byte", "missing-byte"):
        dest = top / fault
        dest.mkdir()
        ready = dest / "ready"
        if fault == "conflict":
            ready.mkdir()
            before = ready.stat()
        if fault.endswith("-byte"):
            os.ftruncate(pack.fileno(), 4 * 1024**2 + (1 if fault == "extra-byte" else -1))
        result = subprocess.run(cmd, cwd=dest, capture_output=True, timeout=20,
                                preexec_fn=small_file_limit if fault == "short-write" else None,
                                **opts)
        assert result.returncode != 0, fault
        if fault == "conflict":
            assert before.st_ino == ready.stat().st_ino and not list(ready.iterdir())
            assert b"renameat2" in result.stderr
        else:
            assert not os.path.lexists(ready)
            if fault == "short-write":
                assert (dest / ".pending/part-0").stat().st_size == 65536
            else:
                assert not os.path.lexists(dest / ".pending")


def check(vm, probe, top):
    env = os.environ | {"PVISOR_GUEST_KERNEL": "rust-native"}
    for mode in ("shared", "local", "interrupted", "pack"):
        root = top / mode
        out = root / "output"
        out.mkdir(parents=True)
        out.chmod(0o777)
        (root / "dev").mkdir()
        shutil.copy2(probe, root / "probe")
        cmd = [vm, "--rootfs", root, "--cpus", "1", "--memory", "256", "--task-stdio", "--", "/probe"]
        if mode in ("shared", "local"):
            p = subprocess.run([*cmd, mode, "start"], env=env, input=b"\n", capture_output=True,
                               check=True, timeout=30)
            assert p.stdout.startswith(b"begin\nTREE_PUBLISHED_NS ")
            verify(out / "ready")
            continue

        capsule = out / "pack"
        with capsule.open("x+b") as pack:
            capsule.chmod(0o666)
            dest = top / f"result-{mode}"
            dest.mkdir()
            with observed([*cmd, "pack", "observe"], env=env) as p:
                phase(p, "generated")
                assert os.fstat(pack.fileno()).st_size == 0 and not list(dest.iterdir())
                resume(p)
                phase(p, "submitted")
                assert os.fstat(pack.fileno()).st_size == 4 * 1024**2 and not list(dest.iterdir())
                if mode == "interrupted":
                    p.kill()
                else:
                    resume(p)
                output, _ = p.communicate(timeout=20)
            if p.returncode != 0:
                assert mode == "interrupted" and p.returncode == -signal.SIGKILL
                assert not list(dest.iterdir())
                continue
            assert mode == "pack" and output == b"PACK_SUBMITTED\n"
            capsule.unlink()
            capsule.symlink_to("/dev/null")  # Import must not reopen a guest-provided name.
            subprocess.run([probe, "import", str(pack.fileno())], cwd=dest,
                           pass_fds=(pack.fileno(),), check=True, timeout=20)
            verify(dest / "ready")
            importer_checks(probe, pack, top)


if __name__ == "__main__":
    if not __debug__:
        raise SystemExit("requires Python assertions enabled")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vm", required=True, type=Path)
    parser.add_argument("--probe", required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        check(args.vm.resolve(strict=True), args.probe.resolve(strict=True), Path(directory))
    print("PASS: shared/local/packed trees, exit admission, pinned input, kill phases, conflict, size and write faults")
