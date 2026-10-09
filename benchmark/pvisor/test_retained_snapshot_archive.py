import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest import mock

import retained_snapshot_archive as retention


def fixture(root):
    job = root / "j0"
    stage = job / "stage"
    active = stage / "attempts/finished"
    active.mkdir(parents=True)
    (active / "run-bundle.json").write_text("{}")
    (stage / "execution-job.json").write_text(
        json.dumps(dict(state="terminal", active_stage=str(active)))
    )
    snapshots = stage / "execution-snapshots"
    snapshots.mkdir()
    (snapshots / "ram.bin").write_bytes(bytes(range(256)) * 4096)
    os.setxattr(snapshots / "ram.bin", "user.benchmark", b"original metadata\x00\xff")
    os.utime(snapshots / "ram.bin", ns=(1700000000123456789, 1700000000123456789))
    (snapshots / "hardlink.bin").hardlink_to(snapshots / "ram.bin")
    (snapshots / "guest-link").symlink_to("/guest/path")
    restore = active / "execution-restore"
    restore.mkdir()
    (restore / "machine.json").write_text("{}")
    ready = dict(
        kind="repeated",
        seed=20261006,
        bytes=64 * 1024**2,
        token="original-token",
        checksum="full-checksum",
        pid=1,
    )
    result = ready | dict(integrity="passed", changes=4, first_scan_ms=1)
    resumed = job / "resumed"
    resumed.mkdir()
    (resumed / "stdout.log").write_text("PVISOR_PARKED_RESULT " + json.dumps(result) + "\n")
    value = dict(
        correctness="passed",
        backend="snapshot-raw",
        concurrency=1,
        attempted=1,
        parked=1,
        completed=1,
        failed=0,
        outcomes=[dict(correctness="passed", logs=str(job), ready=ready, result=result)],
    )
    return value, snapshots, restore


