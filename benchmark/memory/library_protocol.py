"""Private library experiment commands and bounded, out-of-group page audit."""

import json
import os
import subprocess
import time
import traceback
from pathlib import Path

from library_evidence import file_mapping_stats, observe, pool_backing, process_start
from scanner_compare import digest, save

VARIANTS = {
    "firecracker-block": ("firecracker", "block", None),
    "docker-rootless-library": ("docker-rootless", "rootfs", None),
    "pvisor-rootfs-off": ("pvisor-off", "rootfs", None),
    "pvisor-rootfs-ksm": ("pvisor-ksm", "rootfs", None),
    "pvisor-rootfs-physical-no-hints": ("pvisor-physical", "rootfs", "0"),
    "pvisor-rootfs-physical-hints": ("pvisor-physical", "rootfs", "1"),
    "pvisor-dax": ("pvisor-off", "dax", None),
}


def details(cfg):
    arm, mode, hints = VARIANTS[cfg["library_variant"]]
    if cfg["arm"] != arm or cfg["pattern"] != "random-shared":
        raise ValueError("library variant/arm/payload mismatch")
    return mode, hints


def command(cfg, identity, socket):
    if cfg["arm"] == "docker-rootless":
        raise ValueError("Docker commands require the owned Engine transport")
    mode, _ = details(cfg)
    root, inputs = Path(cfg["root"]), Path(cfg["inputs"])
    if mode == "block":
        configuration = {
            "boot-source": dict(
                kernel_image_path=str(inputs / "kernel"),
                boot_args=f"console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/usr/bin/python3 -- -B -u /library-worker.py random-shared {identity} block",
            ),
            "drives": [
                dict(
                    drive_id="rootfs",
                    path_on_host=str(inputs / "library.ext4"),
                    is_root_device=True,
                    is_read_only=True,
                )
            ],
            "machine-config": dict(vcpu_count=1, mem_size_mib=256, smt=False),
        }
        path = root / f"fc{identity}.json"
        save(path, configuration)
        return [
            str(inputs / "firecracker"),
            "--enable-pci",
            "--no-api",
            "--config-file",
            str(path),
            "--seccomp-filter",
            str(inputs / "firecracker-filter.bpf"),
        ]
    result = [
        str(inputs / "probe"),
        "--rootfs",
        str(inputs / "python-root"),
        "--worker",
        str(inputs / "library_worker.py"),
        "--firmware",
        str(inputs / "firmware"),
        "--stage",
        str(root / f"vm{identity}"),
        "--mode",
        cfg["arm"].removeprefix("pvisor-"),
        "--pattern",
        cfg["pattern"],
        "--instance",
        str(identity),
    ]
    result += ["--libraries", str(inputs / "libraries")] if mode == "dax" else ["--rootfs-library"]
    if socket:
        result += ["--pool-socket", str(socket)]
    return result


def environment(cfg):
    _, hints = details(cfg)
    result = os.environ.copy()
    result.pop("PVISOR_MEMORY_FILE_HINTS", None)
    if hints is not None:
        result["PVISOR_MEMORY_FILE_HINTS"] = hints
    return result


def internal_audit(cfg, guests, pool_pid, cgroup):
    root = Path(cfg["root"])
    started = time.monotonic_ns()
    request = dict(
        guest_pids=[g.process.pid for g in guests],
        pool_pid=pool_pid,
        cgroup=str(cgroup),
        starts={str(g.process.pid): process_start(g.process.pid) for g in guests},
    )
    if pool_pid is not None:
        request["starts"][str(pool_pid)] = process_start(pool_pid)
    save(root / "library-audit-request.json", request)
    deadline = time.monotonic() + 25
    response = root / "library-audit-response.json"
    while not response.exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("out-of-group library page audit deadline")
        time.sleep(0.02)
    result = json.loads(response.read_text())
    if result["status"] != "passed":
        raise ValueError("library page audit failed: " + result.get("error", "unknown"))
    if digest(root / "library-evidence.json") != result["evidence_sha256"]:
        raise ValueError("library page evidence changed")
    result["started_ns"] = started
    result["finished_ns"] = time.monotonic_ns()
    result["wait_seconds"] = (result["finished_ns"] - started) / 1e9
    return result


