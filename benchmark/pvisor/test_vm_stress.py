"""The hardware gate must retain failure evidence and never turn a skip green."""

import json
from types import SimpleNamespace

import pytest
from vm_stress import Harness


def test_missing_hardware_is_failure_with_a_retained_report(tmp_path, monkeypatch):
    guest = tmp_path / "guest"
    guest.write_bytes(b"guest-identity")
    binary = tmp_path / "binary"
    # Pass the archived-CLI compatibility check, then exercise the hardware
    # failure path below without starting a real guest or snapshot command.
    binary.write_text(
        '#!/bin/sh\nif [ "$1" = snapshot ] && [ "$2" = --help ]; then exit 0; fi\nexit 1\n'
    )
    binary.chmod(0o755)
    harness = Harness(
        SimpleNamespace(
            binary=binary,
            guest=guest,
            output=tmp_path / "report",
            cycles=1,
            forks=2,
            seed=123,
            storage="both",
        )
    )

    def unavailable(*args, **kwargs):
        raise PermissionError("injected KVM permission failure")

    monkeypatch.setattr("vm_stress.os.open", unavailable)
    # The injected gate fails before any mount/process is created. Supply the
    # empty Linux mount inventory so this unit test also runs on macOS.
    monkeypatch.setattr("vm_stress.mounts_under", lambda root: [])
    monkeypatch.setattr(harness, "live_processes", lambda: [])
    with pytest.raises(PermissionError, match="injected KVM"):
        harness.run()
    report = json.loads((harness.root / "result.json").read_text())
    assert report["correctness"] == "failed"
    assert "injected KVM permission failure" in report["error"]
    assert report["seed"] == 123
    assert report["cleanup_errors"] == []
    assert report["live_processes"] == []
    assert harness.processes == []
    assert harness.instances == []
    assert (harness.root / "harness.py").is_file()
    events = [json.loads(line) for line in (harness.root / "events.jsonl").read_text().splitlines()]
    assert events == report["events"]
    assert events[0]["event"] == "failure"
