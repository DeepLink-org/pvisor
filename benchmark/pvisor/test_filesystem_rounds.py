"""A good first round cannot conceal a later cache or work-count failure."""

import copy
import unittest

from filesystem_rounds import validate_rounds
from filesystem_rounds_workload import OPERATIONS


def result():
    policy = dict(
        dont_write_bytecode=True, prefix="/__pvisor_reference_no_pyc__", prefix_exists=False
    )
    cache = dict(
        TMPDIR="/tmp/.data/pvisor-reference-testing",
        HOME="/tmp/.data/pvisor-reference-testing/reference-home",
        CARGO_HOME="/tmp/.data/pvisor-reference-testing/reference-cargo",
        NODE_COMPILE_CACHE="/tmp/.data/pvisor-reference-testing/node-compile-cache",
        NODE_DISABLE_COMPILE_CACHE=None,
        NODE_OPTIONS=None,
    )
    operations = {}
    for operation in OPERATIONS:
        values = []
        for number in (1, 2, 3):
            check = dict(opens=10000, writes=8192, bytes=8 * 1024**2, fsyncs=1)
            values.append(
                dict(
                    workload=operation,
                    round=number,
                    worker_ms=1.0,
                    check=check,
                    python_cache=policy.copy(),
                    tool_cache=cache.copy(),
                    tool_scratch="executor",
                )
            )
        operations[operation] = dict(values[0], rounds=values)
    return dict(
        filesystem=operations,
        workspace="/work",
        tool_scratch="executor",
        tool_cache=cache,
        python_cache=policy,
    )


class ResidentRoundTests(unittest.TestCase):
    def test_accepts_complete_three_round_result(self):
        validate_rounds(result())

    def test_rejects_late_failures_even_when_first_result_is_good(self):
        for fault in ("missing", "duplicate", "cache", "python", "work_count"):
            with self.subTest(fault=fault):
                value = copy.deepcopy(result())
                later = value["filesystem"]["stream_write"]["rounds"]
                if fault == "missing":
                    later.pop()
                elif fault == "duplicate":
                    later[2]["round"] = 2
                elif fault == "cache":
                    later[1]["tool_cache"]["HOME"] = "/foreign-cache"
                elif fault == "python":
                    later[2]["python_cache"]["dont_write_bytecode"] = False
                else:
                    later[2]["check"]["bytes"] = 1024
                with self.assertRaises(ValueError):
                    validate_rounds(value)


if __name__ == "__main__":
    unittest.main()
