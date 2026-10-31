#!/usr/bin/env python3
"""Bounded, seeded, opt-in native KVM stage correctness driver (stdlib only)."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import math
import os
import random
import selectors
import shlex
import shutil
import signal
import stat
import subprocess
import time
import traceback
from pathlib import Path

LIMIT = 32768
REGULAR_SCENARIOS = ("chain", "conflict", "symlink", "rebind", "project_all", "nested_all")
FAULT_SCENARIOS = ("parallel_apply", "parallel_drop", "live_cancel", "kill_apply")
SCENARIOS = (*REGULAR_SCENARIOS, *FAULT_SCENARIOS)
# A named 40-round cycle; round-dependent seeds vary payloads and race order.
DEFAULT_SCENARIOS = (*SCENARIOS,) * 4
UNCOVERED = [
    "owner/runner-specific SIGKILL and host power loss",
    "automatic pending-apply recovery (pending is observed, never repaired by the driver)",
    "fully-applied stage drop refusal unless outside_scope actually reaches zero",
]
MARKER = ".stage-chaos-owned"


class BudgetExpired(Exception):
    """The monotonic run/case deadline prevents another operation."""


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def safe_ancestors(path):
    """Reject links, foreign owners and writable ancestors, including sticky /tmp.

    Deliberately stricter than the product; never chmod an existing ancestor.
    """
    path = Path(os.path.abspath(path))
    for item in [path, *path.parents]:
        info = item.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid not in (0, os.getuid()):
            raise ValueError(f"unsafe owner or symlink: {item}")
        if info.st_mode & 0o022:
            raise ValueError(f"unsafe writable ancestor: {item}")
    return path


def fresh_output(path):
    path = Path(os.path.abspath(path))
    safe_ancestors(path.parent)
    path.mkdir(mode=0o700)  # Never reuse an existing evidence directory.
    (path / MARKER).write_text("stage-chaos-v1\n")
    return path


def delete_case(root, case):
    """Only remove a direct, private, owned case in this driver's marked root."""
    root, case = Path(root), Path(case)
    safe_ancestors(root)
    require(
        (root / MARKER).is_file() and not (root / MARKER).is_symlink(), "unmarked evidence root"
    )
    require(case.parent == root and case.name.startswith("case-"), "unsafe delete target")
    info = case.lstat()
    require(
        stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
        "unsafe case ownership/mode",
    )
    require(shutil.rmtree.avoids_symlink_attacks, "fd-safe rmtree required")
    shutil.rmtree(case)


def snapshot(root):
    """Recursive no-follow content + metadata oracle; deliberately ignores atime."""
    root = Path(root)
    result = {}

    def visit(path):
        info = path.lstat()
        entry = dict(
            mode=info.st_mode,
            uid=info.st_uid,
            gid=info.st_gid,
            dev=info.st_dev,
            inode=info.st_ino,
            mtime_ns=info.st_mtime_ns,
            ctime_ns=info.st_ctime_ns,
            size=info.st_size,
        )
        if stat.S_ISREG(info.st_mode):
            entry["sha256"] = sha256(path)
        elif stat.S_ISLNK(info.st_mode):
            entry["link"] = os.readlink(path)
        result[str(path.relative_to(root))] = entry
        if stat.S_ISDIR(info.st_mode):
            for child in sorted(path.iterdir()):
                visit(child)

    if root.exists() or root.is_symlink():
        visit(root)
    return result


def content_snapshot(root):
    return {
        name: {key: value for key, value in entry.items() if key in ("mode", "sha256", "link")}
        for name, entry in snapshot(root).items()
    }


def remaining_timeout(deadline, case_deadline, now=None):
    left = min(deadline, case_deadline) - (time.monotonic() if now is None else now)
    if left <= 0:
        raise BudgetExpired("monotonic deadline exhausted")
    return min(30.0, left)


def proc_identity(pid):
    """Linux PID identity, including starttime; never authorize by argv/name."""
    try:
        path = Path(f"/proc/{pid}")
        fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
        return dict(
            pid=pid,
            starttime=int(fields[19]),
            ppid=int(fields[1]),
            state=fields[0],
            uid=path.stat().st_uid,
        )
    except (OSError, ValueError, IndexError):
        return None


def process_table():
    return {
        int(path.name): item
        for path in Path("/proc").iterdir()
        if path.name.isdigit() and (item := proc_identity(int(path.name))) is not None
    }


