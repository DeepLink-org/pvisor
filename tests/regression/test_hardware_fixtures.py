"""Separate interpreters verify assertions survive optimized Python."""

import os
import subprocess
import sys
import unittest


class HardwareFixtureTests(unittest.TestCase):
    def test_optimized_python_cannot_disable_probe_assertions(self):
        code = (
            "import subprocess, unittest; "
            "from tests.hardware.support import assert_response; "
            "assert_response(unittest.TestCase(), subprocess.CompletedProcess([],0,'',''), 'schema')"
        )
        result = subprocess.run([sys.executable, "-O", "-c", code], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("AssertionError", result.stderr)

    @unittest.skipUnless(os.name == "posix", "guest filesystem module uses POSIX interfaces")
    def test_guest_filesystem_errno_oracle_survives_optimized_python(self):
        code = (
            "import errno; from tests.regression.test_vm_filesystem import VmFilesystemTests; "
            "VmFilesystemTests().assertErrno(errno.ENOENT, "
            "lambda: (_ for _ in ()).throw(PermissionError(errno.EACCES, 'denied')))"
        )
        result = subprocess.run([sys.executable, "-O", "-c", code], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("AssertionError", result.stderr)
