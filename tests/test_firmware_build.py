from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import platform
import subprocess
import tarfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"
KERNEL_VERSION = "linux-6.12.1"
LIBRARY_NAME = "libkrunfw.so.5.0.0"


@pytest.fixture
def firmware(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> SimpleNamespace:
    spec = importlib.util.spec_from_file_location(
        "firmware_build_test", ROOT / "scripts" / "packaging" / "firmware.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    fw = tmp_path / "fw"
    fw.mkdir()
    kernel = tmp_path / f"{KERNEL_VERSION}.tar.xz"
    kernel.write_bytes(b"synthetic kernel source archive\n")
    kernel_hash = hashlib.sha256(kernel.read_bytes()).hexdigest()
    files = {
        "Makefile": (
            f"KERNEL_VERSION = {KERNEL_VERSION}\n"
            f"KERNEL_SHA256 = {kernel_hash}\n"
            "FULL_VERSION = 5.0.0\nABI_VERSION = 5\n"
        ),
        "bin2cbundle.py": "# synthetic bundle builder\n",
        "README.md": "Synthetic firmware fixture\n",
        "IMPORT.md": "Synthetic import record\n",
        "config-libkrunfw": "CONFIG_SYNTHETIC=y\n",
        "LICENSE-GPL": "Synthetic license fixture\n",
        "patches/0001-synthetic.patch": "synthetic patch\n",
        "include/synthetic.h": "#define SYNTHETIC 1\n",
    }
    for name, contents in files.items():
        path = fw / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
    builder = tmp_path / "firmware.py"
    builder.write_bytes(Path(module.__file__).read_bytes())
    monkeypatch.setattr(module, "__file__", str(builder))
    monkeypatch.setattr(module, "ROOT", tmp_path)
    monkeypatch.setattr(module, "FW", fw)
    monkeypatch.setattr(module.sys, "platform", "linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.delenv("PVISOR_FW_BUILD_DIR", raising=False)

    tools = Mock(side_effect=lambda target: (["make", "SEV=0", "TDX=0"], {"cc": "fake cc 1"}))
    monkeypatch.setattr(module, "build_tools", tools)
    real_download = module.download_kernel
    download = Mock(return_value=kernel)
    monkeypatch.setattr(module, "download_kernel", download)
    network = Mock(side_effect=AssertionError("Unexpected network access"))
    monkeypatch.setattr(module.urllib.request, "urlopen", network)
    monkeypatch.setattr(
        module.subprocess, "check_output", Mock(side_effect=AssertionError("Unexpected tool probe"))
    )

    def compile_firmware(command, *, check, env):
        assert check is True
        work = Path(command[command.index("-C") + 1])
        (work / LIBRARY_NAME).write_bytes(b"synthetic firmware binary\n")
        config = work / KERNEL_VERSION / ".config"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text("CONFIG_SYNTHETIC=y\nCONFIG_ACTUAL=y\n")
        return subprocess.CompletedProcess(command, 0)

    run = Mock(side_effect=compile_firmware)
    monkeypatch.setattr(module.subprocess, "run", run)
    target_dir = tmp_path / "target"

    def build(**kwargs):
        return module.build_firmware(TARGET, target_dir=str(target_dir), jobs="2", **kwargs)

    return SimpleNamespace(
        module=module,
        fw=fw,
        kernel=kernel,
        builder=builder,
        target_dir=target_dir,
        tools=tools,
        download=download,
        real_download=real_download,
        network=network,
        run=run,
        compile=compile_firmware,
        build=build,
    )


def test_build_reuses_verified_cache(firmware) -> None:
    output = firmware.build()
    receipt = output.parent / "libkrunfw.SOURCE"
    original_record = receipt.read_bytes()

    assert firmware.build(offline=True) == output
    firmware.run.assert_called_once()
    firmware.download.assert_called_once()
    firmware.network.assert_not_called()
    assert receipt.read_bytes() == original_record
    record = json.loads(original_record)
    assert record["firmware_sha256"] == firmware.module.sha256(output)
    assert record["actual_config_sha256"] == firmware.module.sha256(
        output.parent / KERNEL_VERSION / ".config"
    )
    assert record["inputs"]["config-libkrunfw"] == firmware.module.sha256(
        firmware.fw / "config-libkrunfw"
    )


@pytest.mark.parametrize("name", ["config-libkrunfw", "patches/0001-synthetic.patch"])
def test_source_changes_invalidate_build_cache(firmware, name: str) -> None:
    first = firmware.build()
    source = firmware.fw / name
    source.write_text(source.read_text() + "changed input\n")

    second = firmware.build()

    assert second != first
    assert firmware.run.call_count == 2
    record = json.loads((second.parent / "libkrunfw.SOURCE").read_text())
    assert record["inputs"][name] == firmware.module.sha256(source)
    assert (second.parent / name).read_bytes() == source.read_bytes()
    assert firmware.build() == second
    assert firmware.run.call_count == 2


def test_corrupted_artifact_is_rebuilt(firmware) -> None:
    output = firmware.build()
    original = output.read_bytes()
    output.write_bytes(b"corrupted artifact")
    stale = output.parent / "stale-build-file"
    stale.write_text("must not survive rebuilding")

    assert firmware.build() == output
    assert firmware.run.call_count == 2
    assert output.read_bytes() == original
    assert not stale.exists()
    record = json.loads((output.parent / "libkrunfw.SOURCE").read_text())
    assert record["firmware_sha256"] == firmware.module.sha256(output)


def test_failed_build_retries_with_fresh_tree(firmware) -> None:
    failed_work = []

    def fail_build(command, *, check, env):
        work = Path(command[command.index("-C") + 1])
        failed_work.append(work)
        (work / "partially-patched").write_text("stale state")
        (work / "patches/0001-synthetic.patch").write_text("partially applied patch")
        (work / LIBRARY_NAME).write_bytes(b"incomplete binary")
        raise subprocess.CalledProcessError(2, command)

    firmware.run.side_effect = fail_build
    with pytest.raises(subprocess.CalledProcessError):
        firmware.build()
    assert not (failed_work[0] / "libkrunfw.SOURCE").exists()

    def retry_build(command, *, check, env):
        work = Path(command[command.index("-C") + 1])
        assert work == failed_work[0]
        assert not (work / "partially-patched").exists()
        assert not (work / LIBRARY_NAME).exists()
        assert (work / "patches/0001-synthetic.patch").read_bytes() == (
            firmware.fw / "patches/0001-synthetic.patch"
        ).read_bytes()
        return firmware.compile(command, check=check, env=env)

    firmware.run.side_effect = retry_build
    output = firmware.build()
    assert output.is_file()
    assert (output.parent / "libkrunfw.SOURCE").is_file()
    assert firmware.run.call_count == 2


def test_offline_build_without_kernel_source_never_uses_network(firmware, monkeypatch) -> None:
    monkeypatch.setattr(firmware.module, "download_kernel", firmware.real_download)

    with pytest.raises(RuntimeError, match="Offline firmware build needs verified kernel source"):
        firmware.build(offline=True)

    firmware.network.assert_not_called()
    firmware.run.assert_not_called()
    assert not list((firmware.target_dir / "fw").glob(f"{TARGET}-*"))


def test_download_checksum_mismatch_cleans_up(firmware, tmp_path: Path) -> None:
    cache = tmp_path / "downloads"
    firmware.network.side_effect = lambda *args, **kwargs: io.BytesIO(b"wrong kernel bytes")
    info = firmware.module.metadata()

    with pytest.raises(RuntimeError, match="Kernel source checksum mismatch"):
        firmware.real_download(cache, info, offline=False)

    firmware.network.assert_called_once_with(info["KERNEL_URL"], timeout=60)
    assert not (cache / f"{KERNEL_VERSION}.tar.xz").exists()
    assert not (cache / f"{KERNEL_VERSION}.tar.download").exists()
    assert list(cache.iterdir()) == []


def test_source_record_preserves_verified_build_provenance(firmware) -> None:
    output = firmware.build()
    assert firmware.module.source_record(output) == (output.parent / "libkrunfw.SOURCE").read_text()


@pytest.mark.parametrize("stale_receipt", [False, True])
def test_source_record_reports_override_honestly(firmware, tmp_path: Path, stale_receipt) -> None:
    if stale_receipt:
        output = firmware.build()
        output.write_bytes(b"external replacement firmware")
    else:
        output = tmp_path / "external.so"
        output.write_bytes(b"external firmware")

    record = json.loads(firmware.module.source_record(output))

    assert record["origin"] == "explicit-firmware-input"
    assert record["path"] == str(output)
    assert record["firmware_sha256"] == firmware.module.sha256(output)
    assert "build_id" not in record
    assert "kernel_source_sha256" not in record
    with pytest.raises(RuntimeError, match="external firmware override"):
        firmware.module.source_archive(output, tmp_path / "override-sources.tar.gz")
    assert not (tmp_path / "override-sources.tar.gz").exists()


def test_source_archive_contains_exact_corresponding_sources(firmware, tmp_path: Path) -> None:
    output = firmware.build()
    record = json.loads(firmware.module.source_record(output))
    destination = tmp_path / "export" / "sources.tar.gz"

    firmware.module.source_archive(output, destination)

    expected = {f"fw/{name}": (output.parent / name).read_bytes() for name in record["inputs"]}
    expected.update(
        {
            f"fw/tarballs/{KERNEL_VERSION}.tar.xz": firmware.kernel.read_bytes(),
            "actual.config": (output.parent / KERNEL_VERSION / ".config").read_bytes(),
            "libkrunfw.SOURCE": (output.parent / "libkrunfw.SOURCE").read_bytes(),
            "build/firmware.py": firmware.builder.read_bytes(),
        }
    )
    with tarfile.open(destination, "r:gz") as archive:
        assert set(archive.getnames()) == set(expected)
        assert len(archive.getmembers()) == len(expected)
        for name, contents in expected.items():
            member = archive.getmember(name)
            assert member.isfile()
            source = archive.extractfile(member)
            assert source is not None
            with source:
                assert source.read() == contents


@pytest.mark.parametrize("changed", ["input", "kernel", "actual_config", "builder"])
def test_source_archive_rejects_tampering(firmware, tmp_path: Path, changed: str) -> None:
    output = firmware.build()
    path = {
        "input": output.parent / "config-libkrunfw",
        "kernel": firmware.kernel,
        "actual_config": output.parent / KERNEL_VERSION / ".config",
        "builder": firmware.builder,
    }[changed]
    path.write_bytes(path.read_bytes() + b"tampered\n")
    destination = tmp_path / "tampered-sources.tar.gz"

    with pytest.raises(RuntimeError):
        firmware.module.source_archive(output, destination)

    assert not destination.exists()
