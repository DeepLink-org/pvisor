"""Offline contracts for V2 fixtures and trial evidence; never starts a VM."""

import importlib.util
import json
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "fs_v2_runner_for_tests", HERE / "workloads_v2_run.py"
)
RUN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN)


class FakeProcess:
    """The observer sees subject output; no subject process is created."""

    def __init__(self, lines):
        self.stdout = iter(lines)
        self.pid = 999999999

    def wait(self):
        return 0

    def poll(self):
        return 0


class RunnerContracts(unittest.TestCase):
    def test_host_view_hides_only_recognized_empty_metadata_and_preserves_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            upper = Path(temporary) / "upper"
            (upper / "published").mkdir(parents=True)
            (upper / ".wh..wh..opq").touch()
            (upper / "published/.wh..wh..opq").touch()
            (upper / "published/retained.dat").write_bytes(b"complete bytes")
            # Unexpected application files must reach the independent oracle.
            (upper / "unexpected.dat").write_bytes(b"unexpected")
            view = Path(temporary) / "view"
            markers = RUN.materialize_upper_view(upper, view, "S03")
            self.assertEqual(set(markers), {".wh..wh..opq", "published/.wh..wh..opq"})
            self.assertEqual((view / "published/retained.dat").read_bytes(), b"complete bytes")
            self.assertEqual((view / "unexpected.dat").read_bytes(), b"unexpected")
            self.assertEqual(
                (upper / "published/retained.dat").stat().st_ino,
                (view / "published/retained.dat").stat().st_ino,
            )
            self.assertFalse((view / ".wh..wh..opq").exists())

    def test_host_view_recognizes_exact_namespace_deletion_set(self):
        with tempfile.TemporaryDirectory() as temporary:
            upper = Path(temporary) / "upper"
            upper.mkdir()
            (upper / ".wh.temp-0000.bin").touch()
            (upper / ".wh.renamed-0127.bin").touch()
            (upper / "sentinel.bin").write_bytes(b"live")
            view = Path(temporary) / "view"
            markers = RUN.materialize_upper_view(upper, view, "M12")
            self.assertEqual(set(markers), {".wh.temp-0000.bin", ".wh.renamed-0127.bin"})
            self.assertEqual({path.name for path in view.iterdir()}, {"sentinel.bin"})

    def test_host_view_rejects_unknown_nonempty_conflicting_or_misplaced_markers(self):
        for case, marker, content, retained in (
            ("S03", ".wh.temp-0000.bin", b"", False),
            ("M12", ".wh.temp-0128.bin", b"", False),
            ("M12", ".wh.sentinel.bin", b"", False),
            ("S03", ".wh..wh..opq", b"not empty", False),
            ("M12", ".wh.temp-0000.bin", b"", True),
            ("M12", "nested/.wh.temp-0000.bin", b"", False),
        ):
            with self.subTest(case=case, marker=marker), tempfile.TemporaryDirectory() as temporary:
                upper = Path(temporary) / "upper"
                path = upper / marker
                path.parent.mkdir(parents=True)
                path.write_bytes(content)
                if retained:
                    (upper / "temp-0000.bin").write_bytes(b"survived")
                with self.assertRaises(ValueError):
                    RUN.materialize_upper_view(upper, Path(temporary) / "view", case)

    def test_host_view_rejects_symlinks_and_special_files(self):
        for kind in ("file-link", "directory-link", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                upper = Path(temporary) / "upper"
                upper.mkdir()
                outside = Path(temporary) / "outside"
                if kind == "file-link":
                    outside.write_bytes(b"outside")
                    (upper / "bad").symlink_to(outside)
                elif kind == "directory-link":
                    outside.mkdir()
                    (upper / "bad").symlink_to(outside, target_is_directory=True)
                else:
                    RUN.os.mkfifo(upper / "bad")
                with self.assertRaises(ValueError):
                    RUN.materialize_upper_view(upper, Path(temporary) / "view", "S03")

    def test_frozen_application_fixture_oracles(self):
        with tempfile.TemporaryDirectory() as temporary:
            layer = Path(temporary) / "layer"
            fixture = RUN.make_fixture(layer)
            for index in (0, 31, 32, 1023):
                relative = f"d{index // 32:02}/f{index % 32:03}"
                header = f"file {index:04} V2_NEEDLE\n".encode()
                self.assertEqual(
                    (fixture / "repo" / (relative + ".txt")).read_bytes(),
                    header + b"x" * (4096 - len(header)),
                )
            status = subprocess.check_output(
                ["git", "-C", str(fixture / "repo"), "status", "--porcelain"]
            )
            self.assertEqual(status, b"")
            with tarfile.open(fixture / "package.tar", "r:") as archive:
                self.assertEqual(len(archive.getmembers()), 1024)
                for member in archive.getmembers():
                    self.assertTrue(member.isfile())
                    self.assertEqual(member.size, 4096)
                    self.assertEqual((member.uid, member.gid, member.mtime), (0, 0, 0))
                    directory, name = member.name.split("/")
                    index = int(directory[1:]) * 32 + int(name[1:4])
                    header = f"package {index:04}\n".encode()
                    self.assertEqual(
                        archive.extractfile(member).read(), header + b"p" * (4096 - len(header))
                    )
            for index in range(100):
                self.assertEqual(
                    (fixture / "build" / f"unit{index:03}.c").read_text(),
                    f"int unit{index:03}(void) {{ return {index}; }}\n",
                )
            main = (fixture / "build/main.c").read_text()
            self.assertEqual(main.count("sum += unit"), 100)
            self.assertIn('printf("%d\\n", sum);', main)

    def trial(self, temporary, *, broken_clock=False, host_ok=True):
        batch = Path(temporary)
        (batch / "memory.current").write_text("0")
        state = {"cgroup": str(batch), "memory.current": "0", "cpu.stat": "usage_usec 100"}
        rows = [
            dict(
                id="M06",
                round=r,
                correctness=True,
                work_ns=10,
                final_sync_ns=5,
                completed_ns=16 if broken_clock else 15,
                write_ops=4096,
                bytes_written=4 * RUN.MIB,
            )
            for r in range(1, 4)
        ]
        lines = ['WORKLOADS_V2_READY {"actual_guest_memtotal_kib": 500000}\n']
        lines += ["WORKLOADS_V2_RESULT " + json.dumps(row) + "\n" for row in rows]
        lines += ["WORKLOADS_V2_DONE\n"]
        protocol = {
            "rootfs": "/frozen/rootfs",
            "fixture": "/frozen/fixture",
            "conditions": {"baseline": {"env": {}}},
            "trial_timeout_seconds": 30,
            "diagnostic": False,
        }
        receipt = 'WORKLOADS_V2_VERIFIED {"verified": true}\n' if host_ok else ""
        verification = subprocess.CompletedProcess(
            [], 0 if host_ok else 1, stdout=receipt, stderr=""
        )
        with (
            patch.object(RUN, "cgroup_state", return_value=state),
            patch.object(RUN.subprocess, "Popen", return_value=FakeProcess(lines)),
            patch.object(RUN.subprocess, "run", return_value=verification) as verify,
        ):
            record = RUN.one_trial(batch, protocol, "M06", 0, "baseline")
        return record, verify.call_args

    def test_success_requires_partition_and_separate_host_oracle(self):
        with tempfile.TemporaryDirectory() as temporary:
            record, call = self.trial(temporary)
            self.assertTrue(record["valid"])
            self.assertTrue(record["host_verified"])
            command = call.args[0]
            self.assertEqual(command[1:3], ["--verify", "M06"])
            self.assertTrue(command[3].endswith("/trials/M06-000-baseline/host-view"))
            self.assertEqual(command[4], "/frozen/fixture")
            self.assertEqual(len(record["rows"]), 3)

    def test_bad_clock_preserves_failure_observations_without_zero_filling(self):
        with tempfile.TemporaryDirectory() as temporary:
            record, call = self.trial(temporary, broken_clock=True)
            self.assertFalse(record["valid"])
            self.assertFalse(record["host_verified"])
            self.assertIsNone(call)
            self.assertEqual([row["completed_ns"] for row in record["rows"]], [16, 16, 16])
            self.assertIn("clock partition", record["error"])
            self.assertTrue((Path(temporary) / "trials/M06-000-baseline/stage").is_dir())

    def test_successful_guest_with_failed_host_oracle_remains_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            record, _ = self.trial(temporary, host_ok=False)
            self.assertFalse(record["valid"])
            self.assertFalse(record["host_verified"])
            self.assertEqual(len(record["rows"]), 3)
            self.assertIn("host output verification", record["error"])


if __name__ == "__main__":
    unittest.main()
