#!/usr/bin/env python3
"""Derived tables for kernel_cache_runner (B-FS-ENG/B-FS-DIAG), not a measurement entry."""
import argparse
import csv
import json
from pathlib import Path
import shutil
import statistics

import kernel_cache_runner as bench

READ_METRICS = ('lookup', 'getattr', 'open', 'read', 'release')
CORE_METRICS = ('layer_parent_stats', 'layer_leaf_stats', 'immutable_lower_cache_hits',
                'immutable_lower_cache_misses', 'immutable_lower_cache_evictions')


def verify_noatime_containment(directory, report):
    proof = json.loads((directory / 'noatime-proof.json').read_text())
    containment = json.loads((directory / 'containment-receipt.json').read_text())
    assert proof == report['backing']
    assert containment['state'] == 'passed' and containment['returncode'] == 0
    assert not containment['host_mounts'] and not containment['surviving_users']
    assert containment['host_checks'] and all(not r['suspects'] for r in containment['host_checks'])
    assert proof['pid'] == 1 and proof['probe_atime_before'] == proof['probe_atime_after']
    assert set(proof['probe_atime_before'].values()) == {1_000_000_000}
    assert all(proof['namespace'][k] != containment['outer_namespace'][k] for k in ('user', 'mnt', 'pid'))
    assert report['backing_detached']
    assert bench.sha(directory / 'backing.tar') == report['backing_archive_sha256']
    for p in [proof, report['backing_after'], *(p for group in report['backing_proofs'].values() for p in group)]:
        validate_noatime_proof(p, proof['device'], proof['stat_dev'])
    for stage, group in report['backing_proofs'].items():
        assert json.loads((directory / stage / 'backing.json').read_text()) == group
    assert set(report['backing_proofs']) == {c for c in bench.CONDITIONS} | {
        f'case-{i:03}-{w}-{c}' for i in range(report['arguments']['samples'] + report['arguments']['warmups'])
        for c in bench.CONDITIONS
        for w in (bench.WORKLOADS + ('prime-only',) if report['arguments']['profiles'] else ('whole-tools',))}


def validate_noatime_proof(proof, device, stat_dev):
    fields = proof['mountinfo'].split(); separator = fields.index('-')
    assert fields[separator + 1] == proof['filesystem'] == 'tmpfs'
    assert 'noatime' in fields[5].split(',') and proof['noatime'] is True
    assert fields[2] == proof['device'] == device and proof['stat_dev'] == stat_dev
    assert not any(f.startswith(('shared:', 'master:')) for f in fields[6:separator])


def verified_report(path, diagnostic):
    report = json.loads(path.read_text())
    assert report.get('acceptance') == 'noatime-tmpfs-v1', 'old Btrfs cohorts are unaccepted historical diagnostics'
    assert report['state'] == 'passed' and not report['failures']
    verify_noatime_containment(path.parent, report)
    assert report['benchmark'] == ('B-FS-DIAG' if diagnostic else 'B-FS-ENG')
    assert report['arguments']['profiles'] is diagnostic
    directory = Path(report['arguments']['build_receipt']).parent
    assert report['build'] == bench.verify_build(directory)
    assert bench.sha(path.parent / 'driver') == report['build']['binary_sha256']
    before = json.loads((path.parent / 'input-before.json').read_text())
    after = json.loads((path.parent / 'input-after.json').read_text())
    assert before == after and bench.sha(path.parent / 'input-before.json') == report['input_sha256']
    assert report['mount_cleanup'] and all(report['mount_cleanup'].values())
    workloads = bench.WORKLOADS + ('prime-only',) if diagnostic else bench.WORKLOADS
    args = report['arguments']
    wanted = {(r, c, w) for r in range(-args['warmups'], args['samples'])
              for c in bench.CONDITIONS for w in workloads}
    actual = [(r['round'], r['condition'], r['workload']) for r in report['rows']]
    assert len(actual) == len(wanted) and set(actual) == wanted
    assert all(r['correctness'] == 'passed' and r['warmup'] == (r['round'] < 0)
               for r in report['rows'])
    assert set(report['lifecycles']) == set(bench.CONDITIONS)
    assert all(v['correctness']['correctness'] == 'passed' and v['stop']['event'] == 'stopped'
               for v in report['lifecycles'].values())
    assert all(not c['suspects'] for c in report['interference_checks'])
    if diagnostic:
        stages = [(path.parent / stage, next(c for c in bench.CONDITIONS
                   if stage == c or stage.endswith('-' + c))) for stage in report['profiles']]
        assert bench.collect_profiles(path.parent, stages) == report['profiles']
    else:
        assert args['samples'] == 30 and args['warmups'] == 3
        for summary in report['summary']:
            values = [r['operation_ms'] for r in report['rows'] if not r['warmup']
                      and r['condition'] == summary['condition'] and r['workload'] == summary['workload']]
            assert summary == dict(condition=summary['condition'], workload=summary['workload'],
                                   **bench.distribution(values))
        for workload in bench.WORKLOADS:
            for baseline, candidate in bench.COMPARISONS:
                vals = [[r['operation_ms'] for r in report['rows'] if not r['warmup']
                         and r['condition'] == c and r['workload'] == workload]
                        for c in (baseline, candidate)]
                assert report['cache_comparison'][workload][baseline + ':' + candidate] == bench.paired_ci(*vals, args['seed'])
    return report


