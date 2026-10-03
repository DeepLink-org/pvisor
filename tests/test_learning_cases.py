"""Guard the executable learning-path inventory and strict CI verdicts."""

import copy
import importlib.util
import json
import os
import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("learning_cases", ROOT / "scripts/cases/run.py")
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


def report(*verdicts):
    return {
        "results": [
            {"id": f"S-USE-{index:03d}", "verdict": {"verdict": verdict}, "workdir": None}
            for index, verdict in enumerate(verdicts, 1)
        ]
    }


@pytest.mark.parametrize("verdict", ["FAIL", "ERROR", "SKIP", "XFAIL", "XPASS"])
def test_gate_rejects_every_non_pass(verdict):
    with pytest.raises(ValueError, match=verdict):
        runner.require_pass(report("PASS", verdict), ["S-USE-001", "S-USE-002"])


def test_gate_requires_exact_inventory():
    expected = ["S-USE-001", "S-USE-002"]
    assert runner.require_pass(report("PASS", "PASS"), expected) == 2
    for document in [report(), report("PASS"), report("PASS", "PASS", "PASS")]:
        with pytest.raises(ValueError, match="inventory"):
            runner.require_pass(document, expected)
    duplicate = report("PASS", "PASS")
    duplicate["results"][1]["id"] = "S-USE-001"
    with pytest.raises(ValueError, match="inventory"):
        runner.require_pass(duplicate, expected)


def test_gate_does_not_conflate_execution_with_human_approval():
    document = report("PASS")
    document["results"][0]["review"] = {"state": "UNREVIEWED"}
    original = copy.deepcopy(document)
    assert runner.require_pass(document, ["S-USE-001"]) == 1
    assert document == original


def test_bilingual_cases_execute_identical_commands():
    ids = runner.catalog_ids()
    assert len(ids) >= 16
    for zh in runner.CATALOG.glob("*.md"):
        en = ROOT / "docs/src/en/cases" / zh.name
        assert en.is_file()
        for pattern in [r"^### (S-USE-\d{3})[：:]", r"```bash\n(.*?)\n```"]:
            assert re.findall(pattern, zh.read_text(), re.M | re.S) == re.findall(
                pattern, en.read_text(), re.M | re.S
            )
    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    assert "run: just cases-v2" in ci
    assert "just cases \\" in ci
    assert "semspec-use.toml lint" in ci


def test_assertion_vocabulary_is_fail_closed_and_home_is_private(tmp_path):
    workspace = tmp_path / "ws"
    workspace.mkdir()
    document = tmp_path / "value.json"
    document.write_text(json.dumps({"flag": True, "changes": []}))
    vocabulary = ROOT / "tests/semantics/vocab/journey.sh"
    environment = {**os.environ, "CASE_ROOT": str(tmp_path), "SUBJECT_BIN": "/bin/true"}
    setup = 'source "$1"; journey_setup; '
    for assertion in [
        'json_expect "$2" /flag 1',  # Python bool must not equal numeric 1.
        'json_expect "$2" /missing null',
        'json_paths "$2"',  # Missing filesystem evidence must not become empty changes.
    ]:
        result = subprocess.run(
            [
                "bash",
                "-euo",
                "pipefail",
                "-c",
                setup + assertion,
                "test",
                str(vocabulary),
                str(document),
            ],
            cwd=workspace,
            env=environment,
            capture_output=True,
            text=True,
            timeout=5,
        )
        assert result.returncode != 0
    result = subprocess.run(
        [
            "bash",
            "-euo",
            "pipefail",
            "-c",
            setup + 'printf "%s" "$HOME"; json_expect "$2" /flag true',
            "test",
            str(vocabulary),
            str(document),
        ],
        cwd=workspace,
        env=environment,
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == str(tmp_path / "home")


def test_catalog_rejects_empty_or_duplicate_specs(tmp_path):
    with pytest.raises(ValueError):
        runner.catalog_ids(tmp_path)
    (tmp_path / "cases.md").write_text("### S-USE-001: a\n### S-USE-001: b\n")
    with pytest.raises(ValueError):
        runner.catalog_ids(tmp_path)


def test_aborted_run_cannot_reuse_a_successful_old_report(tmp_path, monkeypatch):
    output = tmp_path / "report.json"
    output.write_text(json.dumps(report("PASS")))
    monkeypatch.setattr(
        runner.sys,
        "argv",
        [
            "cases",
            "--subject-bin",
            "/bin/true",
            "--output",
            str(output),
            "--case",
            "S-USE-001",
        ],
    )
    monkeypatch.setattr(
        runner.subprocess, "run", lambda *args, **kwargs: subprocess.CompletedProcess([], 0)
    )
    assert runner.main() == 1
    assert not output.exists()


def test_review_failure_cannot_be_hidden_by_passing_cases(tmp_path, monkeypatch):
    output = tmp_path / "report.json"
    monkeypatch.setattr(
        runner.sys,
        "argv",
        [
            "cases",
            "--subject-bin",
            "/bin/true",
            "--output",
            str(output),
            "--case",
            "S-USE-001",
            "--require-reviewed",
        ],
    )

    def unreviewed_run(command, **kwargs):
        assert "--require-reviewed" in command
        output.write_text(json.dumps(report("PASS")))
        return subprocess.CompletedProcess(command, 1)

    monkeypatch.setattr(runner.subprocess, "run", unreviewed_run)
    assert runner.main() == 1
