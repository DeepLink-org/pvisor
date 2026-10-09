"""Reject unsupported isolation claims before publishing a public matrix."""

import unittest

from publish_isolation import (
    BACKENDS,
    HOST_FIELDS,
    NEGATIVES,
    POSITIVES,
    validate_observations,
)


def observations(backend):
    exposed = backend in ("native", "host")
    value = {key: True for key in POSITIVES}
    value.update({key: exposed for key in NEGATIVES})
    value.update(
        {
            "lower-alias-write": backend != "podman",
            "host-outside-mutated": exposed,
            "host-lower-mutated": exposed or backend == "container",
            "workspace-staged": backend in ("staged", "safe", "vm"),
            "cpu_affinity": [0, 1],
        }
    )
    return value


class PublishIsolationTests(unittest.TestCase):
    def test_pinned_boundary_observations_are_accepted(self):
        for backend in BACKENDS:
            with self.subTest(backend=backend):
                validate_observations(backend, observations(backend), [0, 1])

    def test_denial_without_functional_positive_control_is_rejected(self):
        for behavior in POSITIVES:
            with self.subTest(behavior=behavior):
                value = observations("vm")
                value[behavior] = False
                with self.assertRaisesRegex(ValueError, "positive control"):
                    validate_observations("vm", value, [0, 1])

    def test_any_outside_access_in_staged_is_rejected(self):
        for behavior in NEGATIVES:
            with self.subTest(behavior=behavior):
                value = observations("staged")
                value[behavior] = True
                with self.assertRaisesRegex(ValueError, "outside-view"):
                    validate_observations("staged", value, [0, 1])

    def test_native_control_that_cannot_read_secret_is_rejected(self):
        value = observations("native")
        value["absolute-read"] = False
        with self.assertRaisesRegex(ValueError, "control failed"):
            validate_observations("native", value, [0, 1])

    def test_staging_claim_that_conflicts_with_host_state_is_rejected(self):
        for behavior in HOST_FIELDS:
            with self.subTest(behavior=behavior):
                value = observations("safe")
                value[behavior] = not value[behavior]
                with self.assertRaisesRegex(ValueError, "mutation|staging"):
                    validate_observations("safe", value, [0, 1])

    def test_missing_or_coerced_observations_are_rejected(self):
        for replacement in [None, 1, "false"]:
            with self.subTest(replacement=replacement):
                value = observations("vm")
                value["absolute-read"] = replacement
                with self.assertRaisesRegex(ValueError, "non-boolean"):
                    validate_observations("vm", value, [0, 1])

    def test_payload_affinity_mismatch_is_rejected(self):
        value = observations("container")
        value["cpu_affinity"] = [0, 1, 2]
        with self.assertRaisesRegex(ValueError, "affinity"):
            validate_observations("container", value, [0, 1])
