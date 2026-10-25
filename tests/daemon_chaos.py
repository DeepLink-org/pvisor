#!/usr/bin/env python3
"""Opt-in bounded OpenSandbox SDK correctness/chaos driver; not a benchmark.

Run under Linux with KVM, a delegated systemd user manager, a test-owned
aggregate slice, a frozen daemon and a native prepared image. SDK imports occur
only in the isolated worker, so ordinary test discovery needs no SDK.
"""

import argparse
import asyncio
import base64
import errno
import hashlib
import json
import os
import random
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback
import urllib.error
import urllib.request
import uuid
from collections import Counter
from pathlib import Path


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    temporary.chmod(0o600)
    temporary.replace(path)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def output(execution, channel="stdout"):
    return "".join(item.text for item in getattr(execution.logs, channel))


def expected_reader_error(connection_error, status, sandbox, deleting, retired, overlaps):
    """Revoked credentials are expected only for this reader's deleted identity."""
    return overlaps and (
        connection_error
        or status in {404, 409, 502, 503}
        or (status == 401 and sandbox in deleting | retired)
    )


def observe_guests(group):
    """Fail on a resource contract mismatch; ignore only a concurrently removed cgroup."""
    guests = {}
    for child in group.iterdir():
        if not child.is_dir() or child.name == "api":
            continue
        try:
            pids = (child / "cgroup.procs").read_text().split()
            if not pids:
                guests[child.name] = {"pids": []}
                continue
            limits = {
                name: (child / name).read_text().strip()
                for name in ("memory.max", "memory.swap.max", "cpu.max", "pids.max")
            }
            assert limits["memory.max"] == str(256 * 1024**2), limits
            assert limits["memory.swap.max"] == "0" and limits["pids.max"] == "512", limits
            assert limits["cpu.max"] == "100000 100000", limits
            events = dict(
                (k, int(v))
                for k, v in (
                    line.split() for line in (child / "memory.events").read_text().splitlines()
                )
            )
            assert not any(events.get(k, 0) for k in ["oom", "oom_kill", "max"]), events
            pids_current = int((child / "pids.current").read_text())
            pids_events = dict(
                (k, int(v))
                for k, v in (
                    line.split() for line in (child / "pids.events").read_text().splitlines()
                )
            )
            assert pids_current <= 512 and pids_events.get("max", 0) == 0, (
                pids_current,
                pids_events,
            )
            processes = {}
            for pid in pids:
                try:
                    proc = Path("/proc") / pid
                    nofile = next(
                        line.split()[3:5]
                        for line in (proc / "limits").read_text().splitlines()
                        if line.startswith("Max open files")
                    )
                    threads = next(
                        int(line.split()[1])
                        for line in (proc / "status").read_text().splitlines()
                        if line.startswith("Threads:")
                    )
                    processes[pid] = {
                        "fds": len(list((proc / "fd").iterdir())),
                        "nofile": nofile,
                        "threads": threads,
                    }
                except (FileNotFoundError, ProcessLookupError):
                    continue
            guests[child.name] = {
                "limits": limits,
                "memory": int((child / "memory.current").read_text()),
                "peak": int((child / "memory.peak").read_text()),
                "pids": pids,
                "processes": processes,
                "pids_current": pids_current,
                "pids_events": pids_events,
            }
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENODEV):
                raise
    assert len([g for g in guests.values() if g["pids"]]) <= 4, guests
    return guests


def api(cfg, method, path):
    request = urllib.request.Request(
        "http://" + cfg["domain"] + path,
        headers={"OPEN-SANDBOX-API-KEY": cfg["key"]},
        method=method,
    )
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(
        request, timeout=20
    ) as r:
        body = r.read()
        return r.status, json.loads(body) if body else None


def owned_sandbox(cfg, record):
    metadata = record.get("metadata")
    return (
        bool(cfg.get("ownership"))
        and isinstance(metadata, dict)
        and metadata.get("pvisor-chaos-owner") == cfg["ownership"]
    )


