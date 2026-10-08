import json

import pytest

from publish_review_workflow import publish
from review_workflow import BACKENDS, Runner, validate_content_review


def test_correctness_rejects_unselected_overwrite_and_partial_conflict(tmp_path):
    files = tmp_path / 'files'
    files.mkdir()
    runner = object.__new__(Runner)
    for i in range(20):
        (files / f'f{i:06d}').write_bytes(runner.original(i))
    runner.check_original(tmp_path, 20)
    (files / 'f000000').write_text('concurrent-host-edit\n')
    runner.check_original(tmp_path, 20, conflict=True)
    (files / 'f000010').write_text('new-10\n')
    with pytest.raises(AssertionError, match='wrong original content at 10'):
        runner.check_original(tmp_path, 20, conflict=True)


@pytest.mark.parametrize('invalid', ['missing', 'duplicate', 'failed', 'incorrect'])
def test_publication_rejects_invalid_cohort_before_writing(tmp_path, invalid):
    rows = [dict(files=100, case='normal', backend=backend, trial=i, correctness='passed')
            for backend in BACKENDS for i in range(30)]
    failures = []
    if invalid == 'missing':
        rows.pop()
    elif invalid == 'duplicate':
        rows.append(rows[0])
    elif invalid == 'failed':
        failures.append(dict(error='timeout'))
    else:
        rows[0]['correctness'] = 'failed'
    report = dict(benchmark_id='B-WORKFLOW', arguments=dict(samples=30, sizes='100', cases='normal'),
                  rows=rows, failures=failures)
    raw = tmp_path / 'report.json'
    raw.write_text(json.dumps(report))
    public = tmp_path / 'public'
    with pytest.raises(ValueError):
        publish(raw, public)
    assert not public.exists()


def test_review_requires_complete_diff_not_only_path_and_prefix():
    text = ''.join(f'f{i:06d}\n-' + Runner.original(i).decode().replace('\n', '\n-')
                   + f'\n\\ No newline at end of file\n+new-{i}\n' for i in range(20))
    validate_content_review(text)
    with pytest.raises(AssertionError, match='complete content diff'):
        validate_content_review(text.replace('x' * 4000, 'x' * 10))
