"""Check Rust native metadata routes and bounded namespace stress on Linux KVM."""

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import stat_visibility
from stat_visibility import expect_counter


def profiled_probe(binary, root, log, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_VM_", "PVISOR_FS_"))}
    env.update(PVISOR_GUEST_KERNEL="rust-native", PVISOR_FS_PROFILE="1")
    command = [
        binary,
        "--rootfs",
        root,
        "--cpus",
        "2",
        "--memory",
        "128",
        "--task-stdio",
        "--",
        "/probe",
        *args,
    ]
    with log.open("wb") as err:
        return subprocess.run(
            command, input=b"", stdout=subprocess.PIPE, stderr=err, timeout=30, env=env
        )


def check_statx_requests(binary, guest, output):
    counts = {}
    for method in ("fstatat", "statx", "force", "dont"):
        with tempfile.TemporaryDirectory(dir=output) as directory:
            root = Path(directory)
            files = root / "files"
            files.mkdir()
            for i in range(512):
                file = files / f"f{i}"
                file.write_bytes(b"x" * (i % 7))
                file.chmod(0o644)
            shutil.copy2(guest, root / "probe")
            log = output / f"scan-{method}.stderr.log"
            result = profiled_probe(binary, root, log, "--scan", method)
            if result.returncode != 0 or result.stdout != b"SCAN_OK\n":
                raise RuntimeError(f"scan-{method}: exit {result.returncode}; see {log}")
            profile = log.read_text()
            counts[method] = tuple(
                stat_visibility.final_counter(profile, "virtio-fs-protocol", op)
                for op in ("LOOKUP", "GETATTR")
            )
    lookups, attrs = counts["fstatat"]
    for method, (lookup, getattr_) in counts.items():
        # At zero TTL the explicit sync modes still fetch attributes after the walk.
        extra = 512 if method in ("force", "dont") else 0
        if lookup < 512 or abs(lookup - lookups) > 8 or abs(getattr_ - attrs - extra) > 8:
            raise RuntimeError(f"unexpected metadata scan counts (LOOKUP, GETATTR): {counts}")
    print(f"PASS: metadata scans (LOOKUP, GETATTR): {counts}", flush=True)


def run_wide(binary, guest, output, count):
    with tempfile.TemporaryDirectory(dir=output) as directory:
        root = Path(directory)
        wide = root / "wide"
        wide.mkdir()
        (wide / "mount").mkdir()
        (wide / "watched").touch()
        (wide / "held").write_bytes(b"old")
        (wide / "held").chmod(0o644)
        for i in range(count):
            file = wide / f"file{i}"
            file.touch()
            file.chmod(0o644)
        shutil.copy2(guest, root / "probe")
        log = output / f"wide-{count}.stderr.log"
        result = profiled_probe(binary, root, log, "--wide-dir", str(count))
        fields = result.stdout.split()
        if (
            result.returncode != 7
            or len(fields) != 3
            or fields[:2] != [b"WIDE_STAT_OK", str(count).encode()]
            or int(fields[2]) <= 0
            or (wide / "absent0").read_bytes() != b"old"
        ):
            raise RuntimeError(f"wide-{count}: exit {result.returncode}; see {log}")
        return stat_visibility.final_counter(log.read_text(), "virtio-fs-protocol", "LOOKUP")


def run_staged(binary, guest, output, enabled, seed=None):
    with tempfile.TemporaryDirectory(dir=output) as directory:
        root = Path(directory)
        lower = root / "lower"
        state = root / "state"
        lower.mkdir()
        state.mkdir()
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("PVISOR_VM_", "PVISOR_FS_"))
        } | {
            "TMPDIR": str(state),
            "PVISOR_GUEST_KERNEL": "rust-native",
            "PVISOR_VM_FS_WORKERS": "1" if seed is None else "2",
            "PVISOR_FS_PROFILE": "1",
        }
        label = "staged-default" if enabled else "staged-disabled"
        if not enabled:
            env["PVISOR_VM_FS_RNGK_READINDEX"] = "0"
        command = [binary, lower, "-", guest, "rust-readindex"]
        expected = b"RUST_READINDEX_OK\n"
        if seed is not None:
            label += f"-seed-{seed}"
            command.append(str(seed))
            expected = f"RUST_READINDEX_STRESS_OK seed={seed}\n".encode() + expected
        print(label, flush=True)
        stdout = output / f"{label}.stdout.log"
        stderr = output / f"{label}.stderr.log"
        with stdout.open("wb") as out, stderr.open("wb") as err:
            result = subprocess.run(command, input=b"", stdout=out, stderr=err, env=env, timeout=35)
        if result.returncode != 0 or stdout.read_bytes() != expected:
            raise RuntimeError(f"{label}: exit {result.returncode}; see {stdout} and {stderr}")
        profile = stderr.read_text(errors="replace")
        expect_counter(profile, "virtio-fs-protocol", "READINDEX", enabled)
        expect_counter(profile, "virtio-fs-dispatch", "vcpu_inline_metadata", False, "units")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("vm-api", "stat-guest", "metadata-case", "metadata-guest", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--seed", action="append", type=int, default=[])
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    # One positive and one negative lookup per extra name; allow for bootstrap traffic.
    vm = args.vm_api.resolve(strict=True)
    stat = args.stat_guest.resolve(strict=True)
    check_statx_requests(vm, stat, output)
    small, large = (run_wide(vm, stat, output, n) for n in (1024, 4096))
    expected = 2 * (4096 - 1024)
    if abs((large - small) - expected) > 32:
        raise RuntimeError(f"unrelated LOOKUPs during directory insertion: {small} -> {large}")
    print(f"PASS: wide-directory LOOKUPs {small} -> {large}", flush=True)

    for cpus, workers, inline, label in (
        (1, 1, None, "inline-default"),
        (1, 1, 0, "inline-disabled"),
        (1, 4, None, "inline-multiqueue"),
        (2, 4, None, "smp-multiqueue"),
    ):
        profile = stat_visibility.run(
            vm,
            stat,
            inline,
            output / f"{label}.stderr.log",
            workers=workers,
            cpus=cpus,
        )
        enabled = inline is None and (cpus == 1 or workers == 1)
        expect_counter(profile, "virtio-fs-dispatch", "vcpu_inline_metadata", enabled, "units")
        expect_counter(profile, "virtio-fs-protocol", "READINDEX", False)

    case = args.metadata_case.resolve(strict=True)
    guest = args.metadata_guest.resolve(strict=True)
    run_staged(case, guest, output, enabled=True)
    run_staged(case, guest, output, enabled=False)
    for seed in args.seed:
        run_staged(case, guest, output, enabled=True, seed=seed)


if __name__ == "__main__":
    main()
