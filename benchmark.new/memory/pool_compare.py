#!/usr/bin/env python3
"""Benchmark: B-MEMORY-SCALE, role engineering A/B.

Question: can pooled sharing reclaim guest memory faster than KSM, at what CPU
and refault cost, with stock Firecracker as the primary product baseline?
Same static guest byte oracle in every VM, four 256MiB/1-vCPU VMs, whole-group
4 CPU/2GiB/swap0, 60s idle curves, full reads/COW/peer checks. No host sysfs writes.
Different kernels, device paths, page sizes and recovery mechanisms are disclosed;
this is not a pure-codec or pure-VMM attribution. Preflight is separate from >=5
randomized formal rounds. Every failure is retained and stops the campaign.
"""

import argparse
import json
import os
import random
import shlex
import shutil
import subprocess
import sys
import threading
import time
import traceback
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "benchmark/pvisor"))
from firecracker_ksm import Guest, expected, group_path  # noqa: E402
from linux_cold_runtime import settle_unit  # noqa: E402
from memory_savings import (  # noqa: E402 - import after module-loading setup
    host_identity,
    observer,
    wait_for_quiet,
)  # noqa: E402
from memory_savings import (  # noqa: E402 - import after module-loading setup
    validate_observation as validate_legacy_observation,
)
from scanner_compare import TUNABLES, digest, kernel_state, require_default_ksm, save  # noqa: E402

ARMS = (
    "firecracker",
    "docker-rootless",
    "pvisor-off",
    "pvisor-ksm",
    "pvisor-physical",
    "pvisor-compressed",
    "pvisor-local",
)
PATTERNS = ("repeated", "random-shared", "random-unique")
BOUNDARIES = (0, 1, 2, 5, 10, 20, 40, 60)
POST_WRITE_BOUNDARIES = (1, 2, 5, 10, 20, 30)
CURRENT_SCHEMA = "pvisor-pool-curve-condition/v4"
WRITE_SCHEMAS = ("pvisor-pool-curve-condition/v3", CURRENT_SCHEMA)


def validate_observation(rows, guards, *, tasks_max=128):
    """Keep legacy budgets exact; v4 admits the same 256-task cap for every arm."""
    if tasks_max == 128:
        return validate_legacy_observation(rows, guards)
    if tasks_max != 256:
        raise ValueError("unsupported observation task budget")
    if not rows or any("error" in row for row in rows):
        raise ValueError("missing/failed external memory observation")
    for row in rows:
        budget = row["budget"]
        quota, period = map(int, budget["cpu.max"].split())
        if (
            budget["memory.max"] != str(2**31)
            or budget["memory.swap.max"] != "0"
            or budget["memory.swap.current"] != "0"
            or quota != 4 * period
            or period <= 0
            or budget["pids.max"] != "256"
        ):
            raise ValueError("installed resource budget mismatch")
    if not guards or any(item.get("jobs", []) for item in guards):
        raise ValueError("foreign VM/build or missing host guard during sampling")


def values(text):
    return {
        line.split()[0].rstrip(":"): int(line.split()[1])
        for line in text.splitlines()
        if len(line.split()) >= 2 and line.split()[1].isdigit()
    }


def verify_harness(files):
    return all(digest(Path(name)) == value for name, value in files.items())


def mapping_evidence(proc):
    mergeable, pooled, count, ksm_bytes, header, size = 0, 0, 0, 0, "", 0
    with (proc / "smaps").open() as stream:
        for line in stream:
            if line.startswith("VmFlags:") and "mg" in line.split()[1:]:
                mergeable += size
            elif line.startswith("KSM:"):
                ksm_bytes += int(line.split()[1]) * 1024
            elif line and line[0] in "0123456789abcdef" and "-" in line.split()[0]:
                header = line
                low, high = (int(x, 16) for x in line.split()[0].split("-"))
                size = high - low
                if "memfd:pvisor-shared-ram" in header:
                    pooled += size
                    count += 1
    return dict(
        mergeable_bytes=mergeable, pool_mapping_bytes=pooled, pool_vmas=count, ksm_bytes=ksm_bytes
    )


