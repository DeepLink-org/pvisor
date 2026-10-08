"""Keep HVF signing on native tests, never on the standalone daemon."""

import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]


def test_nextest_signature_selection(monkeypatch):
    path = ROOT / "scripts/sign-vm-tests.py"
    spec = importlib.util.spec_from_file_location("sign_vm_tests", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    packages = ["pvisor-vm", "pvisor", "pvisor-cli", "nativepvisor", "pvisor-daemon", "pvisor-core"]
    suites = {
        package: {"package-name": package, "binary-path": f"/tests/{package}"}
        for package in packages
    }
    commands = []

    def run(command, **kwargs):
        commands.append(command)
        return SimpleNamespace(stdout=json.dumps({"rust-suites": suites}))

    monkeypatch.setattr(module.subprocess, "run", run)
    monkeypatch.setattr(module.sys, "argv", [str(path), "--workspace"])
    module.main()
    assert commands[0] == [
        "cargo",
        "nextest",
        "list",
        "--locked",
        "--message-format",
        "json",
        "--workspace",
    ]
    assert [command[-1] for command in commands[1:]] == [
        "/tests/pvisor-vm",
        "/tests/pvisor",
        "/tests/pvisor-cli",
    ]
    assert all(
        command[command.index("--entitlements") + 1]
        == str(ROOT / "crates/pvisor/macos-hypervisor.entitlements")
        for command in commands[1:]
    )
