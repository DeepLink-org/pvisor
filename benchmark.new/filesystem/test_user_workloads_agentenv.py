"""AgentENV lifecycle and exported-output contracts without a live service."""

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import user_workloads_agentenv as adapter
from user_workloads_backends import tool_paths


class AgentENVTests(unittest.TestCase):
    def test_unsafe_archive_members_rejected(self):
        for name, kind in (
            ("../escape", tarfile.REGTYPE),
            ("/absolute", tarfile.REGTYPE),
            ("link", tarfile.SYMTYPE),
            ("device", tarfile.CHRTYPE),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                archive = Path(tmp) / "out.tar"
                with tarfile.open(archive, "w") as stream:
                    item = tarfile.TarInfo(name)
                    item.type = kind
                    item.linkname = "/etc/passwd" if kind == tarfile.SYMTYPE else ""
                    stream.addfile(item)
                with self.assertRaises(ValueError):
                    adapter.extract_output(archive, Path(tmp) / "output")
                self.assertFalse((Path(tmp) / "output").exists())

    def test_output_bytes_retained_for_common_host_oracle(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / "out.tar"
            with tarfile.open(archive, "w") as stream:
                item = tarfile.TarInfo("./round1/data")
                item.size = 3
                stream.addfile(item, io.BytesIO(b"abc"))
            adapter.extract_output(archive, Path(tmp) / "output")
            self.assertEqual((Path(tmp) / "output/round1/data").read_bytes(), b"abc")

    def test_timeout_cleanup_uses_only_recorded_sandbox(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            record = {
                "sandbox_id": "owned",
                "cleanup": False,
                "service_before": {"memory.events": "oom 0\noom_kill 0"},
            }
            adapter.save(folder / "agentenv.json", record)
            sandbox = mock.Mock()
            sandbox.kill.return_value = True
            with (
                mock.patch.object(
                    adapter, "connect", return_value=(sandbox, {"configuration": {}}, record)
                ),
                mock.patch.object(
                    adapter, "service_state", return_value={"memory.events": "oom 0\noom_kill 0"}
                ),
            ):
                adapter.cleanup_trial(None, None, folder)
                adapter.cleanup_trial(None, None, folder)
            sandbox.kill.assert_called_once_with(request_timeout=30)
            self.assertTrue(json.loads((folder / "agentenv.json").read_text())["cleanup"])

    def test_unknown_creation_outcome_not_silently_successful(self):
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            adapter.save(folder / "agentenv.json", {"token": "owned-create-token"})
            with self.assertRaisesRegex(ValueError, "outcome unknown"):
                adapter.cleanup_trial(None, None, folder)

    def test_compiler_identity_uses_actual_distribution_path(self):
        for path in (
            "usr/lib/gcc/x86_64-linux-gnu/12/cc1",
            "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
        ):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                helper = root / path
                helper.parent.mkdir(parents=True)
                helper.touch()
                self.assertIn(path, tool_paths(root))


if __name__ == "__main__":
    unittest.main()
