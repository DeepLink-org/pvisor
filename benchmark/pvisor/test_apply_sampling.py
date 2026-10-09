import unittest
from collections import Counter
from contextlib import ExitStack
from types import SimpleNamespace
from unittest import mock

from v1 import apply, baselines


class ApplySamplingTests(unittest.TestCase):
    def test_requested_apply_rounds_are_not_silently_reduced_at_large_sizes(self):
        with ExitStack() as resources:
            calls = []
            ctx = SimpleNamespace(
                args=SimpleNamespace(
                    apply_sizes="10,1000,100000", samples=30, warmups=3, seed=20261006
                ),
                metadata={},
                save=lambda: None,
            )
            resources.enter_context(
                mock.patch.object(
                    apply,
                    "staged_trial",
                    lambda ctx, count, action, trial: calls.append((count, action, trial)),
                )
            )
            resources.enter_context(
                mock.patch.object(
                    apply,
                    "copy_trial",
                    lambda ctx, count, trial: calls.append((count, "copy", trial)),
                )
            )
            resources.enter_context(
                mock.patch.object(
                    baselines,
                    "git_trial",
                    lambda ctx, count, trial: calls.append((count, "git-apply", trial)),
                )
            )
            resources.enter_context(mock.patch.object(apply, "crashes", lambda ctx: None))
            apply.run(ctx, include_git=True)
            expected = {
                (count, action, trial)
                for count in (10, 1000, 100000)
                for action in ("apply", "drop", "conflict", "copy", "git-apply")
                for trial in range(-3, 30)
            }
            assert set(calls) == expected
            assert all(value == 1 for value in Counter(calls).values())
            assert ctx.metadata["apply_protocol"]["samples_per_size"] == {
                10: 30,
                1000: 30,
                100000: 30,
            }
            assert ctx.metadata["apply_protocol"]["warmups_per_size"] == {10: 3, 1000: 3, 100000: 3}
