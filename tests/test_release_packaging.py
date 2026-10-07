from __future__ import annotations

import ast
import importlib.util
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
APPLICATION_BINARIES = ("pvisor", "pvisor-cache", "pvisor-tui", "pvisor-replay")


def _load_script(name: str):
    path = ROOT / "scripts" / "ci" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_version = _load_script("check_release_version")
release_artifacts = _load_script("check_release_artifacts")
nightly_version = _load_script("set_nightly_local_version")


def _load_wheel_stage():
    name = "stage_wheel_binaries_test"
    path = ROOT / "scripts" / "packaging" / "stage_wheel_binaries.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    sys.path.insert(0, str(path.parent))
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path.pop(0)
    return module


wheel_stage = _load_wheel_stage()


def _load_wheel_verify():
    name = "verify_wheel_test"
    path = ROOT / "scripts" / "packaging" / "verify_wheel.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


wheel_verify = _load_wheel_verify()


def _resolved_fixture(tmp_path):
    source = tmp_path / "fixture.so"
    source.write_bytes(b"firmware")
    return wheel_stage.firmware_build.ResolvedFirmware(
        source,
        "libkrunfw.so.5",
        "PVISOR_KRUNFW_PATH",
        wheel_stage.firmware_build.source_record(source),
    )


def test_python_wheel_uses_setuptools_and_platform_builds() -> None:
    contents = (ROOT / "pyproject.toml").read_text(encoding="utf-8")

    assert 'requires = ["setuptools>=77", "pyelftools==0.33"]' in contents
    assert 'build-backend = "build_backend"' in contents
    assert 'build = "cp312-*"' in contents
    assert 'manylinux-x86_64-image = "manylinux_2_28"' in contents
    assert 'archs = ["arm64"]' in contents
    assert "PVISOR_CARGO_ZIGBUILD" not in contents
    assert "cargo-zigbuild" in contents


def test_platform_wheels_use_cibuildwheel() -> None:
    wheel = (ROOT / ".github" / "workflows" / "wheel.yml").read_text(encoding="utf-8")
    assert "pypa/cibuildwheel@v4.1.0" in wheel

    for workflow in ("nightly.yml", "release.yml"):
        contents = (ROOT / ".github" / "workflows" / workflow).read_text(encoding="utf-8")
        assert "./.github/workflows/wheel.yml" in contents


def _write_version_tree(root: Path, *, pyproject: str, cargo: str, package: str) -> None:
    (root / "pvisor").mkdir()
    (root / "pyproject.toml").write_text(
        f'[project]\nname = "pvisor"\nversion = "{pyproject}"\n', encoding="utf-8"
    )
    (root / "Cargo.toml").write_text(
        f'[workspace.package]\nversion = "{cargo}"\n', encoding="utf-8"
    )
    (root / "pvisor" / "__init__.py").write_text(f'__version__ = "{package}"\n', encoding="utf-8")


def _write_wheel(path: Path, version: str, name: str = "pvisor") -> None:
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(
            f"pvisor-{version}.dist-info/METADATA",
            f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n",
        )


def test_release_version_accepts_matching_stable_tag(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    assert release_version.validate_versions("v1.2.3", tmp_path) == "1.2.3"


@pytest.mark.parametrize("tag", ["1.2.3", "v1.2", "v1.2.3rc1", "v01.2.3"])
def test_release_version_rejects_non_stable_tags(tmp_path: Path, tag: str) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError):
        release_version.validate_versions(tag, tmp_path)


def test_release_version_rejects_mismatched_sources(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.4", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError, match="do not match"):
        release_version.validate_versions("v1.2.3", tmp_path)


def test_release_version_rejects_tag_version_mismatch(tmp_path: Path) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    with pytest.raises(release_version.ReleaseValidationError, match="does not match"):
        release_version.validate_versions("v1.2.4", tmp_path)


@pytest.mark.skipif(shutil.which("cargo") is None, reason="Cargo is needed to check lockfiles")
def test_release_lockfile_rejects_stale_workspace_version(tmp_path: Path) -> None:
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[package]\nname = "lockfile-check"\nversion = "1.2.3"\n')
    (tmp_path / "src").mkdir()
    (tmp_path / "src/lib.rs").write_text("")
    subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=tmp_path, check=True)
    release_version.validate_lockfile(tmp_path)

    manifest.write_text(manifest.read_text().replace("1.2.3", "1.2.4"))
    with pytest.raises(release_version.ReleaseValidationError, match="--locked"):
        release_version.validate_lockfile(tmp_path)


