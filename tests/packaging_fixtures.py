"""Packaging subjects and file fixtures shared by unit and process regressions."""

import importlib.util
import sys
import zipfile
from pathlib import Path
from unittest import mock

from tests.fixtures import ROOT

APPLICATION_BINARIES = ("pvisor", "pvisor-replay")


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


def _load_packaging_script(name):
    path = ROOT / "scripts" / "packaging" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(f"{name}_test", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    sys.path.insert(0, str(path.parent))
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path.pop(0)
    return module


wheel_stage = _load_packaging_script("stage_wheel_binaries")
wheel_verify = _load_packaging_script("verify_wheel")
with mock.patch.dict(sys.modules, {"stage_wheel_binaries": wheel_stage}):
    build_backend = _load_packaging_script("build_backend")


def _resolved_fixture(tmp_path):
    source = tmp_path / "fixture.so"
    source.write_bytes(b"firmware")
    return wheel_stage.firmware_build.ResolvedFirmware(
        source,
        "libkrunfw.so.5",
        "PVISOR_KRUNFW_PATH",
        wheel_stage.firmware_build.source_record(source),
    )


def _write_version_tree(root: Path, *, pyproject: str, cargo: str, package: str) -> None:
    (root / "pvisor").mkdir()
    (root / "pyproject.toml").write_text(
        f'[project]\nname = "pvisor-sandbox"\nversion = "{pyproject}"\n', encoding="utf-8"
    )
    (root / "Cargo.toml").write_text(
        f'[workspace.package]\nversion = "{cargo}"\n', encoding="utf-8"
    )
    (root / "pvisor" / "__init__.py").write_text(f'__version__ = "{package}"\n', encoding="utf-8")


def _write_wheel(path: Path, version: str, name: str = "pvisor-sandbox") -> None:
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(
            f"pvisor_sandbox-{version}.dist-info/METADATA",
            f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n",
        )
