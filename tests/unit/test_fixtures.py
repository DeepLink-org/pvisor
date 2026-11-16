"""Keep temporary workspaces and scoped mocks isolated after a failed test."""

import os
import unittest
from unittest import mock

from tests.fixtures import workspace


class FixtureTests(unittest.TestCase):
    def test_failure_restores_mocks_before_removing_the_workspace(self):
        variable = "PVISOR_TEST_FIXTURE"
        original = os.environ.get(variable)
        cleanup = []
        with self.assertRaisesRegex(RuntimeError, "fixture failure"):
            with workspace() as (directory, resources):
                assert directory.is_absolute() and directory.is_dir()
                resources.callback(lambda: cleanup.append(directory.is_dir()))
                resources.enter_context(mock.patch.dict(os.environ, {variable: "isolated"}))
                assert os.environ[variable] == "isolated"
                raise RuntimeError("fixture failure")
        assert os.environ.get(variable) == original
        assert cleanup == [True]
        assert not directory.exists()