def pool_storage(pid):
    seen, allocated, logical = set(), 0, 0
    for fd in (Path("/proc") / str(pid) / "fd").iterdir():
        try:
            if "memfd:pvisor-" not in os.readlink(fd):
                continue
            metadata = fd.stat()
        except FileNotFoundError:
            continue
        identity = metadata.st_dev, metadata.st_ino
        if identity in seen:
            continue
        seen.add(identity)
        allocated += metadata.st_blocks * 512
        logical += metadata.st_size
    return dict(
        unique_memfds=len(seen),
        allocated_file_bytes=allocated,
        logical_file_bytes=logical,
        scope="deduplicated memfd filesystem blocks; excludes index/kernel overhead, not net savings",
    )


def snapshot(root, name, guests, pool_pid=None, *, group=None):
    from docker_support import group_pids

    group = group or group_path()
    counters = {
        n: (group / n).read_text()
        for n in (
            "memory.current",
            "memory.peak",
            "memory.stat",
            "memory.events",
            "memory.swap.current",
            "memory.max",
            "memory.swap.max",
            "cpu.stat",
            "cpu.max",
            "pids.max",
            "pids.events",
            "pids.current",
        )
    }
    processes = []
    for pid in group_pids(group):
        word = str(pid)
        proc = Path("/proc") / word
        try:
            rollup = (proc / "smaps_rollup").read_text()
            stat = (proc / "stat").read_text().rsplit(")", 1)[1].split()
            ticks = os.sysconf("SC_CLK_TCK")
            processes.append(
                dict(
                    pid=int(word),
                    smaps_rollup=rollup,
                    pss_bytes=values(rollup)["Pss"] * 1024,
                    comm=(proc / "comm").read_text().strip(),
                    starttime=stat[19],
                    cpu_user_seconds=int(stat[11]) / ticks,
                    cpu_system_seconds=int(stat[12]) / ticks,
                    mapping_evidence=mapping_evidence(proc),
                    cpu_affinity=sorted(os.sched_getaffinity(int(word))),
                    cgroup=(proc / "cgroup").read_text(),
                    ksm_stat=(proc / "ksm_stat").read_text()
                    if (proc / "ksm_stat").exists()
                    else None,
                )
            )
            if group.name.endswith(".slice") and int(word) in {g.process.pid for g in guests}:
                relative = processes[-1]["cgroup"].strip().removeprefix("0::")
                leaf = Path("/sys/fs/cgroup") / relative.lstrip("/")
                processes[-1]["container_limits"] = {
                    name: (leaf / name).read_text().strip()
                    for name in (
                        "memory.max",
                        "memory.swap.max",
                        "memory.swap.current",
                        "cpu.max",
                        "pids.max",
                    )
                }
        except FileNotFoundError:
            continue
    result = dict(
        name=name,
        monotonic_ns=time.monotonic_ns(),
        counters=counters,
        processes=processes,
        guest_pids=[g.process.pid for g in guests],
        cgroup=str(group),
        ksm=kernel_state(),
    )
    if pool_pid is not None:
        result["pool_storage"] = pool_storage(pool_pid)
    save(root / (name + ".json"), result)
    return result


