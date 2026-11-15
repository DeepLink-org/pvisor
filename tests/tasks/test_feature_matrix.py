"""Capability declarations must not turn CLI-only failures into hardware failures."""

import os
import runpy
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DISCOVER = runpy.run_path(str(ROOT / "scripts/tasks/feature-matrix.py"))["discover"]


class FeatureMatrixTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)

    def add(self, feature, identifier="S-NEW-001"):
        directory = self.root / feature
        directory.mkdir(exist_ok=True)
        (directory / f"{identifier}.md").touch()

    def test_native_discovery_has_no_feature_or_id_allowlist(self):
        self.add("new-feature")
        self.add("config", "S-CONFIG-999")
        self.add("config", "S-CONFIG-1000")
        (self.root / "fixtures").mkdir()
        self.assertEqual(
            [row["feature"] for row in DISCOVER(self.root)],
            ["config", "new-feature"],
        )

    def test_cli_features_have_no_hardware_prerequisites(self):
        for feature in ("config", "docker-cli", "docker-compose", "replay", "session"):
            self.add(feature)
        for row in DISCOVER(self.root):
            self.assertFalse(any(value for key, value in row.items() if key != "feature"))

    def test_hardware_features_keep_their_required_capabilities(self):
        expected = {
            "checkpoint": {"userns", "fuse"},
            "container": {"userns", "oci"},
            "gateway": {"userns", "fuse"},
            "isolation": {"userns", "fuse"},
            "network": {"userns", "kvm"},
            "resources": {"userns", "fuse"},
            "stage": {"userns", "fuse"},
            "vm": {"userns", "fuse", "kvm", "vm_sdk"},
        }
        for feature in expected:
            self.add(feature)
        for row in DISCOVER(self.root):
            self.assertEqual(
                {key for key, value in row.items() if key != "feature" and value},
                expected[row["feature"]],
            )

    def test_empty_inventory_refuses(self):
        with self.assertRaisesRegex(ValueError, "empty"):
            DISCOVER(self.root)

    def test_ci_keeps_case_roots_in_the_host_private_tmp_namespace(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("PVISOR_CASE_ARTIFACTS: /tmp/pvisor-feature-", workflow)
        self.assertNotIn("PVISOR_CASE_ARTIFACTS: ${{ runner.temp }}", workflow)
        self.assertIn('--output "$RUNNER_TEMP/feature-evidence/report.json"', workflow)


class FeatureEvidenceArchiveTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(prefix="feature archive ")
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.evidence = self.root / "feature-evidence"
        self.evidence.mkdir()
        (self.evidence / ".diagnostic").write_bytes(b"exact diagnostic\n")
        (self.evidence / ".diagnostic").chmod(0o600)
        self.evidence.chmod(0o700)
        self.outside = self.root / "outside"
        self.outside.mkdir()
        (self.outside / "not-evidence").write_text("must not follow links")
        (self.evidence / "link").symlink_to(self.outside, target_is_directory=True)
        tools = self.root / "bin"
        tools.mkdir()
        # Only the hosted runner elevates. Locally, verify the exact noninteractive
        # boundary and execute real tar against our readable, owned fixtures.
        sudo = tools / "sudo"
        sudo.write_text('#!/bin/sh\n[ "$1" = -n ] || exit 90\nshift\nexec "$@"\n')
        sudo.chmod(0o700)
        self.env = {
            **os.environ,
            "RUNNER_TEMP": str(self.root),
            "PATH": str(tools) + os.pathsep + os.environ["PATH"],
        }
        self.env.pop("PVISOR_CASE_ARTIFACTS", None)
        self.archive = self.root / "feature-evidence.tar.gz"

    def invoke(self):
        return subprocess.run(
            ["bash", str(ROOT / "scripts/tasks/archive-feature-evidence.sh")],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_archive_preserves_permissions_hidden_files_and_symlinks(self):
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.archive) as archive:
            self.assertEqual(archive.getmember("feature-evidence").mode, 0o700)
            diagnostic = archive.getmember("feature-evidence/.diagnostic")
            self.assertEqual(diagnostic.mode, 0o600)
            self.assertEqual(archive.extractfile(diagnostic).read(), b"exact diagnostic\n")
            link = archive.getmember("feature-evidence/link")
            self.assertTrue(link.issym())
            self.assertEqual(link.linkname, str(self.outside))
            self.assertNotIn("feature-evidence/link/not-evidence", archive.getnames())
        self.assertEqual(self.evidence.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.evidence / ".diagnostic").stat().st_mode & 0o777, 0o600)
        self.assertTrue((self.evidence / "link").is_symlink())

    def test_failed_archive_never_publishes_partial_result(self):
        (self.root / "bin/sudo").write_text("#!/bin/sh\nexit 7\n")
        result = self.invoke()
        self.assertEqual(result.returncode, 7)
        self.assertFalse(self.archive.exists())
        self.assertEqual(list(self.root.glob("feature-evidence.??????")), [])
        self.assertEqual((self.evidence / ".diagnostic").read_bytes(), b"exact diagnostic\n")

    def test_separate_case_resources_are_archived_without_relocation(self):
        cases = self.root / "private-cases"
        cases.mkdir(mode=0o700)
        (cases / "stderr").write_text("retained failure evidence")
        self.env["PVISOR_CASE_ARTIFACTS"] = str(cases)
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.archive) as archive:
            self.assertEqual(archive.getmember("private-cases").mode, 0o700)
            self.assertEqual(
                archive.extractfile("private-cases/stderr").read(),
                b"retained failure evidence",
            )
            self.assertIn("feature-evidence/.diagnostic", archive.getnames())
        self.assertEqual((cases / "stderr").read_text(), "retained failure evidence")
        self.assertEqual(cases.stat().st_mode & 0o777, 0o700)

    def test_symlink_case_resource_root_is_refused(self):
        link = self.root / "case-link"
        link.symlink_to(self.outside, target_is_directory=True)
        self.env["PVISOR_CASE_ARTIFACTS"] = str(link)
        self.assertNotEqual(self.invoke().returncode, 0)
        self.assertFalse(self.archive.exists())

    def test_existing_archive_is_not_overwritten(self):
        self.archive.write_bytes(b"original archive")
        self.assertNotEqual(self.invoke().returncode, 0)
        self.assertEqual(self.archive.read_bytes(), b"original archive")

    def test_symlink_evidence_root_is_refused(self):
        self.env["RUNNER_TEMP"] = str(self.outside)
        (self.outside / "feature-evidence").symlink_to(self.evidence, target_is_directory=True)
        self.assertNotEqual(self.invoke().returncode, 0)
        self.assertFalse((self.outside / "feature-evidence.tar.gz").exists())
