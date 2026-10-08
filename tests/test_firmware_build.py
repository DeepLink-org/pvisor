from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import platform
import runpy
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-musl"
KERNEL_VERSION = "linux-6.12.1"
LIBRARY_NAME = "libkrunfw.so.5.0.0"


@pytest.mark.parametrize("builder", ["linux", "native", "krunvm"])
def test_recursive_kernel_make_inherits_parallel_flags(tmp_path, builder):
    make = shutil.which("gmake") or shutil.which("make")
    if make is None:
        pytest.skip("GNU make is not installed")
    shutil.copy2(ROOT / "fw/Makefile", tmp_path / "Makefile")
    (tmp_path / "config-libkrunfw_x86_64").touch()
    kernel = tmp_path / "kernel"
    kernel.mkdir()
    (kernel / ".config").touch()
    (kernel / "Makefile").write_text(
        'vmlinux:\n\t@test -n "$(findstring jobserver,$(MAKEFLAGS))"\n\t@touch $@\n'
    )
    args = ["OS=Linux"] if builder == "linux" else ["OS=Darwin", f"MACOS_BUILDER={builder}"]
    subprocess.run(
        [
            make,
            "-C",
            str(tmp_path),
            "-j2",
            "-w",
            *args,
            "ARCH=x86_64",
            "KERNEL_SOURCES=kernel",
            "KERNEL_TARBALL=Makefile",
            f"KERNEL_MAKE={make}",
            "kernel/vmlinux",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    assert (kernel / "vmlinux").is_file()


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
    for selector in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        monkeypatch.delenv(selector, raising=False)

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


@pytest.mark.parametrize(
    "selectors",
    [
        ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH"),
        ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
        ("PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
        ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"),
    ],
)
def test_resolver_rejects_conflicting_selectors_before_build(firmware, monkeypatch, selectors):
    for selector in selectors:
        monkeypatch.setenv(selector, "missing/input")
    with pytest.raises(RuntimeError, match="Conflicting firmware selectors") as error:
        firmware.module.resolve_firmware(TARGET)
    assert all(selector in str(error.value) for selector in selectors)
    firmware.run.assert_not_called()
    firmware.tools.assert_not_called()


@pytest.mark.parametrize("selector", ["PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH"])
@pytest.mark.parametrize("form", ["relative", "home", "symlink"])
def test_resolver_normalizes_explicit_library(firmware, tmp_path, monkeypatch, selector, form):
    source = tmp_path / "libkrunfw.so.5.0.0"
    source.write_bytes(b"explicit firmware")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("HOME", str(tmp_path))
    link = tmp_path / "firmware-link"
    link.symlink_to(source)
    value = {"relative": source.name, "home": "~/" + source.name, "symlink": str(link)}[form]
    monkeypatch.setenv(selector, value)
    resolved = firmware.module.resolve_firmware(TARGET)
    assert resolved.path == source.resolve()
    assert resolved.cargo_env == {"PVISOR_KRUNFW_PATH": str(source.resolve())}
    assert json.loads(resolved.source_record)["firmware_sha256"] == firmware.module.sha256(source)
    firmware.tools.assert_not_called()


@pytest.mark.parametrize("canonical", [False, True])
def test_library_directory_prefers_canonical_then_unique_versioned(
    firmware, tmp_path, monkeypatch, canonical
):
    versioned = tmp_path / "libkrunfw.so.5.0.0"
    versioned.write_bytes(b"versioned firmware")
    preferred = versioned
    if canonical:
        preferred = tmp_path / "libkrunfw.so.5"
        preferred.write_bytes(b"canonical firmware")
    monkeypatch.setenv("PVISOR_LIBKRUNFW_PATH", str(tmp_path))
    resolved = firmware.module.resolve_firmware(TARGET)
    assert resolved.path == preferred
    assert resolved.cargo_env == {"PVISOR_KRUNFW_PATH": str(preferred)}
    assert json.loads(resolved.source_record)["firmware_sha256"] == firmware.module.sha256(
        preferred
    )
    firmware.tools.assert_not_called()


@pytest.mark.parametrize("selector", ["PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"])
def test_resolver_rejects_missing_explicit_inputs_without_fallback(
    firmware, tmp_path, monkeypatch, selector
):
    monkeypatch.setenv(selector, str(tmp_path / "missing"))
    with pytest.raises(RuntimeError, match="does not exist"):
        firmware.module.resolve_firmware(TARGET)
    firmware.tools.assert_not_called()


def test_resolver_normalizes_kernel_bundle(firmware, tmp_path, monkeypatch):
    bundle = tmp_path / "bundle"
    bundle.mkdir()
    for name in ("kernel.bin", "kernel.json"):
        (bundle / name).write_bytes(name.encode())
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("PVISOR_KRUNFW_KERNEL_BUNDLE", "bundle")
    resolved = firmware.module.resolve_firmware(TARGET)
    assert resolved.cargo_env == {"PVISOR_KRUNFW_KERNEL_BUNDLE": str(bundle)}
    assert resolved.library_name is None
    record = json.loads(resolved.source_record)
    assert record["files"] == {
        name: firmware.module.sha256(bundle / name) for name in ("kernel.bin", "kernel.json")
    }
    with pytest.raises(RuntimeError, match="supported only on Linux"):
        firmware.module.resolve_firmware("aarch64-apple-darwin")
    firmware.tools.assert_not_called()


def test_resolver_ignores_empty_selectors_and_preserves_build_receipt(firmware, monkeypatch):
    for selector in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        monkeypatch.setenv(selector, "")
    resolved = firmware.module.resolve_firmware(
        TARGET, target_dir=str(firmware.target_dir), jobs="2"
    )
    assert resolved.source_record == (resolved.path.parent / "libkrunfw.SOURCE").read_text()
    assert json.loads(resolved.source_record)["firmware_sha256"] == firmware.module.sha256(
        resolved.path
    )
    firmware.run.assert_called_once()


def test_standalone_cli_does_not_import_wheel_staging(firmware, tmp_path, monkeypatch, capsys):
    monkeypatch.setitem(sys.modules, "firmware", firmware.module)
    monkeypatch.setitem(sys.modules, "stage_wheel_binaries", None)
    receipt = tmp_path / "export" / "SOURCE"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "build-firmware.py",
            "--target-dir",
            str(firmware.target_dir),
            "--jobs",
            "2",
            "--source-output",
            str(receipt),
        ],
    )
    original_path = sys.path[:]
    try:
        runpy.run_path(str(ROOT / "scripts" / "build-firmware.py"), run_name="__main__")
    finally:
        sys.path[:] = original_path
    output = Path(capsys.readouterr().out.strip())
    assert output.is_file()
    assert receipt.read_text() == firmware.module.source_record(output)
    firmware.run.assert_called_once()


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
