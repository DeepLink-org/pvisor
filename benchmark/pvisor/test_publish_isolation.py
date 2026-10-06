"""Reject unsupported isolation claims before publishing a public matrix."""

import pytest

from publish_isolation import (
    BACKENDS, HOST_FIELDS, NEGATIVES, POSITIVES, validate_observations,
)


def observations(backend):
    exposed = backend in ('native', 'host')
    value = {key: True for key in POSITIVES}
    value.update({key: exposed for key in NEGATIVES})
    value.update({'lower-alias-write': backend != 'podman',
                  'host-outside-mutated': exposed,
                  'host-lower-mutated': exposed or backend == 'container',
                  'workspace-staged': backend in ('staged', 'safe', 'vm'),
                  'cpu_affinity': [0, 1]})
    return value


@pytest.mark.parametrize('backend', BACKENDS)
def test_pinned_boundary_observations_are_accepted(backend):
    validate_observations(backend, observations(backend), [0, 1])


@pytest.mark.parametrize('behavior', POSITIVES)
def test_denial_without_functional_positive_control_is_rejected(behavior):
    value = observations('vm')
    value[behavior] = False
    with pytest.raises(ValueError, match='positive control'):
        validate_observations('vm', value, [0, 1])


@pytest.mark.parametrize('behavior', NEGATIVES)
def test_any_outside_access_in_staged_is_rejected(behavior):
    value = observations('staged')
    value[behavior] = True
    with pytest.raises(ValueError, match='outside-view'):
        validate_observations('staged', value, [0, 1])


def test_native_control_that_cannot_read_secret_is_rejected():
    value = observations('native')
    value['absolute-read'] = False
    with pytest.raises(ValueError, match='control failed'):
        validate_observations('native', value, [0, 1])


@pytest.mark.parametrize('behavior', HOST_FIELDS)
def test_staging_claim_that_conflicts_with_host_state_is_rejected(behavior):
    value = observations('safe')
    value[behavior] = not value[behavior]
    with pytest.raises(ValueError, match='mutation|staging'):
        validate_observations('safe', value, [0, 1])


@pytest.mark.parametrize('replacement', [None, 1, 'false'])
def test_missing_or_coerced_observations_are_rejected(replacement):
    value = observations('vm')
    value['absolute-read'] = replacement
    with pytest.raises(ValueError, match='non-boolean'):
        validate_observations('vm', value, [0, 1])


def test_payload_affinity_mismatch_is_rejected():
    value = observations('container')
    value['cpu_affinity'] = [0, 1, 2]
    with pytest.raises(ValueError, match='affinity'):
        validate_observations('container', value, [0, 1])
