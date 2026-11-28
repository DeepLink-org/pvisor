"""Exercise Rust immutable backing objects through a real staged workspace on Linux/KVM."""

import argparse
import json
import os
import re
import select
import shutil
import subprocess
import tempfile
from pathlib import Path

from stat_visibility import final_counter


def run(binary, guest, output, mode, enabled=None, *, hardlink=False, mixed=False):
    policy = "auto" if enabled is None else "on" if enabled else "off"
    label = f"{mode}-{policy}" + ("-hardlink" if hardlink else "") + ("-mixed" if mixed else "")
    stage_io = mode.startswith("disk-")
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        tmp = Path(tmp)
        root = tmp / "root"
        root.mkdir()
        if stage_io:
            (root / "work").symlink_to("/workspace")
        fixture = tmp / "fixture"
        lower = fixture / "lower"
        lower.mkdir(parents=True)
        pattern = bytes(i % 251 for i in range(8192 + 17))
        for name in ("source", "victim", "replacement"):
            (lower / name).write_bytes(pattern)
        if hardlink:
            os.link(lower / "source", lower / "alias")
        target = fixture / "target"
        shutil.copytree(lower, target)
        shadow = mode.startswith("shadow")
        if mixed:
            mutable = fixture / "mutable"
            mutable.mkdir()
            if shadow:
                (mutable / "source").write_bytes(b"live")
            (target / "source").write_bytes(b"project")
        if mode in ("hold", "evict"):
            for name in ("large0", "large1"):
                with (lower / name).open("wb") as f:
                    f.truncate(5 << 20)
        if mode.startswith("sparse"):
            with (lower / "sparse").open("wb") as f:
                f.truncate(32 << 20)
        env = {
            k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_FS_", "PVISOR_VM_"))
        }
        env.update(
            PVISOR_GUEST_KERNEL="rust-native",
            PVISOR_FS_PROFILE="1",
        )
        if enabled is not None:
            env["PVISOR_VM_FS_LOWER_DATA_CACHE"] = str(int(enabled))
        if mixed:
            env["PVISOR_VM_FS_RNGK_READINDEX"] = "0"
        log = output / f"{label}.log"
        command = [
            binary,
            root,
            "-",
            guest,
            "rust-mixed-data" if mixed else "rust-lower-data",
            fixture,
            mode,
        ]
        with log.open("wb") as err:
            if mode == "transition":
                with subprocess.Popen(
                    command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err
                ) as child:
                    try:
                        ready, _, _ = select.select([child.stdout], [], [], 30)
                        if not ready or child.stdout.read(1) != b"?":
                            raise RuntimeError(f"{label}: no lower handle barrier; see {log}")
                        pending = mutable / "pending"
                        pending.write_bytes(b"live")
                        pending.rename(mutable / "source")
                        stdout, _ = child.communicate(input=b"!", timeout=30)
                    except BaseException:
                        child.kill()
                        raise
                    status = child.returncode
            else:
                p = subprocess.run(
                    command, env=env, input=b"", stdout=subprocess.PIPE, stderr=err, timeout=45
                )
                status, stdout = p.returncode, p.stdout
        success = (
            re.fullmatch(rb"VM_GUEST_OK " + mode.encode() + rb" [1-9][0-9]*\n", stdout)
            if stage_io
            else stdout == b"IMMUTABLE_LOWER_OK\n"
        )
        if status or not success:
            raise RuntimeError(f"{label}: exit {status}, {stdout!r}; see {log}")
        profile = log.read_text()
        # The root serves only the injected payload, never a qualified lower file.
        grants = final_counter(profile, "virtio-fs-overlay", "lower_data_cache_open_flags", "units")
        negotiated = final_counter(
            profile, "virtio-fs-overlay", "lower_data_cache_enabled", "units"
        )
        expect_grant = enabled is not False and not (hardlink or stage_io or shadow)
        if bool(negotiated) != (enabled is not False) or bool(grants) != expect_grant:
            raise RuntimeError(f"{label}: unexpected negotiation/grants {negotiated}/{grants}")
        if mode == "transition" and (
            grants != 1
            or (mutable / "source").read_bytes() != b"live"
            or (lower / "source").read_bytes() != pattern
            or (target / "source").read_bytes() != b"project"
        ):
            raise RuntimeError(f"{label}: wrong backing receipt or stage write escaped")
        if mode == "check":
            if any(
                (lower / n).read_bytes() != pattern for n in ("source", "victim", "replacement")
            ):
                raise RuntimeError("guest modified the immutable lower")
            if (target / "source").read_bytes() != pattern:
                raise RuntimeError("stage modification reached target before apply")
            subprocess.run([binary, "--verify-rust-lower-stage", fixture], check=True, timeout=20)
            if (target / "source").read_bytes() != b"stage" or (
                target / "victim"
            ).read_bytes() != pattern:
                raise RuntimeError("selective apply/drop changed the wrong target bytes")
        # Only the root receives the supervisor's authenticated exit ioctl.
        protocols = [
            r["measurements"]
            for line in profile.splitlines()
            if line.startswith("pvisor-fs-profile ")
            and (r := json.loads(line.split(" ", 1)[1]))["component"] == "virtio-fs-protocol"
            and r["final_record"]
        ]
        workspace = [p for p in protocols if not p.get("IOCTL", {}).get("calls", 0)]
        if len(protocols) != 2 or len(workspace) != 1:
            raise RuntimeError(f"{label}: cannot distinguish the two protocol owners")
        if stage_io:
            verify_stage_io(mode, workspace[0])
        return {op: workspace[0].get(op, {}).get("calls", 0) for op in ("READ", "OPEN")}


