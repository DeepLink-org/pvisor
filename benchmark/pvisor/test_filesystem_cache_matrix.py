"""A condition label must not conceal fallback or instrumented timing."""

import os
import unittest
from unittest.mock import patch

from filesystem_cache_matrix import (
    CONDITIONS,
    EXPECTED_INPUTS,
    condition_environment,
    trial_environment,
    validate_qualification,
)


class CacheMatrixTests(unittest.TestCase):
    def test_rejects_missing_arm_and_instrumented_timing(self):
        config = {"conditions": {name: {"env": {}} for name in CONDITIONS}}
        config["conditions"].pop("both")
        with self.assertRaisesRegex(ValueError, "all four"):
            condition_environment(config, "baseline")
        config["conditions"]["both"] = {"env": {"PVISOR_FS_PROFILE": "1"}}
        with self.assertRaisesRegex(ValueError, "fixed control"):
            condition_environment(config, "both")

    def test_actual_mutable_fallback_or_dax_fails_qualification(self):
        observed = dict(
            EXPECTED_INPUTS["both"],
            dax=False,
            evidence={"receipt": "stage/owned-inputs-receipt.json"},
        )
        validate_qualification(observed, "both")
        for fault in ("rootfs_immutable", "workspace_immutable", "dax", "evidence"):
            with self.subTest(fault=fault):
                changed = observed.copy()
                changed[fault] = True if fault == "dax" else False
                with self.assertRaises(ValueError):
                    validate_qualification(changed, "both")
        # Integer flags are not an observed boolean qualification.
        changed = observed | {"rootfs_immutable": 1}
        with self.assertRaises(ValueError):
            validate_qualification(changed, "both")

    def test_failed_trial_restores_interactive_environment(self):
        original = {
            "PATH": "/test",
            "PVISOR_VM_FS_WRITEBACK": "1",
            "PVISOR_FS_PROFILE": "1",
            "PVISOR_FS_OWNED_INPUTS_CONDITION": "foreign",
        }
        with patch.dict(os.environ, original, clear=True):
            with self.assertRaisesRegex(RuntimeError, "failed trial"):
                with trial_environment({"PVISOR_VM_FS_WRITEBACK": "0"}):
                    self.assertNotIn("PVISOR_FS_PROFILE", os.environ)
                    self.assertNotIn("PVISOR_FS_OWNED_INPUTS_CONDITION", os.environ)
                    self.assertEqual(os.environ["PVISOR_VM_FS_WRITEBACK"], "0")
                    raise RuntimeError("failed trial")
            self.assertEqual(dict(os.environ), original)
            with self.assertRaises(TypeError):
                with trial_environment({"PVISOR_VM_FS_WRITEBACK": None}):
                    self.fail("invalid child environment must not run")
            self.assertEqual(dict(os.environ), original)


if __name__ == "__main__":
    unittest.main()
