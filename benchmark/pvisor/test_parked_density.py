import copy
import unittest

from parked_density import verify_result


def fixture():
    ready = dict(
        kind="random",
        seed=20261006,
        bytes=64 * 1024**2,
        token="same-saved-execution",
        pid=3,
        checksum="full-private-data",
    )
    result = ready | dict(integrity="passed", changes=4, first_scan_ms=1)
    return ready, result


class ParkedDensityTests(unittest.TestCase):
    def test_recovered_capacity_requires_original_execution_and_useful_work(self):
        ready, result = fixture()
        verify_result(ready, result, "random", 20261006)
        for key, value in (
            ("token", "fresh-execution"),
            ("pid", 4),
            ("checksum", "corrupted"),
            ("bytes", 0),
            ("changes", 0),
            ("integrity", "failed"),
        ):
            broken = copy.deepcopy(result)
            broken[key] = value
            with self.assertRaises(ValueError):
                verify_result(ready, broken, "random", 20261006)

    def test_matching_empty_memory_or_wrong_condition_cannot_count_as_recovery(self):
        ready, result = fixture()
        ready["bytes"] = result["bytes"] = 0
        with self.assertRaises(ValueError):
            verify_result(ready, result, "random", 20261006)
        ready, result = fixture()
        with self.assertRaises(ValueError):
            verify_result(ready, result, "repeated", 20261006)
        with self.assertRaises(ValueError):
            verify_result(ready, result, "random", 1)
