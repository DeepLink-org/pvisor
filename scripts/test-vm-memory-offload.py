#!/usr/bin/env python3
"""Real KVM + cgroup v2 swap test. Uses only its own delegated test scope.

Run inside systemd-run --user --scope --unit=pvisor-residency-test-<unique>
-p Delegate=yes, under the existing pvisor-dev AppArmor profile when needed.
The test moves only itself into a supervisor child and enables memory only in
that disposable scope. It never changes swap, sysctls, or ancestor limits.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import runpy
import shutil
import signal
import subprocess
import sys
import time


def main() -> None:
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firmware-dir", type=Path, required=True)
    parser.add_argument("--cycles", type=int, default=2)
    parser.add_argument("--cancel-after-offload", action="store_true")
    args = parser.parse_args()
    assert args.cycles > 0
    membership = Path("/proc/self/cgroup").read_text().strip()
    assert membership.startswith("0::/")
    scope = Path("/sys/fs/cgroup") / membership.split("::", 1)[1].lstrip("/")
    assert scope.name.startswith("pvisor-residency-test-") and scope.name.endswith(".scope"), scope
    assert scope.stat().st_uid == os.getuid(), "test scope is not delegated"
    supervisor = scope / "supervisor"
    supervisor.mkdir()
    (supervisor / "cgroup.procs").write_text("0")
    assert not (scope / "cgroup.procs").read_text().strip(), "scope has other processes"
    (scope / "cgroup.subtree_control").write_text("+memory")

    output = repo / "target/validation/residency" / f"offload-{time.time_ns()}"
    output.mkdir(parents=True)
    binary = repo / "target/debug/pvisor"
    rootfs = output / "rootfs"
    rootfs.mkdir()
    sys.path.insert(0, str(repo / "benchmark/pvisor"))
    helpers = runpy.run_path(str(repo / "benchmark/pvisor/run_all.py"))
    helpers["prepare_rootfs"](rootfs, binary)
    probe = output / "vm-memory-probe"
    subprocess.run(["cc", "-O2", "-static", str(repo / "scripts/fixtures/vm-memory-probe.c"),
                    "-o", str(probe)], check=True)
    shutil.copy2(probe, rootfs / "bin/vm-memory-probe")
    workspace = output / "workspace"
    workspace.mkdir()
    stage = output / "stage"
    env = dict(os.environ, PERSISTING_RUN_HOME=str(output / "runs"))
    checks: list[dict] = []

    def record(name: str, **values) -> None:
        checks.append({"check": name, **values})
        (output / "report.json").write_text(json.dumps(checks, indent=2) + "\n")
        print(json.dumps(checks[-1]), flush=True)

    def control(command: str, *extra: str, ok: bool = True) -> dict:
        argv = [str(binary), command, str(stage), *extra]
        result = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=25)
        if ok:
            assert result.returncode == 0, result.stderr
            if command == "kill":
                return {"message": result.stdout}
            return json.loads(result.stdout)
        assert result.returncode != 0, f"unexpected success: {argv}"
        return {"error": result.stderr}

    def status() -> dict:
        return control("status", "--json")["vm_status"]

    argv = [str(binary), "run", "--no-agent-defaults", "--executor", "vm",
            "--rootfs", str(rootfs), "--vm-library-dir", str(args.firmware_dir.resolve()),
            "--vm-cgroup-parent", str(scope), "--overlaynet", "off", "--cpu", "2",
            "--memory", "768MiB", "--timeout", "180s", "--stage", str(stage),
            "--stdio", "capture", "--", "/bin/vm-memory-probe"]
    with (output / "run.log").open("wb") as log:
        process = subprocess.Popen(argv, cwd=workspace, env=env, stdout=log, stderr=log,
                                   start_new_session=True)
        counter = stage / "upper/counter"

        def wait_for(predicate, timeout: float = 45) -> None:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                assert process.poll() is None, f"VM exited: see {output / 'run.log'}"
                if predicate(): return
                time.sleep(0.05)
            raise TimeoutError(f"see {output}")

        try:
            wait_for(lambda: counter.exists() and len(counter.read_text().splitlines()) >= 3)
            original_pid = counter.read_text().splitlines()[0].split()[0]
            initial = status()
            cgroup = Path(initial["memory"]["cgroup"])
            runner_pids = (cgroup / "cgroup.procs").read_text().split()
            assert len(runner_pids) == 1 and str(process.pid) not in runner_pids
            assert cgroup.parent == scope
            rejected = control("offload", "--mib", "128", ok=False)
            record("reject-running-offload", **rejected)
            for cycle in range(args.cycles):
                assert control("pause")["state"] == "paused"
                assert control("pause")["state"] == "paused"
                before_counter = counter.read_bytes()
                accepted = control("offload", "--mib", "128")
                operation_id = accepted["memory"]["last_offload"]["operation_id"]
                if args.cancel_after_offload:
                    started = time.monotonic()
                    control("kill")
                    process.wait(timeout=15)
                    assert process.returncode == 130, process.returncode
                    deadline = time.monotonic() + 15
                    while cgroup.exists() and time.monotonic() < deadline:
                        time.sleep(0.05)
                    assert not cgroup.exists(), "runner cgroup leaked after cancellation"
                    record("cancel-after-offload-passed", accepted=accepted,
                           exit_code=process.returncode,
                           elapsed_ms=(time.monotonic() - started) * 1000,
                           output=str(output))
                    return
                samples = []
                deadline = time.monotonic() + 90
                while True:
                    current = status()
                    assert current["state"] == "paused"
                    assert counter.read_bytes() == before_counter
                    report = current["memory"]["last_offload"]
                    assert report["operation_id"] == operation_id
                    samples.append(current["memory"]["sample"])
                    if report["state"] != "reclaiming": break
                    # Completion can race with another CLI invocation. Resume
                    # and duplicate-reclaim rejection are covered by unit tests.
                    assert time.monotonic() < deadline, "reclaim did not finish"
                    time.sleep(0.1)
                assert report["state"] in ("completed", "partial"), report
                after = report["after"]
                before = report["before"]
                assert before["current_bytes"] - after["current_bytes"] >= 64 * 1024 * 1024, report
                assert after["swap_bytes"] > before["swap_bytes"], report
                started = time.monotonic()
                assert control("resume")["state"] == "running"
                ack_ms = (time.monotonic() - started) * 1000
                wait_for(lambda: counter.read_bytes() != before_counter)
                first_progress_ms = (time.monotonic() - started) * 1000
                lines = counter.read_text().splitlines()
                assert all(line.split()[0] == original_pid for line in lines)
                assert [int(line.split()[1]) for line in lines] == list(range(1, len(lines) + 1))
                assert (cgroup / "cgroup.procs").read_text().split() == runner_pids
                record("offload-resume", cycle=cycle, report=report,
                       resume_ack_ms=ack_ms, first_progress_ms=first_progress_ms)
            (workspace / "stop").touch()
            process.wait(timeout=30)
            assert process.returncode == 0
            assert (stage / "upper/verified").read_text() == "all 256 MiB verified\n"
            assert not cgroup.exists(), "runner cgroup leaked after normal exit"
            record("all-passed", output=str(output), runner_pid=runner_pids[0])
        except Exception as error:
            record("failed", error=str(error))
            raise
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
