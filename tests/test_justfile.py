"""Check task routing and argument forwarding without rebuilding the workspace."""

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
pytestmark = pytest.mark.skipif(shutil.which("just") is None, reason="just is not installed")


@pytest.fixture
def run_task(tmp_path):
    log = tmp_path / "commands.jsonl"
    stub = tmp_path / "tool"
    stub.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "from pathlib import Path\n"
        "name, args = Path(sys.argv[0]).name, sys.argv[1:]\n"
        "with open(os.environ['JUST_TEST_LOG'], 'a') as log:\n"
        "    log.write(json.dumps([name, *args]) + '\\n')\n"
        "if name == 'python3' and args[0] == 'scripts/build-pvisor.py':\n"
        "    profile = args[args.index('--profile') + 1]\n"
        "    target = Path(args[args.index('--target-dir') + 1])\n"
        f"    names = {sorted(path.stem for path in (ROOT / 'crates/persisting-pvisor/src/bin').glob('*.rs'))!r}\n"
        "    for binary_name in names:\n"
        "        binary = target / ('debug' if profile == 'dev' else profile) / binary_name\n"
        "        binary.parent.mkdir(parents=True, exist_ok=True)\n"
        "        binary.write_text('#!/bin/sh\\nexit 0\\n')\n"
        "        binary.chmod(0o755)\n"
        "if name == 'codesign': print('com.apple.security.hypervisor')\n"
    )
    stub.chmod(0o755)
    for name in ("cargo", "uv", "uvx", "python3", "codesign"):
        (tmp_path / name).symlink_to(stub)

    def run(*args):
        log.unlink(missing_ok=True)
        subprocess.run(
            ["just", "--justfile", str(ROOT / "justfile"), *args],
            cwd=ROOT,
            env={
                **os.environ,
                "PATH": f"{tmp_path}{os.pathsep}{os.environ['PATH']}",
                "CARGO_TARGET_DIR": str(tmp_path / "target with spaces"),
                "JUST_TEST_LOG": str(log),
            },
            check=True,
            capture_output=True,
            text=True,
        )
        return [json.loads(line) for line in log.read_text().splitlines()]

    return run


def test_test_routes_packages_and_python(run_task):
    commands = run_task("test", "control", "capture", "persisting-overlay-core")
    assert commands == [
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "-p",
            "persisting-control",
            "-p",
            "persisting-gateway",
            "-p",
            "persisting-overlay-core",
        ]
    ]
    commands = run_task("test")
    assert commands[0] == ["cargo", "nextest", "run", "--locked", "--workspace"]
    assert commands[1] == ["uv", "run", "--extra", "dev", "pytest", "-q"]


def test_ci_checks_format_without_rewriting(run_task):
    commands = run_task("ci")
    assert ["cargo", "fmt", "--all", "--", "--check"] in commands
    assert [
        "uvx",
        "ruff",
        "format",
        "pvisor",
        "tests",
        "examples",
        "conftest.py",
        "--check",
    ] in commands
    assert all(
        "--check" in command for command in commands if "fmt" in command or "format" in command
    )
    assert any(command[:2] == ["python3", "scripts/build-pvisor.py"] for command in commands)


def test_cases_preserve_shell_characters_in_arguments(run_task, tmp_path):
    marker = tmp_path / "must-not-exist"
    selection = f"S-DOC-001,$(touch {marker})"
    commands = run_task("cases", "--case", selection, "--keep", "--require-reviewed")
    target = tmp_path / "target with spaces"
    assert commands[-1] == [
        "cargo",
        "run",
        "--quiet",
        "--manifest-path",
        "tools/semspec/Cargo.toml",
        "--locked",
        "--",
        "run",
        "docs/src/zh/reference/cases.md",
        "--domain",
        "DOC",
        "--subject-bin",
        str(target / "release/pvisor"),
        "--format",
        "json",
        "--output",
        str(target / "pvisor-case-report.json"),
        "--case",
        selection,
        "--keep",
        "--require-reviewed",
    ]
    assert commands[0][0:2] == ["python3", "scripts/build-pvisor.py"]
    assert commands[0][commands[0].index("--profile") + 1] == "release"
    assert not marker.exists()


def test_ci_and_task_reference_use_existing_recipes():
    recipes = set(
        subprocess.check_output(
            ["just", "--justfile", str(ROOT / "justfile"), "--summary"], text=True
        ).split()
    )
    workflows = sorted((ROOT / ".github/workflows").glob("*.yml"))
    for path in workflows:
        calls = re.findall(
            r"^\s*(?:-\s*)?(?:run:\s*)?just[ \t]+([\w-]+)", path.read_text(), re.MULTILINE
        )
        assert set(calls) <= recipes, f"Unknown recipes in {path}: {set(calls) - recipes}"
    for language in ("en", "zh"):
        path = ROOT / f"docs/src/{language}/development/engineering.md"
        calls = set(re.findall(r"`just ([\w-]+)", path.read_text()))
        assert calls, f"Missing task reference in {path}"
        assert calls <= recipes, f"Unknown recipes in {path}: {calls - recipes}"
