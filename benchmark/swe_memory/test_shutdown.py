import json
import unittest

from audit_shutdown import REQUIRED, phases


class AuditTest(unittest.TestCase):
    def fixture(self):
        preferred = [
            "runtime.teardown",
            "seal.journal_sync",
            "seal.upper_sync",
            "seal.parent_sync",
            "seal.publish",
            "runtime.overlay_stage",
            "driver.log_drain",
            "driver.summary_total",
        ]
        order = preferred + sorted(REQUIRED - set(preferred))
        out = []
        for i, name in enumerate(order):
            for edge, t in [("begin", i * 10 + 1), ("end", i * 10 + 2)]:
                out.append(
                    "PVISOR_SHUTDOWN "
                    + json.dumps(
                        dict(
                            pid=2,
                            phase=name,
                            edge=edge,
                            unix_ns=t,
                            elapsed_ns=1,
                            success=edge == "end",
                        )
                    )
                )
        return out

    def test_complete(self):
        self.assertEqual(set(phases(self.fixture())), REQUIRED)

    def test_missing(self):
        with self.assertRaises(AssertionError):
            phases(self.fixture()[:-1])

    def test_failed(self):
        x = self.fixture()
        x[1] = x[1].replace("true", "false")
        with self.assertRaises(AssertionError):
            phases(x)

    def test_duplicate(self):
        x = self.fixture()
        with self.assertRaises(AssertionError):
            phases(x + x[:2])

    def test_reordered(self):
        x = self.fixture()
        x[2:6] = x[4:6] + x[2:4]
        # timestamps unchanged, but create an actual reversed temporal dependency
        e = json.loads(x[2].split(" ", 1)[1])
        e["unix_ns"] = 1000
        x[2] = "PVISOR_SHUTDOWN " + json.dumps(e)
        with self.assertRaises(AssertionError):
            phases(x)


if __name__ == "__main__":
    unittest.main()
