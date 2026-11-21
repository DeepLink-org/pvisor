"""Exercise the VM driver's real timeout/reaping path without booting a VM."""

import sys
import unittest

from tests import vm_api_contract as contracts
from tests.fixtures import workspace


class VmApiContractTests(unittest.TestCase):
    def test_timeout_fails_and_reaps_child(self):
        with workspace() as (directory, _):
            with self.assertRaisesRegex(RuntimeError, "deadline exceeded"):
                contracts.invoke(
                    [sys.executable, "-c", "import time; time.sleep(60)"], directory, 0.1
                )