def verify_private_api(cfg):
    assert api(cfg, "GET", "/v1/sandboxes")[1]["items"] == [], "test API state is not empty"
    wrong = {**cfg, "key": uuid.uuid4().hex}
    try:
        api(wrong, "GET", "/v1/sandboxes")
    except urllib.error.HTTPError as error:
        assert error.code == 401, error.code
    else:
        raise AssertionError("test API did not reject an unrelated credential")


def serve(config):
    cfg = json.loads(Path(config).read_text())
    relative = Path("/proc/self/cgroup").read_text().strip().split(":", 2)[2]
    group = Path("/sys/fs/cgroup") / relative.lstrip("/")
    (group / "api").mkdir(exist_ok=True)
    (group / "api/cgroup.procs").write_text(str(os.getpid()))
    (group / "cgroup.subtree_control").write_text("+cpu +memory +pids")
    save(Path(cfg["output"]) / "service-group.json", {"path": str(group)})
    argv = [
        cfg["binary"],
        "serve",
        "--sandbox",
        "--listen",
        cfg["domain"],
        "--images-dir",
        cfg["images"],
        "--state",
        cfg["state"],
        "--cgroup-root",
        str(group),
        "--max-sandboxes",
        "4",
        "--cpu-millis",
        "4000",
        "--memory-bytes",
        str(1024 * 1024**2),
    ]
    # Keep the delegated unit alive while only the API process crashes. Restarting
    # the systemd unit itself can reset delegated child resource attributes.
    stopping = False
    child = None

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True
        if child is not None and child.poll() is None:
            child.terminate()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    while not stopping:
        child = subprocess.Popen(argv, env={**os.environ, "OPEN_SANDBOX_API_KEY": cfg["key"]})
        save(Path(cfg["output"]) / "api-pid.json", {"pid": child.pid})
        code = child.wait()
        event = {"pid": child.pid, "code": code, "stopping": stopping, "time": time.time()}
        with (Path(cfg["output"]) / "api-exits.jsonl").open("a") as log:
            log.write(json.dumps(event) + "\n")
        if not stopping:
            expected = Path(cfg["output"]) / "expected-api-exits.jsonl"
            injected = expected.exists() and any(
                json.loads(line)["pid"] == child.pid for line in expected.read_text().splitlines()
            )
            if code != -signal.SIGKILL or not injected:
                save(Path(cfg["output"]) / "unexpected-api-exit.json", event)
        if not stopping:
            time.sleep(0.1)


def crash_api(cfg):
    pid = json.loads((Path(cfg["output"]) / "api-pid.json").read_text())["pid"]
    descriptor = os.pidfd_open(pid)
    try:
        argv = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        assert argv[0].decode() == cfg["binary"] and cfg["state"].encode() in argv
        with (Path(cfg["output"]) / "expected-api-exits.jsonl").open("a") as log:
            log.write(json.dumps({"pid": pid, "time": time.time()}) + "\n")
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
    finally:
        os.close(descriptor)


