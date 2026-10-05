#!/usr/bin/env python3
"""Historical retired-snapshot harness; use an archived binary.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role historical;
the snapshot command is retired, so this harness produces no new conclusions.

Linux KVM/FUSE snapshot stress and fault injection; never skips missing hardware.

Use a new output directory and snapshot_stress_guest.rs compiled with panic=abort.
Every launch, fault and assertion is journaled, including failures. The seed picks
forks/signals; scheduler interleavings are intentionally not deterministic.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import re
import shutil
import signal
import socket
import subprocess
import threading
import time
from pathlib import Path


def mounts_under(root):
    """Read actual mounts: exists() is false for a disconnected FUSE mount."""
    prefix = str(root.resolve()) + "/"
    mounts = []
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        path = re.sub(r"\\([0-7]{3})", lambda match: chr(int(match[1], 8)), line.split()[4])
        if path.startswith(prefix):
            mounts.append(path)
    return mounts


def ipc_path(directory):
    digest = hashlib.sha256(os.fsencode(directory.resolve())).hexdigest()[:32]
    return Path(f"/tmp/pvisor-snapshots-{os.geteuid()}/{digest}.sock")


class Harness:
    def __init__(self, args):
        legacy_help = subprocess.run([str(args.binary.resolve()), "snapshot", "--help"], capture_output=True, timeout=10)
        if legacy_help.returncode != 0:
            raise ValueError("this historical harness requires an archived binary exposing the retired snapshot command")
        self.args = args
        self.root = args.output.resolve()
        self.root.mkdir(mode=0o700)
        self.binary = self.root / "pvisor"
        shutil.copy2(args.binary.resolve(), self.binary)
        shutil.copy2(Path(__file__).resolve(), self.root / "harness.py")
        shutil.copy2(Path(__file__).with_name("snapshot_stress_guest.rs"), self.root / "guest.rs")
        self.guest = args.guest.resolve()
        self.random = random.Random(args.seed)
        self.processes = []
        self.instances = []
        self.events = []
        self.event_lock = threading.Lock()
        self.journal = (self.root / "events.jsonl").open("w")

    def record(self, event, **details):
        row = dict(event=event, monotonic=time.monotonic(), **details)
        with self.event_lock:
            self.events.append(row)
            self.journal.write(json.dumps(row) + "\n")
            self.journal.flush()

    def command(self, store, *arguments, error=None):
        result = subprocess.run(
            [str(self.binary), "snapshot", "--store", str(store), *map(str, arguments)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.record(
            "command",
            args=list(map(str, arguments)),
            code=result.returncode,
            stdout=result.stdout,
            stderr=result.stderr,
        )
        if error is None:
            assert result.returncode == 0, (arguments, result.stdout, result.stderr)
        else:
            assert result.returncode != 0 and error in result.stderr, result.stderr
        return result.stdout.strip()

    def start(self, store, name, *arguments):
        log_path = self.root / f"{store.parent.name}-{name}.log"
        with log_path.open("w") as log:
            process = subprocess.Popen(
                [
                    str(self.binary),
                    "snapshot",
                    "--store",
                    str(store),
                    *map(str, arguments),
                    "--name",
                    name,
                ],
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        root = store / "runs" / name / "rootfs"
        self.processes.append(process)
        self.instances.append(root.parent)
        self.record("launch", name=name, pid=process.pid, log=str(log_path))
        return root, process

    def wait(self, predicate, process=None, label="condition", timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if process is not None:
                assert process.poll() is None, (
                    f"VM {process.pid} exited {process.returncode}: {label}"
                )
            result = predicate()
            if result:
                return result
            time.sleep(0.005)
        raise TimeoutError(label)

    def changed(self, path, process, previous=""):
        def read():
            try:
                value = path.read_text()
            except FileNotFoundError:
                return None
            return value if value and value != previous else None

        return self.wait(read, process, str(path))

    def request(self, vm, token):
        root, process = vm
        old = (root / "ack").read_text() if (root / "ack").exists() else ""
        (root / "request.tmp").write_text(token)
        (root / "request.tmp").replace(root / "request")
        ack = self.changed(root / "ack", process, old)
        assert ack.split()[0] == token, ack
        assert (root / "private").read_text() == token
        self.record("guest_integrity", name=root.parent.name, ack=ack)
        return ack

    def stop(self, vm, sig=signal.SIGTERM):
        root, process = vm
        # Also kill descendants when the launcher exited before its runner.
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=10)
        self.wait(lambda: not mounts_under(root.parent), label="RAM mount cleanup", timeout=10)
        self.wait(
            lambda: not ipc_path(root.parent).exists(), label="control socket cleanup", timeout=10
        )
        self.record("stopped", name=root.parent.name, signal=int(sig), code=process.returncode)

    def live_processes(self):
        owned = []
        for entry in Path("/proc").iterdir():
            if entry.name.isdigit():
                try:
                    argv = (entry / "cmdline").read_bytes().split(b"\0")
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    continue
                if argv[0] == os.fsencode(self.binary):
                    owned.append(int(entry.name))
        return owned

    def restored(self, store, identity, name):
        baseline = (store / "objects" / identity / "rootfs/heartbeat").read_text()
        vm = self.start(store, name, "restore", identity)
        self.changed(vm[0] / "heartbeat", vm[1], baseline)
        return vm

    def parallel(self, action, values):
        with concurrent.futures.ThreadPoolExecutor(max_workers=self.args.forks) as pool:
            return list(pool.map(action, values))

    def storage(self, storage):
        base = self.root / storage
        base.mkdir()
        store = base / "store"
        source = base / "input"
        source.mkdir()
        (source / "dev").mkdir()
        shutil.copy2(self.guest, source / "init.krun")
        vm = self.start(
            store,
            "source",
            "run",
            "--rootfs",
            source,
            "--native-init",
            "--ram-storage",
            storage,
            "--memory",
            "256",
            "--cpus",
            "2",
        )
        self.changed(vm[0] / "ready", vm[1])
        self.request(vm, "source-before")

        # Reject a special file during publication, then prove thaw/retry works.
        os.mkfifo(vm[0] / "injected-fifo")
        self.command(store, "save", "source", error="unsupported filesystem object")
        (vm[0] / "injected-fifo").unlink()
        before = (vm[0] / "heartbeat").read_text()
        self.changed(vm[0] / "heartbeat", vm[1], before)
        assert not list((store / "objects").iterdir())
        assert not list((store / "pending").iterdir())
        self.request(vm, "source-after-failed-save")

        # Bad and disconnected clients must leave the serial control listener usable.
        for payload in (b"nope\n", b"sa"):
            with socket.socket(socket.AF_UNIX) as stream:
                stream.connect(str(ipc_path(vm[0].parent)))
                stream.sendall(payload)
        identity = self.command(store, "save", "source")
        assert vm[1].wait(timeout=15) == 0
        shutil.rmtree(source)
        shutil.rmtree(vm[0].parent)

        for cycle in range(self.args.cycles):
            sealed = store / "objects" / identity / "rootfs"
            private = (sealed / "private").read_text()
            vms = self.parallel(
                lambda index: self.restored(store, identity, f"c{cycle}-fork{index}"),
                range(self.args.forks),
            )
            # Each guest mutates RAM and its FS; siblings and seal stay private.
            for index, fork in enumerate(vms):
                self.request(fork, f"c{cycle}-private{index}")
                assert (sealed / "private").read_text() == private
                for untouched in vms[index + 1 :]:
                    assert (untouched[0] / "private").read_text() == private
            self.command(store, "delete", identity)
            self.command(store, "gc")
            self.parallel(
                lambda indexed: self.request(indexed[1], f"c{cycle}-gc{indexed[0]}"), enumerate(vms)
            )
            survivor = self.random.randrange(len(vms))
            for index, fork in enumerate(vms):
                if index != survivor:
                    self.stop(fork, self.random.choice([signal.SIGTERM, signal.SIGKILL]))
            identity = self.command(store, "save", vms[survivor][0].parent.name)
            assert vms[survivor][1].wait(timeout=15) == 0
            self.wait(lambda: not mounts_under(base), label="saved VM unmount", timeout=10)
            self.command(store, "gc")
            self.record("generation", storage=storage, cycle=cycle, identity=identity)

        # Kill while a capture inode is being written, rather than relying on a
        # delay that may hit either side of publication on different machines.
        victim = self.restored(store, identity, "capture-victim")
        with (base / "interrupted-save.log").open("w") as log:
            saver = subprocess.Popen(
                [str(self.binary), "snapshot", "--store", str(store), "save", "capture-victim"],
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        self.processes.append(saver)
        capture = self.wait(
            lambda: next(iter((store / "pending").glob("*/capture.ram")), None),
            victim[1],
            "capture started",
        )
        self.record("fault", kind="kill-during-capture", path=str(capture))
        self.stop(victim, signal.SIGKILL)
        assert saver.wait(timeout=15) != 0
        self.command(store, "gc")
        assert not list((store / "pending").iterdir())
        assert set(path.name for path in (store / "objects").iterdir()) == {identity}
        # The prior valid seal must remain recoverable after the interrupted save.
        recovered = self.restored(store, identity, "after-chaos")
        self.request(recovered, "after-chaos-integrity")
        self.stop(recovered, signal.SIGKILL)
        self.command(store, "delete", identity)
        self.command(store, "gc")
        assert not list((store / "content").iterdir())
        assert not list((store / "pending").iterdir())
        self.record("storage_passed", storage=storage)

    def run(self):
        error = None
        try:
            # Fail explicitly rather than producing a green hardware skip.
            for device in ("/dev/kvm", "/dev/fuse"):
                descriptor = os.open(device, os.O_RDWR | os.O_CLOEXEC)
                os.close(descriptor)
            for storage in (
                ("raw", "compressed") if self.args.storage == "both" else (self.args.storage,)
            ):
                self.storage(storage)
        except BaseException as exception:
            error = repr(exception)
            self.record("failure", error=error)
            raise
        finally:
            cleanup = []
            # Signal every group before waiting for any mount: a failed fork
            # must not leave siblings alive while global cleanup is waiting.
            for process in self.processes:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            for process in self.processes:
                try:
                    self.stop((self.root / "cleanup", process), signal.SIGKILL)
                except Exception as exception:
                    cleanup.append(repr(exception))
            # Assert mounts as well as watcher/runner processes are gone. Retain
            # failed socket paths as evidence; never remove foreign resources.
            leftovers = mounts_under(self.root)
            sockets = [str(ipc_path(path)) for path in self.instances if ipc_path(path).exists()]
            try:
                self.wait(
                    lambda: not self.live_processes(), label="runner/watchdog exit", timeout=10
                )
            except TimeoutError as exception:
                cleanup.append(repr(exception))
            processes = self.live_processes()
            report = dict(
                schema="pvisor-kvm-stress/v1",
                seed=self.args.seed,
                cycles=self.args.cycles,
                forks=self.args.forks,
                storage=self.args.storage,
                binary_sha256=hashlib.sha256(self.binary.read_bytes()).hexdigest(),
                guest_sha256=hashlib.sha256(self.guest.read_bytes()).hexdigest(),
                harness_sha256=hashlib.sha256((self.root / "harness.py").read_bytes()).hexdigest(),
                correctness="failed"
                if error or cleanup or leftovers or sockets or processes
                else "passed",
                error=error,
                cleanup_errors=cleanup,
                mounts=leftovers,
                sockets=sockets,
                live_processes=processes,
                events=self.events,
            )
            (self.root / "result.json").write_text(json.dumps(report, indent=2) + "\n")
            self.journal.close()
            if error is None:
                assert not cleanup and not leftovers and not sockets and not processes, {
                    "cleanup": cleanup,
                    "mounts": leftovers,
                    "sockets": sockets,
                    "processes": processes,
                }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cycles", type=int, default=5)
    parser.add_argument("--forks", type=int, default=4)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--storage", choices=("both", "raw", "compressed"), default="both")
    args = parser.parse_args()
    if not (1 <= args.cycles <= 30 and 2 <= args.forks <= 16):
        parser.error("cycles must be 1–30 and forks 2–16")
    Harness(args).run()


if __name__ == "__main__":
    main()