def read_object(path, limit=2 * 1024 * 1024):
    """Read existing real artifacts only, no symlink following or unlimited JSON."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            require(stat.S_ISREG(os.fstat(stream.fileno()).st_mode), "artifact is not a file")
            data = stream.read(limit + 1)
        require(len(data) <= limit, "artifact exceeds JSON cap")
        value = json.loads(data)
        require(isinstance(value, dict), "artifact is not a JSON object")
        return value
    except FileNotFoundError:
        return None


class Descendants:
    """Exclusive subprocess supervisor; Linux subreaper catches double-fork owners.

    Baseline children are excluded. New direct children can only be explicitly
    spawned children or orphaned descendants adopted by this process. This scope
    must not be shared with an unrelated subprocess-launching thread.
    """

    def __init__(self, tick=None):
        self.libc = ctypes.CDLL(None, use_errno=True)
        require(
            hasattr(self.libc, "pidfd_open") and hasattr(self.libc, "pidfd_send_signal"),
            "Linux libc pidfd support required (unsupported is not a passing test)",
        )
        old = ctypes.c_int()
        require(self.libc.prctl(37, ctypes.byref(old), 0, 0, 0) == 0, "GET_CHILD_SUBREAPER failed")
        self.previous = old.value
        require(self.libc.prctl(36, 1, 0, 0, 0) == 0, "SET_CHILD_SUBREAPER failed")
        self.pid = os.getpid()
        self.baseline = process_table()
        self.owned = {}
        self.roots = {}
        self.signals = []
        self.tick = tick or (lambda: None)

    def register(self, process):
        item = proc_identity(process.pid)
        require(item is not None and item["ppid"] == self.pid, "spawn identity unavailable")
        self.owned[process.pid] = item
        self.roots[process.pid] = process
        return item

    def discover(self):
        table = process_table()
        changed = True
        while changed:
            changed = False
            for pid, item in table.items():
                if item["uid"] != os.getuid() or pid == self.pid:
                    continue
                old = self.owned.get(pid)
                if old and old["starttime"] == item["starttime"]:
                    continue
                baseline = self.baseline.get(pid)
                if baseline and baseline["starttime"] == item["starttime"]:
                    continue
                parent = table.get(item["ppid"])
                owned_parent = self.owned.get(item["ppid"])
                if item["ppid"] == self.pid or (
                    parent and owned_parent and parent["starttime"] == owned_parent["starttime"]
                ):
                    self.owned[pid] = item
                    changed = True
        return [
            item
            for pid, original in self.owned.items()
            if (item := table.get(pid)) and item["starttime"] == original["starttime"]
        ]

    def send(self, identity, sig):
        current = proc_identity(identity["pid"])
        if not current or current["starttime"] != identity["starttime"] or current["state"] == "Z":
            return False
        require(
            self.owned.get(identity["pid"], {}).get("starttime") == identity["starttime"],
            "refusing signal to non-owned PID identity",
        )
        try:
            fd = self.libc.pidfd_open(identity["pid"], 0)
            if fd < 0:
                error = ctypes.get_errno()
                raise OSError(error, os.strerror(error))
            try:
                check = proc_identity(identity["pid"])
                if not check or check["starttime"] != identity["starttime"]:
                    return False
                if self.libc.pidfd_send_signal(fd, int(sig), None, 0) < 0:
                    error = ctypes.get_errno()
                    raise OSError(error, os.strerror(error))
                self.signals.append(dict(**identity, signal=int(sig), monotonic=time.monotonic()))
                return True
            finally:
                os.close(fd)
        except ProcessLookupError:
            return False

    def cleanup(self, timeout=5):
        start = time.monotonic()
        end = start + max(0, min(30, timeout))
        sent = len(self.signals)
        while True:
            for process in self.roots.values():
                process.poll()  # Let Popen reap its own direct child.
            items = self.discover()
            for item in items:
                if item["state"] == "Z" and item["pid"] not in self.roots:
                    try:
                        os.waitpid(item["pid"], os.WNOHANG)
                    except ChildProcessError:
                        pass
            live = self.discover()
            if not live or time.monotonic() >= end:
                if not live:
                    self.roots.clear()
                    self.owned.clear()
                return dict(
                    complete=not live,
                    survivors=live,
                    elapsed=time.monotonic() - start,
                    signals=self.signals[sent:],
                )
            already = {(item["pid"], item["starttime"], item["signal"]) for item in self.signals}
            for item in live:
                sig = signal.SIGTERM if time.monotonic() - start < 0.2 else signal.SIGKILL
                if (item["pid"], item["starttime"], int(sig)) not in already:
                    self.send(item, sig)
            self.tick()
            time.sleep(0.02)

    def close(self):
        require(self.libc.prctl(36, self.previous, 0, 0, 0) == 0, "restore subreaper failed")


class WatchedCommand:
    """Popen + concurrently drained, capped memory/log files; no shell wrapper."""

    def __init__(self, args, cwd, env, scope, log=None):
        self.args = [str(arg) for arg in args]
        self.start = time.monotonic()
        self.process = subprocess.Popen(
            self.args,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        self.scope = scope
        self.identity = scope.register(self.process)
        self.selector = selectors.DefaultSelector()
        self.buffers = {"stdout": bytearray(), "stderr": bytearray()}
        self.totals = {"stdout": 0, "stderr": 0}
        self.logs = {}
        for name, stream in (("stdout", self.process.stdout), ("stderr", self.process.stderr)):
            os.set_blocking(stream.fileno(), False)
            self.selector.register(stream, selectors.EVENT_READ, name)
            if log is not None:
                self.logs[name] = Path(str(log) + "." + name).open("xb")
        self.closed = False

    def pump(self, delay=0.005):
        self.scope.discover()
        self.scope.tick()
        for key, _ in self.selector.select(delay):
            data = os.read(key.fileobj.fileno(), 65536)
            if not data:
                self.selector.unregister(key.fileobj)
                continue
            name = key.data
            self.totals[name] += len(data)
            kept = data[: max(0, LIMIT - len(self.buffers[name]))]
            self.buffers[name].extend(kept)
            if name in self.logs:
                self.logs[name].write(kept)
                self.logs[name].flush()
        self.process.poll()

    def done(self):
        return self.process.poll() is not None and not self.selector.get_map()

    def finish(self, deadline):
        while not self.done() and time.monotonic() < deadline:
            self.pump()
        timed_out = not self.done()
        cleanup = None
        if timed_out:
            cleanup = self.scope.cleanup(timeout=3)
            self.process.poll()
        result = dict(
            args=self.args,
            pid_identity=self.identity,
            exit=self.process.returncode,
            elapsed=time.monotonic() - self.start,
            timed_out=timed_out,
            cleanup=cleanup,
            bytes=self.totals.copy(),
            truncated={key: self.totals[key] > LIMIT for key in self.totals},
            **{key: bytes(value).decode("utf-8", "replace") for key, value in self.buffers.items()},
        )
        try:
            result["json"] = (
                json.loads(result["stdout"]) if not result["truncated"]["stdout"] else None
            )
        except ValueError:
            result["json"] = None
        self.close()
        return result

    def close(self):
        if not self.closed:
            self.selector.close()
            self.process.stdout.close()
            self.process.stderr.close()
            for stream in self.logs.values():
                stream.close()
            self.closed = True


def run_bounded(args, cwd, env, timeout, scope=None, tick=None):
    own = scope is None
    scope = scope or Descendants(tick)
    command = None
    try:
        command = WatchedCommand(args, cwd, env, scope)
        result = command.finish(time.monotonic() + timeout)
        if own:
            result["cleanup"] = scope.cleanup(timeout=2)
        return result
    finally:
        if command:
            command.close()
        if own:
            scope.cleanup(timeout=2)
            scope.close()


def apply_object(result, exit_code, status=None):
    require(not result["timed_out"], "apply timed out; lifecycle outcome unknown")
    require(result["exit"] == exit_code, f"apply exit {result['exit']} != {exit_code}")
    obj = result["json"]
    require(
        isinstance(obj, dict) and obj.get("schema_version") == 1,
        "expected complete apply JSON schema_version=1",
    )
    if status is not None:
        require(obj.get("status") == status, f"apply status {obj.get('status')} != {status}")
    require(isinstance(obj.get("selected"), list), "missing selected array")
    return obj


def committed_completion(run, completion):
    if not run or not completion:
        return False
    result = completion.get("result") or {}
    terminal = {"completed", "failed", "cancelled"}
    return (
        run.get("state") in terminal
        and result.get("state") == run.get("state")
        and bool(run.get("finished_at_unix_ms"))
        and bool(result.get("finished_at_unix_ms"))
        and all(
            run.get(key) and run.get(key) == result.get(key) for key in ("run_id", "attempt_id")
        )
        and len(completion.get("bundle_blake3", "")) == 64
        and "unknown" not in json.dumps(result.get("failure", {})).lower()
    )


class Driver:
    def __init__(self, options):
        self.options = options
        self.started = time.monotonic()
        self.deadline = self.started + options.seconds
        self.root = fresh_output(options.output)
        self.events = (self.root / "evidence.jsonl").open("x", buffering=1)
        self.evidence_bytes = 0
        self.counts = {name: 0 for name in SCENARIOS}
        self.active = 0.0
        self.round = 0
        self.scenario = "setup"
        self.case = None
        self.scope = None
        self.watched = []
        self.command_number = 0
        self.last_progress = self.started
        self.retained_cases = []
        self.outcomes = {"pass": 0, "miss": 0, "unknown_verified": 0}
        self.injections = {"injected": 0, "miss": 0}
        self.cleanup_result = None
        self.emit(
            "config",
            config=vars(options),
            deadline_monotonic=self.deadline,
            started_monotonic=self.started,
            uncovered=UNCOVERED,
        )

    def emit(self, event, **values):
        line = (
            json.dumps(
                dict(
                    event=event,
                    seed=self.options.seed,
                    round=self.round,
                    scenario=self.scenario,
                    monotonic=time.monotonic(),
                    **values,
                ),
                ensure_ascii=False,
                default=str,
            )
            + "\n"
        )
        size = len(line.encode("utf-8"))
        if (
            event not in ("aborted", "failure", "cleanup")
            and self.evidence_bytes + size > self.options.max_evidence_mib * 1024 * 1024
        ):
            raise BudgetExpired("evidence byte cap exhausted; retain current case")
        self.events.write(line)
        self.evidence_bytes += size

    def setup(self):
        require(
            os.getuid() != 0 and os.uname().sysname == "Linux",
            "native stage chaos requires non-root Linux; never sudo",
        )
        require(os.access("/dev/kvm", os.R_OK | os.W_OK), "read/write /dev/kvm required")
        self.scope = Descendants(self.progress)
        safe_ancestors(self.options.binary)
        source = Path(self.options.binary)
        firmware = Path(self.options.firmware)
        for original, name in ((source, "pvisor"), (firmware, "libkrunfw.so.5")):
            require(original.is_file(), f"missing input {original}")
            destination = self.root / name
            shutil.copyfile(original, destination)
            destination.chmod(0o700 if name == "pvisor" else 0o600)
            require(sha256(original) == sha256(destination), "input changed while freezing")
            self.emit("frozen", source=original, frozen=destination, sha256=sha256(destination))
        self.binary = self.root / "pvisor"
        repo = Path(__file__).resolve().parents[1]
        git = {}
        for name, args in (
            ("head", ["rev-parse", "HEAD"]),
            ("status", ["--no-optional-locks", "status", "--porcelain=v1"]),
        ):
            git[name] = run_bounded(
                ["git", "--no-pager", *args],
                repo,
                {"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
                5,
                scope=self.scope,
            )
        self.emit(
            "identity", git=git, uid=os.getuid(), kvm=os.access("/dev/kvm", os.R_OK | os.W_OK)
        )

    def command(self, args, expected=None):
        result = run_bounded(
            [self.binary, *args],
            self.project,
            self.env,
            remaining_timeout(self.deadline, self.case_deadline),
            scope=self.scope,
        )
        self.emit("command", **result, expected=expected)
        require(not result["timed_out"], "command timeout: retain case; owner/runner state unknown")
        return result

    def progress(self):
        now = time.monotonic()
        if now - self.last_progress >= 60:
            data = dict(
                round=self.round,
                scenario=self.scenario,
                wall_elapsed=now - self.started,
                round_counts=self.counts,
                outcomes=self.outcomes,
            )
            print(json.dumps(dict(event="progress", **data)), flush=True)
            self.emit(
                "progress",
                **{key: value for key, value in data.items() if key not in ("round", "scenario")},
            )
            self.last_progress = now

    def start_command(self, args):
        remaining_timeout(self.deadline, self.case_deadline)
        self.command_number += 1
        log = self.case / f"command-{self.command_number:04d}"
        command = WatchedCommand([self.binary, *args], self.project, self.env, self.scope, log)
        self.watched.append(command)
        self.emit("command_start", args=command.args, pid_identity=command.identity, logfile=log)
        return command

    def finish_commands(self, commands, deadline=None):
        end = min(
            self.deadline,
            self.case_deadline,
            deadline if deadline is not None else min(c.start + 30 for c in commands),
        )
        while any(not c.done() for c in commands) and time.monotonic() < end:
            for command in commands:
                if not command.done():
                    command.pump(0.001)
        results = []
        for command in commands:
            result = command.finish(end)
            self.emit("command", **result)
            results.append(result)
        require(
            all(not result["timed_out"] for result in results),
            "watched command timeout; outcome unknown, case retained",
        )
        return results

    def cleanup(self, timeout=5):
        if self.scope is None:
            return dict(complete=True, survivors=[], elapsed=0, signals=[])
        result = self.scope.cleanup(timeout)
        self.cleanup_result = result
        self.emit("cleanup", **result)
        require(result["complete"], "owned descendants survived cleanup; stop before another VM")
        return result

    def finish_case(self, outcome="pass", retain=False):
        self.cleanup(timeout=min(5, max(0, self.case_deadline - time.monotonic())))
        for command in self.watched:
            command.close()
        self.watched.clear()
        self.emit("case_result", path=self.case, outcome=outcome, retained=retain)
        if retain:
            self.retained_cases.append(str(self.case))
        else:
            delete_case(self.root, self.case)
        self.case = None
        return outcome

    def ledger(self):
        value = read_object(self.stage / "apply-ledger.json") or {"records": []}
        require(isinstance(value.get("records"), list), "invalid real ledger records")
        return value

    def oracle(self, name, actual, expected):
        self.emit("oracle", name=name, actual=actual, expected=expected, passed=actual == expected)
        require(actual == expected, f"oracle failed: {name}")

    def apply(self, flags, code=0, status=None, preview=False):
        before = {"project": snapshot(self.project), "stage": snapshot(self.stage)}
        original = self.case / "original-project"
        if original.exists():
            before["original_project"] = snapshot(original)
        result = self.command(
            ["apply", self.stage, *flags, "--json"], dict(exit=code, status=status, preview=preview)
        )
        # Check inertness even if the command returned an unexpected status.
        if preview or code != 0:
            after = {"project": snapshot(self.project), "stage": snapshot(self.stage)}
            if original.exists():
                after["original_project"] = snapshot(original)
            if not preview:
                # Execution may refresh/truncate stale lock owner bookkeeping
                # after a kill, but cannot replace the lock inode or mutate any
                # ledger, upper or target file on refusal.
                for tree in (before, after):
                    lock = tree["stage"].get("job.lock", {})
                    lock.pop("mtime_ns", None)
                    lock.pop("ctime_ns", None)
                    lock.pop("size", None)
                    lock.pop("sha256", None)
            self.oracle("inert project/stage", after, before)
        obj = apply_object(result, code, status)
        if preview:
            require(obj.get("execution") is None, "preview published execution")
        return obj

    def drop(self, success=True):
        before = snapshot(self.project)
        outside = snapshot(self.sentinel)
        result = self.command(["drop", self.stage], dict(success=success))
        self.oracle("drop never undoes target", snapshot(self.project), before)
        self.oracle("drop never writes environment", snapshot(self.sentinel), outside)
        require((result["exit"] == 0) == success, "unexpected drop exit")

    def run_case(self):
        self.case = self.root / f"case-{self.round:07d}"
        self.case.mkdir(mode=0o700)
        self.case_deadline = min(self.deadline, time.monotonic() + 60)
        self.project = self.case / "project"
        self.project.mkdir()
        self.stage = self.case / "stage"
        self.sentinel = self.case / "environment"
        self.sentinel.mkdir()
        (self.sentinel / "sentinel").write_text("HOST ENVIRONMENT UNCHANGED\n")
        self.env = {
            "PATH": "/usr/bin:/bin",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "LD_LIBRARY_PATH": str(self.root),
        }
        for key, directory in (
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("PVISOR_RUN_HOME", "runstore"),
            ("TMPDIR", "tmp"),
        ):
            path = self.case / directory
            path.mkdir(mode=0o700)
            self.env[key] = str(path)
        rng = random.Random(f"{self.options.seed}:{self.round}")
        if self.scenario in FAULT_SCENARIOS:
            return self.fault_case(rng)
        token = f"seed={self.options.seed} round={self.round} token={rng.getrandbits(64)}\n"
        names = [
            "modify.txt",
            "delete.txt",
            "rename-old.txt",
            "space name.txt",
            "中文.txt",
            "literal[ab]*?.txt",
        ]
        for name in names:
            (self.project / name).write_text("original " + name + "\n")
            (self.project / name).chmod(0o640)
        (self.project / "nested").mkdir()
        (self.project / "nested" / "guard.txt").write_text("guard original\n")
        initial = snapshot(self.project)
        outside = snapshot(self.sentinel)
        initial_content = content_snapshot(self.project)
        changed = ["modify.txt", "space name.txt", "中文.txt", "literal[ab]*?.txt"]
        if self.scenario in ("symlink", "nested_all"):
            changed.append("nested/guard.txt")
        rng.shuffle(changed)
        quote = shlex.quote
        script = "set -eu\n" + "\n".join(
            f"printf %s {quote(token + name)} > {quote(name)}" for name in changed
        )
        script += "\nrm delete.txt\nmv rename-old.txt rename-new.txt\n"
        script += f"printf %s {quote(token)} > added.txt\n"
        script += f"printf %s staged-only > {quote(str(self.sentinel / 'sentinel'))}\nexit 23\n"
        self.emit(
            "case",
            path=self.case,
            env=self.env,
            workload=script,
            initial=initial,
            environment=outside,
            changed=changed,
        )
        result = self.command(
            [
                "stage",
                "--stage",
                self.stage,
                "--cpus",
                "1",
                "--memory",
                self.options.memory,
                "--network",
                "none",
                "--",
                "/bin/sh",
                "-c",
                script,
            ],
            dict(exit=23, host_unchanged=True),
        )
        self.oracle("stage host unchanged", snapshot(self.project), initial)
        self.oracle("stage environment unchanged", snapshot(self.sentinel), outside)
        require(result["exit"] == 23, f"stage workload expected exit 23, got {result['exit']}")
        if self.scenario == "symlink":
            self.apply(["--path", "nested/guard.txt", "--dry-run"], status="ready", preview=True)
            (self.project / "nested").rename(self.project / "original-nested")
            (self.sentinel / "guard.txt").write_text("outside guard\n")
            (self.project / "nested").symlink_to(self.sentinel, target_is_directory=True)
            guard = snapshot(self.sentinel)
            self.apply(["--path", "nested/guard.txt"], code=3, status="blocked")
            self.oracle("symlink ancestor no escape", snapshot(self.sentinel), guard)
            self.drop()
            return self.finish_case()
        expected_paths = set(
            changed + ["delete.txt", "rename-old.txt", "rename-new.txt", "added.txt"]
        )
        preview = self.apply(["--all", "--dry-run"], status="ready", preview=True)
        self.oracle(
            "all selected paths",
            sorted(item["path"] for item in preview["selected"]),
            sorted(expected_paths),
        )
        for name in ["space name.txt", "中文.txt", "literal[ab]*?.txt"]:
            obj = self.apply(["--path", name, "--dry-run"], status="ready", preview=True)
            self.oracle("literal path", [item["path"] for item in obj["selected"]], [name])
        before_diff = {"project": snapshot(self.project), "stage": snapshot(self.stage)}
        diff = self.command(
            ["apply", self.stage, "--path", "space name.txt", "--dry-run", "--diff"],
            dict(exit=0, preview=True),
        )
        self.oracle(
            "text diff preview inert",
            {"project": snapshot(self.project), "stage": snapshot(self.stage)},
            before_diff,
        )
        require(diff["exit"] == 0, "text diff preview failed")
        obj = self.apply(
            ["--all", "--exclude", "modify.*", "--dry-run"], status="ready", preview=True
        )
        self.oracle(
            "exclude",
            sorted(item["path"] for item in obj["selected"]),
            sorted(expected_paths - {"modify.txt"}),
        )
        for invalid in ["../environment/sentinel", str(self.sentinel / "sentinel")]:
            self.apply(["--path", invalid, "--dry-run"], code=2, status="error", preview=True)

        if self.scenario == "conflict":
            (self.project / "modify.txt").write_text("external editor\n")
            self.apply(["--path", "modify.txt"], code=3, status="blocked")
            self.oracle(
                "conflict no overwrite",
                (self.project / "modify.txt").read_text(),
                "external editor\n",
            )
            self.drop()

        elif self.scenario == "rebind":
            self.project.rename(self.case / "original-project")
            self.project.mkdir()
            (self.project / "modify.txt").write_text("replacement project\n")
            self.apply(["--all"], code=3, status="blocked")
            self.drop()
        else:
            expected = dict(initial_content)
            # New files respect the guest's umask; existing modes must be preserved.
            for name in changed:
                expected[name] = dict(
                    mode=stat.S_IFREG | 0o640,
                    sha256=hashlib.sha256((token + name).encode()).hexdigest(),
                )
            del expected["delete.txt"]
            expected["rename-new.txt"] = expected.pop("rename-old.txt")
            expected["added.txt"] = dict(
                mode=stat.S_IFREG | 0o644, sha256=hashlib.sha256(token.encode()).hexdigest()
            )
            first = ["modify.txt", "space name.txt", "literal[ab]*?.txt"]
            rng.shuffle(first)
            self.apply([flag for name in first for flag in ("--path", name)], status="applied")
            partial = dict(initial_content)
            partial.update({name: expected[name] for name in first})
            self.oracle("selected exact target", content_snapshot(self.project), partial)
            left = self.apply(["--all", "--dry-run"], status="ready", preview=True)
            self.oracle(
                "unselected retained",
                sorted(item["path"] for item in left["selected"]),
                sorted(expected_paths - set(first)),
            )
            if self.scenario == "project_all":
                obj = self.apply(["--all"], status="applied")
                self.oracle("all exact target", content_snapshot(self.project), expected)
                require(
                    obj.get("remaining", {}).get("in_scope") == 0,
                    "project-all unexpectedly left in-scope paths",
                )
                outside_count = obj["remaining"].get("outside_scope")
                require(
                    isinstance(outside_count, int) and outside_count >= 0,
                    "missing outside-scope count",
                )
                self.emit(
                    "drop_scope", remaining=obj["remaining"], fully_applied=outside_count == 0
                )
                self.drop(success=outside_count > 0)
            else:
                self.apply(["--path", "中文.txt"], status="applied")
                partial["中文.txt"] = expected["中文.txt"]
                self.oracle("second apply exact target", content_snapshot(self.project), partial)
                self.drop()
                self.drop()
        # symlink scenario deliberately added a second sentinel; compare original sentinel itself.
        self.oracle(
            "original environment file unchanged",
            snapshot(self.sentinel)["sentinel"],
            outside["sentinel"],
        )
        return self.finish_case()

    def fault_case(self, rng):
        names = (
            [f"payload-{index:02d}" for index in range(64)]
            if self.scenario == "kill_apply"
            else ["race.txt"]
        )
        old = b"original\n" if len(names) == 1 else b"A" * 65536
        new = (
            f"seed={self.options.seed} round={self.round} token={rng.getrandbits(64)}\n".encode()
            if len(names) == 1
            else rng.randbytes(65536)
        )
        source = self.case / "payload.bin"
        source.write_bytes(new)
        for name in names:
            (self.project / name).write_bytes(old)
            (self.project / name).chmod(0o644)
        initial = snapshot(self.project)
        environment = snapshot(self.sentinel)
        script = "set -eu\n" + "\n".join(
            f"cp {shlex.quote(str(source))} {shlex.quote(name)}" for name in names
        )
        script += (
            f"\nprintf %s staged-environment > {shlex.quote(str(self.sentinel / 'sentinel'))}\n"
        )
        marker = f"STAGE_CHAOS_READY_{self.options.seed}_{self.round}"
        script += f"printf '%s\\n' {shlex.quote(marker)}\n"
        script += "sleep 120\n" if self.scenario == "live_cancel" else "exit 23\n"
        args = [
            "stage",
            "--stage",
            self.stage,
            "--cpus",
            "1",
            "--memory",
            self.options.memory,
            "--network",
            "none",
            "--",
            "/bin/sh",
            "-c",
            script,
        ]
        self.emit(
            "fault_fixture",
            path=self.case,
            workload=script,
            initial=initial,
            environment=environment,
            payload_sha256=sha256(source),
            payload_size=len(new),
        )
        if self.scenario == "live_cancel":
            return self.live_cancel(args, marker, initial, environment)
        result = self.command(args, dict(exit=23, real_native_stage=True))
        require(result["exit"] == 23, "fault fixture did not complete real VM workload")
        self.oracle("fault stage host unchanged", snapshot(self.project), initial)
        self.oracle("fault stage environment unchanged", snapshot(self.sentinel), environment)
        if self.scenario == "kill_apply":
            return self.kill_apply(rng, names, old, new, environment)
        return self.parallel_case(rng, old, new, environment)

    def parallel_case(self, rng, old, new, environment):
        apply_args = ["apply", self.stage, "--path", "race.txt", "--json"]
        commands = [
            apply_args,
            apply_args if self.scenario == "parallel_apply" else ["drop", self.stage],
        ]
        order = [0, 1]
        rng.shuffle(order)
        running = {}
        running[order[0]] = self.start_command(commands[order[0]])
        running[order[1]] = self.start_command(commands[order[1]])
        overlap = running[order[0]].process.poll() is None
        self.emit(
            "parallel_launch",
            order=order,
            overlap_observed=overlap,
            identities=[running[index].identity for index in range(2)],
        )
        results = self.finish_commands([running[0], running[1]])
        ledger = self.ledger()
        self.emit(
            "race_ledger",
            ledger=ledger,
            project=snapshot(self.project),
            overlay=read_object(self.stage / "overlay.json"),
        )
        self.oracle("race environment unchanged", snapshot(self.sentinel), environment)
        records = ledger["records"]
        require(
            all(record.get("state") == "committed" for record in records),
            "race left pending ledger",
        )
        require(len(records) <= 1, "single-file race committed duplicate apply intents")
        target = (self.project / "race.txt").read_bytes()
        require(target in (old, new), "single-file race produced a partial/foreign target")
        require(
            {path.name for path in self.project.iterdir()} == {"race.txt"},
            "race wrote unexpected target paths",
        )
        require(
            (self.project / "race.txt").stat().st_mode == stat.S_IFREG | 0o644,
            "race changed target mode",
        )
        require(
            all(
                len(record.get("changes", [])) == 1
                and record["changes"][0].get("path", "").endswith("/race.txt")
                for record in records
            ),
            "race ledger contains mutations outside the single selected file",
        )
        applied = 0
        for index, result in enumerate(results):
            if commands[index][0] == "apply":
                require(result["exit"] in (0, 3), "unexpected race apply exit")
                obj = apply_object(result, result["exit"])
                if result["exit"] == 0:
                    require(
                        obj.get("status") in ("applied", "no_changes"),
                        "unexpected successful race status",
                    )
                    if obj["status"] == "applied":
                        execution = obj.get("execution") or {}
                        require(
                            execution.get("applied") == 1
                            and execution.get("ledger_confirmed") is True,
                            "race apply success lacks single committed file evidence",
                        )
                        require(
                            any(
                                record.get("apply_id") == execution.get("apply_id")
                                for record in records
                            ),
                            "race apply JSON does not identify the committed ledger",
                        )
                        applied += 1
                else:
                    self.race_blocker(obj.get("blockers", []))
            else:
                require(result["exit"] in (0, 1, 3), "unexpected race drop exit")
                if result["exit"] != 0:
                    self.race_blocker([result["stderr"], result["stdout"]])
        require(applied <= 1, "both concurrent applies claimed the same mutation")
        require(
            (target == new) == (applied == 1 and len(records) == 1),
            "race target disagrees with apply JSON and committed ledger",
        )
        if self.scenario == "parallel_apply":
            require(applied == 1, "apply/apply race had no committed winner")
        elif target == old:
            require(
                results[1]["exit"] == 0 and not records, "old target without a proven drop winner"
            )
        self.emit(
            "serializable",
            winner="apply" if target == new else "drop",
            applied=applied,
            ledger_records=len(records),
            overlap_observed=overlap,
        )
        if not overlap:
            return self.finish_case("miss", retain=True)
        return self.finish_case()

    @staticmethod
    def race_blocker(blockers):
        text = " ".join(str(item) for item in blockers).lower()
        require(
            any(
                word in text
                for word in (
                    "lock",
                    "lease",
                    "busy",
                    "in use",
                    "dropped",
                    "discard",
                    "operation already in progress",
                )
            ),
            f"race refusal lacks lock/terminal-state evidence: {text}",
        )

    def live_cancel(self, args, marker, initial, environment):
        frontend = self.start_command(args)
        ready_end = min(frontend.start + 20, self.deadline, self.case_deadline)
        ready = False
        while time.monotonic() < ready_end and frontend.process.poll() is None:
            frontend.pump()
            run = read_object(self.stage / "run.json")
            if (
                marker.encode() in frontend.buffers["stdout"]
                and run
                and run.get("state") == "running"
            ):
                ready = True
                self.emit(
                    "live_ready",
                    run=run,
                    marker=marker,
                    frontend=frontend.identity,
                    descendants=self.scope.discover(),
                )
                break
        require(ready, "real workload ready marker plus running run record not observed")
        self.apply(["--path", "race.txt"], code=3, status="blocked")
        live_upper = snapshot(self.stage / "upper")
        self.drop(success=False)
        self.oracle("live drop retains upper", snapshot(self.stage / "upper"), live_upper)
        self.oracle("live refusals target unchanged", snapshot(self.project), initial)
        self.oracle("live refusals environment unchanged", snapshot(self.sentinel), environment)
        self.scope.discover()
        require(
            self.scope.send(frontend.identity, signal.SIGTERM), "frontend SIGTERM identity lost"
        )
        sent_at = time.monotonic()
        self.emit("fault_signal", signal="SIGTERM", identity=frontend.identity)
        # Wait at most 25 seconds, reserving five seconds for adopted owners.
        result = frontend.finish(min(sent_at + 25, self.case_deadline - 5, self.deadline))
        self.emit("command", **result)
        cleanup = self.cleanup(
            timeout=min(
                5,
                max(0, sent_at + 30 - time.monotonic()),
                max(0, self.case_deadline - time.monotonic()),
            )
        )
        require(time.monotonic() - sent_at <= 30.1, "SIGTERM cleanup exceeded 30 seconds")
        run = read_object(self.stage / "run.json")
        completion = read_object(self.stage / "completion.json")
        known = committed_completion(run, completion)
        self.emit(
            "cancel_outcome",
            branch="known_committed" if known else "unknown",
            run=run,
            completion=completion,
            cleanup=cleanup,
            project=snapshot(self.project),
            upper=snapshot(self.stage / "upper"),
        )
        self.oracle("cancel host isolation", snapshot(self.project), initial)
        self.oracle("cancel environment isolation", snapshot(self.sentinel), environment)
        if known:
            review = self.command(["review", self.stage])
            require(
                review["exit"] == 0, "known committed cancellation/completion cannot be reviewed"
            )
            self.drop()
            return self.finish_case()
        # Never equate an absent completion with no changes or with cancellation.
        self.apply(["--path", "race.txt"], code=3, status="blocked")
        before = snapshot(self.stage)
        self.drop(success=False)
        after = snapshot(self.stage)
        for tree in (before, after):
            tree.get("job.lock", {}).pop("mtime_ns", None)
            tree.get("job.lock", {}).pop("ctime_ns", None)
            tree.get("job.lock", {}).pop("size", None)
            tree.get("job.lock", {}).pop("sha256", None)
        self.oracle("unknown drop retains stage", after, before)
        return self.finish_case("unknown_verified", retain=True)

    def kill_apply(self, rng, names, old, new, environment):
        upper_before = content_snapshot(self.stage / "upper")
        command = self.start_command(["apply", self.stage, "--all", "--json"])
        self.injections["miss"] += 1
        end = min(command.start + 30, self.case_deadline, self.deadline)
        gate = None
        injected = False
        delay = rng.uniform(0, 0.002)
        while command.process.poll() is None and time.monotonic() < end:
            command.pump(0)
            ledger = self.ledger()
            prepared = [record for record in ledger["records"] if record.get("state") == "prepared"]
            if prepared:
                time.sleep(min(delay, max(0, end - time.monotonic())))
                confirm = self.ledger()
                still = [
                    record
                    for record in confirm["records"]
                    if record.get("state") == "prepared"
                    and record.get("apply_id") == prepared[0].get("apply_id")
                ]
                if still:
                    gate = dict(
                        observed=prepared[0],
                        confirmed=still[0],
                        monotonic=time.monotonic(),
                        identity=command.identity,
                        delay=delay,
                    )
                    # Persist observation before signalling: evidence-cap failure
                    # must never cause an unrecorded fault to be called injected.
                    self.emit("prepared_observation", gate=gate)
                    injected = self.scope.send(command.identity, signal.SIGKILL)
                    if injected:
                        self.injections["miss"] -= 1
                        self.injections["injected"] += 1
                    self.emit("prepared_gate", injected=injected, gate=gate)
                    break
            time.sleep(0.001)
        result = self.finish_commands([command], deadline=end)[0]
        ledger = self.ledger()
        self.emit(
            "fault_injection",
            status="injected" if injected else "miss",
            gate=gate,
            ledger=ledger,
            project=snapshot(self.project),
            upper=snapshot(self.stage / "upper"),
        )
        self.oracle("kill-apply environment unchanged", snapshot(self.sentinel), environment)
        states = [record.get("state") for record in ledger["records"]]
        require(
            all(state in ("prepared", "target_applied", "committed") for state in states),
            "unexpected apply ledger state",
        )
        old_hash, new_hash = hashlib.sha256(old).hexdigest(), hashlib.sha256(new).hexdigest()
        target = snapshot(self.project)
        for name in names:
            require(
                target.get(name, {}).get("sha256") in (old_hash, new_hash),
                f"crash target {name} is not a complete old/new file",
            )
            require(target[name]["mode"] == stat.S_IFREG | 0o644, "crash target mode changed")
        # A crash can leave the product's private atomic-publication temp namespace.
        for name, item in target.items():
            if name == "." or name in names:
                continue
            require(
                name.split("/")[0].startswith(".pvisor-apply-")
                and item["uid"] == os.getuid()
                and not stat.S_ISLNK(item["mode"]),
                "unexpected crash target path/escape",
            )
            if "/" not in name:
                require(
                    stat.S_ISDIR(item["mode"]) and not item["mode"] & 0o077,
                    "crash publication namespace is not private",
                )
        pending = any(state != "committed" for state in states)
        if injected:
            require(
                result["exit"] == -signal.SIGKILL,
                "injection did not kill the observed apply process",
            )
            require(bool(states), "observed Prepared ledger disappeared after kill")
        if pending:
            if "prepared" in states:
                self.oracle(
                    "Prepared retains all upper payload",
                    content_snapshot(self.stage / "upper"),
                    upper_before,
                )
            self.apply(["--all", "--dry-run"], code=3, status="blocked", preview=True)
            before = snapshot(self.stage)
            self.drop(success=False)
            after = snapshot(self.stage)
            for tree in (before, after):
                tree.get("job.lock", {}).pop("mtime_ns", None)
                tree.get("job.lock", {}).pop("ctime_ns", None)
                tree.get("job.lock", {}).pop("size", None)
                tree.get("job.lock", {}).pop("sha256", None)
            self.oracle("pending drop retains ledger and upper", after, before)
        else:
            require(
                bool(states) and all(state == "committed" for state in states),
                "apply exited without a ledger or a gated fault",
            )
            require(
                all(target[name]["sha256"] == new_hash for name in names),
                "Committed ledger disagrees with completed project payload",
            )
        # Keep the real ledger, upper, temporary publication state and bounded logs.
        return self.finish_case("pass" if injected else "miss", retain=True)

    def run(self):
        failure = None
        aborted = False
        completed = 0
        try:
            self.setup()
            if time.monotonic() >= self.deadline:
                raise BudgetExpired("deadline exhausted during input setup")
            while time.monotonic() < self.deadline and (
                self.options.rounds is None or completed < self.options.rounds
            ):
                if (
                    self.options.rounds is None
                    and completed
                    and self.deadline - time.monotonic() < 60
                ):
                    self.emit(
                        "budget_complete",
                        reason="insufficient budget for another bounded case",
                        remaining_seconds=max(0, self.deadline - time.monotonic()),
                    )
                    break
                self.round = completed + 1
                self.scenario = (
                    self.options.scenario or DEFAULT_SCENARIOS[completed % len(DEFAULT_SCENARIOS)]
                )
                start = time.monotonic()
                try:
                    outcome = self.run_case() or "pass"
                finally:
                    self.active += time.monotonic() - start
                completed += 1
                self.outcomes[outcome] += 1
                if outcome == "pass":
                    self.counts[self.scenario] += 1
                self.progress()
        except (KeyboardInterrupt, BudgetExpired) as error:
            aborted = True
            failure = f"{type(error).__name__}: {error}"
            self.emit("aborted", error=failure, retained_case=self.case)
        except Exception as error:
            failure = f"{type(error).__name__}: {error}"
            self.emit(
                "failure", error=failure, traceback=traceback.format_exc(), retained_case=self.case
            )
        finally:
            if self.case is not None and str(self.case) not in self.retained_cases:
                self.retained_cases.append(str(self.case))
            try:
                self.cleanup(timeout=5)
            except Exception as error:
                failure = failure or f"cleanup failed: {error}"
            for command in self.watched:
                command.close()
            if self.scope is not None and self.cleanup_result and self.cleanup_result["complete"]:
                self.scope.close()
            summary = dict(
                seed=self.options.seed,
                aborted=aborted,
                failure=failure,
                completed_rounds=sum(self.counts.values()),
                finished_rounds=completed,
                attempted_rounds=self.round,
                outcomes=self.outcomes,
                round_counts=self.counts,
                active_elapsed=self.active,
                wall_elapsed=time.monotonic() - self.started,
                evidence_bytes=self.evidence_bytes,
                deadline_monotonic=self.deadline,
                retained_case=self.case,
                retained_cases=self.retained_cases,
                cleanup=self.cleanup_result,
                owned_signals=self.scope.signals if self.scope else [],
                uncovered=UNCOVERED,
                injections=self.injections,
                fault_injection="injected" if self.injections["injected"] else "not_injected",
            )
            (self.root / "summary.json").write_text(
                json.dumps(summary, indent=2, default=str) + "\n"
            )
            self.events.close()
            print(json.dumps(summary, indent=2, default=str), flush=True)
        return (
            1
            if failure
            else (2 if self.outcomes["miss"] or self.outcomes["unknown_verified"] else 0)
        )


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--binary", type=Path, required=True)
    result.add_argument("--firmware", type=Path, required=True)
    result.add_argument("--output", type=Path, required=True)
    result.add_argument("--seed", type=int, required=True)
    result.add_argument("--seconds", type=float, required=True)
    result.add_argument("--rounds", type=int)
    result.add_argument("--memory", choices=("128m", "256m", "512m"), default="256m")
    result.add_argument("--scenario", choices=SCENARIOS)
    result.add_argument("--max-evidence-mib", type=int, default=1024)
    return result


def main(argv=None):
    args = parser().parse_args(argv)
    if not math.isfinite(args.seconds) or not 0 < args.seconds <= 86400:
        raise SystemExit("--seconds must be finite and in (0, 86400]")
    if not 1 <= args.max_evidence_mib <= 8192:
        raise SystemExit("--max-evidence-mib must be in [1, 8192]")
    if args.rounds is not None and not 1 <= args.rounds <= 1000000:
        raise SystemExit("--rounds must be in [1, 1000000]")
    args.binary = Path(os.path.abspath(args.binary))
    args.firmware = Path(os.path.abspath(args.firmware))
    args.output = Path(os.path.abspath(args.output))

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")

    previous = signal.signal(signal.SIGTERM, interrupted)
    try:
        return Driver(args).run()
    except (ValueError, OSError, AssertionError) as error:
        print(f"setup refused: {error}")
        return 1
    finally:
        signal.signal(signal.SIGTERM, previous)


if __name__ == "__main__":
    raise SystemExit(main())
