import json
import signal
import struct
import subprocess
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import filesystem_exec_probe
from filesystem_exec_probe import elf_interpreter, prepare_copies, run_owned_probe
from reference_baselines import digest


class FilesystemExecProbeTests(unittest.TestCase):
    def test_loader_probe_rejects_static_or_invalid_elf_and_reads_declared_interpreter(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            path = tmp_path / "tool"
            path.write_bytes(b"not an executable")
            with self.assertRaisesRegex(ValueError, "ELF64"):
                elf_interpreter(path)
            data = bytearray(128)
            data[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<Q", data, 32, 64)
            struct.pack_into("<HH", data, 54, 56, 0)
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "no dynamic interpreter"):
                elf_interpreter(path)
            interpreter = b"/lib64/ld-linux-x86-64.so.2\0"
            data.extend(interpreter)
            struct.pack_into("<HH", data, 54, 56, 1)
            struct.pack_into("<I", data, 64, 3)
            struct.pack_into("<Q", data, 72, 128)
            struct.pack_into("<Q", data, 96, len(interpreter))
            path.write_bytes(data)
            assert str(elf_interpreter(path)) == "/lib64/ld-linux-x86-64.so.2"
            data[-1] = ord("x")
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "interpreter entry"):
                elf_interpreter(path)

    def test_copy_preparation_uses_independent_inodes_and_rejects_changed_inputs(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            original = tmp_path / "tool"
            original.write_bytes(b"identical ELF input")
            work = tmp_path / "workspace"
            work.mkdir()
            inputs = [dict(path=str(original), copy_path="copies/0", sha256=digest(original))]
            prepare_copies(work, inputs)
            copied = work / "copies/0"
            assert copied.read_bytes() == original.read_bytes()
            assert (copied.stat().st_dev, copied.stat().st_ino) != (
                original.stat().st_dev,
                original.stat().st_ino,
            )
            original.write_bytes(b"changed source")
            with self.assertRaisesRegex(ValueError, "identical bytes"):
                prepare_copies(work, inputs)

    def test_probe_timeout_kills_owned_group_and_retains_partial_evidence(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            calls = []

            class Process:
                pid = 12345
                returncode = None

                def communicate(self, timeout=None):
                    if timeout is not None:
                        raise subprocess.TimeoutExpired(["probe"], timeout)
                    self.returncode = -signal.SIGKILL
                    return b"partial guest output", b"partial filesystem counters"

            def launch(command, **kwargs):
                assert kwargs["start_new_session"] is True
                kwargs["stderr"].write(b"partial filesystem counters")
                return Process()

            resources.enter_context(
                mock.patch.object(filesystem_exec_probe.subprocess, "Popen", launch)
            )
            resources.enter_context(
                mock.patch.object(
                    filesystem_exec_probe.os, "killpg", lambda pid, sig: calls.append((pid, sig))
                )
            )
            with self.assertRaises(subprocess.TimeoutExpired):
                run_owned_probe(["probe"], tmp_path, {}, tmp_path, timeout=1)
            assert calls == [(12345, signal.SIGKILL)]
            assert (tmp_path / "stdout.log").read_bytes() == b"partial guest output"
            assert (tmp_path / "stderr.log").read_bytes() == b"partial filesystem counters"
            assert json.loads((tmp_path / "command.json").read_text())["timed_out"] is True

    def test_nonblocking_large_profile_retains_all_original_bytes(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            payload = b"profile record\xff\n" * 65536
            code = "import os; os.set_blocking(2,False); data=b'profile record\\xff\\n'*65536; offset=0\nwhile offset<len(data): offset+=os.write(2,data[offset:])\nprint('checked result')"
            result = run_owned_probe([sys.executable, "-c", code], tmp_path, None, tmp_path)
            assert result.returncode == 0
            assert result.stdout == b"checked result\n"
            assert result.stderr == payload
            assert (tmp_path / "stderr.log").read_bytes() == payload
            assert (
                json.loads((tmp_path / "command.json").read_text())["stderr_capture"]
                == "regular-file diagnostic"
            )

    def test_final_gate_rejects_changed_inputs(self):
        for changed in ["prepared-input", "binary", "firmware", "library"]:
            with self.subTest(changed=changed):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    (tmp_path / "bin").mkdir()
                    (tmp_path / "firmware").mkdir()
                    binary = tmp_path / "bin/pvisor"
                    binary.write_bytes(b"frozen binary")
                    firmware = tmp_path / "firmware/libkrunfw.so.5"
                    firmware.write_bytes(b"frozen firmware")
                    library = tmp_path / "library"
                    library.write_bytes(b"original library")
                    initial = dict(
                        binary_sha256=digest(binary),
                        firmware_sha256=digest(firmware),
                        reference={"files": 3},
                    )
                    actual = dict(initial)
                    inputs = [dict(path=str(library), sha256=digest(library))]
                    args = SimpleNamespace(output=tmp_path)
                    resources.enter_context(
                        mock.patch.object(
                            filesystem_exec_probe, "verified_probe_inputs", lambda _: actual
                        )
                    )
                    assert (
                        filesystem_exec_probe.verify_final_inputs(args, initial, inputs) == initial
                    )
                    if changed == "prepared-input":
                        actual["reference"] = {"files": 4}
                    elif changed == "binary":
                        binary.write_bytes(b"replaced binary")
                    elif changed == "firmware":
                        firmware.write_bytes(b"replaced firmware")
                    else:
                        library.write_bytes(b"replaced library")
                    with self.assertRaisesRegex(ValueError, "changed"):
                        filesystem_exec_probe.verify_final_inputs(args, initial, inputs)
