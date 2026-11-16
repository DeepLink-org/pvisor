"""Native Windows ConPTY regression; no WHP or guest is needed."""

import os
import sys
import unittest


class WindowsContainerContractTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "native ConPTY requires Windows")
    def test_conpty_attaches_console_handles_and_preserves_child_exit(self):
        from tests.windows_pty import Pty

        probe = (
            "import sys; "
            "assert sys.stdin.isatty() and sys.stdout.isatty() and sys.stderr.isatty(); "
            "print('CONSOLE-READY',flush=True); "
            "line=sys.stdin.readline().strip(); "
            "assert line=='literal input',repr(line); "
            "print('CONSOLE-DONE',flush=True); sys.exit(7)"
        )
        with Pty([sys.executable, "-c", probe], dict(os.environ)) as console:
            console.expect(b"CONSOLE-READY", timeout=15)
            console.send(b"literal input\r")
            console.expect(b"CONSOLE-DONE", timeout=15)
            self.assertEqual(console.wait(timeout=10), 7)
