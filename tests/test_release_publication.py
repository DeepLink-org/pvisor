from __future__ import annotations

import hashlib
import importlib.util
import unittest
from pathlib import Path
from unittest import mock

from tests.fixtures import workspace

SPEC = importlib.util.spec_from_file_location(
    "publication", Path(__file__).resolve().parents[1] / "scripts/ci/publish_release.py"
)
publication = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publication)


class ReleasePublicationTests(unittest.TestCase):
    def test_mismatch_fails_before_any_channel_mutates(self):
        with workspace() as (root, _):
            wheel = root / "pvisor-0.3.0-py3-none-platform.whl"
            wheel.write_bytes(b"new build")
            remote = {"urls": [{"filename": wheel.name, "digests": {"sha256": "0" * 64}}]}
            with (
                mock.patch.object(publication, "fetch_json", return_value=remote),
                mock.patch.object(publication.subprocess, "run") as run,
            ):
                with self.assertRaisesRegex(RuntimeError, "immutable release mismatch"):
                    publication.github_publish(root, "0.3.0", "owner/repo", "v0.3.0")
                run.assert_not_called()
                with self.assertRaises(RuntimeError):
                    publication.prepare_pypi(root, "0.3.0")
            self.assertEqual(wheel.read_bytes(), b"new build")

    def test_identical_github_files_are_retained_and_only_missing_are_uploaded(self):
        with workspace() as (root, _):
            wheel = root / "pvisor-0.3.0-py3-none-platform.whl"
            wheel.write_bytes(b"accepted")
            source = root / "pvisor-firmware-source-linux-x86_64.tar.gz"
            source.write_bytes(b"corresponding source")
            release = {
                "assets": [{"name": wheel.name, "digest": "sha256:" + publication.sha256(wheel)}]
            }
            with (
                mock.patch.object(publication, "fetch_json", side_effect=[None, release]),
                mock.patch.object(publication.subprocess, "run") as run,
            ):
                publication.github_publish(root, "0.3.0", "owner/repo", "v0.3.0")
            self.assertEqual(
                run.call_args.args[0],
                ["gh", "release", "upload", "v0.3.0", str(source), "--repo", "owner/repo"],
            )

    def test_new_release_contains_wheels_and_corresponding_sources(self):
        with workspace() as (root, _):
            wheel = root / "accepted.whl"
            source = root / "pvisor-firmware-source-linux-x86_64.tar.gz"
            wheel.write_bytes(b"accepted")
            source.write_bytes(b"corresponding source")
            native = root / "pvisor-0.3.0-linux-x86_64.tar.gz"
            native.write_bytes(b"native archive")
            (root / "SHA256SUMS").write_text("checksums")
            (root / "pvisor.rb").write_text("formula")
            with (
                mock.patch.object(publication, "fetch_json", return_value=None),
                mock.patch.object(publication.subprocess, "run") as run,
            ):
                publication.github_publish(root, "0.3.0", "owner/repo", "v0.3.0")
            args = run.call_args.args[0]
            self.assertEqual(args[:4], ["gh", "release", "create", "v0.3.0"])
            self.assertIn(str(wheel), args)
            self.assertIn(str(source), args)
            self.assertIn(str(native), args)
            self.assertIn(str(root / "SHA256SUMS"), args)
            self.assertIn(str(root / "pvisor.rb"), args)
            self.assertNotIn("--clobber", args)

    def test_pypi_retry_prunes_only_verified_identical_files(self):
        with workspace() as (root, _):
            existing = root / "existing.whl"
            pending = root / "pending.whl"
            existing.write_bytes(b"accepted")
            pending.write_bytes(b"pending")
            remote = {
                "urls": [
                    {
                        "filename": existing.name,
                        "digests": {"sha256": hashlib.sha256(b"accepted").hexdigest()},
                    }
                ]
            }
            with mock.patch.object(publication, "fetch_json", return_value=remote):
                self.assertTrue(publication.prepare_pypi(root, "0.3.0"))
            self.assertFalse(existing.exists())
            self.assertEqual(pending.read_bytes(), b"pending")

    def test_github_mismatch_also_rejects_without_upload(self):
        with workspace() as (root, _):
            wheel = root / "accepted.whl"
            wheel.write_bytes(b"changed")
            release = {"assets": [{"name": wheel.name, "digest": "sha256:" + "0" * 64}]}
            with (
                mock.patch.object(publication, "fetch_json", side_effect=[None, release]),
                mock.patch.object(publication.subprocess, "run") as run,
            ):
                with self.assertRaises(RuntimeError):
                    publication.github_publish(root, "0.3.0", "owner/repo", "v0.3.0")
                run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
