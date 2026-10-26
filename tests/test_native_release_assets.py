from __future__ import annotations

import importlib.util
import sys
import tarfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from tests.fixtures import workspace

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts/ci"
with mock.patch.object(sys, "path", [str(SCRIPTS), *sys.path]):
    spec = importlib.util.spec_from_file_location(
        "native_release", SCRIPTS / "build_release_assets.py"
    )
    assets = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(assets)


def wheel(root, platform, *, missing=None, version="0.3.0"):
    tag = "macosx_11_0_arm64" if platform == "macos-arm64" else "manylinux_2_28_x86_64"
    path = root / f"pvisor_sandbox-{version}-py3-none-{tag}.whl"
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(
            f"pvisor_sandbox-{version}.dist-info/METADATA",
            f"Name: pvisor-sandbox\nVersion: {version}\n",
        )
        names = [*assets.BINARIES, "libkrunfw.SOURCE"]
        for name in names:
            if name != missing:
                archive.writestr(f"pvisor_sandbox-{version}.data/scripts/{name}", f"payload {name}")
        archive.writestr("../../unexpected", "must not be extracted")
    return path


class NativeReleaseTests(unittest.TestCase):
    def test_archives_preserve_payload_and_are_reproducible(self):
        with workspace() as (root, _):
            for platform in ("linux-x86_64", "macos-arm64"):
                with self.subTest(platform=platform):
                    source = wheel(root, platform)
                    first, second = root / "first.tar.gz", root / "second.tar.gz"
                    assets.native_archive(source, first)
                    assets.native_archive(source, second)
                    self.assertEqual(first.read_bytes(), second.read_bytes())
                    with tarfile.open(first) as archive:
                        self.assertEqual(
                            archive.extractfile("bin/pvisor").read(), b"payload pvisor"
                        )
                        self.assertEqual(archive.getmember("bin/pvisor").mode, 0o755)
                        self.assertIn("LICENSE", archive.getnames())
                        self.assertIn("NOTICE", archive.getnames())
                        self.assertNotIn("../../unexpected", archive.getnames())
                        if platform == "macos-arm64":
                            self.assertNotIn("bin/libkrunfw.5.dylib", archive.getnames())
                        self.assertEqual(
                            archive.extractfile("bin/libkrunfw.SOURCE").read(),
                            b"payload libkrunfw.SOURCE",
                        )

    def test_missing_companion_or_firmware_fails(self):
        with workspace() as (root, _):
            for missing in (*assets.BINARIES, "libkrunfw.SOURCE"):
                with self.subTest(missing=missing):
                    source = wheel(root, "macos-arm64", missing=missing)
                    with self.assertRaisesRegex(ValueError, "expected exactly one native payload"):
                        assets.native_archive(source, root / "out.tar.gz")

    def test_release_checksums_cover_all_channels_and_sources(self):
        with workspace() as (root, _):
            for platform in ("linux-x86_64", "macos-arm64"):
                wheel(root, platform)
                (root / f"pvisor-firmware-source-{platform}.tar.gz").write_bytes(b"source")
            assets.build_assets(root, "0.3.0", "owner/repo", stable=True)
            manifest = (root / "SHA256SUMS").read_text().splitlines()
            self.assertEqual(len(manifest), 7)
            for line in manifest:
                checksum, name = line.split("  ")
                self.assertEqual(checksum, assets.digest(root / name))
            formula = (root / "pvisor.rb").read_text()
            for platform in ("linux-x86_64", "macos-arm64"):
                archive = root / f"pvisor-0.3.0-{platform}.tar.gz"
                self.assertIn(assets.digest(archive), formula)
                self.assertIn(
                    f"https://github.com/owner/repo/releases/download/v0.3.0/{archive.name}",
                    formula,
                )
            self.assertIn('bin.install_symlink libexec/"pvisor"', formula)

    def test_missing_sources_prevents_release_generation(self):
        with workspace() as (root, _):
            for platform in ("linux-x86_64", "macos-arm64"):
                wheel(root, platform)
            with self.assertRaisesRegex(ValueError, "missing corresponding firmware sources"):
                assets.build_assets(root, "0.3.0", "owner/repo", stable=True)
            self.assertFalse((root / "SHA256SUMS").exists())

    def test_nightly_set_has_matching_versions_and_sources_without_a_formula(self):
        version = "0.3.0+g42.abcdef0"
        with workspace() as (root, _):
            for platform in ("linux-x86_64", "macos-arm64"):
                wheel(root, platform, version=version)
                (root / f"pvisor-firmware-source-{platform}.tar.gz").write_bytes(b"source")
            assets.build_assets(root, version, "owner/repo", stable=False)
            self.assertFalse((root / "pvisor.rb").exists())
            manifest = (root / "SHA256SUMS").read_text().splitlines()
            self.assertEqual(len(manifest), 6)
            for platform in ("linux-x86_64", "macos-arm64"):
                native = root / f"pvisor-{version}-{platform}.tar.gz"
                self.assertTrue(native.is_file())
                with tarfile.open(native) as archive:
                    self.assertEqual(archive.extractfile("bin/pvisor").read(), b"payload pvisor")
                for path in (native, root / f"pvisor-firmware-source-{platform}.tar.gz"):
                    self.assertIn(f"{assets.digest(path)}  {path.name}", manifest)
            for entry in manifest:
                checksum, name = entry.split("  ")
                self.assertEqual(checksum, assets.digest(root / name))

    def test_formula_rejects_nightly_and_injected_inputs(self):
        for version, repository in (
            ("0.3.0+g42.abcd", "owner/repo"),
            ('0.3.0"', "owner/repo"),
            ("0.3.0", 'owner/repo"'),
        ):
            with self.subTest(version=version, repository=repository):
                with self.assertRaises(ValueError):
                    assets.build_assets(Path("unused"), version, repository, stable=True)