@unittest.skipIf(shutil.which("zstd") is None, "real archive roundtrip needs zstd")
@unittest.skipUnless(hasattr(os, "setxattr"), "requires Linux user.* extended attributes")
class RetainedSnapshotArchiveTests(unittest.TestCase):
    def test_real_archive_preserves_bytes_links_metadata_and_readable_final_evidence(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            value, snapshots, restore = fixture(tmp_path)
            result = retention.archive_completed_snapshots(tmp_path, value, 0)
            manifest = json.loads((tmp_path / result["manifest"]).read_text())
            archive = tmp_path / result["archive"]
            assert (
                retention.file_sha(archive)
                == manifest["archive_sha256"]
                == result["archive_sha256"]
            )
            assert not snapshots.exists() and not restore.exists()
            assert (tmp_path / "j0/resumed/stdout.log").is_file()
            assert (tmp_path / "j0/stage/attempts/finished/run-bundle.json").is_file()
            assert (tmp_path / "j0/stage/execution-job.json").is_file()
            retention.validate_archive(archive, manifest["members"])
            payload = subprocess.check_output(["zstd", "-q", "--long=29", "-d", "-c", str(archive)])
            with tarfile.open(fileobj=io.BytesIO(payload)) as tar:
                assert (
                    tar.extractfile("j0/stage/execution-snapshots/ram.bin").read()
                    == bytes(range(256)) * 4096
                )
                assert (
                    tar.getmember("j0/stage/execution-snapshots/guest-link").linkname
                    == "/guest/path"
                )
                assert sum(member.islnk() for member in tar.getmembers()) == 1
            destination = tmp_path / "restored"
            retention.restore_archive(tmp_path / result["manifest"], destination)
            restored = destination / "j0/stage/execution-snapshots/ram.bin"
            assert restored.read_bytes() == bytes(range(256)) * 4096
            assert restored.stat().st_mtime_ns == 1700000000123456789
            assert os.getxattr(restored, "user.benchmark") == b"original metadata\x00\xff"
            assert restored.stat().st_ino == (restored.parent / "hardlink.bin").stat().st_ino

    def test_noncompleted_snapshot_conditions_are_untouched(self):
        for key, value in [
            ("correctness", "failed"),
            ("backend", "native-paused"),
            ("backend", "podman-paused"),
        ]:
            with self.subTest(key=key, value=value):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    row, snapshots, restore = fixture(tmp_path)
                    row[key] = value
                    assert retention.archive_completed_snapshots(tmp_path, row, 0) is None
                    assert (
                        snapshots.exists()
                        and restore.exists()
                        and not (tmp_path / "snapshot-artifacts.tar.zst").exists()
                    )

    def test_nonzero_measured_service_keeps_all_artifacts(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            assert retention.archive_completed_snapshots(tmp_path, row, 1) is None
            assert snapshots.exists()

    def test_incomplete_successful_counts_refuse_deletion(self):
        for key, value in [("completed", 0), ("parked", 0), ("failed", 1), ("attempted", 2)]:
            with self.subTest(key=key, value=value):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    row, snapshots, _ = fixture(tmp_path)
                    row[key] = value
                    with self.assertRaisesRegex(ValueError, "incomplete"):
                        retention.archive_completed_snapshots(tmp_path, row, 0)
                    assert snapshots.exists()

    def test_nonterminal_state_refuses_deletion(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            path = tmp_path / "j0/stage/execution-job.json"
            state = json.loads(path.read_text())
            state["state"] = "suspended"
            path.write_text(json.dumps(state))
            with self.assertRaisesRegex(ValueError, "not terminal"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert snapshots.exists()

    def test_independent_stdout_mismatch_refuses_deletion(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            (tmp_path / "j0/resumed/stdout.log").write_text("PVISOR_PARKED_RESULT {}\n")
            with self.assertRaisesRegex(ValueError, "independent"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert snapshots.exists()

    def test_path_escape_refuses_deletion(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            row["outcomes"][0]["logs"] = str(tmp_path.parent / "outside")
            with self.assertRaisesRegex(ValueError, "path"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert snapshots.exists()

    def test_snapshot_tree_symlink_cannot_delete_external_data(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            outside = tmp_path / "outside"
            snapshots.rename(outside)
            snapshots.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert (outside / "ram.bin").is_file()

    def test_archive_verification_failure_keeps_originals(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, restore = fixture(tmp_path)

            def reject(*args):
                raise ValueError("archive verification failed")

            resources.enter_context(mock.patch.object(retention, "validate_archive", reject))
            with self.assertRaisesRegex(ValueError, "verification"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert (
                snapshots.exists()
                and restore.exists()
                and not (tmp_path / "snapshot-artifacts.json").exists()
            )

    def test_source_mutation_after_compression_keeps_originals(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, restore = fixture(tmp_path)
            original = retention.validate_archive

            def change(*args):
                original(*args)
                (snapshots / "ram.bin").write_bytes(b"externally changed")

            resources.enter_context(mock.patch.object(retention, "validate_archive", change))
            with self.assertRaisesRegex(ValueError, "changed during"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert snapshots.exists() and restore.exists()

    def test_corrupt_archive_frame_is_rejected(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, _, _ = fixture(tmp_path)
            result = retention.archive_completed_snapshots(tmp_path, row, 0)
            manifest = json.loads((tmp_path / result["manifest"]).read_text())
            archive = tmp_path / result["archive"]
            archive.write_bytes(archive.read_bytes()[:-3])
            with self.assertRaises((ValueError, tarfile.TarError)):
                retention.validate_archive(archive, manifest["members"])

    def test_archive_inventory_content_and_metadata_are_checked(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, _, _ = fixture(tmp_path)
            result = retention.archive_completed_snapshots(tmp_path, row, 0)
            manifest = json.loads((tmp_path / result["manifest"]).read_text())
            archive = tmp_path / result["archive"]
            name = "j0/stage/execution-snapshots/ram.bin"
            manifest["members"][name]["sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "content|hard link"):
                retention.validate_archive(archive, manifest["members"])

    def test_existing_archive_is_never_overwritten(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, snapshots, _ = fixture(tmp_path)
            archive = tmp_path / "snapshot-artifacts.tar.zst"
            archive.write_bytes(b"original archive")
            with self.assertRaisesRegex(ValueError, "already exists"):
                retention.archive_completed_snapshots(tmp_path, row, 0)
            assert snapshots.exists() and archive.read_bytes() == b"original archive"

    def test_restore_never_overwrites_existing_directory(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, _, _ = fixture(tmp_path)
            result = retention.archive_completed_snapshots(tmp_path, row, 0)
            with self.assertRaisesRegex(ValueError, "new empty"):
                retention.restore_archive(tmp_path / result["manifest"], tmp_path)

    def test_restore_rejects_tampered_receipt_before_creating_destination(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            row, _, _ = fixture(tmp_path)
            result = retention.archive_completed_snapshots(tmp_path, row, 0)
            path = tmp_path / result["manifest"]
            manifest = json.loads(path.read_text())
            manifest["archive_sha256"] = "0" * 64
            path.write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "receipt"):
                retention.restore_archive(path, tmp_path / "restored")
            assert not (tmp_path / "restored").exists()
