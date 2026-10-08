"""Do not mistake an early guest shell or a console line for a complete OS."""

import json

import pytest
from ubuntu_baselines import protocol_line, validate_os


def proof(**overrides):
    value = {
        "os_release": 'NAME="Ubuntu"\nVERSION_ID="26.04"\n',
        "kernel": "7.0.0-34-generic",
        "pid1": "systemd",
        "multi_user": "active",
        "network_online": "active",
        "cloud_final": "active",
        "ssh_socket": "active",
    }
    return "REFERENCE_OS_READY " + json.dumps(value | overrides)


def test_full_distribution_readiness():
    assert validate_os(proof())["kernel"] == "7.0.0-34-generic"


@pytest.mark.parametrize(
    "change",
    [
        {"pid1": "sh"},
        {"kernel": "6.12.109"},
        {"os_release": 'NAME="Fedora"'},
        {"multi_user": "activating"},
        {"network_online": "inactive"},
        {"cloud_final": "failed"},
        {"ssh_socket": "inactive"},
    ],
)
def test_incomplete_or_trimmed_guest_does_not_pass(change):
    with pytest.raises(ValueError):
        validate_os(proof(**change))


@pytest.mark.parametrize("output", ["REFERENCE_READY", proof() + "\n" + proof()])
def test_os_proof_required_once(output):
    with pytest.raises(ValueError):
        validate_os(output)


def test_named_journal_record_preserves_protocol():
    assert (
        protocol_line("[   6.123456] reference-bench[123]: REFERENCE_READY\r\n")
        == "REFERENCE_READY"
    )
    assert protocol_line("REFERENCE_READY\r\n") == "REFERENCE_READY"
    assert protocol_line("[   6.123456] other-unit[123]: REFERENCE_READY") != "REFERENCE_READY"


def test_getty_controls_before_named_record():
    prefix = "\x1b[!p\x1b]104\x1b\\\x1b[0m\x1b[6n"
    assert (
        protocol_line(prefix + "[ 12.123456] reference-bench[123]: REFERENCE_READY")
        == "REFERENCE_READY"
    )
    assert (
        protocol_line(prefix + "[ 12.123456] other-unit[123]: REFERENCE_READY") != "REFERENCE_READY"
    )


def test_partial_benchmark_cannot_be_published():
    from render_ubuntu_baselines import validate_complete

    report = {
        "arguments": {"modes": "ready", "backends": "native", "samples": "2"},
        "capabilities": {"ready/native": {"state": "available"}},
        "rows": [{"mode": "ready", "backend": "native", "trial": 0, "correctness": "passed"}],
    }
    with pytest.raises(ValueError, match="Incomplete"):
        validate_complete(report)
    report["rows"].append(dict(report["rows"][0], trial=1))
    validate_complete(report)
    report["rows"][1]["correctness"] = "failed"
    with pytest.raises(ValueError, match="Failed workloads"):
        validate_complete(report)


def test_failed_preflight_has_no_latency_samples():
    from render_ubuntu_baselines import validate_complete

    report = {
        "arguments": {"modes": "claude", "backends": "pvisor-vm-hostroot", "samples": "10"},
        "capabilities": {"claude/pvisor-vm-hostroot": {"state": "failed-preflight"}},
        "rows": [],
    }
    validate_complete(report)
    report["rows"].append(
        {"mode": "claude", "backend": "pvisor-vm-hostroot", "trial": 0, "correctness": "passed"}
    )
    with pytest.raises(ValueError, match="Failed preflight"):
        validate_complete(report)


def test_login_prompt_before_named_record():
    line = "pvisor-ubuntu-reference login: [ 14.820672] reference-bench[1226]: REFERENCE_READY"
    assert protocol_line(line) == "REFERENCE_READY"
    assert protocol_line(line.replace("reference-bench", "other-unit")) != "REFERENCE_READY"
    assert protocol_line("pvisor-ubuntu-reference login: REFERENCE_READY") != "REFERENCE_READY"


def test_terminal_capability_request_before_named_record():
    line = "\x1bP+q6E616D65\x1b\\[ 13.804676] reference-bench[1217]: REFERENCE_READY"
    assert protocol_line(line) == "REFERENCE_READY"
    assert protocol_line(line.replace("reference-bench", "other-unit")) != "REFERENCE_READY"


@pytest.mark.parametrize("backend", ["qemu-ubuntu", "qemu-microvm-ubuntu"])
def test_qemu_uses_kvm_and_complete_vendor_boot_inputs(tmp_path, backend):
    from types import SimpleNamespace

    from ubuntu_baselines import qemu_command

    meta = {"initrd": "/downloads/ubuntu-initrd-generic", "root_partuuid": "abc-123"}
    argv = qemu_command(
        SimpleNamespace(memory_mib=16384), backend, tmp_path / "whole-gpt.raw", meta, "tools"
    )
    assert argv[argv.index("-accel") + 1] == "kvm"
    assert argv[argv.index("-kernel") + 1] == "/downloads/ubuntu-vmlinuz-generic"
    assert argv[argv.index("-initrd") + 1] == meta["initrd"]
    assert "root=PARTUUID=abc-123" in argv[argv.index("-append") + 1]
    assert "reboot=t" in argv[argv.index("-append") + 1] and "-no-reboot" in argv
    assert f"file={tmp_path / 'whole-gpt.raw'},format=raw,if=none,id=root" in argv
    assert argv[argv.index("-m") + 1] == "16384"
    assert (
        "virtio-blk-device,drive=root" in argv
        if "microvm" in backend
        else "virtio-blk-pci,drive=root" in argv
    )


def test_ubuntu_prepared_disk_kernel_and_initrd_receipt_rejects_tampering(tmp_path):
    from ubuntu_baselines import verify_ubuntu_assets
    from reference_baselines import digest
    names=('ubuntu-stock.raw','ubuntu-agent.raw','payload.ext4','ubuntu-vmlinux','ubuntu-initrd-generic')
    for name in names:(tmp_path/name).write_text(name)
    assets=dict(prepared_sha256={name:digest(tmp_path/name) for name in names[:3]},kernel_elf_sha256=digest(tmp_path/'ubuntu-vmlinux'),
                initrd=str(tmp_path/'ubuntu-initrd-generic'),assets={'ubuntu-initrd-generic':dict(sha256=digest(tmp_path/'ubuntu-initrd-generic'))})
    (tmp_path/'assets.json').write_text(json.dumps(assets))
    verify_ubuntu_assets(tmp_path)
    for name in names:
        (tmp_path/name).write_text('changed input')
        with pytest.raises(ValueError):verify_ubuntu_assets(tmp_path)
        (tmp_path/name).write_text(name)