def test_nightly_version_updates_python_package(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _write_version_tree(tmp_path, pyproject="1.2.3", cargo="1.2.3", package="1.2.3")
    monkeypatch.setattr(nightly_version, "ROOT", tmp_path)
    monkeypatch.setattr(sys, "argv", ["set_nightly_local_version.py", "g42.abcdef0"])
    nightly_version.main()
    assert set(release_version.read_versions(tmp_path).values()) == {"1.2.3+g42.abcdef0"}


@pytest.mark.parametrize("include_pvisor", [True, False])
@pytest.mark.parametrize("missing_binary", [None, *APPLICATION_BINARIES, "pvisor-daemon"])
def test_nightly_installer_selects_pvisor_wheel(
    tmp_path: Path, include_pvisor: bool, missing_binary: str | None
) -> None:
    """Exercise the installer offline, with both old and new release assets."""
    interpreter = tmp_path / "python"
    interpreter.write_text(
        f"#!{sys.executable}\n"
        "import io, json, sys, sysconfig, urllib.request\n"
        "from pathlib import Path\n"
        f"root = Path({str(tmp_path)!r})\n"
        f"sys.path.insert(0, {str(ROOT)!r})\n"
        "if sys.argv[1:3] == ['-m', 'pip']:\n"
        "    with (root / 'pip.log').open('a') as log:\n"
        "        log.write(json.dumps(sys.argv[3:]) + '\\n')\n"
        "elif sys.argv[1] == '-c':\n"
        "    sysconfig.get_path = lambda name: str(root)\n"
        "    exec(sys.argv[2])\n"
        "elif sys.argv[1] == '-':\n"
        "    assets = [{'name': f'{name}-0.3.0-py3-none-{platform}.whl',\n"
        "               'browser_download_url': f'https://example.test/{name}-{platform}.whl'}\n"
        f"              for name in {('legacy-package', 'pvisor') if include_pvisor else ('legacy-package',)!r}\n"
        "              for platform in ('macosx_11_0_arm64', 'manylinux_2_28_x86_64')]\n"
        "    urllib.request.urlopen = lambda *a, **k: io.BytesIO(json.dumps({'assets': assets}).encode())\n"
        "    sys.argv = sys.argv[1:]\n"
        "    exec(sys.stdin.read())\n",
        encoding="utf-8",
    )
    interpreter.chmod(0o755)
    executable = tmp_path / "pvisor"
    executable.write_text('#!/bin/sh\necho "pvisor 0.3.0"\n', encoding="utf-8")
    executable.chmod(0o755)
    for name in wheel_stage.EXPECTED_BINARIES[1:]:
        (tmp_path / name).symlink_to(executable)
    if missing_binary is not None:
        (tmp_path / missing_binary).unlink()
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/install-nightly.sh")],
        env={**os.environ, "PYTHON": str(interpreter)},
        capture_output=True,
        text=True,
        timeout=30,
    )
    if not include_pvisor:
        assert result.returncode != 0
        assert "no platform wheel" in result.stderr
        assert not (tmp_path / "pip.log").exists()
        return
    if missing_binary is not None and (
        missing_binary != "pvisor-daemon" or sys.platform == "linux"
    ):
        assert result.returncode != 0
        assert f"wheel did not install executable {tmp_path / missing_binary}" in result.stderr
        return
    assert result.returncode == 0, result.stderr
    assert "pvisor-memory-pool" not in result.stdout + result.stderr
    installs = (tmp_path / "pip.log").read_text()
    assert "https://example.test/pvisor-" in installs
    assert "legacy-package" not in installs
    assert "pVisor 0.3.0" in result.stdout


@pytest.mark.parametrize("name", ["legacy-package", "unrelated"])
def test_release_artifacts_reject_wrong_distribution(tmp_path: Path, name: str) -> None:
    for platform in ("manylinux_2_28_x86_64", "macosx_11_0_arm64"):
        wheel = tmp_path / f"pvisor-1.2.3-py3-none-{platform}.whl"
        _write_wheel(wheel, "1.2.3", name=name)
    with pytest.raises(release_artifacts.ArtifactValidationError, match="METADATA Name"):
        release_artifacts.validate_artifacts(tmp_path, "1.2.3")
    with pytest.raises(RuntimeError, match="wheel Name"):
        wheel_verify._wheel_contents(wheel)


