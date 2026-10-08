#!/usr/bin/env python3
"""Exercise real Linux/KVM pause/resume using an isolated, minimal guest rootfs.

This integration runner requires a built pvisor binary and libkrunfw. It keeps
all logs and Run Bundles in a newly-created output directory; it never mounts
the host root filesystem or applies staged changes. The guest holds one file
descriptor open across pauses, so monotonically increasing records also check
that resume preserves the original process and open-file state.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import time


def main() -> int:
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pvisor", type=Path, default=repo / "target/debug/pvisor")
    parser.add_argument("--firmware-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--cycles", type=int, default=3)
    parser.add_argument("--long-pause-seconds", type=float, default=30)
    parser.add_argument("--overlaynet", choices=("off", "auto"), default="off")
    args = parser.parse_args()
    if args.cycles < 1 or args.long_pause_seconds < 0:
        parser.error("cycles must be positive and pause duration nonnegative")
    binary = args.pvisor.resolve(strict=True)
    firmware = args.firmware_dir.resolve(strict=True)
    if not (firmware / "libkrunfw.so.5").is_file():
        parser.error("firmware directory must contain libkrunfw.so.5")
    output = (args.output or repo / "target/validation" / f"kvm-{time.time_ns()}").resolve()
    output.mkdir(parents=True, exist_ok=False)
    rootfs = output / "rootfs"
    rootfs.mkdir()
    sys.path.insert(0, str(repo / "benchmark/pvisor"))
    helpers = runpy.run_path(str(repo / "benchmark/pvisor/run_all.py"))
    helpers["prepare_rootfs"](rootfs, binary)
    env = os.environ.copy()
    env["PERSISTING_RUN_HOME"] = str(output / "runs")
    checks = []

    def record(name: str, **details: object) -> None:
        entry = {"check": name, **details}
        checks.append(entry)
        print(json.dumps(entry), flush=True)
        (output / "report.json").write_text(json.dumps(checks, indent=2) + "\n")

    def control(command: str, stage: Path) -> dict:
        argv = [str(binary), command, str(stage)]
        if command == "status":
            argv.append("--json")
        started = time.monotonic()
        result = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=20)
        with (output / "control.log").open("a") as log:
            log.write(f"$ {argv!r} ({time.monotonic() - started:.3f}s)\n{result.stdout}\n{result.stderr}\n")
        if result.returncode:
            raise RuntimeError(f"{command}: {result.stderr.strip() or result.stdout.strip()}")
        return json.loads(result.stdout) if command != "kill" and result.stdout.strip() else {}

    def case(name: str, cancel_paused: bool) -> None:
        directory = output / name
        workspace = directory / "workspace"
        workspace.mkdir(parents=True)
        stage = directory / "stage"
        # A host-created stop file is read through the lower workspace only to
        # finish the successful case; all guest output remains in stage/upper.
        script = (
            'printf "%s\\n" "$$" > guest-pid; exec 3>>counter; i=0; '
            'while [ ! -f stop ]; do i=$((i + 1)); '
            'printf "%s\\n" "$i" >&3; sleep 0.05; done; '
            'printf "finished\\n" > finished'
        )
        argv = [
            str(binary), "run", "--no-agent-defaults", "--executor", "vm", "--rootfs", str(rootfs),
            "--vm-library-dir", str(firmware), "--overlaynet", args.overlaynet,
            "--cpu", "2", "--timeout", "10s", "--stage", str(stage), "--stdio", "capture",
            "--", "/bin/sh", "-c", script,
        ]
        with (directory / "run.log").open("wb") as log:
            process = subprocess.Popen(argv, cwd=workspace, env=env, stdout=log, stderr=log,
                                       start_new_session=True)
            counter = stage / "upper/counter"

            def records() -> list[int]:
                if not counter.exists():
                    return []
                data = counter.read_bytes()
                # Ignore the final partial line while the guest is running.
                return [int(line) for line in data.split(b"\n")[:-1]]

            def wait_for(predicate, description: str, timeout: float = 10) -> None:
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError(f"guest exited ({process.returncode}) waiting for {description}; see {directory / 'run.log'}")
                    if predicate():
                        return
                    time.sleep(0.05)
                raise TimeoutError(f"waiting for {description}; see {directory / 'run.log'}")

            try:
                wait_for(lambda: len(records()) >= 3, "guest activity", 30)
                pid = (stage / "upper/guest-pid").read_text()
                for cycle in range(1 if cancel_paused else args.cycles):
                    for repeat in range(2):
                        paused = control("pause", stage)
                        assert paused["state"] == "paused", paused
                    status = control("status", stage)
                    assert status["vm_status"]["state"] == "paused", status
                    before = counter.read_bytes()
                    hold = 0.4 if cancel_paused or cycle else args.long_pause_seconds
                    started = time.monotonic()
                    while time.monotonic() - started < hold:
                        assert process.poll() is None, "watchdog killed paused VM"
                        assert counter.read_bytes() == before, "guest writes continued after pause ACK"
                        time.sleep(0.05)
                    record(f"{name}:pause", cycle=cycle, duplicates=2, paused_seconds=hold,
                           records=len(records()), state=status["vm_status"]["state"])
                    if cancel_paused:
                        control("kill", stage)
                        process.wait(timeout=15)
                        record(f"{name}:cancel-paused", exit_code=process.returncode)
                        return
                    for repeat in range(2):
                        resumed = control("resume", stage)
                        assert resumed["state"] == "running", resumed
                    previous = len(records())
                    wait_for(lambda: len(records()) >= previous + 3, "writes after resume")
                    assert (stage / "upper/guest-pid").read_text() == pid
                    values = records()
                    assert values == list(range(1, len(values) + 1)), "guest counter restarted or skipped"
                    record(f"{name}:resume", cycle=cycle, records=len(values), guest_pid=pid.strip())
                (workspace / "stop").touch()
                process.wait(timeout=15)
                assert process.returncode == 0, f"guest final exit: {process.returncode}"
                assert (stage / "upper/finished").read_text() == "finished\n"
                record(f"{name}:completed", exit_code=process.returncode)
            finally:
                if process.poll() is None:
                    try:
                        control("kill", stage)
                        process.wait(timeout=8)
                    except Exception:
                        # Kill only the dedicated process group created above.
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)

    try:
        case("continuity", cancel_paused=False)
        case("cancellation", cancel_paused=True)
    except Exception as error:
        record("failure", error=f"{type(error).__name__}: {error}")
        raise
    record("all-passed", overlaynet=args.overlaynet, output=str(output))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
