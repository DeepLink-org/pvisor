"""Conventional benchmark tests; no semspec approvals or crate changes."""
import json
from collections import Counter
from pathlib import Path
import pytest
import kernel_cache_runner as bench


def test_shape_and_complete_plan():
    paths = list(bench.fixture_paths())
    assert len(paths) == len(set(paths)) == 2048
    assert len({p.parts[0] for p in paths}) == 32
    assert sum(len(p.parts) > 2 for p in paths) == 1024
    wanted = {(c, w) for c in bench.CONDITIONS for w in bench.WORKLOADS}
    plan = bench.order(4207, 33)
    assert plan == bench.order(4207, 33)
    assert plan != bench.order(4208, 33)
    assert all(len(cells) == len(wanted) and set(cells) == wanted for cells in plan)
    assert bench.COMPARISONS == (('legacy-writable', 'metadata-writable'),
                                ('metadata-readonly', 'metadata-and-data-readonly'))


def test_paired_bootstrap():
    result = bench.paired_ci([100] * 30, [50] * 30)
    assert result['percent_change'] == -50
    assert result['ci95_percent'] == [-50, -50]


def records(stage, components, final=True):
    stage.mkdir()
    data = [dict(pid=123, component=c, instance=i, final_record=final, measurements={})
            for i, c in enumerate(components)]
    (stage / 'stderr.log').write_text('\n'.join('pvisor-fs-profile ' + json.dumps(r) for r in data))
    return data


@pytest.mark.parametrize('components,final', [([], True), (['host-fuse'], True),
    (['host-fuse', 'overlay-core'], False),
    (['host-fuse', 'overlay-core', 'notifier'], True),
    (['host-fuse', 'overlay-core', 'overlay-core'], True)])
def test_reject_incomplete_or_unexpected_profiles(tmp_path, components, final):
    stage = tmp_path / 'metadata-writable'
    records(stage, components, final)
    with pytest.raises(ValueError, match='incomplete profile coverage'):
        bench.collect_profiles(tmp_path, [(stage, 'metadata-writable')])


def test_all_actual_instances_final_and_no_duplicate_final(tmp_path):
    stage = tmp_path / 'metadata-writable'
    data = records(stage, ['host-fuse', 'overlay-core'])
    parsed = bench.collect_profiles(tmp_path, [(stage, 'metadata-writable')])
    assert parsed['metadata-writable']['records'] == data
    with (stage / 'stderr.log').open('a') as stream:
        stream.write('\npvisor-fs-profile ' + json.dumps(data[0]))
    with pytest.raises(ValueError):
        bench.collect_profiles(tmp_path, [(stage, 'metadata-writable')])


def test_profiles_check_missing_files(tmp_path):
    with pytest.raises(FileNotFoundError):
        bench.collect_profiles(tmp_path, [(tmp_path / 'missing', 'metadata-readonly')])


def test_inventory_is_exact(tmp_path):
    p = tmp_path / 'file'
    p.write_bytes(b'abc')
    before = bench.inventory(tmp_path)
    p.write_bytes(b'def')
    assert before != bench.inventory(tmp_path)


def test_driver_admission_and_read_validation():
    driver = (bench.HERE / 'kernel_cache_driver.rs').read_text()
    assert 'config.validate_kernel_cache()?' in driver
    assert 'config.lower_mutability = vec![LayerMutability::Immutable]' in driver
    assert 'fixed_metadata_and_aliases: true' in driver
    assert 'ReadObservationSemantics::StableView' in driver
    assert 'Duration::from_secs(60)' in driver
    assert 'e.raw_os_error() == Some(30)' in driver
    assert 'bytes == expected(p)' in driver
    assert 'set_len(7)' in driver
    assert 'config.allow_other = false' in driver
    assert 'config.allow_root = false' in driver


def test_mountinfo_detection_does_not_stat_disconnected_fuse(monkeypatch, tmp_path):
    mount = tmp_path / 'mnt'
    monkeypatch.setattr(Path, 'read_text', lambda self: f'29 57 0:81 / {mount} rw - fuse test rw\n')
    monkeypatch.setattr(Path, 'resolve', lambda self: pytest.fail('must not resolve disconnected mount'))
    assert bench.mounted(mount)
    assert not bench.mounted(tmp_path / 'other')


def test_source_derived_profile_contract():
    contract = bench.derive_profile_contract(bench.ROOT)
    assert Counter(contract['expected_components']) == Counter({'overlay-core': 1, 'host-fuse': 1})
    assert contract['sources']['crates/pvisor-overlayfs/src/cache.rs']['constructors'] == []


def test_counter_export_only_final_instances(tmp_path):
    instance = dict(pid=1, component='host-fuse', instance=2, final_record=True,
                    measurements={'lookup': dict(calls=5, units=0),
                                  'reclaim_inode': dict(calls=3, units=0)})
    report = dict(build=dict(binary_sha256='abc'), profiles={'case-000-hot-metadata-writable':
                  dict(instances=[instance])})
    bench.export_counters(tmp_path, report)
    import csv
    rows = list(csv.DictReader((tmp_path / 'counters.csv').open()))
    assert len(rows) == 2
    assert rows[0]['calls'] == '5' and rows[0]['fuse_callback'] == 'True'
    assert rows[1]['fuse_callback'] == 'False'


def test_noatime_backing_requires_tmpfs_and_no_shared_propagation(monkeypatch, tmp_path):
    p = tmp_path
    monkeypatch.setattr(Path, 'read_text', lambda self: f'22 1 0:9 / {p} rw,noatime - tmpfs tmpfs rw\n')
    assert bench.prove_backing(p)['noatime']
    monkeypatch.setattr(Path, 'read_text', lambda self: f'22 1 0:9 / {p} rw,relatime - tmpfs tmpfs rw\n')
    with pytest.raises(AssertionError): bench.prove_backing(p)
    monkeypatch.setattr(Path, 'read_text', lambda self: f'22 1 0:9 / {p} rw,noatime shared:3 - tmpfs tmpfs rw\n')
    with pytest.raises(AssertionError): bench.prove_backing(p)


def test_server_supervision_rejects_unexpected_exit(monkeypatch):
    from types import SimpleNamespace
    worker = SimpleNamespace(stage='owned', proc=SimpleNamespace(poll=lambda: 70))
    monkeypatch.setattr(bench, 'ACTIVE_WORKERS', [worker])
    with pytest.raises(RuntimeError, match='stop all users'): bench.supervise_workers()
    worker.closing = True
    bench.supervise_workers()


def test_namespace_contract_is_enforced_in_harness():
    source = Path(bench.__file__).read_text()
    assert "'--kill-child=KILL'" in source and "'--pid'" in source
    assert "os.getpid() == 1" in source
    assert 'surviving_users' in source
    assert 'probe_atime_before' in source
    assert '86_400_000_000_000' not in source


def test_no_changes_to_prior_experiment_receipts():
    source = Path(bench.__file__).read_text()
    assert 'immutable-lower-cache-driver' not in source
    assert 'test_immutable_lower_cache.py' not in source
    assert 'immutable_lower_cache_driver.rs' not in source