def write_csv(path, rows):
    with path.open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), lineterminator='\n')
        writer.writeheader(); writer.writerows(rows)


def case_metrics(report, index, workload, condition):
    stage = f'case-{index:03}-{workload}-{condition}'
    instances = report['profiles'][stage]['instances']
    if condition == 'native':
        assert not instances
        return {k: 0 for k in (*READ_METRICS, *CORE_METRICS, 'four_callback_subtotal')}
    fuse = next(r['measurements'] for r in instances if r['component'] == 'host-fuse')
    core = next(r['measurements'] for r in instances if r['component'] == 'overlay-core')
    result = {k: fuse.get(k, {}).get('calls', 0) for k in READ_METRICS}
    result.update({k: core.get(k, {}).get('units', 0) for k in CORE_METRICS})
    result['four_callback_subtotal'] = sum(result[k] for k in ('lookup', 'getattr', 'open', 'read'))
    return result


def counter_rows(report, batch):
    rows = []
    samples = report['arguments']['samples']
    for workload in (*bench.WORKLOADS, 'prime-only'):
        for condition in bench.CONDITIONS:
            cases = [case_metrics(report, i, workload, condition) for i in range(samples)]
            scopes = [('fresh_mount_lifecycle', cases)]
            if workload in ('hot', 'ttl', 'readsearch'):
                prime = [case_metrics(report, i, 'prime-only', condition) for i in range(samples)]
                scopes.append(('warm_operation_minus_independent_prime',
                               [{k: case[k] - p[k] for k in case} for case, p in zip(cases, prime)]))
            for scope, values in scopes:
                for metric in values[0]:
                    v = [r[metric] for r in values]
                    rows.append(dict(batch=batch, binary_sha256=report['build']['binary_sha256'],
                        workload=workload, condition=condition, scope=scope, samples=samples, backing='private-noatime-tmpfs',
                        metric=metric, unit='callbacks' if metric in (*READ_METRICS, 'four_callback_subtotal') else 'units',
                        median=statistics.median(v), minimum=min(v), maximum=max(v)))
    return rows


def publish_blocked(timing_paths, profile_path, version_path, output):
    """Export explicit unavailable timing cells and separately verified diagnostics.

    This is not formal performance publication. Failed samples never enter
    statistics, and no percent/CI value is manufactured from partial cohorts.
    """
    output.mkdir(parents=True, exist_ok=False)
    profile = verified_report(profile_path, True)
    version = json.loads(version_path.read_text())
    assert version['source_inventory_sha256'] == profile['build']['source_inventory_sha256']
    assert version['binary_sha256'] == profile['build']['binary_sha256']
    build_directory = Path(profile['arguments']['build_receipt']).parent
    assert bench.sha(build_directory / 'build-receipt.json') == version['build_receipt_sha256']
    manifest = json.loads((build_directory / 'source-manifest.json').read_text())
    assert all(manifest[name] == digest for name, digest in version['P1_source_sha256'].items())
    failed = []
    for path in timing_paths:
        raw = json.loads(path.read_text())
        containment_path = path.parent / 'containment-receipt.json'
        containment = json.loads(containment_path.read_text())
        assert raw['state'] == containment['state'] == 'failed' and raw['failures']
        assert raw['build'] == profile['build']
        try:
            verified_report(path, False)
        except AssertionError:
            pass
        else:
            raise ValueError('accepted timing must use the normal publication path')
        failed.append(dict(batch=path.parent.name, report_sha256=bench.sha(path),
                           containment_sha256=bench.sha(containment_path),
                           partial_rows=len(raw['rows']), failures=raw['failures']))
    assert failed
    rows = blocked_timing_rows(version, [r['batch'] for r in failed])
    write_csv(output / 'kernel-cache-summary.csv', rows)
    write_csv(output / 'kernel-cache-counters.csv', counter_rows(profile, profile_path.parent.name))
    receipt = dict(state='blocked-no-accepted-final-timing', formal_performance_published=False,
                   source_version_sha256=bench.sha(version_path), source_version=version,
                   failed_timing=failed, profile=dict(path=str(profile_path), sha256=bench.sha(profile_path)),
                   profile_evidence={f: bench.sha(profile_path.parent / f) for f in
                       ('containment-receipt.json', 'noatime-proof.json', 'backing.tar')},
                   processed={n: bench.sha(output / n) for n in
                       ('kernel-cache-summary.csv', 'kernel-cache-counters.csv')},
                   harness={n: bench.sha(bench.HERE / n) for n in
                       ('kernel_cache_report.py', 'test_kernel_cache_report.py')})
    for name in receipt['harness']:
        shutil.copy2(bench.HERE / name, output / name)
    bench.write_json(output / 'publication-receipt.json', receipt)
    return receipt