def verify_stage_io(mode, measurements):
    # The probe creates and fills its own file. The timed hot reads follow a full warm-up.
    overwrites = mode == "disk-shuffled-overwrite-fsync"
    size = (2 if overwrites else 1) * (4 << 20)
    if (
        measurements["write_payload_bytes"]["units"] != size
        or measurements["FSYNC"]["calls"] < (2 if overwrites else 1)
        or measurements["UNLINK"]["calls"] != 1
    ):
        raise RuntimeError(f"{mode}: missing staged writes, fsync or cleanup")
    getattrs = measurements.get("GETATTR", {}).get("calls", 0)
    reads = measurements.get("READ", {}).get("calls", 0)
    # Allow one-second TTL expirations throughout the 45-second VM deadline and setup.
    if getattrs > 64:
        raise RuntimeError(f"{mode}: {getattrs} GETATTRs for 1,024 file operations")
    if mode in ("disk-warm-read", "disk-shuffled-read") and reads > 1024:
        raise RuntimeError(f"{mode}: {reads} READs for 1,024 pages including warm-up")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "guest", "io-guest", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binary, guest = (p.resolve(strict=True) for p in (args.binary, args.guest))
    for enabled, hardlink in ((False, False), (True, False), (None, False), (None, True)):
        once, three = (
            run(binary, guest, output, n, enabled, hardlink=hardlink) for n in ("1", "3")
        )
        reads = three["READ"] - once["READ"]
        if (
            three["OPEN"] - once["OPEN"] != 2
            or (reads == 0) != (enabled is not False and not hardlink)
            or reads < 0
        ):
            raise RuntimeError(f"unexpected reopen counters: {once} -> {three}")
    for prefix in ("", "shadow"):
        once, three = (run(binary, guest, output, f"{prefix}{n}", mixed=True) for n in (1, 3))
        reads = three["READ"] - once["READ"]
        if three["OPEN"] - once["OPEN"] != 2 or reads != (2 if prefix == "shadow" else 0):
            raise RuntimeError(f"mixed-layer precedence or reuse changed: {once} -> {three}")
    run(binary, guest, output, "transition", mixed=True)
    print("PASS: automatic mixed-layer reuse, mutable shadow, stage copy-up and whiteout")
    held, evicted = (run(binary, guest, output, m) for m in ("hold", "evict"))
    if evicted["READ"] <= held["READ"]:
        raise RuntimeError(
            f"expected an evicted inactive object to be refetched: {held}, {evicted}"
        )
    once, three = (run(binary, guest, output, m) for m in ("sparse1", "sparse3"))
    if three["READ"] != once["READ"] or three["OPEN"] - once["OPEN"] != 2:
        raise RuntimeError(f"sparse file was charged for untouched holes: {once}, {three}")
    run(binary, guest, output, "check")
    print("PASS: immutable data reopen, fallback, eviction, mmap/copy-up and selective apply/drop")
    for mode in (
        "disk-warm-read",
        "disk-shuffled-read",
        "disk-write-fsync",
        "disk-shuffled-overwrite-fsync",
    ):
        run(binary, args.io_guest.resolve(strict=True), output, mode)
        print(f"PASS: {mode} on stage")


if __name__ == "__main__":
    main()
