from __future__ import annotations

import importlib.util
import json
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
    checker = sys.modules["check_release_artifacts"]


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


def published_set(root):
    for platform in ("linux-x86_64", "macos-arm64"):
        wheel(root, platform)
        (root / f"pvisor-firmware-source-{platform}.tar.gz").write_bytes(b"source")
    assets.build_assets(root, "0.3.0", "owner/repo", stable=True)


def accepted_run():
    return {
        "repository": {"full_name": "owner/repo"},
        "head_repository": {"full_name": "owner/repo"},
        "path": ".github/workflows/release.yml",
        "event": "push",
        "status": "completed",
        "conclusion": "failure",  # A downstream failure must remain recoverable.
        "head_branch": "v0.3.0",
        "head_sha": "a" * 40,
    }


class NativeReleaseTests(unittest.TestCase):
    def test_recovery_source_requires_original_tag_and_successful_distribution(self):
        repository = "owner/repo"
        sha = "a" * 40
        accepted = accepted_run()
        jobs = [
            {
                "name": "Build and verify release / Verify release artifact set",
                "conclusion": "success",
            }
        ]
        checker.validate_recovery_source(accepted, jobs, repository, "0.3.0", sha)
        for key, value in (
            ("repository", {"full_name": "other/repo"}),
            ("head_repository", {"full_name": "fork/repo"}),
            ("path", ".github/workflows/nightly.yml"),
            ("event", "pull_request"),
            ("event", "workflow_dispatch"),
            ("status", "in_progress"),
            ("head_branch", "v0.4.0"),
            ("head_sha", "b" * 40),
        ):
            with self.subTest(key=key, value=value):
                with self.assertRaises(checker.ArtifactValidationError):
                    checker.validate_recovery_source(
                        {**accepted, key: value}, jobs, repository, "0.3.0", sha
                    )
        for invalid_jobs in (
            [],
            jobs * 2,
            [{**jobs[0], "conclusion": "failure"}],
            [{**jobs[0], "conclusion": "skipped"}],
        ):
            with self.subTest(jobs=invalid_jobs):
                with self.assertRaisesRegex(checker.ArtifactValidationError, "distribution gates"):
                    checker.validate_recovery_source(
                        accepted, invalid_jobs, repository, "0.3.0", sha
                    )

    def test_recovery_checks_provenance_before_downloading(self):
        with workspace() as (root, resources):
            api = resources.enter_context(mock.patch.object(checker.subprocess, "check_output"))
            download = resources.enter_context(mock.patch.object(checker.subprocess, "run"))
            api.side_effect = [b'{"event":"pull_request"}', b'[{"jobs":[]}]', b'{"sha":"wrong"}']
            with self.assertRaises(checker.ArtifactValidationError):
                checker.restore_artifacts(root, "0.3.0", "owner/repo", 42)
            download.assert_not_called()
            api.reset_mock()
            for version, repository, run_id in (
                ("../0.3.0", "owner/repo", 42),
                ("0.3.0", "../owner/repo", 42),
                ("0.3.0", "owner/repo", 0),
            ):
                with self.assertRaises(checker.ArtifactValidationError):
                    checker.restore_artifacts(root, version, repository, run_id)
            api.assert_not_called()

    def test_restore_merges_original_artifacts_and_rejects_collisions(self):
        for collision in (False, True):
            with self.subTest(collision=collision), workspace() as (root, resources):
                original, recovered = root / "original", root / "recovered"
                original.mkdir()
                published_set(original)
                before = {path.name: path.read_bytes() for path in original.iterdir()}
                api = resources.enter_context(mock.patch.object(checker.subprocess, "check_output"))
                api.side_effect = [
                    json.dumps(value).encode()
                    for value in (
                        accepted_run(),
                        [
                            {
                                "jobs": [
                                    {
                                        "name": "Build and verify release / Verify release artifact set",
                                        "conclusion": "success",
                                    }
                                ]
                            }
                        ],
                        {"sha": "a" * 40},
                    )
                ]

                def download(*args, **kwargs):
                    wheels, native = recovered / "release-wheels", recovered / "release-native"
                    wheels.mkdir(parents=True)
                    native.mkdir()
                    for name, content in before.items():
                        ((wheels if name.endswith(".whl") else native) / name).write_bytes(content)
                    if collision:
                        (native / next(wheels.iterdir()).name).write_bytes(b"collision")

                resources.enter_context(mock.patch.object(checker.subprocess, "run", download))
                if collision:
                    with self.assertRaisesRegex(
                        checker.ArtifactValidationError, "duplicate restored"
                    ):
                        checker.restore_artifacts(recovered, "0.3.0", "owner/repo", 42)
                else:
                    checker.restore_artifacts(recovered, "0.3.0", "owner/repo", 42)
                    checker.validate_release_set(recovered, "0.3.0")
                    self.assertEqual(
                        before, {path.name: path.read_bytes() for path in recovered.iterdir()}
                    )

    def test_recovery_validates_complete_set_without_modifying_it(self):
        with workspace() as (root, _):
            published_set(root)
            before = {path.name: path.read_bytes() for path in root.iterdir()}
            self.assertEqual(
                set(checker.validate_release_set(root, "0.3.0")),
                {"linux-x86_64", "macos-arm64"},
            )
            self.assertEqual(before, {path.name: path.read_bytes() for path in root.iterdir()})
            with self.assertRaisesRegex(
                checker.ArtifactValidationError, "expected filename prefix"
            ):
                checker.validate_release_set(root, "0.4.0")
            with self.assertRaisesRegex(checker.ArtifactValidationError, "stable X.Y.Z"):
                checker.validate_release_set(root, "../0.3.0")

    def test_recovery_rejects_corrupt_incomplete_and_unexpected_assets(self):
        for name in (
            "pvisor-0.3.0-linux-x86_64.tar.gz",
            "pvisor-firmware-source-macos-arm64.tar.gz",
            "pvisor.rb",
        ):
            for fault in ("corrupt", "missing", "unlisted", "symlink"):
                with self.subTest(name=name, fault=fault), workspace() as (root, _):
                    published_set(root)
                    path = root / name
                    manifest = root / "SHA256SUMS"
                    if fault == "corrupt":
                        path.write_bytes(b"changed")
                    elif fault == "missing":
                        path.unlink()
                        # Reject even a manifest that agrees with an incomplete set.
                        manifest.write_text(
                            "".join(
                                line
                                for line in manifest.read_text().splitlines(keepends=True)
                                if not line.endswith(f"  {name}\n")
                            )
                        )
                    elif fault == "unlisted":
                        (root / "extra.txt").write_text("unexpected")
                    else:
                        path.unlink()
                        path.symlink_to(root / "SHA256SUMS")
                    with self.assertRaises(checker.ArtifactValidationError):
                        checker.validate_release_set(root, "0.3.0")

    def test_recovery_rejects_invalid_manifests(self):
        for fault in ("missing", "duplicate", "traversal", "oversize", "symlink"):
            with self.subTest(fault=fault), workspace() as (root, _):
                published_set(root)
                manifest = root / "SHA256SUMS"
                if fault == "missing":
                    manifest.unlink()
                elif fault == "duplicate":
                    manifest.write_text(manifest.read_text() * 2)
                elif fault == "traversal":
                    manifest.write_text(f"{'0' * 64}  ../outside\n")
                elif fault == "oversize":
                    manifest.write_text("x" * 65537)
                else:
                    target = root / "manifest"
                    manifest.rename(target)
                    manifest.symlink_to(target)
                with self.assertRaises(checker.ArtifactValidationError):
                    checker.validate_release_set(root, "0.3.0")

    def test_formula_recovery_is_repeatable_and_preserves_published_bytes(self):
        with workspace() as (root, resources):
            original = root / "published"
            original.mkdir()
            published_set(original)
            # Model an older formula whose tests need a fix on main.
            (original / "pvisor.rb").write_text("old formula tests\n")
            manifest = original / "SHA256SUMS"
            manifest.write_text(
                "".join(
                    f"{assets.digest(path)}  {path.name}\n"
                    for path in sorted(original.iterdir())
                    if path != manifest
                )
            )
            before = {path.name: path.read_bytes() for path in original.iterdir()}
            build = resources.enter_context(mock.patch.object(assets, "native_archive"))
            output = root / "formula/pvisor.rb"
            for _ in range(2):
                assets.refresh_formula(original, "0.3.0", "owner/repo", output)
                self.assertIn('system bin/"pvisor-daemon", "protocol"', output.read_text())
                for platform in ("linux-x86_64", "macos-arm64"):
                    self.assertIn(
                        assets.digest(original / f"pvisor-0.3.0-{platform}.tar.gz"),
                        output.read_text(),
                    )
                self.assertEqual(
                    before, {path.name: path.read_bytes() for path in original.iterdir()}
                )
            build.assert_not_called()
            with self.assertRaisesRegex(ValueError, "outside the immutable release set"):
                assets.refresh_formula(original, "0.3.0", "owner/repo", original / "pvisor.rb")
            (original / "pvisor-0.3.0-linux-x86_64.tar.gz").write_bytes(b"corrupt")
            output.unlink()
            with self.assertRaisesRegex(checker.ArtifactValidationError, "SHA-256 mismatch"):
                assets.refresh_formula(original, "0.3.0", "owner/repo", output)
            self.assertFalse(output.exists())

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
            self.assertIn('system bin/"pvisor-daemon", "protocol"', formula)
            self.assertNotIn(" spawn ", formula, "Homebrew's Seatbelt cannot nest pVisor's")

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
                with self.assertRaises(ValueError):
                    assets.homebrew_formula(version, repository, {})
