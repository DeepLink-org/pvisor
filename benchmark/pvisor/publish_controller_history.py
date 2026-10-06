#!/usr/bin/env python3
"""Publish B-CLUSTER history observations without pooling query batches."""
import argparse
import json
import math
from pathlib import Path
import statistics

from controller_history import validate_result
from publication import write_csv
from publish_density import verify_harness
from reference_baselines import digest


def summarize(report):
    args=report['arguments'];n=int(args['repetitions']);sizes=list(map(int,args['sizes'].split(',')))
    if report.get('benchmark_id')!='B-CLUSTER' or n<3:raise ValueError('history needs three independent processes per size')
    keys=[(r['size'],r['trial']) for r in report['rows']]
    expected={(size,trial) for size in sizes for trial in range(n)}
    if len(keys)!=len(set(keys)) or set(keys)!=expected:raise ValueError('missing or duplicate history observations')
    metrics={size:dict(physical_peak_mib=[],warm_reopen_ms=[],wal_mib=[],whole_cgroup_cpu_ms=[],typed_counts_process_median_ns=[],process_rss_before_query_mib=[]) for size in sizes}
    for row in report['rows']:
        if row.get('correctness')!='passed' or row['memory_bytes']!=int(args['memory_mib'])*1024**2:
            raise ValueError('history failed or memory budget differs')
        value=validate_result(row['example_report'],row['size'],int(args['query_samples']),args['cpu_affinity'])
        points=[row['before'],*row['samples'],row['after']]
        if any(p['events'].get('oom',0) or p['events'].get('oom_kill',0) for p in points):raise ValueError('history OOM invalidates complete observation')
        cpu=(row['after']['cpu']['usage_usec']-row['before']['cpu']['usage_usec'])/1000
        values=dict(physical_peak_mib=max(p['peak_bytes'] for p in points)/1024**2,
            warm_reopen_ms=value['reopen_us']/1000,wal_mib=value['wal_bytes_after_completion']/1024**2,
            whole_cgroup_cpu_ms=cpu,typed_counts_process_median_ns=value['indexed_counts']['p50'],
            process_rss_before_query_mib=value['controller_process_memory_before_query']['VmRSS_kib']/1024)
        if any(not math.isfinite(v) or v<=0 for v in values.values()):raise ValueError('invalid history timing/resource value')
        for name,value in values.items():metrics[row['size']][name].append(value)
    output=[]
    for size,columns in metrics.items():
        for metric,values in columns.items():
            output.append(dict(records=size,metric=metric,n=n,p50=statistics.median(values),minimum=min(values),maximum=max(values),
                unit='MiB' if metric.endswith('_mib') else 'ns' if metric.endswith('_ns') else 'ms',
                query_batches_per_process=args['query_samples'],cpu_affinity=args['cpu_affinity'],memory_budget_mib=args['memory_mib'],swap_bytes=0,
                statistics='independent process observations; query medians summarized per process, no pooling of correlated batches; no P95/P99',
                memory_scope='whole lifecycle peak includes validation temporaries, preparation, WAL/cache and warm replay; RSS secondary, not stationary physical footprint'))
    return output


def publish(path, output):
    report=json.loads(path.read_text());receipt=report['binary_build'];directory=path.parent.resolve()
    if (digest(directory/'bin/scheduler_load')!=receipt['binaries']['scheduler_load']['sha256']
            or digest(directory/'source-manifest.json')!=receipt['source_manifest_sha256']
            or json.loads((directory/'build-receipt.json').read_text())!=receipt):raise ValueError('history source/build receipt differs')
    verify_harness(report,directory);rows=summarize(report);sources={}
    for row in report['rows']:
        root=Path(row['logs']).resolve()
        if not root.is_relative_to(directory):raise ValueError('history evidence outside cohort')
        result=root/'result.json';example=root/'example-report.json'
        for p in (result,example,root/'config.json',root/'example.stdout',root/'example.stderr',root/'service.stdout',root/'service.stderr'):
            if not p.resolve().is_relative_to(directory):raise ValueError('history evidence outside cohort')
            sources[str(p.relative_to(directory))]=digest(p)
        if json.loads(example.read_text())!=row['example_report'] or json.loads(result.read_text())!={k:v for k,v in row.items() if k not in ('size','trial','logs')}:
            raise ValueError('history report differs from retained observation')
        config=json.loads((root/'config.json').read_text())
        if config['size']!=row['size'] or config['memory_bytes']!=row['memory_bytes'] or config['cpu_affinity']!=report['arguments']['cpu_affinity']:
            raise ValueError('history condition differs from retained invocation')
    sha=digest(path)
    for row in rows:row.update(cohort=directory.name,report_sha256=sha,example_sha256=receipt['binaries']['scheduler_load']['sha256'],source_manifest_sha256=receipt['source_manifest_sha256'])
    audit=directory/'history-output-evidence-audit.json';audit.write_text(json.dumps(dict(report_sha256=sha,retained_evidence_sha256=sources),indent=2)+'\n')
    provenance=[dict(field=k,value=json.dumps(v,sort_keys=True) if isinstance(v,(dict,list)) else v) for k,v in report.items() if k!='rows']
    provenance.extend([dict(field='report_sha256',value=sha),dict(field='output_evidence_audit_sha256',value=digest(audit))])
    output.mkdir(parents=True,exist_ok=True);write_csv(output/'controller-history-summary.csv',rows);write_csv(output/'controller-history-provenance.csv',provenance)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--report',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.report,args.output)
