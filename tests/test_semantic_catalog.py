from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest
import tomllib

ROOT = Path(__file__).resolve().parents[1]
VOCAB = ROOT / "tests/semantics/vocab"


def run_vocab(tmp_path: Path, script: str, subject: Path | None = None):
    case_root = tmp_path / "case with spaces"
    workspace = case_root / "ws"
    workspace.mkdir(parents=True)
    environment = os.environ.copy()
    environment.update(
        CASE_ROOT=str(case_root),
        WS=str(workspace),
        SUBJECT_BIN=str(subject or shutil.which("true")),
    )
    result = subprocess.run(
        [
            "bash",
            "-euo",
            "pipefail",
            "-c",
            'builtin source "$1"; builtin source "$2"; ' + script,
            "catalog-test",
            str(VOCAB / "core.sh"),
            str(VOCAB / "cases.sh"),
        ],
        cwd=workspace,
        env=environment,
        capture_output=True,
        text=True,
        timeout=15,
    )
    return result, case_root


@pytest.mark.parametrize(
    ("expectation", "command", "exit_code"),
    [
        ("nonzero", "printf before; false; printf leaked > leaked", 0),
        ("success", "false; printf leaked > leaked", 1),
        ("nonzero", "true", 1),
    ],
)
def test_expected_nonzero_preserves_errexit(tmp_path, expectation, command, exit_code):
    result, root = run_vocab(
        tmp_path,
        f"case_setup\ncase_run {expectation} <<'COMMAND'\n{command}\nCOMMAND\n",
    )
    assert result.returncode == exit_code, result.stderr
    assert not (root / "ws/leaked").exists()
    if exit_code == 0:
        assert (root / "command.log").read_text() == "before"


def test_fixture_subject_and_original_artifact_assertions(tmp_path):
    subject = tmp_path / "subject with spaces"
    subject.write_text("#!/bin/sh\nprintf 'chosen subject\\n'\n")
    subject.chmod(0o755)
    script = """case_setup
case_run success <<'COMMAND'
pvisor
COMMAND
stdout_has 'chosen subject'
mkdir -p "$PVISOR_CASE_RECORDS/job"
cat > "$PVISOR_CASE_RECORDS/job/run-bundle.json" <<'JSON'
{"run":{"state":"completed","exit_code":0},"safety":{"enabled":true},"changes":[{"path":"note"}]}
JSON
printf '{"overlay":null}' > "$PVISOR_CASE_RECORDS/job/run.json"
bundle_expect run.state completed
bundle_expect run.exit_code 0
bundle_expect safety.enabled true
bundle_expect changes.0.path note
bundle_contains changes note
record_expect overlay null
"""
    result, root = run_vocab(tmp_path, script, subject)
    assert result.returncode == 0, result.stderr
    assert (root / "command.log").read_text() == "chosen subject\n"
    spec = json.loads((root / "ws/run-spec.json").read_text())
    assert Path(spec["invocation"]["program"]).is_absolute()
    proxy, gateway = (root / "ports").read_text().split()
    assert proxy != gateway


def test_migrated_catalog_retains_active_scenarios_and_bash_prerequisites():
    contents = (ROOT / "docs/src/zh/reference/cases.md").read_text()
    titles = re.findall(r"^### (S-DOC-\d{3})：([A-M]\d{2}) ", contents, re.MULTILINE)
    labels = [
        "A07",
        "B04",
        "C06",
        "D06",
        "E06",
        "F04",
        "G07",
        "H02",
        "I03",
        "J03",
        "K04",
        "M02",
    ]
    expected = [
        f"{label[0]}{number:02d}" for label in labels for number in range(1, int(label[1:]) + 1)
    ]
    assert [label for _, label in titles] == expected
    assert len({identifier for identifier, _ in titles}) == 54
    assert contents.count("case_run success <<'CASE_COMMAND'") == 46
    assert contents.count("case_run nonzero <<'CASE_COMMAND'") == 8
    assert "xfail-on" not in contents
    scripts = re.findall(
        r"^```bash\n((?:require_\w+\n)+case_setup\n.*?)^```", contents, re.MULTILINE | re.DOTALL
    )
    assert len(scripts) == 54
    assert all(
        "/tmp/pvisor-cases" not in script and "/path/to/" not in script for script in scripts
    )

    configuration = tomllib.loads((ROOT / "semspec-doc.toml").read_text())
    assert "docs/src/zh/reference" in configuration["project"]["spec_dirs"]
    assert not (ROOT / "tests/semantics/documented-cases.md").exists()
    assert "retired" not in configuration["project"]
    assert "requirements" not in configuration
    assert "requires=" not in contents
    assert not {"S-DOC-053", "S-DOC-054"} & {identifier for identifier, _ in titles}


def test_default_review_scope_is_stage_and_doc_regressions_remain_configured():
    stage = tomllib.loads((ROOT / "semspec.toml").read_text())
    doc = tomllib.loads((ROOT / "semspec-doc.toml").read_text())
    assert stage["project"]["spec_dirs"] == ["tests/semantics"]
    assert doc["project"]["spec_dirs"] == ["docs/src/zh/reference"]
    assert stage["project"]["ledger"] == doc["project"]["ledger"]


def test_vm_catalog_has_executable_sdk_checks_and_explicit_prerequisites():
    contents = (ROOT / "docs/src/zh/reference/cases-vm.md").read_text()
    identifiers = re.findall(r"^### (S-DOC-\d{3})：", contents, re.MULTILINE)
    assert identifiers == [f"S-DOC-{number:03d}" for number in range(57, 63)]
    assert contents.count("**语义**") == contents.count("**违反示例**") == 6
    assert contents.count("vocab=core.sh,cases.sh,pvisor.sh,vm.sh") == 6
    assert (ROOT / "docs/src/zh/reference/vocab/vm.sh").resolve() == (VOCAB / "vm.sh").resolve()
    assert (ROOT / "docs/src/zh/reference/vocab/vm.sh").is_file()
    blocks = re.findall(r"^```bash\n(.*?)^```", contents, re.MULTILINE | re.DOTALL)
    assert len(blocks) == 6
    assert all(
        block.startswith(("require_vm_case\n", "require_vm_sdk\n", "require_vm_compression\n"))
        for block in blocks
    )
    assert contents.count('"$VM_CASE_DRIVER"') == 4
    assert "xfail-on" not in contents


def test_vm_prerequisite_skips_missing_rootfs_before_invoking_product(tmp_path):
    environment = {
        **os.environ,
        "PVISOR_CASE_ROOTFS": str(tmp_path / "missing"),
        "PVISOR_CASE_VM_DRIVER": str(tmp_path / "missing-driver"),
    }
    result = subprocess.run(
        [
            "bash", "-euo", "pipefail", "-c",
            'source "$1"; source "$2"; require_vm_sdk; echo reached',
            "vm-prerequisite", str(VOCAB / "pvisor.sh"), str(VOCAB / "vm.sh"),
        ],
        env=environment,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 77
    assert "SKIP:" in result.stderr
    assert "reached" not in result.stdout


def test_bash_prerequisites_skip_without_turning_assertions_into_pass():
    result = subprocess.run(
        [
            "bash",
            "-euo",
            "pipefail",
            "-c",
            'source "$1"; require_agent; echo reached',
            "prerequisite-test",
            str(VOCAB / "pvisor.sh"),
        ],
        env={**os.environ, "PVISOR_CASE_AGENT": ""},
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 77
    assert "SKIP:" in result.stderr
    assert "reached" not in result.stdout
