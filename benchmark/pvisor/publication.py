#!/usr/bin/env python3
"""Publish derived benchmark CSVs; retain reports and samples under .data/.

Helper for B-STARTUP, B-FS-TOOLS and B-AGENT-TASK (benchmark/README.md).
Publication never merges cohorts, drops slow valid samples, or emits P99.
"""
import argparse
import csv
import hashlib
import json
import math
import statistics
from pathlib import Path


def percentile(values, q):
    ordered = sorted(values)
    pos = (len(ordered) - 1) * q / 100
    lo = int(pos)
    return ordered[lo] + (ordered[min(lo + 1, len(ordered) - 1)] - ordered[lo]) * (pos - lo)


def distribution(values):
    if not values or any(not math.isfinite(v) or v < 0 for v in values):
        raise ValueError('timings must be finite, nonnegative and nonempty')
    ordered = sorted(values)
    n = len(ordered)
    result = dict(n=n, p50=statistics.median(ordered), p95_reference=percentile(ordered, 95) if n >= 30 else '',
                  minimum=ordered[0], maximum=ordered[-1], distribution='unsplit',
                  low_n='', low_p50='', high_n='', high_p50='')
    # A descriptive split, not a fitted mixture or causal diagnosis.
    minimum_cluster = max(5, math.ceil(n * .1))
    candidates = [(ordered[i] - ordered[i-1], i) for i in range(minimum_cluster, n-minimum_cluster+1)]
    if candidates:
        gap, split = max(candidates)
        ordinary_gaps = [b-a for a,b in zip(ordered, ordered[1:])]
        low, high = ordered[:split], ordered[split:]
        if (gap >= .2 * result['p50'] and gap > 3 * statistics.median(ordinary_gaps)
                and statistics.median(high) >= 1.5 * statistics.median(low)):
            result.update(p50='', distribution='separated-clusters', low_n=len(low),
                          low_p50=statistics.median(low), high_n=len(high), high_p50=statistics.median(high))
    return result


def write_csv(path, rows):
    with path.open('w', newline='') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]), lineterminator='\n')
        writer.writeheader()
        writer.writerows(rows)


def publish(report_path, output):
    report = json.loads(report_path.read_text())
    output.mkdir(parents=True, exist_ok=True)
    rows = report['rows']
    if any(row.get('correctness') != 'passed' for row in rows):
        raise ValueError('incorrect rows cannot be published')
    records = []
    for mode in report['arguments']['modes'].split(','):
        for backend in report['arguments']['backends'].split(','):
            selected = [r for r in rows if r['mode'] == mode and r['backend'] == backend]
            capability = report['capabilities'][f'{mode}/{backend}']
            planned = int(report['arguments']['samples'])
            failed = len([r for r in capability.get('failures', []) if r['trial'] >= 0])
            if capability['state'] == 'available' and len(selected) + failed != planned:
                raise ValueError(f'incomplete cohort {mode}/{backend}')
            if len({r['trial'] for r in selected}) != len(selected):
                raise ValueError('duplicate trials')
            metrics = {key: [r[key] for r in selected] for key in ['ready_ms','result_ms','completion_ms']}
            if mode == 'filesystem' and selected:
                metrics.update({op+'_worker_ms':[r['result']['filesystem'][op]['worker_ms'] for r in selected]
                                for op in selected[0]['result']['filesystem']})
            for metric, values in metrics.items():
                stats = distribution(values) if values else {k:'' for k in distribution([0])}
                stats['n'] = len(values)
                records.append(dict(benchmark_id=report['benchmark_ids'][mode], batch=report_path.parent.name,
                                    mode=mode,backend=backend,metric=metric,unit='ms',planned=planned,
                                    failed=failed,capability=capability['state'],**stats))
    write_csv(output/'runtime-summary.csv', records)
    provenance = [{'field':key,'value':str(report.get(key,''))} for key in
                  ['recorded_at','source_commit','pvisor_sha256','kernel_sha256','bzimage_sha256',
                   'driver_sha256','workload_sha256','host_kernel','docker_version','firecracker_version','qemu-system-x86_64_version']]
    provenance += [{'field':'report_sha256','value':hashlib.sha256(report_path.read_bytes()).hexdigest()},
                   {'field':'raw_location','value':str(report_path)},
                   {'field':'arguments','value':json.dumps(report['arguments'],sort_keys=True)},
                   {'field':'statistics','value':'batches separate; P95 descriptive at N>=30; no P99; separated clusters replace P50'},
                   {'field':'split_rule','value':'both clusters >=max(5,10% N); largest gap >=20% median and >3x median adjacent gap; cluster medians >=1.5x'}]
    write_csv(output/'runtime-provenance.csv', provenance)
    return records


def write_derived_summary(path, summary):
    """Export nested aggregate distributions, never individual samples."""
    rows=[]
    def walk(value, prefix='', inherited_n=None):
        if not isinstance(value,dict): return
        n=value.get('n',inherited_n)
        if 'p50' in value:
            rows.append(dict(metric=prefix,n=n if n is not None else '',p50=value['p50'],
                             p95_reference=value.get('p95','') if n and n>=30 else '',
                             minimum=value.get('min',''),maximum=value.get('max','')))
        else:
            for key,child in value.items():walk(child,prefix+'/'+key,n)
    walk(summary)
    if rows:write_csv(path,rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    publish(args.report,args.output)


if __name__=='__main__':
    main()
