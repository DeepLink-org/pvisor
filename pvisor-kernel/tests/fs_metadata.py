"""Check Rust native metadata routes and bounded namespace stress on Linux KVM."""

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path

import stat_visibility


def expect_counter(profile, component, name, enabled, unit="calls"):
    records = [
        json.loads(line.removeprefix("pvisor-fs-profile "))
        for line in profile.splitlines()
        if line.startswith("pvisor-fs-profile ")
    ]
    final = [
        record["measurements"]
        for record in records
        if record["component"] == component and record["final_record"]
    ]
    if not final:
        raise RuntimeError(f"missing final {component} profile")
    count = sum(m.get(name, {}).get(unit, 0) for m in final)
    if bool(count) != enabled:
        raise RuntimeError(f"expected {name} {'> 0' if enabled else '= 0'}, got {count}")
    print(f"{name}={count}", flush=True)


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
            result = subprocess.run(
                command, input=b"", stdout=out, stderr=err, env=env, timeout=35
            )
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

    for cpus, workers, inline, label in (
        (1, 1, None, "inline-default"),
        (1, 1, 0, "inline-disabled"),
        (1, 4, None, "inline-multiqueue"),
        (2, 4, None, "smp-multiqueue"),
    ):
        profile = stat_visibility.run(
            args.vm_api.resolve(strict=True),
            args.stat_guest.resolve(strict=True),
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
