#!/usr/bin/env python3
"""Derived useful-task scaling charts from the current fixed-budget report.

Helper for B-CLUSTER. Ready-only/legacy TSV inputs are deliberately rejected.
Matplotlib is imported only when rendering; summary gates have no plot dependency.
"""
import argparse
import json
from pathlib import Path

from publication import distribution, write_csv
from reference_baselines import digest
from cluster_scalability import validate_result
from publish_reference_campaign import paired_comparison

MIB=1024**2


def verify_execution_evidence(report, directory):
    directory=directory.resolve();sources={};identities=set()
    for name,field in [('cluster_scalability.py','harness_sha256'),('cluster_worker.py','worker_sha256'),('cluster-quickstart.py','quickstart_sha256'),('input-manifest.json','input_manifest_sha256')]:
        if digest(directory/name)!=report[field]:raise ValueError('retained Cluster harness/input mismatch')
    for row in report['rows']:
        root=Path(row['logs']).resolve()
        if not root.is_relative_to(directory):raise ValueError('task evidence outside cohort')
        if len(row['tasks'])!=int(report['arguments']['tasks']):raise ValueError('missing individual task evidence')
        for task in row['tasks']:
            path=(root/(task['id']+'.json')).resolve()
            if not path.is_relative_to(root):raise ValueError('task evidence outside owned batch')
            if task['id'] in identities:raise ValueError('duplicate retained task identity')
            identities.add(task['id'])
            result=validate_result(json.loads(path.read_text()),task['id'],task['node'])
            if result!=task['result']:raise ValueError('task report differs from retained completion')
            sources[str(path.relative_to(directory))]=digest(path)
    return sources


def compare_completion(report, report_sha):
    sizes=sorted(map(int,report['arguments']['sizes'].split(',')))
    def values(size):return [dict(trial=r['trial'],value=r['completion_ms']) for r in report['rows'] if r['workers']==size and r['trial']>=0]
    return [dict(candidate_workers=size,control_workers=sizes[0],metric='completion_ms',unit='ms',
        **paired_comparison(values(size),values(sizes[0])),report_sha256=report_sha,
        confidence_method='5000 paired-round bootstrap resamples, seed 20261006; percentile 95% CI') for size in sizes[1:]]


def summarize_execution(report, report_sha):
    if report.get('benchmark_id')!='B-CLUSTER':raise ValueError('requires current B-CLUSTER useful-task report')
    args=report['arguments'];n=int(args['repetitions']);warmups=int(args['warmups'])
    sizes=list(map(int,args['sizes'].split(',')));tasks=int(args['tasks'])
    if n<30:raise ValueError('requires >=30 full batches per Worker count')
    expected={(size,trial) for size in sizes for trial in range(-warmups,n)}
    actual=[(row['workers'],row['trial']) for row in report['rows']]
    if len(actual)!=len(set(actual)) or set(actual)!=expected:raise ValueError('incomplete or duplicated cluster conditions')
    for row in report['rows']:
        if row['correctness']!='passed' or row['completed']!=tasks or row['submitted']!=tasks or row['failed'] or row.get('monitor_error'):
            raise ValueError('failed or incomplete useful-task batch')
        if row['after']['events'].get('oom',0) or row['after']['events'].get('oom_kill',0):raise ValueError('OOM invalidates reliable throughput')
        controls=row['controls'];quota,period=controls['cpu_max']
        if controls['memory_max']!=int(args['budget_mib'])*MIB or quota/period!=2 or controls['swap_max']!=0:
            raise ValueError('fixed total memory/CPU/swap controls differ')
    records=[]
    for size in sizes:
        rows=[row for row in report['rows'] if row['workers']==size and row['trial']>=0]
        metrics=dict(completion_ms=[r['completion_ms'] for r in rows],
            validated_tasks_per_second=[r['validated_per_second'] for r in rows],
            complete_cgroup_peak_mib=[max(v['peak_bytes'] for v in [r['before'],r['after'],*r['samples']])/MIB for r in rows])
        for metric,values in metrics.items():
            records.append(dict(benchmark_id='B-CLUSTER',workers=size,metric=metric,unit='MiB' if metric.endswith('_mib') else 'tasks/s' if metric.endswith('_second') else 'ms',
                planned_batches=n,completed_tasks=n*tasks,failed_batches=0,warmups=warmups,total_cpu_cores=2,memory_budget_mib=args['budget_mib'],
                cpu_affinity=args['cpu_affinity'],**distribution(values),report_sha256=report_sha))
    return records