def audit_observer(unit, stop, root):
    request = root / "library-audit-request.json"
    while not stop.wait(0.05):
        if not request.exists():
            continue
        try:
            observed_start = time.monotonic_ns()
            cfg = json.loads((root / "config.json").read_text())
            inputs = Path(cfg["inputs"])
            mode, hints = details(cfg)
            wanted = json.loads(request.read_text())
            # Bind every inspected PID to this attempt's actual systemd cgroup.
            before = json.loads((root / "before.json").read_text())
            actual_group = subprocess.check_output(
                ["systemctl", "--user", "show", unit, "-p", "ControlGroup", "--value"],
                text=True,
                timeout=5,
            ).strip()
            if (
                not actual_group
                or wanted["cgroup"] != str(Path("/sys/fs/cgroup") / actual_group.lstrip("/"))
                or wanted["cgroup"] != before["cgroup"]
            ):
                raise ValueError("library audit cgroup mismatch")
            relative = wanted["cgroup"].removeprefix("/sys/fs/cgroup")
            pids = wanted["guest_pids"]
            if len(pids) != 4 or len(set(pids)) != 4:
                raise ValueError("four live VMs required for library audit")
            pool_pid = wanted["pool_pid"]
            owned = set(pids) | ({pool_pid} if pool_pid is not None else set())
            if {int(pid) for pid in wanted["starts"]} != owned or (
                cfg["arm"] == "pvisor-physical"
            ) != (pool_pid is not None):
                raise ValueError("library audit process ownership set mismatch")
            for pid, start in wanted["starts"].items():
                from docker_support import belongs

                placement = Path(f"/proc/{pid}/cgroup").read_text()
                owned_group = (
                    belongs(placement, relative)
                    if cfg["arm"] == "docker-rootless"
                    else f"0::{relative}" in placement.splitlines()
                )
                if process_start(int(pid)) != start or not owned_group:
                    raise ValueError("library audit escaped owned process identity")
            hint_settings = []
            for pid in pids:
                fields = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                actual = [
                    entry.split(b"=", 1)[1].decode("ascii")
                    for entry in fields
                    if entry.startswith(b"PVISOR_MEMORY_FILE_HINTS=")
                ]
                if actual != ([] if hints is None else [hints]):
                    raise ValueError("VM inherited a different file-hint setting")
                hint_settings.append(dict(pid=pid, file_hints=hints))
            for index in range(1, 5):
                lines = (root / f"vm{index}.stdout").read_text(errors="replace").splitlines()
                if not any(
                    line.startswith(f"LIBRARY_MODE {mode} statx_attributes ") for line in lines
                ):
                    raise ValueError("guest did not attest the intended library data path")
            if wanted["pool_pid"] is not None:
                if str(wanted["pool_pid"]) not in wanted["starts"]:
                    raise ValueError("pool identity missing")
                source = pool_backing(wanted["pool_pid"])
            elif mode == "dax":
                source = inputs / "libraries/libpayload.so"
            else:
                source = None
            evidence = dict(
                variant=cfg["library_variant"],
                status="passed",
                observer_scope="outside product cgroup; after timed first full readback, before COW",
                observed_start_ns=observed_start,
                observed_hint_settings=hint_settings,
                payload_sha256=digest(inputs / "payload"),
            )
            if source is not None:
                evidence["sharing"] = observe(pids, source, inputs / "payload")
            else:
                evidence["sharing"] = None
            if cfg["arm"] == "docker-rootless":
                canonical = digest(inputs / "libraries/libpayload.so")
                views = []
                for pid in pids:
                    source = Path("/proc") / str(pid) / "root/libraries/libpayload.so"
                    if digest(source) != canonical:
                        raise ValueError("Docker loaded a different library image")
                    before_path = root / f"docker-library-{pid}-before.smaps"
                    before_path.write_text(Path(f"/proc/{pid}/smaps").read_text())
                    proof = observe([pid], source, inputs / "payload")
                    after_path = root / f"docker-library-{pid}-after.smaps"
                    after_path.write_text(Path(f"/proc/{pid}/smaps").read_text())
                    if digest(source) != canonical:
                        raise ValueError("Docker library source changed during observation")
                    file_id = proof["maps_device"], proof["inode"]
                    views.append(
                        dict(
                            pid=pid,
                            source_sha256=canonical,
                            target=proof,
                            smaps_sha256={
                                path.name: digest(path) for path in (before_path, after_path)
                            },
                            accounting_before=file_mapping_stats(
                                before_path.read_text(), file_id, proof["mapping_path"]
                            ),
                            accounting_after=file_mapping_stats(
                                after_path.read_text(), file_id, proof["mapping_path"]
                            ),
                        )
                    )
                evidence["docker_file_views"] = dict(
                    schema="pvisor-docker-library-views/v1",
                    processes=views,
                    shared_target_pages_all_processes=None,
                    scope="each OverlayFS view independently attested; cross-view same-slot count unavailable; kernel library PSS retained without equating different devices/inodes",
                )
            evidence["observed_end_ns"] = time.monotonic_ns()
            if mode == "dax" and evidence["sharing"]["shared_pages_all_processes"] == 0:
                evidence.update(
                    status="failed", error="shared-file library path has no shared target pages"
                )
                save(root / "library-evidence.json", evidence)
                raise ValueError(evidence["error"])
            save(root / "library-evidence.json", evidence)
            response = dict(status="passed", evidence_sha256=digest(root / "library-evidence.json"))
        except Exception as error:
            response = dict(status="failed", error=str(error), traceback=traceback.format_exc())
        save(root / "library-audit-response.json", response)
        return