async def worker(cfg):
    import importlib.metadata
    from datetime import timedelta

    from opensandbox.config import ConnectionConfig
    from opensandbox.exceptions.sandbox import (
        SandboxApiException,
        SandboxConnectionException,
        SandboxTimeoutException,
    )
    from opensandbox.models.execd import RunCommandOpts
    from opensandbox.models.filesystem import (
        DirectoryListEntry,
        MoveEntry,
        SetPermissionEntry,
        WriteEntry,
    )
    from opensandbox.sandbox import Sandbox

    assert importlib.metadata.version("opensandbox") == "1.1.0"
    root = Path(cfg["output"])
    rng = random.Random(cfg["seed"])
    sandboxes = []
    owned = []
    tokens = {}
    models = {}
    deleting = set()
    retired = set()
    counts = {}
    background_errors = []
    fault_epochs = {}
    faulting = set()
    stopped = False
    started = time.monotonic()
    connections = [
        ConnectionConfig(
            domain=cfg["domain"],
            protocol="http",
            api_key=cfg["key"],
            use_server_proxy=bool(i % 2),
        )
        for i in range(4)
    ]

    def record(kind, **fields):
        counts[kind] = counts.get(kind, 0) + 1
        with (root / "operations.jsonl").open("a") as log:
            log.write(
                json.dumps(
                    {"seconds": time.monotonic() - started, "kind": kind, **fields},
                    ensure_ascii=False,
                )
                + "\n"
            )

    async def new(index):
        token = f"sandbox-{cfg['seed']}-{index}-{rng.getrandbits(64):016x}"
        sb = await Sandbox.create(
            "native-ubuntu",
            connection_config=connections[index],
            resource={"cpu": "1", "memory": "256Mi"},
            entrypoint=["/bin/sleep", "infinity"],
            env={"CHAOS_TOKEN": token},
            metadata={"pvisor-chaos-owner": cfg["ownership"]},
            timeout=timedelta(seconds=cfg["seconds"] + 60),
            ready_timeout=timedelta(seconds=30),
        )
        # Register ownership before any correctness check can fail.
        owned.append(sb)
        assert owned_sandbox(
            cfg, (await asyncio.to_thread(api, cfg, "GET", "/v1/sandboxes/" + sb.id))[1]
        ), "sandbox ownership metadata missing"
        tokens[sb.id] = token.encode()
        models[sb.id] = {}
        fault_epochs[sb.id] = 0
        await sb.files.create_directories([WriteEntry(path="/tmp/chaos", mode=755)])
        await sb.files.write_file("/tmp/chaos/heartbeat", token.encode(), mode=600)
        result = await sb.commands.run('printf %s "$CHAOS_TOKEN"')
        assert result.exit_code == 0 and output(result).encode() == tokens[sb.id]
        # Publish to the continuous reader only after initialization commits.
        sandboxes.append(sb)
        record("create", id=sb.id, index=index, token_sha256=sha(token.encode()))
        return sb

    async def check_bytes(sb, path, expected):
        try:
            actual = await sb.files.read_bytes(path)
        except Exception as error:
            raise AssertionError(
                f"read failed: sandbox={sb.id} path={path!r} expected_bytes={len(expected)}: {error}"
            ) from error
        if actual != expected:
            save(
                root / "corruption.json",
                {
                    "sandbox": sb.id,
                    "path": path,
                    "expected_sha256": sha(expected),
                    "actual_sha256": sha(actual),
                    "actual_base64": base64.b64encode(actual).decode(),
                },
            )
            raise AssertionError(f"byte corruption: {sb.id} {path}")
        record("read", id=sb.id, path=path, bytes=len(actual), sha256=sha(actual))

    async def background():
        while not stopped:
            if not sandboxes:
                await asyncio.sleep(0.02)
                continue
            sb = sandboxes[counts.get("background_read", 0) % len(sandboxes)]
            epoch, during = fault_epochs[sb.id], sb.id in faulting
            try:
                actual = await sb.files.read_bytes("/tmp/chaos/heartbeat")
                if actual != tokens[sb.id]:
                    save(
                        root / "background-corruption.json",
                        {
                            "sandbox": sb.id,
                            "actual_base64": base64.b64encode(actual).decode(),
                            "expected_base64": base64.b64encode(tokens[sb.id]).decode(),
                            "epoch": epoch,
                            "faulting_at_start": during,
                        },
                    )
                assert actual == tokens[sb.id], "cross-sandbox or corrupted background bytes"
                record("background_read", id=sb.id, bytes=len(actual), sha256=sha(actual))
            except asyncio.CancelledError:
                raise
            except Exception as error:
                # Content mismatches remain failures even inside an injected fault.
                status = error.status_code if isinstance(error, SandboxApiException) else None
                expected_error = expected_reader_error(
                    isinstance(error, (SandboxConnectionException, SandboxTimeoutException)),
                    status,
                    sb.id,
                    deleting,
                    retired,
                    during or sb.id in faulting or epoch != fault_epochs[sb.id],
                )
                if not expected_error:
                    save(
                        root / "background-error.json",
                        {
                            "sandbox": sb.id,
                            "status": status,
                            "error": repr(error),
                            "epoch_at_start": epoch,
                            "epoch_at_error": fault_epochs[sb.id],
                            "faulting_at_start": during,
                            "faulting_at_error": sb.id in faulting,
                            "deleting": sorted(deleting),
                            "retired": sorted(retired),
                        },
                    )
                    background_errors.append(repr(error))
                    return
                record(
                    "expected_revoked_token" if status == 401 else "expected_disconnect",
                    id=sb.id,
                    error=repr(error),
                    epoch_at_start=epoch,
                    epoch_at_error=fault_epochs[sb.id],
                    faulting_at_start=during,
                    faulting_at_error=sb.id in faulting,
                )
            await asyncio.sleep(0.01)

    async def file_operation(sb, slot, seed):
        local = random.Random(seed)
        payload = local.randbytes(local.choice([0, 1, 31, 4095, 4096, 8193, 32769, 65537]))
        path = f"/tmp/chaos/slot {slot} ✓"
        temporary = path + ".next"
        record(
            "file_start", id=sb.id, slot=slot, seed=seed, bytes=len(payload), sha256=sha(payload)
        )
        await sb.files.write_files([WriteEntry(path=temporary, data=payload, mode=600)])
        await sb.files.move_files([MoveEntry(src=temporary, dest=path)])
        models[sb.id][path] = payload
        await check_bytes(sb, path, payload)
        info = (await sb.files.get_file_info([path]))[path]
        assert info.size == len(payload), (path, info, len(payload))
        listing = await sb.files.list_directory(DirectoryListEntry(path="/tmp/chaos"))
        assert path in {entry.path for entry in listing}, (path, listing)
        if payload:
            offset = local.randrange(len(payload))
            length = local.randint(1, len(payload) + 17)
            part = await sb.files.read_bytes(
                path, range_header=f"bytes={offset}-{offset + length - 1}"
            )
            assert part == payload[offset : offset + length], "range content mismatch"
        mode = local.choice([0, 400, 600, 640, 644])
        await sb.files.set_permissions([SetPermissionEntry(path=path, mode=mode)])
        info = (await sb.files.get_file_info([path]))[path]
        assert info.mode == mode, (info.mode, mode)
        digest = await sb.commands.run(["/usr/bin/sha256sum", path])
        assert digest.exit_code == 0 and output(digest).split()[0] == sha(payload), repr(digest)
        record("file_roundtrip", id=sb.id, slot=slot, seed=seed, bytes=len(payload), mode=mode)

    async def command_operation(sb, seed):
        local = random.Random(seed)
        token = "literal $HOME; ' ✓ " + f"{seed:x}"
        code = local.choice([0, 3, 7, 23, 125])
        result = await sb.commands.run(
            [
                "/bin/sh",
                "-c",
                'printf %s "$1"; printf %s "$2" >&2; exit "$3"',
                "chaos",
                token,
                token[::-1],
                str(code),
            ]
        )
        assert (
            result.exit_code == code
            and output(result) == token
            and output(result, "stderr") == token[::-1]
        ), repr(result)
        record("command", id=sb.id, seed=seed, exit_code=code)

    async def jobs(sb, seed):
        token = f"background-{seed}"
        job = await sb.commands.run(
            f"sleep 0.03; printf %s {token}", opts=RunCommandOpts(background=True)
        )
        for _ in range(200):
            status = await sb.commands.get_command_status(job.id)
            if not status.running:
                break
            await asyncio.sleep(0.01)
        assert not status.running and status.exit_code == 0, status
        log = await sb.commands.get_background_command_logs(job.id)
        assert log.content == token, log
        assert (
            await sb.commands.get_background_command_logs(job.id, cursor=log.cursor)
        ).content == ""
        record("background_command", id=sb.id)

    async def append(sb, seed):
        local = random.Random(seed)
        sizes = [local.randint(1, 8193) for _ in range(8)]
        path = "/tmp/chaos/append"
        await sb.files.write_file(path, b"", mode=600)
        results = await asyncio.gather(
            *(
                sb.commands.run(
                    f"head -c {size} /dev/zero | tr '\\000' {chr(65 + index)} >> {path}"
                )
                for index, size in enumerate(sizes)
            ),
            return_exceptions=True,
        )
        for result in results:
            if isinstance(result, BaseException):
                raise result
            assert result.exit_code == 0 and not output(result) and not output(result, "stderr"), (
                repr(result)
            )
        actual = await sb.files.read_bytes(path)
        # Compare byte counts, not whole shell records: a shell/tool can split
        # its output into several writes, whose append ordering is unspecified.
        expected = {65 + index: size for index, size in enumerate(sizes)}
        assert len(actual) == sum(sizes) and Counter(actual) == expected, (
            "lost or overlapping append writes"
        )
        record(
            "append",
            id=sb.id,
            seed=seed,
            bytes=len(actual),
            sha256=sha(actual),
            byte_counts=expected,
        )
        assert (await sb.commands.run(["/bin/rm", path])).exit_code == 0

    async def cancellation(sb, seed):
        marker, late = f"/tmp/chaos/cancel-{seed}", f"/tmp/chaos/late-{seed}"
        task = asyncio.create_task(
            sb.commands.run(f"printf started > {marker}; sleep 0.5; printf escaped > {late}")
        )
        try:
            for _ in range(100):
                result = await sb.commands.run(f"test -e {marker}")
                if result.exit_code == 0:
                    break
                await asyncio.sleep(0.01)
            assert result.exit_code == 0, "cancelled command never started"
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
            await asyncio.sleep(0.6)
            result = await sb.commands.run(
                f"test ! -e {late}; status=$?; rm -f {marker} {late}; exit $status"
            )
            assert result.exit_code == 0, "cancelled foreground process continued"
            record("cancel", id=sb.id)
        finally:
            if not task.done():
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    async def reconcile():
        for sb in list(sandboxes):
            await check_bytes(sb, "/tmp/chaos/heartbeat", tokens[sb.id])
            for path, value in models[sb.id].items():
                await check_bytes(sb, path, value)

    traffic = None
    summary = {"passed": False, "seed": cfg["seed"]}
    try:
        # Observe all creates before raising, preserving every accepted identity.
        results = await asyncio.gather(*(new(i) for i in range(4)), return_exceptions=True)
        for result in results:
            if isinstance(result, BaseException):
                raise result
        sandboxes[:] = results
        traffic = asyncio.create_task(background())
        cases = [
            "files",
            "commands",
            "jobs",
            "pause",
            "restart_api",
            "recreate",
            "cancel",
            "append",
            "background_crash",
        ]
        weights = [12, 8, 3, 1, 1, 1, 1, 3, 1]
        if cfg.get("profile", "chaos") == "steady":
            cases = ["files", "commands", "jobs", "pause", "cancel", "append"]
            weights = [12, 8, 3, 1, 1, 3]
        schedule = cases + rng.choices(cases, weights=weights, k=max(0, cfg["rounds"] - len(cases)))
        save(root / "schedule.json", schedule)
        for number, case in enumerate(schedule):
            save(root / "current.json", {"round": number + 1, "case": case})
            assert time.monotonic() - started < cfg["seconds"], "daemon chaos deadline exceeded"
            assert not background_errors, background_errors
            assert not (root / "unexpected-api-exit.json").exists(), "unplanned API process exit"
            sb = rng.choice(sandboxes)
            if case in {"pause", "restart_api", "recreate", "background_crash"}:
                affected = (
                    {s.id for s in sandboxes}
                    if case in {"restart_api", "background_crash"}
                    else {sb.id}
                )
                faulting.update(affected)
                for identity in affected:
                    fault_epochs[identity] += 1
                record("fault_start", case=case, ids=sorted(affected), round=number)
                try:
                    if case == "pause":
                        await sb.pause()
                        await asyncio.sleep(rng.uniform(0.005, 0.05))
                        resumed = await Sandbox.resume(sb.id, connection_config=connections[0])
                        await resumed.close()
                    elif case in {"restart_api", "background_crash"}:
                        if case == "background_crash":
                            commit_seed = rng.getrandbits(64)
                            commit_path = f"/tmp/chaos/background-{commit_seed}"
                            gate = commit_path + ".gate"
                            token = f"commit-{commit_seed:x}"
                            job = await sb.commands.run(
                                f"while ! test -e {gate}; do sleep 0.01; done; printf %s {token} > {commit_path}.next && mv {commit_path}.next {commit_path} && printf %s {token}",
                                opts=RunCommandOpts(background=True),
                            )
                            assert (await sb.commands.get_command_status(job.id)).running
                        await asyncio.to_thread(crash_api, cfg)
                        for _ in range(200):
                            try:
                                code, body = await asyncio.to_thread(
                                    api, cfg, "GET", "/v1/sandboxes"
                                )
                                if code == 200 and {x["id"] for x in body["items"]} == {
                                    x.id for x in sandboxes
                                }:
                                    break
                            except (OSError, urllib.error.URLError):
                                pass
                            await asyncio.sleep(0.025)
                        else:
                            raise TimeoutError("daemon did not recover owned identities")
                        if case == "background_crash":
                            assert (await sb.commands.get_command_status(job.id)).running, (
                                "API death terminated the acknowledged background process"
                            )
                            await sb.files.write_file(gate, b"commit", mode=600)
                            for _ in range(200):
                                status = await sb.commands.get_command_status(job.id)
                                if not status.running:
                                    break
                                await asyncio.sleep(0.01)
                            assert not status.running and status.exit_code == 0, status
                            logs = await sb.commands.get_background_command_logs(job.id)
                            actual = await sb.files.read_bytes(commit_path)
                            assert logs.content == token and actual == token.encode()
                            record(
                                "background_commit",
                                id=sb.id,
                                seed=commit_seed,
                                sha256=sha(actual),
                                bytes=len(actual),
                            )
                            assert (
                                await sb.commands.run(
                                    ["/bin/rm", "-f", gate, commit_path, commit_path + ".next"]
                                )
                            ).exit_code == 0
                    else:
                        deleting.add(sb.id)
                        record("delete_start", id=sb.id)
                        await sb.kill()
                        retired.add(sb.id)
                        deleting.remove(sb.id)
                        await sb.close()
                        sandboxes.remove(sb)
                        owned.remove(sb)
                        models.pop(sb.id)
                        try:
                            await asyncio.to_thread(api, cfg, "GET", "/v1/sandboxes/" + sb.id)
                        except urllib.error.HTTPError as error:
                            assert error.code == 404
                        else:
                            raise AssertionError("deleted sandbox still present")
                        record("delete", id=sb.id)
                        await new(rng.randrange(4))
                    await reconcile()
                    record(case, round=number, id=sb.id)
                finally:
                    for identity in affected:
                        fault_epochs[identity] += 1
                    faulting.difference_update(affected)
                    record("fault_end", case=case, ids=sorted(affected), round=number)
            elif case == "files":
                results = await asyncio.gather(
                    *(
                        file_operation(s, slot, rng.getrandbits(64))
                        for s in sandboxes
                        for slot in range(2)
                    ),
                    return_exceptions=True,
                )
                for result in results:
                    if isinstance(result, BaseException):
                        raise result
                await reconcile()
            elif case == "commands":
                results = await asyncio.gather(
                    *(
                        command_operation(s, rng.getrandbits(64))
                        for s in sandboxes
                        for _ in range(3)
                    ),
                    return_exceptions=True,
                )
                for result in results:
                    if isinstance(result, BaseException):
                        raise result
            elif case == "jobs":
                await asyncio.gather(*(jobs(s, rng.getrandbits(32)) for s in sandboxes))
            elif case == "append":
                results = await asyncio.gather(
                    *(append(s, rng.getrandbits(64)) for s in sandboxes), return_exceptions=True
                )
                for result in results:
                    if isinstance(result, BaseException):
                        raise result
            else:
                await cancellation(sb, rng.getrandbits(32))
            save(
                root / "progress.json",
                {
                    "round": number + 1,
                    "total": len(schedule),
                    "case": case,
                    "counts": counts,
                    "seconds": time.monotonic() - started,
                },
            )
            if number % 25 == 0:
                print(
                    f"round {number + 1}/{len(schedule)}: {case}, operations={sum(counts.values())}",
                    flush=True,
                )
        await reconcile()
        assert not background_errors, background_errors
        assert not (root / "unexpected-api-exit.json").exists(), "unplanned API process exit"
        summary["passed"] = True
    except BaseException as error:
        save(
            root / "failure.json",
            {"error": repr(error), "traceback": traceback.format_exc(), "counts": counts},
        )
        diagnostics = {}
        for sb in list(owned):
            snapshot = {}
            for path in [
                "/proc/1/status",
                "/proc/1/limits",
                "/proc/meminfo",
                "/proc/loadavg",
                "/proc/sys/kernel/threads-max",
                "/proc/sys/kernel/pid_max",
            ]:
                try:
                    data = await asyncio.wait_for(sb.files.read_bytes(path), timeout=0.5)
                    snapshot[path] = data.decode(errors="replace")
                except Exception as diagnostic_error:
                    snapshot[path] = {"error": repr(diagnostic_error)}
            diagnostics[sb.id] = snapshot
        save(root / "failure-guest-proc.json", diagnostics)
        raise
    finally:
        stopped = True
        if traffic:
            traffic.cancel()
            await asyncio.gather(traffic, return_exceptions=True)
        errors = []
        for sb in owned:
            try:
                await sb.kill()
            except Exception as error:
                errors.append(repr(error))
            finally:
                await sb.close()
        summary.update(
            counts=counts,
            background_errors=background_errors,
            cleanup_errors=errors,
            seconds=time.monotonic() - started,
        )
        summary["passed"] &= not errors and not background_errors
        save(root / "summary.json", summary)
        for connection in connections:
            if connection.transport:
                await connection.transport.aclose()


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "_serve":
        serve(sys.argv[2])
        return
    if len(sys.argv) == 3 and sys.argv[1] == "_worker":
        cfg = json.loads(Path(sys.argv[2]).read_text())
        asyncio.run(worker(cfg))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--prepared-manifest", type=Path, required=True)
    parser.add_argument(
        "--pool-store", type=Path, required=True, help="Prepared local pooled-image store"
    )
    parser.add_argument("--sdk-python", type=Path, required=True)
    parser.add_argument("--product-slice", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=11)
    parser.add_argument("--rounds", type=int, default=200)
    parser.add_argument("--seconds", type=int, default=900)
    parser.add_argument("--profile", choices=["chaos", "steady"], default="chaos")
    args = parser.parse_args()
    assert sys.platform == "linux" and args.rounds >= (6 if args.profile == "steady" else 9)
    assert 0 < args.seconds <= 3600
    assert re.fullmatch(r"[A-Za-z0-9_-]+\.slice", args.product_slice)
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, root / "daemon_chaos.py")
    manifest = json.loads(args.prepared_manifest.read_text())
    # This input is a new manifest, never a rewrite of historical evidence.
    assert manifest.get("native_guest"), "requires a native guest template"
    manifest["rootfs"] = str(args.pool_store.resolve(strict=True))
    manifest["pooled_image"]["cache"]["location"] = str(
        (args.pool_store / "images/pool-v2").resolve(strict=True)
    )
    manifest["library_dir"] = str(args.binary.resolve().parent)
    images = root / "images"
    images.mkdir()
    save(images / "native-ubuntu.json", manifest)
    state = Path(tempfile.mkdtemp(prefix="pvsoak-"))
    ports = random.Random(args.seed)
    # Keep this controller-owned endpoint below the host ephemeral client range.
    for _ in range(100):
        with socket.socket() as listener:
            try:
                listener.bind(("127.0.0.1", ports.randrange(18000, 28000)))
            except OSError:
                continue
            domain = "127.0.0.1:" + str(listener.getsockname()[1])
            break
    else:
        raise RuntimeError("no free test API port")
    unit = "pvsoakdaemon" + uuid.uuid4().hex[:10] + ".service"
    cfg = {
        "output": str(root),
        "binary": str(args.binary.resolve()),
        "images": str(images),
        "state": str(state),
        "domain": domain,
        "key": "pvisor-local-chaos-" + uuid.uuid4().hex,
        "unit": unit,
        "seed": args.seed,
        "rounds": args.rounds,
        "seconds": args.seconds,
        "profile": args.profile,
        "ownership": uuid.uuid4().hex,
    }
    save(root / "config.json", cfg)
    save(
        root / "inputs.json",
        {
            "binary_sha256": sha(args.binary.read_bytes()),
            "driver_sha256": sha((root / "daemon_chaos.py").read_bytes()),
            "original_manifest_sha256": sha(args.prepared_manifest.read_bytes()),
            "derived_manifest_sha256": sha((images / "native-ubuntu.json").read_bytes()),
            "pool_handle": manifest["pooled_image"]["cache"]["handle"],
        },
    )
    child = None
    verified_api = False
    try:
        with (root / "api.log").open("wb") as log:
            subprocess.run(
                [
                    "systemd-run",
                    "--user",
                    "--quiet",
                    "--collect",
                    "--unit=" + unit,
                    "--slice=" + args.product_slice,
                    "-p",
                    "Delegate=yes",
                    "-p",
                    "KillMode=mixed",
                    "-p",
                    "TimeoutStopSec=10",
                    "-p",
                    f"RuntimeMaxSec={args.seconds + 90}",
                    sys.executable,
                    str(root / "daemon_chaos.py"),
                    "_serve",
                    str(root / "config.json"),
                ],
                stdout=log,
                stderr=subprocess.STDOUT,
                check=True,
            )
        for _ in range(300):
            try:
                if api(cfg, "GET", "/v1/sandboxes")[0] == 200:
                    break
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.025)
        else:
            raise TimeoutError("daemon startup failed")
        verify_private_api(cfg)
        verified_api = True
        with (root / "worker.log").open("wb") as log:
            child = subprocess.Popen(
                [
                    "systemd-run",
                    "--user",
                    "--scope",
                    "--quiet",
                    "--slice=" + args.product_slice,
                    "-p",
                    "MemoryMax=256M",
                    "-p",
                    "MemorySwapMax=0",
                    "-p",
                    "CPUQuota=100%",
                    str(args.sdk_python),
                    str(root / "daemon_chaos.py"),
                    "_worker",
                    str(root / "config.json"),
                ],
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            deadline = time.monotonic() + args.seconds + 60
            group = Path(json.loads((root / "service-group.json").read_text())["path"])
            with (root / "resources.jsonl").open("w") as resources:
                while child.poll() is None:
                    assert time.monotonic() < deadline, "worker deadline exceeded"
                    resources.write(
                        json.dumps({"time": time.time(), "guests": observe_guests(group)}) + "\n"
                    )
                    resources.flush()
                    time.sleep(0.2)
            code = child.returncode
            assert code == 0, f"SDK chaos failed: {root / 'worker.log'}"
        assert api(cfg, "GET", "/v1/sandboxes")[1]["items"] == []
        assert not observe_guests(group), "deleted guest cgroups remain"
    finally:
        if child is not None and child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=10)
        cleanup_errors = []
        try:
            items = api(cfg, "GET", "/v1/sandboxes")[1]["items"] if verified_api else []
            for sb in items:
                if not owned_sandbox(cfg, sb):
                    cleanup_errors.append("refused unowned sandbox: " + str(sb.get("id")))
                    continue
                try:
                    api(cfg, "DELETE", "/v1/sandboxes/" + sb["id"])
                except urllib.error.HTTPError as error:
                    if error.code != 404:
                        cleanup_errors.append(repr(error))
                except Exception as error:
                    cleanup_errors.append(repr(error))
        except Exception as error:
            cleanup_errors.append(repr(error))
        subprocess.run(
            ["journalctl", "--user", "-u", unit, "--no-pager", "-o", "cat"],
            stdout=(root / "api-journal.log").open("wb"),
            check=False,
        )
        subprocess.run(["systemctl", "--user", "stop", unit], check=False)
        save(root / "cleanup.json", {"errors": cleanup_errors, "state_retained": str(state)})
        if cleanup_errors:
            raise RuntimeError(cleanup_errors)


if __name__ == "__main__":
    main()