def internal(path):
    cfg = json.loads(path.read_text())
    if cfg["arm"] not in ARMS:
        raise ValueError("unsupported arm")
    root, inputs = Path(cfg["root"]), Path(cfg["inputs"])
    guests, handles, pool = [], [], None
    engine = None
    group = group_path()
    if cfg["arm"] == "docker-rootless":
        group = Path(cfg["accounting_group"])
        if group_path().parent != group or group.name != cfg["docker_slice"]:
            raise ValueError("Docker controller escaped owned parent slice")
    result = dict(status="failed", condition=cfg, checks=[], phases=[], cleanup=[])
    try:
        if not verify_harness(cfg["harness_files"]):
            raise ValueError("measurement source changed before VM start")
        initial = snapshot(root, "before", [], group=group)
        result["before"] = initial
        counters = initial["counters"]
        quota, period = counters["cpu.max"].split()
        if (
            counters["memory.max"].strip() != str(2**31)
            or counters["memory.swap.max"].strip() != "0"
            or int(quota) != 4 * int(period)
        ):
            raise ValueError("whole-group budget was not installed")
        state = kernel_state()
        require_default_ksm(state)
        result["kernel_before"] = state
        if cfg["arm"] == "docker-rootless":
            from docker_support import Engine

            engine = Engine(root, cfg["docker_slice"], root / "docker-data")
            result["docker_info"] = engine.start()
            docker_receipt = json.loads((Path(cfg["docker_inputs"]) / "receipt.json").read_text())
            result["docker_image_id"] = docker_receipt["image_id"]
        mode = cfg["arm"].removeprefix("pvisor-")
        socket = None
        if mode in ("physical", "compressed"):
            directory = root / "pool"
            directory.mkdir(mode=0o700)
            save(
                directory / "config.json",
                dict(
                    mode=mode,
                    max_bytes=536870912,
                    max_objects=32768,
                    max_connections=32,
                    max_references=32768,
                ),
            )
            log = (root / "pool.log").open("wb")
            handles.append(log)
            pool = subprocess.Popen(
                [str(inputs / "daemon"), "memory-pool", "--directory", str(directory)],
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
            )
            socket = directory / "pool.sock"
            deadline = time.monotonic() + 5
            while not socket.exists():
                if pool.poll() is not None or time.monotonic() >= deadline:
                    raise ValueError("pool did not become ready")
                time.sleep(0.02)
        for identity in range(1, 5):
            if engine:
                error = (root / f"vm{identity}.stderr").open("wb")
                log = (root / f"vm{identity}.stdout").open("wb")
                handles.extend([error, log])
                process, command = engine.create(cfg, identity, result["docker_image_id"], error)
                guest = Guest(process, identity, log)
                guests.append(guest)
                result.setdefault("commands", []).append(command)
                result["checks"].append(guest.ack("ready", 0, cfg["hashes"][f"{identity}:0"]))
                continue
            guest_environment = os.environ.copy()
            guest_environment.pop("PVISOR_MEMORY_FILE_HINTS", None)
            if "library_variant" in cfg:
                from library_protocol import command as library_command
                from library_protocol import environment

                command = library_command(cfg, identity, socket)
                guest_environment = environment(cfg)
            elif cfg["arm"].startswith("firecracker"):
                config = {
                    "boot-source": dict(
                        kernel_image_path=str(inputs / "kernel"),
                        boot_args=f"console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/worker -- {cfg['pattern']} {identity}",
                    ),
                    "drives": [
                        dict(
                            drive_id="rootfs",
                            path_on_host=str(inputs / "guest.ext4"),
                            is_root_device=True,
                            is_read_only=True,
                        )
                    ],
                    "machine-config": dict(vcpu_count=1, mem_size_mib=256, smt=False),
                }
                # The retained reference kernel's root device uses virtio PCI.
                config_path = root / f"fc{identity}.json"
                save(config_path, config)
                command = [
                    str(inputs / "firecracker"),
                    "--enable-pci",
                    "--no-api",
                    "--config-file",
                    str(config_path),
                ]
                if (inputs / "firecracker-filter.bpf").exists():
                    command += ["--seccomp-filter", str(inputs / "firecracker-filter.bpf")]
            else:
                command = [
                    str(inputs / "probe"),
                    "--rootfs",
                    str(inputs / "root"),
                    "--worker",
                    str(inputs / "worker"),
                    "--firmware",
                    str(inputs / "firmware"),
                    "--stage",
                    str(root / f"vm{identity}"),
                    "--mode",
                    mode,
                    "--pattern",
                    cfg["pattern"],
                    "--instance",
                    str(identity),
                ]
                if socket:
                    command += ["--pool-socket", str(socket)]
            error = (root / f"vm{identity}.stderr").open("wb")
            log = (root / f"vm{identity}.stdout").open("wb")
            handles.extend([error, log])
            process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=error,
                env=guest_environment,
            )
            guest = Guest(process, identity, log)
            guests.append(guest)
            result.setdefault("commands", []).append(command)
            result["checks"].append(guest.ack("ready", 0, cfg["hashes"][f"{identity}:0"]))
        origin = time.monotonic()
        result["all_ready_ns"] = time.monotonic_ns()
        for boundary in BOUNDARIES:
            delay = origin + boundary - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            result["phases"].append(
                snapshot(root, f"idle-{boundary}", guests, pool.pid if pool else None, group=group)
            )
            save(root / "worker.json", result)
        start = time.monotonic_ns()
        for g in guests:
            result["checks"].append(
                g.command("readback", "read", 0, cfg["hashes"][f"{g.identity}:0"])
            )
        result["first_full_read_ms"] = (time.monotonic_ns() - start) / 1e6
        result["phases"].append(
            snapshot(root, "readback", guests, pool.pid if pool else None, group=group)
        )
        if "library_variant" in cfg:
            from library_protocol import internal_audit

            result["library_audit"] = internal_audit(cfg, guests, pool.pid if pool else None, group)
        states = {n: 0 for n in range(1, 5)}
        for percent in (25, 100):
            for g in guests:
                result["checks"].append(
                    g.command(
                        f"write-{percent}-{g.identity}",
                        "mutate",
                        percent,
                        cfg["hashes"][f"{g.identity}:{percent}"],
                    )
                )
                states[g.identity] = percent
                for peer in guests:
                    result["checks"].append(
                        peer.command(
                            f"peer-{percent}-{g.identity}",
                            "read",
                            states[peer.identity],
                            cfg["hashes"][f"{peer.identity}:{states[peer.identity]}"],
                        )
                    )
            if percent == 100 and cfg.get("schema") in WRITE_SCHEMAS:
                result["post_write_origin_ns"] = time.monotonic_ns()
            result["phases"].append(
                snapshot(root, f"cow-{percent}", guests, pool.pid if pool else None, group=group)
            )
        if cfg.get("schema") in WRITE_SCHEMAS:
            for boundary in POST_WRITE_BOUNDARIES:
                delay = (
                    result["post_write_origin_ns"] + boundary * 10**9 - time.monotonic_ns()
                ) / 1e9
                if delay > 0:
                    time.sleep(delay)
                result["phases"].append(
                    snapshot(
                        root,
                        f"post-write-{boundary}",
                        guests,
                        pool.pid if pool else None,
                        group=group,
                    )
                )
                save(root / "worker.json", result)
        while guests:
            g = guests.pop(0)
            result["checks"].append(
                g.command(f"exit-{g.identity}", "exit", 100, cfg["hashes"][f"{g.identity}:100"])
            )
            if cfg["arm"].startswith("firecracker"):
                g.process.terminate()
            code = g.process.wait(timeout=10)
            if not cfg["arm"].startswith("firecracker") and code != 0:
                raise ValueError("workload failed at exit")
            result["cleanup"].append(dict(pid=g.process.pid, returncode=code, reaped=True))
            for peer in guests:
                result["checks"].append(
                    peer.command(
                        f"survivor-{g.identity}", "read", 100, cfg["hashes"][f"{peer.identity}:100"]
                    )
                )
        if pool:
            pool.terminate()
            pool.wait(timeout=10)
        if engine:
            if not engine.stop():
                raise ValueError("Docker cleanup failed")
            engine = None
        result["after"] = snapshot(root, "after", [], group=group)
        after = kernel_state()
        result["kernel_after"] = after
        require_default_ksm(after)
        for phase in result["phases"]:
            events = values(phase["counters"]["memory.events"])
            if any(events.get(n, 0) for n in ("oom", "oom_kill", "max")):
                raise ValueError("memory budget event")
            if int(phase["counters"]["memory.swap.current"]):
                raise ValueError("unexpected swap")
        result["status"] = "passed"
    except Exception as error:
        result.update(error=str(error), traceback=traceback.format_exc())
    finally:
        for g in guests:
            if g.process.poll() is None:
                g.process.kill()
            result["cleanup"].append(
                dict(pid=g.process.pid, returncode=g.process.wait(timeout=10), reaped=True)
            )
        if pool and pool.poll() is None:
            pool.kill()
            pool.wait(timeout=10)
        if engine:
            if not engine.stop():
                result.update(status="failed", error="Docker cleanup failed")
        for handle in handles:
            handle.close()
        save(root / "worker.json", result)
    return 0 if result["status"] == "passed" else 1


