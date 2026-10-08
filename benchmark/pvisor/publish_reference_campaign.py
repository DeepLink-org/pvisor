#!/usr/bin/env python3
"""Publish complete current reference cohorts without pooling different workloads."""
import argparse
import json
from pathlib import Path
import random
import statistics
import tempfile

from publication import distribution, percentile, publish, write_csv
from reference_baselines import digest
from firecracker_kernels import verify_kernel_receipt


def verify_input_records(path, report):
    """New input gates must pass before and after a complete timing cohort."""
    if 'input_verification' not in report:
        return  # Legacy cohorts retain their separately stated evidence scope.
    initial = report['input_verification']
    expected = {'state': 'passed', **initial}
    if (report.get('input_final_verification') != expected
            or initial.get('input_manifest_sha256') != report.get('input_manifest_sha256')):
        raise ValueError('prepared inputs lack matching successful before/after identities')
    for name in ('input-verification.json', 'input-final-verification.json'):
        try:
            retained = json.loads((path.parent / name).read_text())
        except (OSError, ValueError) as error:
            raise ValueError('missing retained input verification evidence') from error
        if retained != expected:
            raise ValueError('retained input verification differs from report')


def paired_comparison(candidate, control, iterations=5000):
    left = {r['trial']: r['value'] for r in candidate}
    right = {r['trial']: r['value'] for r in control}
    if len(left) != len(candidate) or len(right) != len(control) or set(left) != set(right) or len(left) < 30:
        raise ValueError('paired comparison requires unique, matching trial IDs and >=30 samples')
    result = dict(n=len(left), difference_ms='', ci95_low_ms='', ci95_high_ms='',
                  conclusion='separated distribution; inspect clusters')
    if any(distribution(list(values.values()))['distribution'] != 'unsplit' for values in (left, right)):
        return result
    pairs = sorted(left)
    rng = random.Random(20261006)
    differences = []
    for _ in range(iterations):
        sampled = rng.choices(pairs, k=len(pairs))
        differences.append(statistics.median(left[i] for i in sampled)-statistics.median(right[i] for i in sampled))
    low, high = percentile(differences, 2.5), percentile(differences, 97.5)
    result.update(difference_ms=statistics.median(left.values())-statistics.median(right.values()),
                  ci95_low_ms=low, ci95_high_ms=high,
                  conclusion='candidate faster' if high < 0 else 'candidate slower' if low > 0 else 'no detected difference')
    return result


def verify_kernel_records(report):
    for backend, identity in report.get('reference_kernels', {}).items():
        if identity is None:
            continue
        kind = 'fc-reference' if backend == 'fc-reference' else 'fc-system'
        receipt = report['arguments']['fc_reference_receipt' if kind == 'fc-reference'
                                     else 'qemu_system_receipt' if backend.startswith('qemu')
                                     else 'fc_system_receipt']
        if verify_kernel_receipt(receipt, kind) != identity:
            raise ValueError('retained reference kernel differs from measured identity')
        if any(row.get('kernel_provenance') != identity for row in report['rows']
               if row['backend'] == backend):
            raise ValueError('trial kernel provenance differs from cohort')


def publish_campaign(paths, output):
    reports = [(path, json.loads(path.read_text())) for path in paths]
    identities = {(r['pvisor_sha256'], r.get('binary_source_manifest_sha256'),
                   r.get('input_manifest_sha256')) for _,r in reports}
    if len(identities) != 1 or any(not value or value == 'unknown' for value in next(iter(identities))):
        raise ValueError('campaign requires one verified current binary/source/input identity')
    for path, report in reports:
        verify_input_records(path, report)
        verify_kernel_records(report)
        receipt = report.get('binary_build') or {}
        if receipt.get('pvisor_sha256') != report['pvisor_sha256'] or receipt.get('source_manifest_sha256') != report['binary_source_manifest_sha256']:
            raise ValueError('report identity disagrees with binary build receipt')
        if digest(path.parent/'bin/pvisor') != report['pvisor_sha256'] or digest(path.parent/'source-manifest.json') != report['binary_source_manifest_sha256']:
            raise ValueError('retained binary/source evidence differs from report')
    keys = [(mode, backend) for _,r in reports for mode in r['arguments']['modes'].split(',')
            for backend in r['arguments']['backends'].split(',')]
    if len(keys) != len(set(keys)):
        raise ValueError('duplicate workload/backend cohorts; never pool batches')
    statistics_rows, provenance, comparisons = [], [], []
    # Temporary derived files remain ignored. Nothing public is written until
    # every input cohort has passed validation.
    staging = output.parent / '.data'
    staging.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=staging, prefix='publication-') as temp:
        for index,(path, report) in enumerate(reports):
            if any(c['state'] == 'available' and c.get('failures') for c in report['capabilities'].values()):
                raise ValueError('incomplete successful cohort with measured failures')
            published = publish(path, Path(temp)/str(index))
            statistics_rows.extend(published)
            import csv
            for item in csv.DictReader((Path(temp)/str(index)/'runtime-provenance.csv').open()):
                provenance.append(dict(batch=path.parent.name,benchmark_id=report['benchmark_id'],**item))
            for key in ('binary_source_commit','binary_source_manifest_sha256','input_manifest_sha256','source_commit_scope'):
                provenance.append(dict(batch=path.parent.name,benchmark_id=report['benchmark_id'],field=key,value=report.get(key,'unknown')))
            for mode in report['arguments']['modes'].split(','):
                metrics=['ready_ms','result_ms','completion_ms']
                if mode == 'filesystem':
                    metrics += [op+'_worker_ms' for op in ('metadata','read','write','git','rg','cargo','npm')]
                for candidate in ('pvisor-host','pvisor-staged','pvisor-vm'):
                    for control in ('native','docker','firecracker','fc-system','fc-reference','qemu','qemu-microvm'):
                        if any(report['capabilities'].get(mode+'/'+backend,{}).get('state') != 'available' for backend in (candidate,control)):
                            continue
                        for metric in metrics:
                            def values(backend):
                                def value(row):
                                    return row['result']['filesystem'][metric.removesuffix('_worker_ms')]['worker_ms'] if metric.endswith('_worker_ms') else row[metric]
                                return [dict(trial=row['trial'],value=value(row)) for row in report['rows'] if row['mode']==mode and row['backend']==backend]
                            candidate_values, control_values = values(candidate), values(control)
                            if any(row['value'] is None for row in candidate_values + control_values):
                                continue  # Ready-only supplies no completion comparison.
                            comparisons.append(dict(batch=path.parent.name,benchmark_id=report['benchmark_id'],mode=mode,
                                candidate=candidate,control=control,metric=metric,unit='ms',
                                **paired_comparison(candidate_values,control_values),
                                confidence_method='5000 paired-round bootstrap resamples, seed 20261006; percentile 95% CI'))
    output.mkdir(parents=True, exist_ok=True)
    write_csv(output/'runtime-summary.csv',statistics_rows)
    write_csv(output/'runtime-provenance.csv',provenance)
    if comparisons:
        write_csv(output/'runtime-comparisons.csv',comparisons)
    return statistics_rows


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reports',type=Path,nargs='+',required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    publish_campaign(args.reports,args.output)
