#!/usr/bin/env python3
"""Publish derived B-WORKFLOW statistics; preserve raw evidence under .data/."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import statistics

from publication import distribution, percentile, write_csv
from review_workflow import BACKENDS


def publish(report_path, output):
    report = json.loads(report_path.read_text())
    if report['benchmark_id'] != 'B-WORKFLOW':
        raise ValueError('not a B-WORKFLOW report')
    args = report['arguments']
    planned = args['samples']
    if planned < 30 or report['failures']:
        raise ValueError('publication requires at least 30 samples per condition and no failed trials')
    rows = report['rows']
    expected = {(int(size), case, backend, trial) for size in args['sizes'].split(',')
        for case in args['cases'].split(',') for backend in BACKENDS for trial in range(planned)}
    actual = [(r['files'], r['case'], r['backend'], r['trial']) for r in rows]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError('incomplete or duplicate cohort')
    if any(r['correctness'] != 'passed' for r in rows):
        raise ValueError('incorrect sample')
    report_sha = hashlib.sha256(report_path.read_bytes()).hexdigest()
    summary, comparisons = [], []
    for size in map(int, args['sizes'].split(',')):
        for case in args['cases'].split(','):
            timings = {}
            for backend in BACKENDS:
                selected = sorted((r for r in rows if r['files'] == size and r['case'] == case and r['backend'] == backend), key=lambda r: r['trial'])
                timings[backend] = [r['wall_ms'] for r in selected]
                for metric in ('prepare_ms', 'run_ms', 'review_ms', 'apply_ms', 'dispose_ms', 'wall_ms'):
                    summary.append(dict(benchmark_id='B-WORKFLOW', batch=report_path.parent.name,
                        files=size,case=case,backend=backend,metric=metric,unit='ms',
                        planned=planned,failed=0,warmups=args['warmups'],cpu_affinity=args['cpu_affinity'],
                        **distribution([r[metric] for r in selected]),report_sha256=report_sha))
            for control in BACKENDS[1:]:
                stage, baseline = timings[BACKENDS[0]], timings[control]
                split = any(distribution(values)['distribution'] != 'unsplit' for values in (stage, baseline))
                record = dict(files=size,case=case,control=control,n=planned,
                    stage_minus_control_ms='',ci95_low_ms='',ci95_high_ms='',control_over_stage_ratio='',
                    confidence_method='5000 paired-round bootstrap resamples; percentile 95% CI; seed 20261006',
                    conclusion='separated distribution: inspect cluster statistics' if split else '',report_sha256=report_sha)
                if not split:
                    rng = random.Random(20261006)
                    deltas = []
                    for _ in range(5000):
                        indices = rng.choices(range(planned), k=planned)
                        deltas.append(statistics.median(stage[i] for i in indices) - statistics.median(baseline[i] for i in indices))
                    low, high = percentile(deltas, 2.5), percentile(deltas, 97.5)
                    record.update(stage_minus_control_ms=statistics.median(stage)-statistics.median(baseline),
                        ci95_low_ms=low,ci95_high_ms=high,control_over_stage_ratio=statistics.median(baseline)/statistics.median(stage),
                        conclusion='stage faster' if high < 0 else 'stage slower' if low > 0 else 'no detected difference')
                comparisons.append(record)
    output.mkdir(parents=True,exist_ok=True)
    write_csv(output / 'workflow-summary.csv', summary)
    write_csv(output / 'workflow-comparisons.csv', comparisons)
    provenance = [dict(field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value)
        for key,value in report.items() if key not in ('rows','failures')]
    provenance.extend([dict(field='report_sha256',value=report_sha),
        dict(field='raw_location',value=str(report_path)),dict(field='successful_measured_samples',value=len(rows)),
        dict(field='statistics',value='linear P95 descriptive; separated clusters replace P50; no P99; no pooled cohorts')])
    write_csv(output / 'workflow-provenance.csv', provenance)
    return summary, comparisons


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args()
    publish(args.report,args.output)