def expected_checks():
    checks, states = {}, {n: 0 for n in range(1, 5)}
    for token in ("ready", "readback"):
        for n in states:
            checks[token, n] = 0
    for percent in (25, 100):
        for n in states:
            checks[f"write-{percent}-{n}", n] = percent
            states[n] = percent
            for peer, state in states.items():
                checks[f"peer-{percent}-{n}", peer] = state
    for n in states:
        checks[f"exit-{n}", n] = 100
        for peer in range(n + 1, 5):
            checks[f"survivor-{n}", peer] = 100
    return checks


def validate_worker(raw, cfg):
    from docker_support import belongs

    if cfg["arm"] not in ARMS:
        raise ValueError("unsupported arm")
    if cfg.get("schema") not in (None, "pvisor-pool-curve-condition/v2", *WRITE_SCHEMAS):
        raise ValueError("unsupported condition schema")
    if raw.get("status") != "passed" or raw.get("condition") != cfg:
        raise ValueError("worker/condition mismatch")
    phases = raw.get("phases", [])
    names = [f"idle-{n}" for n in BOUNDARIES] + ["readback", "cow-25", "cow-100"]
    if cfg.get("schema") in WRITE_SCHEMAS:
        names += [f"post-write-{n}" for n in POST_WRITE_BOUNDARIES]
    if [p.get("name") for p in phases] != names:
        raise ValueError("missing or reordered memory phases")
    if phases[len(BOUNDARIES) - 1]["monotonic_ns"] - raw["all_ready_ns"] < 60_000_000_000:
        raise ValueError("idle scan window too short")
    if cfg.get("schema") in WRITE_SCHEMAS:
        origin = raw["post_write_origin_ns"]
        cow = next(p for p in phases if p["name"] == "cow-100")
        if origin > cow["monotonic_ns"] or phases[-1]["monotonic_ns"] - origin < 30_000_000_000:
            raise ValueError("post-write scan window too short or origin misplaced")
    expected_pids = phases[0]["guest_pids"]
    if len(expected_pids) != 4 or len(set(expected_pids)) != 4:
        raise ValueError("four distinct simultaneous VMs required")
    cpu, clock = 0, 0
    for phase in [raw["before"], *phases, raw["after"]]:
        c = phase["counters"]
        quota, period = c["cpu.max"].split()
        if (
            c["memory.max"].strip() != str(2**31)
            or c["memory.swap.max"].strip() != "0"
            or int(quota) != 4 * int(period)
        ):
            raise ValueError("phase escaped resource budget")
        if cfg.get("schema") == CURRENT_SCHEMA and c.get("pids.max", "").strip() != "256":
            raise ValueError("phase escaped current task budget")
        if cfg.get("schema") == CURRENT_SCHEMA and values(c["pids.events"]).get("max", 0):
            raise ValueError("task budget exhausted during condition")
        if any(values(c["memory.events"]).get(n, 0) for n in ("oom", "oom_kill", "max")) or int(
            c["memory.swap.current"]
        ):
            raise ValueError("OOM/swap/budget failure")
        usage = values(c["cpu.stat"])["usage_usec"]
        if usage < cpu or phase["monotonic_ns"] < clock:
            raise ValueError("nonmonotonic accounting")
        cpu, clock = usage, phase["monotonic_ns"]
        if phase["name"].startswith(("idle-", "post-write-")) or phase["name"] in (
            "readback",
            "cow-25",
            "cow-100",
        ):
            if phase["guest_pids"] != expected_pids:
                raise ValueError("VM missing from live phase")
            pids = [p["pid"] for p in phase["processes"]]
            if len(pids) != len(set(pids)) or not set(expected_pids) <= set(pids):
                raise ValueError("incomplete process PSS accounting")
        for process in phase["processes"]:
            if process["pss_bytes"] != values(process["smaps_rollup"])["Pss"] * 1024:
                raise ValueError("PSS evidence mismatch")
        if cfg.get("schema") in ("pvisor-pool-curve-condition/v2", *WRITE_SCHEMAS):
            require_default_ksm(phase["ksm"])
            if any(phase["ksm"]["files"][n] != raw["kernel_before"]["files"][n] for n in TUNABLES):
                raise ValueError("KSM changed during observation")
            expected_group = phase["cgroup"].removeprefix("/sys/fs/cgroup")
            for process in phase["processes"]:
                placed = (
                    belongs(process["cgroup"], expected_group)
                    if cfg["arm"] == "docker-rootless"
                    else f"0::{expected_group}" in process["cgroup"].splitlines()
                )
                if process["cpu_affinity"] != [0, 1, 2, 3] or not placed:
                    raise ValueError("process escaped CPU/cgroup placement")
    checks = raw.get("checks", [])
    expected = expected_checks()
    if len(checks) != len(expected) or {(c["token"], c["instance"]) for c in checks} != set(
        expected
    ):
        raise ValueError("missing/duplicated payload or isolation acknowledgement")
    for c in checks:
        percent = expected[c["token"], c["instance"]]
        if c["percent"] != percent or c["sha256"] != cfg["hashes"][f"{c['instance']}:{percent}"]:
            raise ValueError("full payload oracle mismatch")
    if {x["pid"] for x in raw["cleanup"] if x.get("reaped")} != set(expected_pids) or len(
        raw["cleanup"]
    ) != 4:
        raise ValueError("native reaping incomplete")
    for state in (raw["kernel_before"], raw["kernel_after"]):
        require_default_ksm(state)
    if any(raw["kernel_before"]["files"][n] != raw["kernel_after"]["files"][n] for n in TUNABLES):
        raise ValueError("KSM configuration changed")
    for phase in phases:
        mapped = {p["pid"]: p["mapping_evidence"]["mergeable_bytes"] for p in phase["processes"]}
        if cfg["arm"] == "pvisor-ksm":
            if not all(mapped[pid] > 0 for pid in expected_pids):
                raise ValueError("KSM advice missing from a VM")
        elif any(mapped[pid] for pid in expected_pids):
            raise ValueError("unexpected KSM participation")


