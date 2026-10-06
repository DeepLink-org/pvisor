"""Density evidence rejects incomplete useful work and wrong memory fixtures."""
import pytest

from density import verify_result


def result(useful=True):
    ready = dict(token='unique-live-process', bytes=32*1024*1024 if useful else 0, checksum='full-payload')
    done = ready | dict(changes=4 if useful else 0, integrity='passed')
    return ready, done


@pytest.mark.parametrize('useful', [True, False])
def test_complete_matching_density_task_is_valid(useful):
    verify_result(*result(useful), useful)


@pytest.mark.parametrize('field,value', [('token','other-process'),('checksum','changed'),('changes',3),('integrity','failed')])
def test_partial_or_wrong_task_cannot_count_as_density_completion(field, value):
    ready, done = result()
    done[field] = value
    with pytest.raises(ValueError):verify_result(ready, done, True)


def test_matching_empty_checksums_cannot_replace_useful_private_memory():
    ready, done = result()
    ready['bytes'] = done['bytes'] = 0
    with pytest.raises(ValueError, match='requested private payload'):
        verify_result(ready, done, True)