def blocked_timing_rows(version, batches):
    return [dict(source_version=version['version'], source_inventory_sha256=version['source_inventory_sha256'],
                 binary_sha256=version['binary_sha256'], failed_batches=';'.join(batches),
                 backing='private-noatime-tmpfs', condition=c, workload=w,
                 state='blocked-no-accepted-final-timing', planned_samples=30, accepted_samples=0,
                 unit='ms', p50='', p95_reference='', percent_change='',
                 ci95_percent_low='', ci95_percent_high='')
            for c in bench.CONDITIONS for w in bench.WORKLOADS]


def publish(timing_path, profile_path, output):
    output.mkdir(parents=True, exist_ok=False)
    timing = verified_report(timing_path, False)
    profile = verified_report(profile_path, True)
    assert timing['build'] == profile['build']
    assert timing['arguments']['affinity'] == profile['arguments']['affinity'] == '0,1'
    timing_rows = []
    for s in timing['summary']:
        baseline = next((b for b, c in bench.COMPARISONS if c == s['condition']), '')
        comp = timing['cache_comparison'][s['workload']].get(baseline + ':' + s['condition'], {})
        timing_rows.append(dict(batch=timing_path.parent.name,
            binary_sha256=timing['build']['binary_sha256'], input_sha256=timing['input_sha256'],
            workload=s['workload'], condition=s['condition'], baseline=baseline, unit='ms', backing='private-noatime-tmpfs',
            **{k: v for k, v in s.items() if k not in ('workload', 'condition')},
            percent_change=comp.get('percent_change', ''),
            ci95_percent_low=comp.get('ci95_percent', ['', ''])[0],
            ci95_percent_high=comp.get('ci95_percent', ['', ''])[1]))
    counters = counter_rows(profile, profile_path.parent.name)
    write_csv(output / 'kernel-cache-summary.csv', timing_rows)
    write_csv(output / 'kernel-cache-counters.csv', counters)
    receipt = dict(timing=dict(path=str(timing_path), sha256=bench.sha(timing_path)),
                   profile=dict(path=str(profile_path), sha256=bench.sha(profile_path)),
                   build=timing['build'], acceptance='noatime-tmpfs-v1',
                   containment={name: {f: bench.sha(directory / f) for f in
                       ('containment-receipt.json', 'noatime-proof.json', 'backing.tar')}
                       for name, directory in (('timing', timing_path.parent), ('profile', profile_path.parent))},
                   processed={name: bench.sha(output / name) for name in ('kernel-cache-summary.csv', 'kernel-cache-counters.csv')},
                   harness={name: bench.sha(bench.HERE / name) for name in
                            ('kernel_cache_report.py', 'test_kernel_cache_report.py')})
    for name in receipt['harness']:
        shutil.copy2(bench.HERE / name, output / name)
    bench.write_json(output / 'publication-receipt.json', receipt)
    print(json.dumps({k: v for k, v in receipt.items() if k != 'build'}, indent=2))
    return timing_rows, counters


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--timing', type=Path)
    parser.add_argument('--failed-timings', type=Path, nargs='+')
    parser.add_argument('--source-version', type=Path)
    parser.add_argument('--profiles', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    if '.data' not in args.output.resolve().parts:
        parser.error('publication receipts must be retained in a NEW .data directory')
    if args.failed_timings:
        if args.timing or not args.source_version:
            parser.error('--failed-timings requires --source-version and excludes --timing')
        receipt = publish_blocked([p.resolve() for p in args.failed_timings], args.profiles.resolve(),
                                  args.source_version.resolve(), args.output.resolve())
        print(json.dumps(receipt, indent=2))
    elif args.timing:
        publish(args.timing.resolve(), args.profiles.resolve(), args.output.resolve())
    else:
        parser.error('--timing or --failed-timings is required')