def pss_sample(group):
    """Sample live members; retain read failures and identities that exited."""
    from docker_support import group_pids

    row = dict(monotonic_ns=time.monotonic_ns(), processes=[], exited_pids=[])
    try:
        for pid in group_pids(group):
            try:
                text = (Path("/proc") / str(pid) / "smaps_rollup").read_text()
                row["processes"].append(dict(pid=int(pid), pss_bytes=values(text)["Pss"] * 1024))
            except (FileNotFoundError, ProcessLookupError):
                row["exited_pids"].append(int(pid))
        row["pss_bytes"] = sum(p["pss_bytes"] for p in row["processes"])
    except FileNotFoundError:
        return None
    except OSError as error:
        row["error"] = str(error)
    return row


def pss_observer(unit, stop, root):
    rows, group = [], None
    while not stop.wait(0.5):
        if group is None:
            r = subprocess.run(
                ["systemctl", "--user", "show", unit, "-p", "ControlGroup", "--value"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            if r.stdout.strip():
                group = Path("/sys/fs/cgroup") / r.stdout.strip().lstrip("/")
        if group is None or not group.exists():
            continue
        row = pss_sample(group)
        if row is not None:
            rows.append(row)
    save(root / "pss.json", rows)


def attempt(
    inputs, root, arm, pattern, hashes, harness, *, library_variant=None, docker_inputs=None
):
    root.mkdir()
    row = dict(arm=arm, pattern=pattern, status="failed")
    if arm == "docker-rootless":
        from docker_support import prepare_case

        try:
            prepare_case(docker_inputs, root)
        except Exception as error:
            row.update(error=str(error), traceback=traceback.format_exc())
            save(root / "result.json", row)
            return row
    if not wait_for_quiet(root):
        row["error"] = "quiet admission failed"
        return row
    cfg = dict(
        schema=CURRENT_SCHEMA,
        root=str(root),
        inputs=str(inputs),
        arm=arm,
        pattern=pattern,
        hashes=hashes,
        harness_files=harness,
    )
    if library_variant is not None:
        from library_protocol import details

        cfg["library_variant"] = library_variant
        details(cfg)
    if docker_inputs is not None:
        cfg["docker_inputs"] = str(docker_inputs)
    unit = "pvisor-pool-curve-" + uuid.uuid4().hex + ".service"
    service = unit
    extra = []
    if arm == "docker-rootless":
        from docker_support import create_slice

        try:
            unit, group = create_slice(root)
        except Exception as error:
            row.update(error=str(error), traceback=traceback.format_exc())
            save(root / "result.json", row)
            return row
        service = unit.removesuffix(".slice") + "-worker.service"
        extra = ["--slice=" + unit, "-p", "Delegate=yes"]
        cfg.update(accounting_group=str(group), docker_slice=unit)
    save(root / "config.json", cfg)
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit=" + service,
        *extra,
        "-p",
        "MemoryMax=2147483648",
        "-p",
        "MemorySwapMax=0",
        "-p",
        "CPUQuota=400%",
        "-p",
        "CPUAffinity=0 1 2 3",
        "-p",
        "TasksMax=256",
        "-p",
        "RuntimeMaxSec=180",
        "-p",
        "TimeoutStopSec=10",
        "-p",
        "KillMode=control-group",
        sys.executable,
        str(Path(__file__).resolve()),
        "--internal",
        str(root / "config.json"),
    ]
    row["command"] = command
    stop = threading.Event()
    observers = [observer, pss_observer]
    if library_variant is not None:
        from library_protocol import audit_observer

        observers.append(audit_observer)
    watches = [threading.Thread(target=f, args=(unit, stop, root)) for f in observers]
    for watch in watches:
        watch.start()
    try:
        with (
            (root / "service.stdout").open("wb") as out,
            (root / "service.stderr").open("wb") as err,
        ):
            completed = subprocess.run(command, stdout=out, stderr=err, timeout=210)
        raw = json.loads((root / "worker.json").read_text())
        if completed.returncode or raw["status"] != "passed":
            raise ValueError(raw.get("error", "worker failed"))
        validate_worker(raw, cfg)
        if arm == "docker-rootless":
            from docker_support import validate_case

            validate_case(raw, cfg, root)
        if library_variant is not None:
            from library_protocol import validate_library

            validate_library(raw, cfg, root)
        row["worker_sha256"] = digest(root / "worker.json")
        row["status"] = "successful"
    except Exception as error:
        row["error"] = str(error)
    finally:
        row["unit_quiescent"] = settle_unit(unit, root)
        stop.set()
        for watch in watches:
            watch.join()
        try:
            validate_observation(
                json.loads((root / "monitor.json").read_text()),
                json.loads((root / "guard.json").read_text()),
                tasks_max=256,
            )
            pss = json.loads((root / "pss.json").read_text())
            if not pss or any("error" in p for p in pss):
                raise ValueError("PSS curve missing or failed")
        except Exception as error:
            row.update(status="failed", error=str(error))
        if not row["unit_quiescent"]:
            row.update(status="failed", error="owned service cleanup unproven")
    row["observation_hashes"] = {
        name: digest(root / name)
        for name in ("monitor.json", "pss.json", "guard.json")
        if (root / name).is_file()
    }
    if arm == "docker-rootless":
        row["docker_evidence_hashes"] = {
            path.name: digest(path) for path in root.glob("docker-*.json")
        }
    save(root / "result.json", row)
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--internal", type=Path)
    p.add_argument("--inputs", type=Path)
    p.add_argument("--docker-inputs", type=Path)
    p.add_argument("--output", type=Path)
    p.add_argument("--preflight", action="store_true")
    p.add_argument(
        "--prepare", action="store_true", help="write a new sibling .input.json for semspec bench"
    )
    p.add_argument("--samples", type=int, default=5)
    p.add_argument("--arms", default=",".join(ARMS))
    p.add_argument("--patterns", default=",".join(PATTERNS))
    a = p.parse_args()
    if a.internal:
        return internal(a.internal)
    if not a.inputs or not a.output:
        p.error("inputs and output required")
    arms, patterns = a.arms.split(","), a.patterns.split(",")
    if len(arms) != len(set(arms)) or not set(arms) <= set(ARMS):
        p.error("invalid arm selection")
    if len(patterns) != len(set(patterns)) or not set(patterns) <= set(PATTERNS):
        p.error("invalid pattern selection")
    if not a.preflight and (
        a.samples < 5 or set(arms) != set(ARMS) or set(patterns) != set(PATTERNS)
    ):
        p.error("formal cohort requires all arms/patterns and at least five rounds")
    inputs, output = a.inputs.resolve(), a.output.resolve()
    docker_inputs = a.docker_inputs.resolve() if a.docker_inputs else None
    if "docker-rootless" in arms and docker_inputs is None:
        p.error("Docker arm requires --docker-inputs")
    if a.prepare:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "--inputs",
            str(inputs),
            "--output",
            str(output),
            "--samples",
            str(a.samples),
            "--arms",
            a.arms,
            "--patterns",
            a.patterns,
        ]
        if a.preflight:
            command.append("--preflight")
        if docker_inputs:
            command += ["--docker-inputs", str(docker_inputs)]
        destination = output.with_suffix(".input.json")
        with destination.open("x") as stream:
            json.dump({"pool-curves": "$ " + shlex.join(command)}, stream, indent=2)
            stream.write("\n")
        print(destination)
        return 0
    output.mkdir()
    require_default_ksm(kernel_state())
    receipt = json.loads((inputs / "receipt.json").read_text())

    def verify():
        return all(digest(inputs / n) == h for n, h in receipt["hashes"].items())

    if not verify():
        raise ValueError("frozen inputs mismatch")
    hashes = {
        pattern: {
            f"{n}:{percent}": expected(pattern, n, percent)
            for n in range(1, 5)
            for percent in (0, 25, 100)
        }
        for pattern in patterns
    }
    harness_paths = [
        Path(__file__),
        Path(__file__).with_name("scanner_compare.py"),
        Path(__file__).with_name("docker_support.py"),
        Path(__file__).with_name("prepare_pool.py"),
        *(
            REPO / "benchmark/pvisor" / n
            for n in ("firecracker_ksm.py", "memory_savings.py", "linux_cold_runtime.py")
        ),
    ]
    harness = {str(path.resolve()): digest(path) for path in harness_paths}
    frozen = output / "harness"
    frozen.mkdir()
    for path in harness_paths:
        shutil.copy2(path, frozen / path.name)
    report = dict(
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        primary_baseline="firecracker",
        preflight=a.preflight,
        samples=1 if a.preflight else a.samples,
        arms=arms,
        patterns=patterns,
        receipt=receipt,
        harness_sha256=digest(Path(__file__)),
        attempts=[],
        complete=False,
        harness_files=harness,
        host=host_identity(),
        caveats=[
            "different kernels, block/virtio-fs root and guest supervisor",
            "stock Firecracker has no validated KSM API here; do not label it FC+KSM",
            "compression changes block size and fault path; not pure codec attribution",
            "ksmd is global, outside the product cgroup quota",
            "scanner can run during sequential VM startup",
        ],
    )
    if docker_inputs:
        from docker_support import verify_inputs as verify_docker_inputs

        report["docker_receipt"] = json.loads((docker_inputs / "receipt.json").read_text())
        verify_docker_inputs(docker_inputs, report["docker_receipt"])
        if report["docker_receipt"]["pool_receipt_sha256"] != digest(inputs / "receipt.json"):
            raise ValueError("Docker and VM frozen source inputs differ")
        report["caveats"].append(
            "rootless Docker includes a dedicated daemon/runtime in the bounded group; shared host kernel, image page cache and per-container 256MiB/one-CPU limits"
        )
    save(output / "report.json", report)
    for r in range(report["samples"]):
        cells = [(pattern, arm) for pattern in patterns for arm in arms]
        random.Random(20261018 + r).shuffle(cells)
        for pattern, arm in cells:
            if not verify_harness(harness) or not verify():
                report["error"] = "source/input changed between conditions"
                save(output / "report.json", report)
                return 1
            row = attempt(
                inputs,
                output / str(len(report["attempts"])),
                arm,
                pattern,
                hashes[pattern],
                harness,
                docker_inputs=docker_inputs,
            )
            row["round"] = r
            report["attempts"].append(row)
            save(output / "report.json", report)
            print(f"round={r} {pattern}/{arm}: {row['status']}", file=sys.stderr, flush=True)
            if row["status"] != "successful":
                print(row.get("error"), file=sys.stderr)
                return 1
    if docker_inputs:
        verify_docker_inputs(docker_inputs, report["docker_receipt"])
    report["complete"] = verify() and verify_harness(harness)
    save(output / "report.json", report)
    print(
        json.dumps(
            dict(
                complete=report["complete"],
                conditions=len(report["attempts"]),
                evidence=str(output),
            )
        )
    )
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
