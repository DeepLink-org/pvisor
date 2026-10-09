"""Benchmark contract checks without hardware or wall-clock timing assertions."""

import io
import json
import tempfile
import threading
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import macos_migration as bench


def run_fake_trial(
    tmp_path, resources, run_record, *, filesystem_profile=False, case="startup-1cpu-128"
):
    output = tmp_path / "output"
    output.mkdir()
    if case == "rg-parallel-4":
        bench.create_parallel_fixture(output / "fixture-parallel")
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
            assert kwargs["env"]["PVISOR_FS_PROFILE"] == ("1" if filesystem_profile else "0")
            if case == "rg-parallel-4":
                files = list((kwargs["cwd"] / "fixture").rglob("*.txt"))
                assert len(files) == 2048
                assert {path.relative_to(kwargs["cwd"] / "fixture").parts[0] for path in files} == {
                    "q0",
                    "q1",
                    "q2",
                    "q3",
                }
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

    resources.enter_context(mock.patch.object(bench.subprocess, "Popen", Process))
    resources.enter_context(mock.patch.object(bench.time, "monotonic_ns", lambda: clock.now))
    args = SimpleNamespace(
        output=output,
        baseline=tmp_path / "baseline",
        rootfs=tmp_path / "rootfs",
        firmware=tmp_path / "firmware",
        filesystem_profile=filesystem_profile,
    )
    return bench.trial(args, "baseline", case, 0, 0)


class MacosMigrationTests(unittest.TestCase):
    def test_completion_records_exit_without_polling_quantization(self):
        for filesystem_profile in [False, True]:
            with self.subTest(filesystem_profile=filesystem_profile):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    row = run_fake_trial(
                        tmp_path,
                        resources,
                        dict(
                            state="completed",
                            exit_code=0,
                            executor=dict(isolation="virtual_machine"),
                        ),
                        filesystem_profile=filesystem_profile,
                    )
                    assert row["ready_ms"] == 2
                    assert row["completion_ms"] == 4
                    work = bench.Path(row["work"])
                    assert (work / "run-bundle.json").is_file()
                    assert not (tmp_path / "output/run-storage" / work.name).exists()

    def test_fast_invalid_bundle_is_not_a_performance_sample(self):
        for run_record in [
            dict(state="failed", exit_code=0, executor=dict(isolation="virtual_machine")),
            dict(state="completed", exit_code=2, executor=dict(isolation="virtual_machine")),
            dict(state="completed", exit_code=0, executor=dict(isolation="host_process")),
        ]:
            with self.subTest(run_record=run_record):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    with self.assertRaises(RuntimeError):
                        run_fake_trial(tmp_path, resources, run_record)

    def test_fixture_depth_changes_paths_without_changing_workload(self):
        for depth in [1, 8]:
            with self.subTest(depth=depth):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    root = tmp_path / "fixture"
                    bench.create_fixture(root, depth=depth)
                    files = list(root.rglob("*.txt"))
                    assert len(files) == 2048
                    assert sum("needle" in path.read_text() for path in files) == 32
                    assert {len(path.relative_to(root).parts) - 1 for path in files} == {depth}

    def test_parallel_fixture_preserves_total_work_and_checks_each_worker(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            root = tmp_path / "fixture"
            bench.create_parallel_fixture(root)
            for quarter in range(4):
                files = list((root / f"q{quarter}").rglob("*.txt"))
                assert len(files) == 512
                assert sum("needle" in file.read_text() for file in files) == 8
            payload = bench.CASES["rg-parallel-4"][3]
            assert "wait" in payload
            # Execute the actual shell payload with a tiny host rg shim to validate
            # all four worker results and propagation of a failed background command.
            shim = tmp_path / "rg"
            shim.write_text(
                '#!/bin/sh\nfor last; do :; done\nfind "$last" -type f -exec grep -l needle {} +\n'
            )
            shim.chmod(0o755)
            import os
            import subprocess

            env = dict(os.environ, PATH=f"{tmp_path}:" + os.environ["PATH"])
            subprocess.run(["/bin/sh", "-ec", payload], cwd=root.parent, env=env, check=True)
            for path in (root / "q2").rglob("*.txt"):
                path.write_text("no match\n")
            assert (
                subprocess.run(["/bin/sh", "-ec", payload], cwd=root.parent, env=env).returncode
                != 0
            )

    def test_parallel_trial_copies_all_quarters_before_launch(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row = run_fake_trial(
                tmp_path,
                resources,
                dict(state="completed", exit_code=0, executor=dict(isolation="virtual_machine")),
                case="rg-parallel-4",
            )
            assert row["case"] == "rg-parallel-4"