def validate_library(raw, cfg, root):
    """Recheck retained data-path/target evidence independently of worker success."""
    mode, hints = details(cfg)
    receipt = raw.get("library_audit", {})
    path = root / "library-evidence.json"
    if (
        receipt.get("status") != "passed"
        or not path.is_file()
        or digest(path) != receipt.get("evidence_sha256")
    ):
        raise ValueError("library target evidence missing or changed")
    evidence = json.loads(path.read_text())
    if (
        evidence.get("status") != "passed"
        or evidence.get("variant") != cfg["library_variant"]
        or evidence.get("payload_sha256") != cfg["hashes"]["1:0"]
    ):
        raise ValueError("library target identity mismatch")
    phases = {p["name"]: p for p in raw["phases"]}
    if not (
        phases["readback"]["monotonic_ns"]
        <= receipt["started_ns"]
        <= evidence["observed_start_ns"]
        <= evidence["observed_end_ns"]
        <= receipt["finished_ns"]
        <= phases["cow-25"]["monotonic_ns"]
    ):
        raise ValueError("library audit overlaps timed readback or private writes")
    phase = next(p for p in raw["phases"] if p["name"] == "idle-60")
    guests = set(phase["guest_pids"])
    settings = evidence.get("observed_hint_settings", [])
    if (
        len(settings) != 4
        or {row["pid"] for row in settings} != guests
        or any(row["file_hints"] != hints for row in settings)
    ):
        raise ValueError("VM file-hint setting evidence missing or changed")
    for index in range(1, 5):
        lines = (root / f"vm{index}.stdout").read_text(errors="replace").splitlines()
        if not any(line.startswith(f"LIBRARY_MODE {mode} statx_attributes ") for line in lines):
            raise ValueError("library guest data path missing")
    sharing = evidence.get("sharing")
    if cfg["arm"] == "docker-rootless":
        validate_docker_views(evidence.get("docker_file_views"), cfg, root, phase)
    elif evidence.get("docker_file_views") is not None:
        raise ValueError("unexpected Docker file-view evidence")
    requires_target = cfg["arm"] == "pvisor-physical" or mode == "dax"
    if requires_target != (sharing is not None):
        raise ValueError("library sharing evidence scope mismatch")
    if sharing is None:
        return evidence
    starts = {p["pid"]: p["starttime"] for p in phase["processes"]}
    rows = sharing["processes"]
    if (
        sharing.get("schema") != "pvisor-library-file-sharing/v2"
        or sharing.get("target_pages") != 16384
        or sharing.get("payload_sha256") != cfg["hashes"]["1:0"]
        or len(rows) != 4
        or {p["pid"] for p in rows} != guests
    ):
        raise ValueError("library target process/payload evidence mismatch")
    if (
        not isinstance(sharing.get("maps_device"), int)
        or sharing["maps_device"] < 0
        or not isinstance(sharing.get("inode"), int)
        or sharing["inode"] <= 0
        or not isinstance(sharing.get("mapping_path"), str)
        or not sharing["mapping_path"]
    ):
        raise ValueError("calibrated mapping identity missing")
    all_slots = []
    for row in rows:
        slots = [tuple(x) for x in row["payload_index_and_file_offset"]]
        if (
            row["starttime"] != starts[row["pid"]]
            or len(set(slots)) != len(slots)
            or any(
                not (0 <= index < 16384) or offset < 0 or offset % 4096 for index, offset in slots
            )
            or row["payload_pages"] != len({index for index, _ in slots})
        ):
            raise ValueError("invalid target file-page evidence")
        all_slots.append(set(slots))
    common = set.intersection(*all_slots)
    retained = [tuple(x) for x in sharing["shared_payload_index_and_file_offset"]]
    if (
        len(set(retained)) != len(retained)
        or set(retained) != common
        or sharing["shared_pages_all_processes"] != len({index for index, _ in common})
    ):
        raise ValueError("cross-VM same-slot intersection mismatch")
    if mode == "dax" and not common:
        raise ValueError("shared-file target sharing absent")
    return evidence


