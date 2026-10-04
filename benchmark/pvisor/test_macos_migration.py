"""Benchmark contract checks without hardware or wall-clock timing assertions."""

import io
import json
import threading
from types import SimpleNamespace

import macos_migration as bench
import pytest


def run_fake_trial(tmp_path, monkeypatch, run_record):
    output = tmp_path / "output"
    output.mkdir()
    clock = SimpleNamespace(now=0)
    stdout_done = threading.Event()

    class Stdout(io.BytesIO):
        def __iter__(self):
            clock.now = 2_000_000
            yield b"PVISOR_BENCH_READY\n"
            stdout_done.set()

    class Process:
        returncode = 0

        def __init__(self, command, **kwargs):
            home = bench.Path(kwargs["env"]["PVISOR_RUN_HOME"])
            bundle = home / "run-test" / "run-bundle.json"
            bundle.parent.mkdir(parents=True)
            bundle.write_text(json.dumps({"run": run_record}))
            self.stdout = Stdout()
            self.stderr = io.BytesIO(f"Run Bundle: {bundle}\n".encode())

        def wait(self, timeout=None):
            assert stdout_done.wait(1)
            # Model a process that exits at 4 ms. A timeout-based polling wait
            # notices it 50 ms later; blocking wait observes its actual exit.
            clock.now = 4_000_000 + (50_000_000 if timeout is not None else 0)
            return self.returncode

    monkeypatch.setattr(bench.subprocess, "Popen", Process)
    monkeypatch.setattr(bench.time, "monotonic_ns", lambda: clock.now)
    args = SimpleNamespace(
        output=output,
        baseline=tmp_path / "baseline",
        rootfs=tmp_path / "rootfs",
        firmware=tmp_path / "firmware",
    )
    return bench.trial(args, "baseline", "startup-1cpu-128", 0, 0)


def test_completion_records_exit_without_polling_quantization(tmp_path, monkeypatch):
    row = run_fake_trial(
        tmp_path,
        monkeypatch,
        dict(state="completed", exit_code=0, executor=dict(isolation="virtual_machine")),
    )
    assert row["ready_ms"] == 2
    assert row["completion_ms"] == 4
    work = bench.Path(row["work"])
    assert (work / "run-bundle.json").is_file()
    assert not (tmp_path / "output/run-storage" / work.name).exists()


@pytest.mark.parametrize(
    "run_record",
    [
        dict(state="failed", exit_code=0, executor=dict(isolation="virtual_machine")),
        dict(state="completed", exit_code=2, executor=dict(isolation="virtual_machine")),
        dict(state="completed", exit_code=0, executor=dict(isolation="host_process")),
    ],
)
def test_fast_invalid_bundle_is_not_a_performance_sample(tmp_path, monkeypatch, run_record):
    with pytest.raises(RuntimeError):
        run_fake_trial(tmp_path, monkeypatch, run_record)
