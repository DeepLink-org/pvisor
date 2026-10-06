#!/usr/bin/env python3
"""Audit B-SUPERVISION review/select/apply/disposal cost, excluding task setup."""

import argparse
import json
import math
import random
from pathlib import Path

from publication import distribution, write_csv
from publish_density import verify_harness
from publish_reference_campaign import paired_comparison
from reference_baselines import digest, verified_build_receipt
from v1.common import Context
from v1.supervision import validate_review, validate_target

BACKENDS = ('staged', 'git-worktree')


def summarize(report):
    args = report['cli_arguments']
    n = int(args['samples'])
    keys = [(row['backend'], row['trial']) for row in report['rows']]
    if (report.get('benchmark_id') != 'B-SUPERVISION' or n < 30
            or args['suites'] != 'supervision' or int(args['warmups']) < 3
            or report['capabilities'] or len(keys) != len(set(keys))
            or set(keys) != {(backend, trial) for backend in BACKENDS for trial in range(n)}):
        raise ValueError('requires complete paired supervision samples and three warmups')
    for row in report['rows']:
        if (row['correctness'] != 'passed' or row['content_review_complete'] is not True
                or (row['files_reviewed'], row['files_applied'], row['files_dropped']) != (20, 10, 10)):
            raise ValueError('review or selective result contract failed')
    metrics = {
        'review_ms': lambda r: r['review_ms'],
        'selected_apply_ms': lambda r: r['apply_ms'] + r.get('selection_ms', 0) + r.get('check_ms', 0),
        'dispose_ms': lambda r: r['drop_ms'],
        'total_ms': lambda r: r['wall_ms'],
    }
    records, comparisons = [], []
    for metric, extract in metrics.items():
        selected = {backend: [row for row in report['rows'] if row['backend'] == backend]
                    for backend in BACKENDS}
        for backend, rows in selected.items():
            records.append(dict(backend=backend, metric=metric, unit='ms', planned=n, failed=0,
                                warmups=args['warmups'], **distribution([extract(row) for row in rows])))
        comparisons.append(dict(metric=metric, control='git-worktree', unit='ms',
                                **paired_comparison(
                                    [dict(trial=r['trial'], value=extract(r)) for r in selected['staged']],
                                    [dict(trial=r['trial'], value=extract(r)) for r in selected['git-worktree']]),
                                method='5000 paired-round bootstrap resamples; seed 20261006'))
    return records, comparisons


