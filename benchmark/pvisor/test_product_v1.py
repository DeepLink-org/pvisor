"""Evidence gates reject fast failures and silently weakened boundaries."""

import json
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from v1.common import Context


def bundle(tmp_path, **changes):
    value = {
        "run": {
            "state": "completed",
            "exit_code": 0,
            "executor": {"isolation": "rootless_process"},
        },
        "safety": {"filesystem_changes_staged": True, "filesystem_non_bypassable": True},
    }
    for section, fields in changes.items():
        value[section].update(fields)
    stage = tmp_path / "stage"
    stage.mkdir()
    (stage / "run-bundle.json").write_text(json.dumps(value))
    return stage


class ProductV1Tests(unittest.TestCase):
    def test_safe_evidence_rejects_downgraded_or_failed_job(self):
        for changes, message in [
            ({"run": {"state": "failed"}}, "completed zero-exit"),
            ({"run": {"exit_code": 1}}, "completed zero-exit"),
            ({"run": {"executor": {"isolation": "host_process"}}}, "observed isolation"),
            ({"safety": {"filesystem_changes_staged": False}}, "staging not observed"),
            ({"safety": {"filesystem_non_bypassable": False}}, "non-bypassable"),
        ]:
            with self.subTest(changes=changes, message=message):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    ctx = object.__new__(Context)
                    stage = bundle(tmp_path, **changes)
                    with self.assertRaisesRegex(RuntimeError, message):
                        ctx.validate_bundle("safe", tmp_path / "runs", stage)

    def test_timeout_retains_stderr_and_kills_owned_process(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            ctx = object.__new__(Context)
            ctx.env = {}
            work = tmp_path / "workspace"
            work.mkdir()
            with self.assertRaises(TimeoutError):
                ctx.run(
                    [
                        sys.executable,
                        "-c",
                        'import sys,time; print("before-timeout",file=sys.stderr,flush=True); time.sleep(30)',
                    ],
                    cwd=work,
                    timeout=0.2,
                )
            assert b"before-timeout" in (tmp_path / "command.stderr").read_bytes()
            evidence = json.loads((tmp_path / "command.json").read_text())
            assert evidence["timed_out"] and evidence["exit_code"] < 0

    def test_failed_sample_cannot_create_performance_evidence(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            ctx = object.__new__(Context)
            ctx.output = tmp_path
            ctx.rows = []
            with self.assertRaisesRegex(RuntimeError, "failed samples"):
                ctx.record({"correctness": "failed", "wall_ms": 0.01})
            assert not ctx.rows
            assert not (tmp_path / "samples.jsonl").exists()

    def test_current_staging_requires_observed_rootless_read_and_write_boundaries(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            ctx = object.__new__(Context)
            stage = bundle(tmp_path)
            with self.assertRaisesRegex(RuntimeError, "non-bypassable read/write"):
                ctx.validate_bundle("staged", tmp_path / "runs", stage)
            path = stage / "run-bundle.json"
            value = json.loads(path.read_text())
            value["safety"].update(
                filesystem_read_non_bypassable=True, filesystem_write_non_bypassable=True
            )
            path.write_text(json.dumps(value))
            assert (
                ctx.validate_bundle("staged", tmp_path / "runs", stage)["run"]["executor"][
                    "isolation"
                ]
                == "rootless_process"
            )
            value["run"]["executor"]["isolation"] = "host_process"
            path.write_text(json.dumps(value))
            with self.assertRaisesRegex(RuntimeError, "observed isolation"):
                ctx.validate_bundle("staged", tmp_path / "runs", stage)

    def test_product_suite_cannot_mix_benchmark_ids_or_use_superseded_filesystem(self):
        from product_v1 import benchmark_for_suites

        assert benchmark_for_suites("apply,baselines") == "B-APPLY"
        for suites in ("network,apply", "filesystem", "network,network"):
            with self.assertRaises(ValueError):
                benchmark_for_suites(suites)

    def test_later_commands_do_not_erase_earlier_reproduction_evidence(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            ctx = object.__new__(Context)
            ctx.env = {}
            work = tmp_path / "workspace"
            work.mkdir()
            for marker in ("first-command", "second-command"):
                ctx.run([sys.executable, "-c", f"print({marker!r})"], cwd=work)
            retained = {
                path.read_text().strip()
                for path in (tmp_path / "commands").glob("*/command.stdout")
            }
            assert retained == {"first-command", "second-command"}
            assert (tmp_path / "command.stdout").read_text().strip() == "second-command"
