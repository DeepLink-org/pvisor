#!/usr/bin/env python3
"""Strict real-VM CLI contracts; no runtime capability skips or SDK adapter."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import time
from pathlib import Path


def invoke(command, directory, timeout):
    started = time.monotonic()
    with (
        (directory / "stdout.log").open("wb") as stdout,
        (directory / "stderr.log").open("wb") as stderr,
    ):
        with subprocess.Popen(
            command, stdout=stdout, stderr=stderr, start_new_session=os.name != "nt"
        ) as process:
            try:
                code = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], check=False)
                else:
                    os.killpg(process.pid, signal.SIGKILL)
                process.kill()
                process.wait()
                raise RuntimeError(f"VM deadline exceeded ({timeout}s)") from None
    return code, time.monotonic() - started


def verify(directory, cpus, seed, code, actual):
    if actual != code:
        raise AssertionError(f"guest exit: expected {code}, got {actual}")
    expected = f"vm-contract-v1 cpus={cpus} seed={seed} exit={code}\n".encode()
    if (directory / "root/result").read_bytes() != expected:
        raise AssertionError("missing or incorrect guest result")
    if (directory / "readonly/fixture").read_bytes() != b"immutable":
        raise AssertionError("read-only share changed")
    if list((directory / "root/work").iterdir()):
        raise AssertionError("guest file workload left unexpected entries")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--seed", type=int, default=1729)
    args = parser.parse_args()
    if args.rounds < 1 or args.timeout <= 0 or not 0 <= args.seed < 2**64 - args.rounds:
        parser.error("positive rounds/timeout and a uint64 seed range are required")
    binary, guest = args.binary.resolve(strict=True), args.guest.resolve(strict=True)
    firmware = args.firmware.resolve(strict=True) if args.firmware else None
    args.output.mkdir(parents=True, exist_ok=False)
    report = {
        "schema": 1,
        "platform": platform.platform(),
        "machine": platform.machine(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "guest_sha256": hashlib.sha256(guest.read_bytes()).hexdigest(),
        "firmware_sha256": hashlib.sha256(firmware.read_bytes()).hexdigest() if firmware else None,
        "cases": [],
        "passed": False,
    }
    try:
        for round_id in range(args.rounds):
            for cpus, code in [(1, 0), (2, 0), (2, 7), (1, 255)]:
                seed = args.seed + round_id
                directory = args.output / f"round-{round_id}-cpu-{cpus}-exit-{code}"
                root, readonly = directory / "root", directory / "readonly"
                root.mkdir(parents=True)
                readonly.mkdir()
                shutil.copyfile(guest, root / "probe")
                (root / "probe").chmod(0o755)
                (readonly / "fixture").write_bytes(b"immutable")
                command = [
                    str(binary),
                    "vm-run",
                    "--rootfs",
                    str(root.resolve()),
                    "--readonly",
                    str(readonly.resolve()),
                    "--cpus",
                    str(cpus),
                    "--memory",
                    "256",
                ]
                if firmware:
                    command += ["--firmware", str(firmware)]
                command += ["--", "/probe", str(cpus), str(seed), str(code)]
                case = {
                    "round": round_id,
                    "cpus": cpus,
                    "seed": seed,
                    "exit": code,
                    "command": command,
                    "passed": False,
                }
                report["cases"].append(case)
                actual, elapsed = invoke(command, directory, args.timeout)
                case.update(actual_exit=actual, seconds=elapsed)
                verify(directory, cpus, seed, code, actual)
                case["passed"] = True
                print(f"VM_CLI round={round_id} cpus={cpus} exit={code} PASS", flush=True)
        report["passed"] = True
    except Exception as error:
        report["error"] = str(error)
        raise
    finally:
        (args.output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