def validate_docker_views(observation, cfg, root, phase):
    """Audit each overlay view and its raw kernel PSS without inventing aliases."""
    if (
        not observation
        or observation.get("schema") != "pvisor-docker-library-views/v1"
        or observation.get("shared_target_pages_all_processes") is not None
    ):
        raise ValueError("Docker file-view evidence missing or falsely equated")
    views = observation["processes"]
    guests = set(phase["guest_pids"])
    starts = {p["pid"]: p["starttime"] for p in phase["processes"]}
    if len(views) != 4 or {view["pid"] for view in views} != guests:
        raise ValueError("Docker file-view process ownership mismatch")
    library = Path(cfg["inputs"]) / "libraries/libpayload.so"
    canonical, source_bytes = digest(library), library.stat().st_size
    for view in views:
        pid, proof = view["pid"], view["target"]
        processes = proof.get("processes", [])
        if (
            view["source_sha256"] != canonical
            or proof.get("schema") != "pvisor-library-file-sharing/v2"
            or proof.get("target_pages") != 16384
            or proof.get("payload_sha256") != cfg["hashes"]["1:0"]
            or len(processes) != 1
            or processes[0]["pid"] != pid
            or processes[0]["starttime"] != starts[pid]
            or proof.get("mapping_path") != "/libraries/libpayload.so"
            or not isinstance(proof.get("maps_device"), int)
            or proof["maps_device"] < 0
            or not isinstance(proof.get("inode"), int)
            or proof["inode"] <= 0
        ):
            raise ValueError("Docker per-view canonical identity mismatch")
        slots = [tuple(x) for x in processes[0]["payload_index_and_file_offset"]]
        if (
            len(slots) != 16384
            or len(set(slots)) != 16384
            or len({offset for _, offset in slots}) != 16384
            or {index for index, _ in slots} != set(range(16384))
            or processes[0]["payload_pages"] != 16384
            or proof["shared_pages_all_processes"] != 16384
            or [tuple(x) for x in proof["shared_payload_index_and_file_offset"]] != slots
            or any(
                offset < 0 or offset % 4096 or offset + 4096 > source_bytes for _, offset in slots
            )
        ):
            raise ValueError("Docker target payload PTE evidence incomplete")
        expected = {f"docker-library-{pid}-{boundary}.smaps" for boundary in ("before", "after")}
        if set(view["smaps_sha256"]) != expected:
            raise ValueError("Docker exact-file raw accounting missing")
        for boundary in ("before", "after"):
            path = root / f"docker-library-{pid}-{boundary}.smaps"
            if digest(path) != view["smaps_sha256"][path.name]:
                raise ValueError("Docker exact-file raw accounting changed")
            stats = file_mapping_stats(
                path.read_text(), (proof["maps_device"], proof["inode"]), proof["mapping_path"]
            )
            if stats != view["accounting_" + boundary] or stats["rss_bytes"] < 64 * 1024**2:
                raise ValueError("Docker exact-file accounting mismatch")