@pytest.mark.parametrize("version", ["1.2.3", "1.2.3+g42.abcdef0"])
def test_release_artifacts_accept_supported_matrix(tmp_path: Path, version: str) -> None:
    names = [
        f"pvisor-{version}-py3-none-manylinux_2_28_x86_64.whl",
        f"pvisor-{version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    found = release_artifacts.validate_artifacts(tmp_path, version)
    assert set(found) == {"linux-x86_64", "macos-arm64"}


def test_release_artifacts_reject_missing_platform(tmp_path: Path) -> None:
    version = "1.2.3"
    _write_wheel(
        tmp_path / f"pvisor-{version}-py3-none-macosx_11_0_arm64.whl",
        version,
    )
    with pytest.raises(release_artifacts.ArtifactValidationError, match="expected 2 wheels"):
        release_artifacts.validate_artifacts(tmp_path, version)


def test_release_artifacts_reject_metadata_version_mismatch(tmp_path: Path) -> None:
    filename_version = "1.2.3"
    names = [
        f"pvisor-{filename_version}-py3-none-manylinux_2_28_x86_64.whl",
        f"pvisor-{filename_version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, "1.2.4")

    with pytest.raises(release_artifacts.ArtifactValidationError, match="METADATA version"):
        release_artifacts.validate_artifacts(tmp_path, filename_version)


def test_release_artifacts_reject_oversized_wheel(tmp_path: Path) -> None:
    version = "1.2.3"
    names = [
        f"pvisor-{version}-py3-none-manylinux_2_28_x86_64.whl",
        f"pvisor-{version}-py3-none-macosx_11_0_arm64.whl",
    ]
    for name in names:
        _write_wheel(tmp_path / name, version)

    with pytest.raises(release_artifacts.ArtifactValidationError, match="exceeds"):
        release_artifacts.validate_artifacts(tmp_path, version, max_bytes=1)


@pytest.mark.parametrize(
    ("editable", "bundle_firmware"),
    [(True, False), (False, True)],
)
def test_build_backend_options_only_skip_firmware_for_editable_builds(
    editable: bool,
    bundle_firmware: bool,
) -> None:
    options = wheel_stage.options_from_build_backend(None, editable=editable)

    assert options.bundle_firmware is bundle_firmware


@pytest.mark.parametrize("macos", [False, True])
@pytest.mark.parametrize(
    "selector", [None, "PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"]
)
def test_staging_resolves_once_for_components_and_receipt(monkeypatch, tmp_path, macos, selector):
    import json
    from types import SimpleNamespace
    from unittest.mock import Mock

    if macos and selector == "PVISOR_KRUNFW_KERNEL_BUNDLE":
        pytest.skip("kernel bundles are Linux-only")
    for variable in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        monkeypatch.delenv(variable, raising=False)
    source = tmp_path / ("libkrunfw.5.dylib" if macos else "libkrunfw.so.5.0.0")
    source.write_bytes(b"selected firmware")
    bundle = tmp_path / "bundle"
    bundle.mkdir()
    (bundle / "kernel.bin").write_bytes(b"selected kernel")
    (bundle / "kernel.json").write_text("{}")
    if selector:
        monkeypatch.setenv(selector, str(bundle if selector.endswith("BUNDLE") else source))
    build_firmware = Mock(return_value=source)
    monkeypatch.setattr(wheel_stage.firmware_build, "build_firmware", build_firmware)
    resolver = Mock(wraps=wheel_stage.firmware_build.resolve_firmware)
    monkeypatch.setattr(wheel_stage.firmware_build, "resolve_firmware", resolver)
    monkeypatch.setattr(wheel_stage, "WHEEL_DATA", tmp_path / "wheel-data")
    monkeypatch.setattr(wheel_stage, "_prepare_zig_file_limit", lambda: None)
    monkeypatch.setattr(wheel_stage, "_sign_macos_pvisor", lambda path: None)
    binaries = tmp_path / "binaries"
    binaries.mkdir()
    environments = []

    def popen(command, **kwargs):
        environments.append(kwargs["env"])
        names = ("pvisor-daemon",) if "pvisor-daemon" in command else wheel_stage.NATIVE_BINARIES
        messages = []
        for name in names:
            executable = binaries / name
            executable.write_bytes(b"binary")
            messages.append(
                json.dumps(
                    {
                        "reason": "compiler-artifact",
                        "executable": str(executable),
                        "target": {"name": name, "kind": ["bin"]},
                    }
                )
            )
        # Later components and staging must not re-read a changed selector.
        monkeypatch.setenv("PVISOR_LIBKRUNFW_PATH", str(tmp_path / "wrong-input"))
        return SimpleNamespace(stdout=messages, wait=lambda: 0)

    monkeypatch.setattr(wheel_stage.subprocess, "Popen", popen)
    options = wheel_stage.BuildOptions(
        target="aarch64-apple-darwin" if macos else "x86_64-unknown-linux-musl"
    )
    staged = wheel_stage.stage_wheel_binaries(options)
    resolver.assert_called_once()
    assert build_firmware.call_count == (0 if selector else 1)
    assert len(environments) == (1 if macos else 2)
    cargo_variable = (
        "PVISOR_KRUNFW_KERNEL_BUNDLE"
        if selector == "PVISOR_KRUNFW_KERNEL_BUNDLE"
        else "PVISOR_KRUNFW_PATH"
    )
    selected = bundle if cargo_variable.endswith("BUNDLE") else source
    for env in environments:
        assert env[cargo_variable] == str(selected)
        assert "PVISOR_LIBKRUNFW_PATH" not in env
        assert ("PVISOR_KRUNFW_PATH" in env) != ("PVISOR_KRUNFW_KERNEL_BUNDLE" in env)
    record = json.loads((staged / "libkrunfw.SOURCE").read_text())
    assert record["path"] == str(selected)
    if cargo_variable.endswith("BUNDLE"):
        assert record["files"]["kernel.bin"] == wheel_stage.firmware_build.sha256(
            bundle / "kernel.bin"
        )
    else:
        assert record["firmware_sha256"] == wheel_stage.firmware_build.sha256(source)
    if macos:
        assert (staged / source.name).read_bytes() == source.read_bytes()
        assert (binaries / source.name).read_bytes() == source.read_bytes()
        assert (binaries / "libkrunfw.SOURCE").read_bytes() == (
            staged / "libkrunfw.SOURCE"
        ).read_bytes()


def test_build_backend_options_accept_explicit_cargo_settings() -> None:
    options = wheel_stage.options_from_build_backend(
        {
            "cargo-profile": "dev",
            "cargo-locked": "false",
            "cargo-jobs": "3",
            "bundle-firmware": "false",
        },
        editable=False,
    )

    assert options.profile == "dev"
    assert options.locked is False
    assert options.jobs == "3"
    assert options.bundle_firmware is False


def test_editable_staging_does_not_bundle_firmware(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    artifacts = {}
    for name in (*wheel_stage.EXPECTED_BINARIES, "pvisor-memory-pool"):
        artifact = tmp_path / "artifacts" / name
        artifact.parent.mkdir(exist_ok=True)
        artifact.write_text(name, encoding="utf-8")
        artifacts[name] = artifact

    wheel_data = tmp_path / "wheel-data"
    stale_scripts = wheel_data / "scripts"
    stale_scripts.mkdir(parents=True)
    (stale_scripts / "pvisor-memory-pool").write_bytes(b"stale binary")
    monkeypatch.setattr(wheel_stage, "WHEEL_DATA", wheel_data)
    monkeypatch.setattr(wheel_stage, "_build", lambda _options, **kwargs: artifacts)
    monkeypatch.setattr(wheel_stage, "_is_macos", lambda _options: False)

    monkeypatch.setattr(
        wheel_stage, "_resolve_firmware", lambda options: _resolved_fixture(tmp_path)
    )

    scripts = wheel_stage.stage_wheel_binaries(wheel_stage.BuildOptions(bundle_firmware=False))

    assert {path.name for path in scripts.iterdir()} == set(wheel_stage.EXPECTED_BINARIES)


def test_release_staging_resolves_firmware_before_build(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[str] = []

    def missing_firmware(_options):
        events.append("firmware")
        raise RuntimeError("missing firmware")

    def unexpected_build(_options):
        events.append("build")
        raise AssertionError("Cargo must not run before firmware is ready")

    monkeypatch.setattr(wheel_stage, "_is_macos", lambda _options: True)
    monkeypatch.setattr(wheel_stage, "_resolve_firmware", missing_firmware)
    monkeypatch.setattr(wheel_stage, "_build", unexpected_build)

    with pytest.raises(RuntimeError, match="missing firmware"):
        wheel_stage.stage_wheel_binaries(wheel_stage.BuildOptions())

    assert events == ["firmware"]


def test_firmware_source_prefers_explicit_path(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    firmware = tmp_path / "libkrunfw.5.dylib"
    firmware.write_bytes(b"firmware")
    monkeypatch.setenv("PVISOR_LIBKRUNFW_PATH", str(tmp_path))

    resolved = wheel_stage._resolve_firmware(
        wheel_stage.BuildOptions(target="aarch64-apple-darwin")
    )

    assert resolved.path == firmware.resolve()
    assert resolved.library_name == firmware.name


def test_firmware_source_builds_in_tree_when_path_is_not_configured(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    firmware = tmp_path / "libkrunfw.5.dylib"
    firmware.write_bytes(b"firmware")
    monkeypatch.delenv("PVISOR_LIBKRUNFW_PATH", raising=False)
    monkeypatch.setattr(
        wheel_stage.firmware_build, "build_firmware", lambda *args, **kwargs: firmware
    )

    resolved = wheel_stage._resolve_firmware(
        wheel_stage.BuildOptions(target="aarch64-apple-darwin")
    )

    assert resolved.path == firmware
    assert resolved.library_name == firmware.name


def test_daemon_cargo_command_selects_native_static_package(monkeypatch):
    monkeypatch.setattr(wheel_stage.sys, "platform", "linux")
    monkeypatch.setattr(wheel_stage.platform, "machine", lambda: "x86_64")
    command = wheel_stage._cargo_command(wheel_stage.BuildOptions(), daemon=True)
    assert command[command.index("-p") + 1] == "pvisor-daemon"
    assert command[command.index("--bin") + 1] == "pvisor-daemon"
    assert command.count("-p") == 1
    assert "--no-default-features" in command
    assert "--features" not in command
    assert "--bins" not in command
    assert "pvisor" not in command
    assert "nativepvisor" not in command
    assert command[:2] == ["cargo", "zigbuild"]
    assert command[command.index("--target") + 1] == "x86_64-unknown-linux-musl"


@pytest.mark.parametrize("target", [None, "aarch64-apple-darwin"])
def test_daemon_rejects_macos_builds(monkeypatch, target):
    monkeypatch.setattr(wheel_stage.sys, "platform", "darwin")
    with pytest.raises(RuntimeError, match="supported only on Linux x86_64"):
        wheel_stage._cargo_command(wheel_stage.BuildOptions(target=target), daemon=True)


def test_wheel_build_separates_daemon_from_native_components(monkeypatch, tmp_path):
    monkeypatch.setattr(wheel_stage, "_is_macos", lambda options: False)
    calls = []
    monkeypatch.setattr(
        wheel_stage, "_resolve_firmware", lambda options: _resolved_fixture(tmp_path)
    )

    def build_component(options, *, shim_vm=False, daemon=False, firmware=None):
        calls.append((shim_vm, daemon))
        names = ("pvisor-daemon",) if daemon else wheel_stage.NATIVE_BINARIES
        return {name: tmp_path / name for name in names}

    monkeypatch.setattr(wheel_stage, "_build_component", build_component)
    artifacts = wheel_stage._build(wheel_stage.BuildOptions())
    assert calls == [(False, False), (False, True)]
    assert set(artifacts) == set(wheel_stage.EXPECTED_BINARIES)
    assert wheel_stage.EXPECTED_BINARIES == wheel_verify.EXPECTED_BINARIES
    assert "pvisor-daemon" not in wheel_verify.COMPANION_BINARIES
    assert "pvisor-cluster" not in artifacts
    assert "pvisor-worker" not in artifacts


def test_macos_wheel_signs_native_components_and_excludes_daemon(monkeypatch, tmp_path):
    artifacts = {}
    for name in wheel_stage.NATIVE_BINARIES:
        artifact = tmp_path / name
        artifact.write_bytes(b"binary")
        artifacts[name] = artifact
    signed = []
    monkeypatch.setattr(wheel_stage, "WHEEL_DATA", tmp_path / "wheel-data")
    monkeypatch.setattr(wheel_stage, "_build", lambda options, **kwargs: artifacts)
    monkeypatch.setattr(
        wheel_stage, "_resolve_firmware", lambda options: _resolved_fixture(tmp_path)
    )
    monkeypatch.setattr(wheel_stage, "_is_macos", lambda options: True)
    monkeypatch.setattr(wheel_stage, "_sign_macos_pvisor", lambda path: signed.append(path.name))
    scripts = wheel_stage.stage_wheel_binaries(wheel_stage.BuildOptions(bundle_firmware=False))
    assert signed == list(wheel_stage.NATIVE_BINARIES)
    assert "pvisor-daemon" not in signed
    assert {path.name for path in scripts.iterdir()} == set(wheel_stage.NATIVE_BINARIES)


def test_cargo_command_selects_static_musl_on_linux(monkeypatch):
    monkeypatch.setattr(wheel_stage.sys, "platform", "linux")
    monkeypatch.setattr(wheel_stage.platform, "machine", lambda: "x86_64")
    command = wheel_stage._cargo_command(wheel_stage.BuildOptions())
    assert [command[index + 1] for index, arg in enumerate(command) if arg == "-p"] == [
        "pvisor-cli"
    ]
    assert [command[index + 1] for index, arg in enumerate(command) if arg == "--bin"] == list(
        wheel_stage.NATIVE_BINARIES
    )
    assert "pvisor-cli/gateway" in command
    assert "pvisor/gateway" not in command
    assert "--bins" not in command
    assert "pvisor-memory-pool" not in command
    assert "pvisor-cluster" not in command
    assert "pvisor-worker" not in command
    assert "pvisor-daemon" not in command
    assert command[:2] == ["cargo", "zigbuild"]
    assert command[command.index("--target") + 1] == "x86_64-unknown-linux-musl"
    with pytest.raises(RuntimeError, match="unsupported wheel target"):
        wheel_stage._normalize_target("x86_64-unknown-linux-gnu")


@pytest.mark.parametrize(
    "soft,hard,expected_soft",
    [(1024, 32_768, 16_384), (1024, 4096, 4096), (32_768, 32_768, 32_768)],
)
def test_zig_linker_inherits_file_limit_without_changing_callers_limit(soft, hard, expected_soft):
    resource = pytest.importorskip("resource")
    caller_limits = resource.getrlimit(resource.RLIMIT_NOFILE)
    if caller_limits[1] != resource.RLIM_INFINITY and caller_limits[1] < hard:
        pytest.skip("test requires a sufficient open-file hard limit")
    # Lower limits only in a disposable build process, then observe what its
    # linker child inherits. The calling shell/pytest limit must stay intact.
    code = (
        "import json, resource, subprocess, sys\n"
        f"sys.path.insert(0, {str(ROOT / 'scripts/packaging')!r})\n"
        "from stage_wheel_binaries import _prepare_zig_file_limit\n"
        f"resource.setrlimit(resource.RLIMIT_NOFILE, ({soft}, {hard}))\n"
        "_prepare_zig_file_limit()\n"
        "child = subprocess.check_output([sys.executable, '-c', "
        "'import json, resource; print(json.dumps(resource.getrlimit(resource.RLIMIT_NOFILE)))'])\n"
        "print(child.decode().strip())\n"
    )
    result = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, check=True, timeout=30
    )

    import json

    assert json.loads(result.stdout) == [expected_soft, hard]
    assert resource.getrlimit(resource.RLIMIT_NOFILE) == caller_limits


@pytest.mark.parametrize(
    "headers,dynamic",
    [
        ("INTERP", ""),
        ("LOAD", "(NEEDED) Shared library: [libc.so.6]"),
    ],
)
def test_static_linux_rejects_dynamic_dependencies(monkeypatch, headers, dynamic):
    monkeypatch.setattr(
        wheel_verify, "_run", lambda command: headers if "-l" in command else dynamic
    )
    with pytest.raises(RuntimeError, match="fully static"):
        wheel_verify._assert_static_linux("pvisor", Path("pvisor"))


def test_static_linux_accepts_static_pie(monkeypatch):
    monkeypatch.setattr(
        wheel_verify, "_run", lambda command: "LOAD DYNAMIC" if "-l" in command else "(RELACOUNT)"
    )
    wheel_verify._assert_static_linux("pvisor", Path("pvisor"))


def test_linux_wheel_embeds_firmware(monkeypatch, tmp_path):
    monkeypatch.setattr(
        wheel_stage, "_resolve_firmware", lambda options: _resolved_fixture(tmp_path)
    )
    artifact = tmp_path / "pvisor"
    artifact.write_bytes(b"static pvisor with embedded kernel")
    monkeypatch.setattr(wheel_stage, "WHEEL_DATA", tmp_path / "wheel-data")
    monkeypatch.setattr(
        wheel_stage,
        "_build",
        lambda options, **kwargs: dict.fromkeys(wheel_stage.EXPECTED_BINARIES, artifact),
    )
    scripts = wheel_stage.stage_wheel_binaries(
        wheel_stage.BuildOptions(target="x86_64-unknown-linux-musl")
    )
    assert {p.name for p in scripts.iterdir()} == set(wheel_stage.EXPECTED_BINARIES) | {
        "libkrunfw.SOURCE"
    }


def test_shim_vm_build_uses_static_musl():
    command = wheel_stage._cargo_command(
        wheel_stage.BuildOptions(target="x86_64-unknown-linux-musl"), shim_vm=True
    )
    assert command[:2] == ["cargo", "zigbuild"]
    assert command[command.index("-p") + 1] == "pvisor-shim"
    assert command[command.index("--features") + 1] == "vm"


def test_linux_wheel_requires_no_firmware_shared_library(tmp_path):
    wheel = tmp_path / "pvisor-1.2.3-py3-none-manylinux_2_28_x86_64.whl"
    _write_wheel(wheel, "1.2.3")
    with zipfile.ZipFile(wheel, "a") as archive:
        for name in wheel_stage.EXPECTED_BINARIES:
            binary = zipfile.ZipInfo(f"pvisor-1.2.3.data/scripts/{name}")
            binary.external_attr = 0o100755 << 16
            archive.writestr(binary, b"static ELF")
    version, scripts, firmware = wheel_verify._wheel_contents(wheel)
    assert (
        version == "1.2.3"
        and set(scripts) == set(wheel_stage.EXPECTED_BINARIES)
        and firmware is None
    )
    with zipfile.ZipFile(wheel, "a") as archive:
        archive.writestr("pvisor-1.2.3.data/scripts/libkrunfw.so.5", b"shared library")
    with pytest.raises(RuntimeError, match="expected 0 libkrunfw"):
        wheel_verify._wheel_contents(wheel)


def test_native_macos_build_keeps_host_target(monkeypatch):
    monkeypatch.setattr(wheel_stage.sys, "platform", "darwin")
    monkeypatch.setattr(wheel_stage.platform, "machine", lambda: "x86_64")
    command = wheel_stage._cargo_command(wheel_stage.BuildOptions())
    assert command[:2] == ["cargo", "build"] and "--target" not in command


def test_native_macos_explicit_firmware_does_not_require_supported_builder_host(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(wheel_stage.sys, "platform", "darwin")
    monkeypatch.setattr(wheel_stage.platform, "machine", lambda: "x86_64")
    for selector in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        monkeypatch.delenv(selector, raising=False)
    source = tmp_path / "libkrunfw.5.dylib"
    source.write_bytes(b"external firmware")
    monkeypatch.setenv("PVISOR_KRUNFW_PATH", str(source))
    resolved = wheel_stage._resolve_firmware(wheel_stage.BuildOptions())
    assert resolved.path == source
    assert resolved.library_name == source.name


def test_macos_build_never_invokes_daemon_pipeline(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(
        wheel_stage, "_resolve_firmware", lambda options: _resolved_fixture(tmp_path)
    )
    monkeypatch.setattr(wheel_stage, "_is_macos", lambda options: True)

    def build_component(options, *, shim_vm=False, daemon=False, firmware=None):
        calls.append((shim_vm, daemon))
        return {name: tmp_path / name for name in wheel_stage.NATIVE_BINARIES}

    monkeypatch.setattr(wheel_stage, "_build_component", build_component)
    artifacts = wheel_stage._build(wheel_stage.BuildOptions())
    assert calls == [(False, False)]
    assert set(artifacts) == set(wheel_stage.NATIVE_BINARIES)


@pytest.mark.parametrize("component", ["daemon", "native", "shim"])
@pytest.mark.parametrize("configured", [None, "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"])
def test_all_native_builds_prepare_firmware_before_cargo(
    monkeypatch, tmp_path, component, configured
):
    import json
    from types import SimpleNamespace

    for variable in ("PVISOR_LIBKRUNFW_PATH", "PVISOR_KRUNFW_PATH", "PVISOR_KRUNFW_KERNEL_BUNDLE"):
        monkeypatch.delenv(variable, raising=False)
    source = tmp_path / "libkrunfw.so.5"
    source.write_bytes(b"firmware")
    bundle = tmp_path / "bundle"
    bundle.mkdir()
    (bundle / "kernel.bin").write_bytes(b"kernel")
    (bundle / "kernel.json").write_text("{}")
    if configured:
        monkeypatch.setenv(configured, str(bundle if configured.endswith("BUNDLE") else source))
    names = (
        ("pvisor-daemon",)
        if component == "daemon"
        else ("containerd-shim-pvisor-v2",)
        if component == "shim"
        else wheel_stage.NATIVE_BINARIES
    )
    events = []
    monkeypatch.setattr(wheel_stage, "_prepare_zig_file_limit", lambda: events.append("limit"))

    real_resolve = wheel_stage._resolve_firmware

    def firmware(options):
        events.append("firmware")
        return real_resolve(options)

    monkeypatch.setattr(
        wheel_stage.firmware_build, "build_firmware", lambda *args, **kwargs: source
    )

    def popen(command, **kwargs):
        events.append("cargo")
        assert command[:2] == ["cargo", "zigbuild"]
        env = kwargs["env"]
        if configured:
            assert env[configured] == str(bundle if configured.endswith("BUNDLE") else source)
        else:
            assert env["PVISOR_KRUNFW_PATH"] == str(tmp_path / "libkrunfw.so.5")
        return SimpleNamespace(
            stdout=[
                json.dumps(
                    {
                        "reason": "compiler-artifact",
                        "executable": str(tmp_path / name),
                        "target": {"name": name, "kind": ["bin"]},
                    }
                )
                for name in names
            ],
            wait=lambda: 0,
        )

    monkeypatch.setattr(wheel_stage, "_resolve_firmware", firmware)
    monkeypatch.setattr(wheel_stage.subprocess, "Popen", popen)
    artifacts = wheel_stage._build_component(
        wheel_stage.BuildOptions(target="x86_64-unknown-linux-musl"),
        daemon=component == "daemon",
        shim_vm=component == "shim",
    )
    assert set(artifacts) == set(names)
    assert events == ["firmware", "limit", "cargo"]


def test_macos_build_prepares_and_bundles_firmware_before_cargo(monkeypatch, tmp_path):
    import json
    from types import SimpleNamespace

    source = tmp_path / "firmware" / "libkrunfw.5.dylib"
    source.parent.mkdir()
    source.write_bytes(b"custom firmware")
    binaries = tmp_path / "binaries"
    binaries.mkdir()
    events = []

    def firmware(options):
        events.append("firmware")
        return wheel_stage.firmware_build.ResolvedFirmware(
            source,
            source.name,
            "PVISOR_KRUNFW_PATH",
            wheel_stage.firmware_build.source_record(source),
        )

    def popen(command, **kwargs):
        events.append("cargo")
        assert command[:2] == ["cargo", "build"]
        return SimpleNamespace(
            stdout=[
                json.dumps(
                    {
                        "reason": "compiler-artifact",
                        "executable": str(binaries / name),
                        "target": {"name": name, "kind": ["bin"]},
                    }
                )
                for name in wheel_stage.NATIVE_BINARIES
            ],
            wait=lambda: 0,
        )

    monkeypatch.setattr(wheel_stage, "_resolve_firmware", firmware)
    monkeypatch.setattr(wheel_stage.subprocess, "Popen", popen)
    wheel_stage._build_component(wheel_stage.BuildOptions(target="aarch64-apple-darwin"))
    assert events == ["firmware", "cargo"]
    assert (binaries / source.name).read_bytes() == source.read_bytes()
    record = json.loads((binaries / "libkrunfw.SOURCE").read_text())
    assert record["origin"] == "explicit-firmware-input"
    assert record["firmware_sha256"] == wheel_stage.firmware_build.sha256(source)


def test_daemon_firmware_failure_prevents_cargo(monkeypatch):
    monkeypatch.delenv("PVISOR_KRUNFW_PATH", raising=False)
    monkeypatch.delenv("PVISOR_KRUNFW_KERNEL_BUNDLE", raising=False)
    monkeypatch.setattr(wheel_stage, "_prepare_zig_file_limit", lambda: None)

    def missing_firmware(options):
        raise RuntimeError("missing firmware")

    def unexpected_cargo(*args, **kwargs):
        raise AssertionError("Cargo must not run without prepared firmware")

    monkeypatch.setattr(wheel_stage, "_resolve_firmware", missing_firmware)
    monkeypatch.setattr(wheel_stage.subprocess, "Popen", unexpected_cargo)
    with pytest.raises(RuntimeError, match="missing firmware"):
        wheel_stage._build_component(
            wheel_stage.BuildOptions(target="x86_64-unknown-linux-musl"),
            daemon=True,
        )


def test_macos_wheel_requires_native_set_and_rejects_daemon(tmp_path):
    wheel = tmp_path / "pvisor-1.2.3-py3-none-macosx_11_0_arm64.whl"
    _write_wheel(wheel, "1.2.3")
    with zipfile.ZipFile(wheel, "a") as archive:
        for name in wheel_stage.NATIVE_BINARIES:
            binary = zipfile.ZipInfo(f"pvisor-1.2.3.data/scripts/{name}")
            binary.external_attr = 0o100755 << 16
            archive.writestr(binary, b"Mach-O")
        archive.writestr("pvisor-1.2.3.data/scripts/libkrunfw.5.dylib", b"firmware")
    _, scripts, firmware = wheel_verify._wheel_contents(wheel)
    assert set(scripts) == set(wheel_stage.NATIVE_BINARIES)
    assert firmware is not None
    with zipfile.ZipFile(wheel, "a") as archive:
        archive.writestr("pvisor-1.2.3.data/scripts/pvisor-daemon", b"unsupported")
    with pytest.raises(RuntimeError, match="supported only in Linux x86_64"):
        wheel_verify._wheel_contents(wheel)


def test_daemon_ci_uses_native_linux_pipeline():
    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    daemon_job = ci.split("\n  daemon:\n", 1)[1].split("\n  rust-test:", 1)[0]
    assert "runs-on: ubuntu-latest" in daemon_job
    assert "macos" not in daemon_job
    assert 'static-musl: "true"' in daemon_job
    assert "just daemon-build" in daemon_job
    assert "no native executor or firmware" not in ci


def test_nightly_release_describes_native_daemon_prerequisites():
    contents = (ROOT / ".github/workflows/nightly.yml").read_text()
    assert "daemon included in the Linux x86_64 wheel" in contents
    assert "native `pvisor-vm` execution" in contents
    assert "KVM access and delegated cgroup v2" in contents
    assert "trusted prepared-image manifest" in contents
    assert "bootstrap is not supplied or end-to-end validated" in contents
    assert "only the Linux x86_64 wheel" in contents
    assert "Python package is a version marker, not a launcher" in contents
    assert "rootless Podman" not in contents
    assert "no wheel or native executor is required" not in contents


def test_nightly_installer_uses_platform_specific_component_set():
    contents = (ROOT / "scripts/install-nightly.sh").read_text()
    assert "== Linux-x86_64 ]]; then binaries+=(pvisor-daemon)" in contents
    assert 'for binary in "${binaries[@]}"' in contents
    assert "Linux-aarch64)" not in contents
    assert "binaries=(pvisor pvisor-cache pvisor-tui pvisor-replay)" in contents
    assert "pvisor-memory-pool" not in contents


@pytest.mark.parametrize("workflow", ["nightly.yml", "release.yml"])
def test_releases_publish_wheels_and_sources_without_standalone_daemon(workflow):
    assert not (ROOT / ".github/workflows/daemon-dist.yml").exists()
    contents = (ROOT / ".github/workflows" / workflow).read_text()
    for retired in (
        "daemon-dist",
        "nightly-daemon",
        "release-daemon",
        "daemon-sources",
        "pvisor-daemon-",
        "firmware-source-daemon",
        "Download standalone daemon",
        "\n  daemon:\n",
        "Verify daemon archive checksum",
    ):
        assert retired not in contents
    assert "dist/*.whl" in contents
    assert "dist/pvisor-firmware-source-*.tar.gz" in contents
    assert "test -s dist/pvisor-firmware-source-linux-x86_64.tar.gz" in contents
    assert "test -s dist/pvisor-firmware-source-macos-arm64.tar.gz" in contents
    if workflow == "nightly.yml":
        assert "needs: [meta, wheels]" in contents
    else:
        assert "needs: [validate, verify]" in contents
        assert "needs: [validate, verify, github-release]" in contents


@pytest.mark.parametrize("platform", ["manylinux_2_28_aarch64", "win_amd64", "macosx_11_0_x86_64"])
def test_wheel_verifier_rejects_unsupported_platforms(platform):
    with pytest.raises(RuntimeError, match="unsupported wheel platform"):
        wheel_verify.expected_binaries(Path(f"pvisor-1.2.3-py3-none-{platform}.whl"))


@pytest.mark.parametrize(
    "platform,target,expected",
    [
        (
            "manylinux_2_28_x86_64",
            "x86_64-unknown-linux-musl",
            (*APPLICATION_BINARIES, "pvisor-daemon"),
        ),
        ("macosx_11_0_arm64", "aarch64-apple-darwin", APPLICATION_BINARIES),
    ],
)
def test_native_binary_contract_excludes_retired_pool(platform, target, expected):
    options = wheel_stage.BuildOptions(target=target)
    assert wheel_stage.NATIVE_BINARIES == APPLICATION_BINARIES
    assert wheel_stage.expected_binaries(options) == expected
    wheel = Path(f"pvisor-1.2.3-py3-none-{platform}.whl")
    assert wheel_verify.expected_binaries(wheel) == expected
    command = wheel_stage._cargo_command(options)
    assert (
        tuple(command[i + 1] for i, arg in enumerate(command) if arg == "--bin")
        == APPLICATION_BINARIES
    )
    assert "pvisor-memory-pool" not in command


@pytest.mark.parametrize("platform", ["manylinux_2_28_x86_64", "macosx_11_0_arm64"])
def test_wheel_rejects_retired_pool_even_with_all_maintained_binaries(tmp_path, platform):
    wheel = tmp_path / f"pvisor-1.2.3-py3-none-{platform}.whl"
    _write_wheel(wheel, "1.2.3")
    with zipfile.ZipFile(wheel, "a") as archive:
        for name in (*wheel_verify.expected_binaries(wheel), "pvisor-memory-pool"):
            binary = zipfile.ZipInfo(f"pvisor-1.2.3.data/scripts/{name}")
            binary.external_attr = 0o100755 << 16
            archive.writestr(binary, b"native binary")
        if "macosx" in platform:
            archive.writestr("pvisor-1.2.3.data/scripts/libkrunfw.5.dylib", b"firmware")
    with pytest.raises(RuntimeError, match="retired executable pvisor-memory-pool"):
        wheel_verify._wheel_contents(wheel)


@pytest.mark.parametrize("platform", ["manylinux_2_28_x86_64", "macosx_11_0_arm64"])
def test_install_smoke_uses_standalone_cache_and_daemon(monkeypatch, platform):
    commands = []
    wheel = Path(f"pvisor-1.2.3-py3-none-{platform}.whl")
    expected = wheel_verify.expected_binaries(wheel)

    def create_environment(self, environment):
        scripts = wheel_verify._installed_script_dir(environment)
        scripts.mkdir(parents=True)
        for name in expected:
            executable = scripts / name
            executable.write_bytes(b"binary")
            executable.chmod(0o755)

    def run(command, **kwargs):
        commands.append([Path(command[0]).name, *command[1:]])
        return "pvisor 1.2.3"

    monkeypatch.setattr(wheel_verify.venv.EnvBuilder, "create", create_environment)
    monkeypatch.setattr(wheel_verify, "_run", run)
    wheel_verify.install_smoke(wheel, "1.2.3")
    for name in expected:
        assert [name, "--version"] in commands
        assert [name, "--help"] in commands
    for command in ("prepare", "publish", "read"):
        assert ["pvisor-cache", command, "--help"] in commands
    for command in ("run", "tui", "replay"):
        assert ["pvisor", command, "--help"] in commands
    if "pvisor-daemon" in expected:
        assert ["pvisor-daemon", "serve", "--help"] in commands
        assert ["pvisor-daemon", "protocol"] in commands
    else:
        assert not any(command[0] == "pvisor-daemon" for command in commands)
    assert not any("service" in command or "pvisor-memory-pool" in command for command in commands)


def test_setuptools_excludes_stale_retired_and_unrelated_wheel_scripts(tmp_path, monkeypatch):
    # Execute only the selector and its constants, without invoking setuptools.setup.
    tree = ast.parse((ROOT / "setup.py").read_text())
    selector = ast.Module(
        body=[
            node
            for node in tree.body
            if isinstance(node, ast.Assign)
            or isinstance(node, ast.FunctionDef)
            and node.name == "wheel_scripts"
        ],
        type_ignores=[],
    )
    namespace = {"Path": Path, "os": os, "__file__": str(tmp_path / "setup.py")}
    exec(compile(selector, "setup.py", "exec"), namespace)
    scripts = namespace["WHEEL_SCRIPTS"]
    scripts.mkdir(parents=True)
    payloads = {*APPLICATION_BINARIES, "pvisor-daemon", "libkrunfw.5.dylib", "libkrunfw.SOURCE"}
    for name in payloads | {"pvisor-memory-pool", "unrelated-binary"}:
        (scripts / name).write_bytes(b"payload")
    monkeypatch.delenv("PVISOR_SETUP_SKIP_NATIVE_SCRIPTS", raising=False)
    assert {Path(path).name for path in namespace["wheel_scripts"]()} == payloads
    assert (scripts / "pvisor-memory-pool").is_file()


def test_daemon_helper_uses_shared_component_builder_without_cli_dependency():
    contents = (ROOT / "scripts/packaging/build_daemon.py").read_text()
    assert 'target="x86_64-unknown-linux-musl"' in contents
    assert '_build_component(options, daemon=True)["pvisor-daemon"]' in contents
    assert "cargo build" not in contents and "scripts/build-pvisor.py" not in contents
    justfile = (ROOT / "justfile").read_text()
    assert 'daemon-install: (daemon-build "release")' in justfile
    assert "service-build:" not in justfile
    assert "scripts/packaging/build_daemon.py --profile" in justfile
    assert "independently built daemon" not in justfile
