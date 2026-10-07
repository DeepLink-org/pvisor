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
        "if name == 'python3' and args[0] in ['scripts/build-pvisor.py', 'scripts/packaging/build_daemon.py']:\n"
        "    profile = args[args.index('--profile') + 1]\n"
        "    target = Path(args[args.index('--target-dir') + 1])\n"
        "    names = ['pvisor', 'pvisor-daemon', 'pvisor-cache', 'pvisor-tui', 'pvisor-replay', 'pvisor-memory-pool']\n"
        "    if args[0] == 'scripts/packaging/build_daemon.py': names = ['pvisor-daemon']\n"
        "    for binary_name in names:\n"
        "        binary = target / ('debug' if profile == 'dev' else profile) / binary_name\n"
        "        binary.parent.mkdir(parents=True, exist_ok=True)\n"
        "        binary.write_text('#!/bin/sh\\nexit 0\\n')\n"
        "        binary.chmod(0o755)\n"
        "if name == 'cargo' and args[:1] == ['build'] and '--example' in args:\n"
        "    target = Path(args[args.index('--target-dir') + 1])\n"
        "    binary = target / 'release/examples' / args[args.index('--example') + 1]\n"
        "    binary.parent.mkdir(parents=True, exist_ok=True)\n"
        "    binary.write_text('#!/bin/sh\\nexit 0\\n')\n"
        "    binary.chmod(0o755)\n"
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

    run.target_dir = tmp_path / "target with spaces"
    return run


def test_test_routes_packages_and_python(run_task):
    commands = run_task("test", "core", "capture", "pvisor-overlay-core")
    assert commands == [
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "-p",
            "pvisor-core",
            "-p",
            "pvisor-gateway",
            "-p",
            "pvisor-overlay-core",
        ]
    ]
    commands = run_task("test")
    signing = (
        [["python3", "scripts/sign-vm-tests.py", "--workspace"]] if sys.platform == "darwin" else []
    )
    assert commands == signing + [
        ["cargo", "nextest", "run", "--locked", "--workspace"],
        ["uv", "run", "--extra", "dev", "pytest", "-q"],
    ]


def test_firmware_tasks_use_in_tree_sources_and_forward_make_arguments(run_task):
    assert run_task("fw-build", "-j4", "--offline") == [
        [
            "python3",
            "scripts/build-firmware.py",
            "--target-dir",
            str(run_task.target_dir),
            "-j4",
            "--offline",
        ]
    ]
    assert run_task("test-fw") == [
        [
            "uv",
            "run",
            "--no-project",
            "--with",
            "pyelftools==0.33",
            "python",
            "-m",
            "unittest",
            "discover",
            "-s",
            "fw/tests",
            "-v",
        ]
    ]


def test_vm_package_signs_before_running_native_tests(run_task):
    commands = run_task("test", "pvisor-vm")
    signing = (
        [["python3", "scripts/sign-vm-tests.py", "-p", "pvisor-vm"]]
        if sys.platform == "darwin"
        else []
    )
    assert commands == signing + [["cargo", "nextest", "run", "--locked", "-p", "pvisor-vm"]]


def test_daemon_build_routes_through_native_packaging_pipeline(run_task, tmp_path):
    commands = run_task("daemon-build")
    assert commands == [
        [
            "python3",
            "scripts/packaging/build_daemon.py",
            "--profile",
            "dev",
            "--target-dir",
            str(tmp_path / "target with spaces"),
        ]
    ]
    assert run_task("test-daemon") == [
        ["cargo", "nextest", "run", "--locked", "-p", "pvisor-daemon"]
    ]


@pytest.mark.parametrize(
    "selector,package", [("pvisor", "pvisor"), ("cli", "pvisor-cli"), ("pvisor-cli", "pvisor-cli")]
)
def test_native_executor_package_keeps_hvf_signing(run_task, selector, package):
    commands = run_task("test", selector)
    signing = (
        [["python3", "scripts/sign-vm-tests.py", "-p", package]] if sys.platform == "darwin" else []
    )
    assert commands == signing + [["cargo", "nextest", "run", "--locked", "-p", package]]


def test_product_check_selects_application(run_task):
    assert run_task("check") == [["cargo", "check", "--locked", "-p", "pvisor-cli"]]


def test_isolation_selects_moved_application_tests(run_task):
    assert run_task("test-isolation") == [
        [
            "cargo",
            "nextest",
            "run",
            "--locked",
            "-p",
            "pvisor-cli",
            "--test",
            "rootless_local",
            "--test",
            "run_config_cli",
            "--no-capture",
        ]
    ]


def test_retired_nativepvisor_is_not_a_vm_signing_selector():
    for path in (ROOT / "justfile", ROOT / "scripts/sign-vm-tests.py"):
        contents = path.read_text()
        assert "nativepvisor" not in contents
        assert "pvisor-vm" in contents


def test_cluster_only_recipes_are_retired():
    recipes = set(
        subprocess.check_output(
            ["just", "--justfile", str(ROOT / "justfile"), "--summary"], text=True
        ).split()
    )
    assert not any("cluster" in name for name in recipes)
    assert "test-service" not in recipes
    assert "test-service-vm" not in recipes
    assert {"service-build", "daemon-build", "daemon-install", "test-daemon"} <= recipes
    assert {"test-hvf-cold-restore", "test-vm-snapshot-state", "vm-cases"} <= recipes


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
        "--config",
        "semspec-doc.toml",
        "run",
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


def test_vm_cases_build_driver_and_forward_selection(run_task, tmp_path):
    commands = run_task("vm-cases", "--case", "S-DOC-060", "--keep")
    target = tmp_path / "target with spaces"
    assert [
        "cargo",
        "build",
        "--locked",
        "-p",
        "pvisor",
        "--release",
        "--example",
        "vm_control_case",
        "--target-dir",
        str(target),
    ] in commands
    assert commands[-1][-3:] == ["--case", "S-DOC-060", "--keep"]
    assert "docs/src/zh/reference/cases-vm.md" in commands[-1]
    assert str(target / "pvisor-vm-case-report.json") in commands[-1]


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
    for path in sorted((ROOT / "docs/src").glob("*/development/engineering.md")):
        calls = set(re.findall(r"`just ([\w-]+)", path.read_text()))
        assert calls, f"Missing task reference in {path}"
        assert calls <= recipes, f"Unknown recipes in {path}: {calls - recipes}"