def audit(path):
    path = Path(path).resolve()
    root = path.parent
    report = json.loads(path.read_text())
    summarize(report)
    verify_harness(report, root)
    receipt = verified_build_receipt(root / 'build-receipt.json', root / 'bin/pvisor')
    if receipt != report['binary_build'] or receipt['pvisor_sha256'] != report['binary_sha256']:
        raise ValueError('supervision build identity differs')
    for name in ('v1/supervision.py', 'v1/common.py', 'v1/apply_worker.py'):
        if digest(root / 'harness' / name) != digest(Path(__file__).parent / name):
            raise ValueError('retained review/command contract differs from publisher')
    protocol = report['supervision_protocol']
    if (protocol['files'] != 20 or protocol['selected'] != 10
            or protocol['human_participants'] != 0 or protocol['human_time_measured'] is not False
            or digest(protocol['git_binary']) != protocol['git_binary_sha256']):
        raise ValueError('workload or Git executable identity differs')
    args = report['cli_arguments']
    affinity = args['cpu_affinity']
    if len(set(map(int, affinity.split(',')))) != 2:
        raise ValueError('requires two declared CPU affinities')
    rng = random.Random(int(args['seed']))
    ordered = []
    for trial in range(-int(args['warmups']), int(args['samples'])):
        backends = list(BACKENDS)
        rng.shuffle(backends)
        ordered.extend((backend, trial) for backend in backends)
    directories = sorted((root / 'trials').iterdir())
    if len(directories) != len(ordered):
        raise ValueError('missing formal or warmup evidence')
    by_key = {(row['backend'], row['trial']): row for row in report['rows']}
    retained, identities = {}, set()
    for ordinal, (directory, (backend, trial)) in enumerate(zip(directories, ordered), 1):
        label = 'stage' if backend == 'staged' else 'git'
        if directory.name != f'{ordinal:05d}-supervision-{label}':
            raise ValueError('retained trial order differs')
        work, stage, view = (directory / name for name in ('workspace', 'stage', 'view'))
        validate_target(work)
        if {p.name for p in (work / 'files').iterdir()} != {f'f{i:06d}' for i in range(20)}:
            raise ValueError('final target inventory differs')
        review = (directory / 'review.diff').read_text()
        validate_review(review)
        commands = list((directory / 'commands').glob('*/command.json'))
        if len(commands) != (4 if backend == 'staged' else 5):
            raise ValueError('missing or extra retained command')
        by_argv = {}
        for command in commands:
            details = json.loads(command.read_text())
            if details['exit_code'] != 0 or details.get('timed_out') or tuple(details['argv']) in by_argv:
                raise ValueError('failed or duplicate command')
            by_argv[tuple(details['argv'])] = (command, details)
            for p in (command, command.parent / 'command.stdout', command.parent / 'command.stderr'):
                retained[str(p.relative_to(root))] = digest(p)

        def get(argv, cwd):
            key = tuple(['taskset', '--cpu-list', affinity, *argv])
            if key not in by_argv:
                raise ValueError('declared operation missing from command evidence')
            command, details = by_argv[key]
            if (details['cwd'] != str(cwd) or not math.isfinite(details['wall_ms'])
                    or details['wall_ms'] <= 0):
                raise ValueError('invalid operation time or cwd')
            return details['wall_ms'], (command.parent / 'command.stdout').read_text()

        if backend == 'staged':
            binary = str(root / 'bin/pvisor')
            get([binary, 'run', '--no-agent-defaults', '--stdio', 'capture', '--timeout', '600s',
                 '--overlaynet', 'off', '--stage', str(stage), '--', '/usr/bin/python3', 'worker.py', '20'], work)
            if digest(work / 'worker.py') != digest(root / 'harness/v1/apply_worker.py'):
                raise ValueError('edit worker differs')
            review_ms, output = get([binary, 'status', '--review', '--diff', str(stage)], work)
            argv = [binary, 'apply', str(stage)]
            for i in range(10):
                argv += ['--path', f'files/f{i:06d}']
            apply_ms, _ = get(argv, work)
            drop_ms, _ = get([binary, 'drop', str(stage)], work)
            Context.validate_bundle(object.__new__(Context), 'staged', directory / 'runs', stage)
            bundle_path = stage / 'run-bundle.json'
            bundle = json.loads(bundle_path.read_text())
            identity = (bundle['run']['run_id'], bundle['run']['attempt_id'])
            if not all(identity) or identity in identities or (stage / 'upper').exists():
                raise ValueError('independent staged Run or complete disposal missing')
            identities.add(identity)
            retained[str(bundle_path.relative_to(root))] = digest(bundle_path)
            measured = dict(review_ms=review_ms, apply_ms=apply_ms, drop_ms=drop_ms)
        else:
            review_ms, output = get(['git', 'diff', '--no-ext-diff', 'HEAD', '--', 'files'], view)
            select_ms, patch = get(['git', 'diff', '--no-ext-diff', 'HEAD', '--',
                                   *[f'files/f{i:06d}' for i in range(10)]], view)
            patch_path = directory / 'selected.diff'
            if patch_path.read_text() != patch:
                raise ValueError('selected patch differs from retained output')
            validate_review(patch, 10)
            if any(f'f{i:06d}' in patch for i in range(10, 20)):
                raise ValueError('patch includes an unselected file')
            check_ms, _ = get(['git', 'apply', '--check', str(patch_path)], work)
            apply_ms, _ = get(['git', 'apply', str(patch_path)], work)
            drop_ms, _ = get(['git', 'worktree', 'remove', '--force', str(view)], work)
            if view.exists():
                raise ValueError('Git task view not disposed')
            retained[str(patch_path.relative_to(root))] = digest(patch_path)
            measured = dict(review_ms=review_ms, selection_ms=select_ms, check_ms=check_ms,
                            apply_ms=apply_ms, drop_ms=drop_ms)
        if output != review:
            raise ValueError('full review differs from retained command output')
        if trial >= 0:
            row = by_key[backend, trial]
            if (Path(row['logs']).resolve() != directory or any(row[k] != v for k, v in measured.items())
                    or not math.isclose(sum(measured.values()), row['wall_ms'], rel_tol=1e-12)):
                raise ValueError('reported timing differs from retained operation times')
        for p in [directory / 'review.diff', *(work / 'files').iterdir()]:
            retained[str(p.relative_to(root))] = digest(p)
    audit_record = dict(state='passed', formal_conditions=len(report['rows']),
                        warmup_conditions=len(ordered) - len(report['rows']),
                        report_sha256=digest(path), script_sha256=digest(Path(__file__)),
                        retained_evidence_sha256=retained,
                        scope='all retained commands, full/selected diffs, final target bytes and staged Run boundaries; excludes task setup and human reading')
    (root / 'supervision-publication-audit.json').write_text(json.dumps(audit_record, indent=2) + '\n')
    return report, receipt


def publish(path, output):
    path = Path(path).resolve()
    report, receipt = audit(path)
    records, comparisons = summarize(report)
    identity = dict(cohort=path.parent.name, recorded_at=report['recorded_at'],
                    report_sha256=digest(path), binary_sha256=receipt['pvisor_sha256'],
                    source_manifest_sha256=receipt['source_manifest_sha256'],
                    audit_sha256=digest(path.parent / 'supervision-publication-audit.json'))
    for row in records + comparisons:
        row.update(identity)
    provenance = [dict(field=k, value=json.dumps(v, sort_keys=True) if isinstance(v, (dict, list)) else v)
                  for k, v in {**identity, 'protocol': report['supervision_protocol'],
                               'platform': report['platform'], 'cpu': report['cpu'],
                               'cpu_affinity': report['cli_arguments']['cpu_affinity'],
                               'memory_limit': 'no benchmark-specific host memory cap'}.items()]
    output.mkdir(parents=True, exist_ok=True)
    write_csv(output / 'supervision-summary.csv', records)
    write_csv(output / 'supervision-comparisons.csv', comparisons)
    write_csv(output / 'supervision-provenance.csv', provenance)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    publish(args.report, args.output)