def render(records,output):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    plt.rcParams.update({'font.family':'DejaVu Sans','svg.fonttype':'none','axes.spines.top':False,'axes.spines.right':False})
    fig,axes=plt.subplots(1,3,figsize=(14,4.8))
    for ax,metric,label in zip(axes,('validated_tasks_per_second','completion_ms','complete_cgroup_peak_mib'),('Validated tasks / second','Complete batch (ms)','Whole cgroup peak (MiB)')):
        selected=sorted((r for r in records if r['metric']==metric),key=lambda r:r['workers'])
        for row in selected:
            points=[row['p50']] if row['distribution']=='unsplit' else [row['low_p50'],row['high_p50']]
            ax.plot([row['workers']]*len(points),points,'o',color='#2563eb')
            ax.vlines(row['workers'],row['minimum'],row['maximum'],color='#2563eb',alpha=.25)
        ax.set_xticks([r['workers'] for r in selected]);ax.set_xlabel('One-slot Workers');ax.set_ylabel(label);ax.grid(alpha=.15);ax.set_ylim(bottom=0)
    first=records[0]
    fig.suptitle(f'Validated Python/Git work: fixed 2-core / {first["memory_budget_mib"]} MiB total budget')
    fig.text(.5,.015,f'{first["planned_batches"]} full batches per condition; identical task count. Dots: medians (both clusters when separated).\nBars: observed min/max, not confidence intervals. Prepared inputs; no model inference or multi-host claim.',ha='center',fontsize=9)
    fig.tight_layout(rect=(0,.10,1,.93))
    for suffix in ('svg','png'):fig.savefig(output/f'cluster-throughput.{suffix}',dpi=160,bbox_inches='tight',metadata={'Date':None} if suffix=='svg' else None)
    plt.close(fig)


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--output-dir',type=Path,required=True);parser.add_argument('--csv-only',action='store_true')
    args=parser.parse_args();report=json.loads(args.report.read_text());receipt=report['binary_build']
    if digest(args.report.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:raise ValueError('Cluster source receipt mismatch')
    for name in ('pvisor-cluster','pvisor-worker'):
        if digest(args.report.parent/'bin'/name)!=receipt['binaries'][name]['sha256']:raise ValueError('Cluster binary receipt mismatch')
    records=summarize_execution(report,digest(args.report))
    sources=verify_execution_evidence(report,args.report.parent)
    comparisons=compare_completion(report,digest(args.report))
    args.output_dir.mkdir(parents=True,exist_ok=True)
    write_csv(args.output_dir/'cluster-summary.csv',records)
    write_csv(args.output_dir/'cluster-comparisons.csv',comparisons)
    audit=args.report.parent/'cluster-execution-evidence-audit.json'
    audit.write_text(json.dumps(dict(report_sha256=digest(args.report),task_evidence_sha256=sources,
        scope='each unique task matched to retained succeeded record, expected Worker and full workload result'),indent=2)+'\n')
    provenance=[dict(field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value) for key,value in report.items() if key!='rows']
    provenance.extend([dict(field='report_sha256',value=digest(args.report)),dict(field='raw_location',value=str(args.report)),
        dict(field='execution_evidence_audit_sha256',value=digest(audit)),
        dict(field='statistics',value='warmups excluded; complete fixed-budget useful-task batches; separated clusters retain both medians; P95 descriptive, no P99; no ready-rate throughput')])
    write_csv(args.output_dir/'cluster-provenance.csv',provenance)
    if not args.csv_only:render(records,args.output_dir)


if __name__=='__main__':main()
