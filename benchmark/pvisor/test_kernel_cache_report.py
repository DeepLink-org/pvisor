"""Tests for derived kernel-cache publication, separate from measurements."""
import copy
import json
from pathlib import Path
import pytest
import kernel_cache_report as report
import kernel_cache_runner as bench


def synthetic_profile():
    profiles = {}
    for i in range(3):
        for workload in (*bench.WORKLOADS, 'prime-only'):
            for condition in bench.CONDITIONS:
                prime = workload == 'prime-only'
                measurements = {k: dict(calls=10 if prime else 30, units=0)
                                for k in report.READ_METRICS}
                profiles[f'case-{i:03}-{workload}-{condition}'] = dict(instances=[] if condition == 'native' else [
                    dict(component='host-fuse', measurements=measurements),
                    dict(component='overlay-core', measurements={})])
    return dict(profiles=profiles, arguments=dict(samples=3), build=dict(binary_sha256='abc'))


def test_warm_delta_uses_independent_prime_once():
    rows = report.counter_rows(synthetic_profile(), 'diagnostic-only')
    warm = [r for r in rows if r['workload'] == 'hot' and r['condition'] == 'metadata-writable'
            and r['scope'] == 'warm_operation_minus_independent_prime']
    lookup = next(r for r in warm if r['metric'] == 'lookup')
    assert lookup['median'] == lookup['minimum'] == lookup['maximum'] == 20
    assert next(r for r in warm if r['metric'] == 'four_callback_subtotal')['median'] == 80
    assert not any(r['scope'] == 'warm_operation_minus_independent_prime' and r['workload'] == 'tools'
                   for r in rows)


def test_profile_publication_rejects_missing_case():
    p = synthetic_profile()
    del p['profiles']['case-001-hot-metadata-writable']
    with pytest.raises(KeyError):
        report.counter_rows(p, 'diagnostic')


def test_failed_batch_not_publishable(tmp_path):
    p = tmp_path / 'report.json'
    p.write_text(json.dumps(dict(state='failed', failures=['real error'])))
    with pytest.raises(AssertionError):
        report.verified_report(p, False)


def test_noatime_publication_rejects_wrong_backing_or_shared_view():
    p = dict(mountinfo='22 1 0:9 / /owned rw,noatime - tmpfs tmpfs rw',
             filesystem='tmpfs', noatime=True, device='0:9', stat_dev=9)
    report.validate_noatime_proof(p, '0:9', 9)
    for line in ('22 1 0:9 / /owned rw,relatime - tmpfs tmpfs rw',
                 '22 1 0:9 / /owned rw,noatime - btrfs btrfs rw',
                 '22 1 0:9 / /owned rw,noatime shared:4 - tmpfs tmpfs rw'):
        q = dict(p, mountinfo=line)
        with pytest.raises(AssertionError): report.validate_noatime_proof(q, '0:9', 9)
    with pytest.raises(AssertionError): report.validate_noatime_proof(p, '0:10', 9)


def test_old_passed_btrfs_batch_is_unaccepted(tmp_path):
    p = tmp_path / 'report.json'
    p.write_text(json.dumps(dict(state='passed', failures=[], benchmark='B-FS-ENG')))
    with pytest.raises(AssertionError, match='unaccepted historical'): report.verified_report(p, False)


def test_failed_noatime_batch_rejected_before_statistics_or_hash_checks(tmp_path, monkeypatch):
    path = tmp_path / 'report.json'
    path.write_text(json.dumps(dict(acceptance='noatime-tmpfs-v1', state='failed', failures=['interference'])))
    monkeypatch.setattr(report, 'verify_noatime_containment', lambda *args: pytest.fail('must reject failure first'))
    with pytest.raises(AssertionError): report.verified_report(path, False)


def test_blocked_summary_never_manufactures_zero_latency_or_old_percentages():
    rows = report.blocked_timing_rows(dict(version='final-P1', source_inventory_sha256='source',
                                         binary_sha256='binary'), ['failed-a', 'failed-b'])
    assert len(rows) == 25
    assert len({(r['condition'], r['workload']) for r in rows}) == 25
    assert all(r['accepted_samples'] == 0 and r['planned_samples'] == 30 for r in rows)
    assert all(r['state'] == 'blocked-no-accepted-final-timing' for r in rows)
    assert all(r[k] == '' for r in rows for k in
               ('p50', 'p95_reference', 'percent_change', 'ci95_percent_low', 'ci95_percent_high'))
    assert all(r['source_version'] == 'final-P1' for r in rows)


def test_profile_not_formal_timing(tmp_path):
    p = tmp_path / 'report.json'
    p.write_text(json.dumps(dict(state='passed', failures=[], benchmark='B-FS-DIAG')))
    with pytest.raises(AssertionError):
        report.verified_report(p, False)
