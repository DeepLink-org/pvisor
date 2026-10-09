"""VM interaction regressions: PVISOR_TEST_VM_BIN=target/release/pvisor just test-py tests/test_vm_terminal.py."""

import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time
import unittest
from pathlib import Path

from tests.fixtures import workspace


class VmTerminalTests(unittest.TestCase):
    def test_vm_bash_accepts_terminal_input(self):
        for tui in [False, True]:
            with self.subTest(tui=tui):
                with workspace() as (tmp_path, resources):
                    vm_bin = os.environ.get("PVISOR_TEST_VM_BIN")
                    if not vm_bin:
                        self.skipTest("requires PVISOR_TEST_VM_BIN and VM hardware")
                    vm_bin = str(Path(vm_bin).resolve())
                    self.assertTrue(Path(vm_bin).is_file(), f"VM CLI does not exist: {vm_bin}")
                    master, slave = pty.openpty()
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

                    def acquire_terminal():
                        os.setsid()
                        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

                    child = subprocess.Popen(
                        [
                            vm_bin,
                            "run",
                            "--vm",
                            *(["--tui"] if tui else []),
                            "--rootfs",
                            "image=ubuntu:latest",
                            "--",
                            "bash",
                        ],
                        cwd=tmp_path,
                        stdin=slave,
                        stdout=slave,
                        stderr=slave,
                        env={
                            **os.environ,
                            "TERM": "xterm-256color",
                            "PVISOR_RUN_HOME": str(tmp_path / "runs"),
                        },
                        preexec_fn=acquire_terminal,
                    )
                    os.close(slave)
                    screen = bytearray()
                    sent = False
                    deadline = time.monotonic() + 45
                    try:
                        while time.monotonic() < deadline:
                            ready, _, _ = select.select([master], [], [], 0.1)
                            if ready:
                                try:
                                    data = os.read(master, 65536)
                                except OSError:
                                    break
                                if not data:
                                    break
                                screen.extend(data)
                            if not sent and b"root@" in screen:
                                os.write(
                                    master,
                                    b"test -t 0 && test -t 1 && printf 'VM_%s\\n' TERMINAL_OK; exit\n",
                                )
                                sent = True
                            if child.poll() is not None and not ready:
                                break
                        assert sent, screen.decode(errors="replace")
                        assert child.wait(timeout=2) == 0, screen.decode(errors="replace")
                        assert b"VM_TERMINAL_OK" in screen, screen.decode(errors="replace")
                    finally:
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGINT)
                            try:
                                child.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                child.kill()
                                child.wait()
                        os.close(master)
