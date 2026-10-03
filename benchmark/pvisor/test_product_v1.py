"""Evidence gates reject fast failures and silently weakened boundaries."""

import json
import sys
from pathlib import Path

import pytest

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


@pytest.mark.parametrize(
    "changes, message",
    [
        ({"run": {"state": "failed"}}, "completed zero-exit"),
        ({"run": {"exit_code": 1}}, "completed zero-exit"),
        ({"run": {"executor": {"isolation": "host_process"}}}, "observed isolation"),
        ({"safety": {"filesystem_changes_staged": False}}, "staging not observed"),
        ({"safety": {"filesystem_non_bypassable": False}}, "non-bypassable"),
    ],
)
def test_safe_evidence_rejects_downgraded_or_failed_job(tmp_path, changes, message):
    ctx = object.__new__(Context)
    stage = bundle(tmp_path, **changes)
    with pytest.raises(RuntimeError, match=message):
        ctx.validate_bundle("safe", tmp_path / "runs", stage)


def test_timeout_retains_stderr_and_kills_owned_process(tmp_path):
    ctx = object.__new__(Context)
    ctx.env = {}
    work = tmp_path / "workspace"
    work.mkdir()
    with pytest.raises(TimeoutError):
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


def test_failed_sample_cannot_create_performance_evidence(tmp_path):
    ctx = object.__new__(Context)
    ctx.output = tmp_path
    ctx.rows = []
    with pytest.raises(RuntimeError, match="failed samples"):
        ctx.record({"correctness": "failed", "wall_ms": 0.01})
    assert not ctx.rows
    assert not (tmp_path / "samples.jsonl").exists()
