#!/usr/bin/env python3
"""B-VM-MEMORY: publish complete user-facing memory-choice cohorts only."""
import argparse
import json
from pathlib import Path
import random
import statistics

from memory_savings import digest, matrix, validate, validate_observation
from publication import distribution, write_csv


def load_cohort(path,static=False):
    report=json.loads(path.read_text())
    args=report['arguments']; root=path.parent
    if (report.get('schema')!='pvisor-memory-savings-cohort/v1'
            or report.get('benchmark_id')!='B-VM-MEMORY' or report.get('role')!='user-facing' or not report.get('complete')
            or report.get('budget')!=dict(cpu_cores=4,memory_max=2147483648,swap_max=0)
            or (static and (not args.get('static') or args['samples']!=1 or args['warmups']!=0 or args['wait']!=5))
            or (not static and (args['samples']<30 or args['warmups']<3 or args['wait']!=60))
            or (not static and args.get('static',False))
            or len(report['selected'])!=16 or set(map(tuple,report['selected']))!=set(matrix())
            or report.get('host_verification')!={'kernel':True,'cpu_model':True,'ksm':True,'host_cp_sha256':True}
            or report.get('input_verification')!={'rootfs':True,'firmware':True}):
        raise ValueError('requires complete 16-condition formal cohort')
    receipt=json.loads((root/'build-receipt.json').read_text())
    for file, expected in [('probe',report['binary_sha256']),('harness.py',report['harness_sha256']),
                           ('linux_cold_runtime.py',report['helper_sha256']),
                           ('source-manifest.json',receipt['source_manifest_sha256'])]:
        if digest(root/file)!=expected:raise ValueError('retained artifact mismatch: '+file)
    if receipt['example_sha256']!=report['binary_sha256']:raise ValueError('binary receipt mismatch')
    expected={(r,m,p) for r in range(-args['warmups'],args['samples']) for m,p in matrix()}
    records={}
    for index,row in enumerate(report['attempts']):
        condition=row['condition']; key=(row['round'],condition['mode'],condition['pattern'])
        if key not in expected or key in records or row['warmup']!=(row['round']<0):
            raise ValueError('duplicate/unplanned trial')
        wanted=dict(mode=key[1],pattern=key[2],wait=args['wait'])
        if 'memory_mib' in args:wanted['memory_mib']=args['memory_mib']
        if condition!=wanted:
            raise ValueError('condition does not match formal protocol')
        if row['status']!='successful' or row['returncode']!=0 or not row['unit_quiescent']:
            raise ValueError('failed trial')
        trial=root/str(index); raw_path=trial/'w/raw.json'
        if digest(raw_path)!=row['raw_sha256']:raise ValueError('raw evidence mismatch')
        raw=json.loads(raw_path.read_text()); phases,tasks=validate(raw,condition)
        monitor=json.loads((trial/'monitor.json').read_text())
        guard=json.loads((trial/'guard.json').read_text());validate_observation(monitor,guard,allow_builds=static)
        group=next(iter(phases.values()))['memory']['cgroup']
        if any(item['group']!=group for item in monitor):raise ValueError('observer group mismatch')
        values={name+'_mib':phase['memory']['current']/1024**2 for name,phase in phases.items()}
        for name,phase in phases.items():
            if 'pss_bytes' in phase['memory']:
                values[name+'_physical_mib']=phase['memory']['pss_bytes']/1024**2
            for field in ('anon','file','kernel'):
                values[name+'_'+field+'_mib']=phase['memory']['stat'][field]/1024**2
        idle=phases[f"idle-{args['wait']}"]
        values.update(peak_mib=max([item['memory_peak'] for item in monitor]+
                                  [phase['memory']['peak'] for phase in phases.values()])/1024**2,
                      park_cpu_ms=(phases['parked']['memory']['cpu']['usage_usec']-
                                   phases['active']['memory']['cpu']['usage_usec'])/1000,
                      idle_cpu_ms=(idle['memory']['cpu']['usage_usec']-
                                   phases['parked']['memory']['cpu']['usage_usec'])/1000,
                      recovery_cpu_ms=(phases['restored']['memory']['cpu']['usage_usec']-
                                   idle['memory']['cpu']['usage_usec'])/1000,
                      park_ms=raw['park_ms'],resume_ack_ms=raw['resume_ack_ms'],
                      next_task_ms=raw['resume_to_task_ms'],guest_task_ms=tasks['restored']['task_ms'])
        if condition['mode'] in ('raw','compressed'):
            if raw.get('storage_allocated_bytes',0)<=0:raise ValueError('missing backing storage')
            values['backing_mib']=raw['storage_allocated_bytes']/1024**2
        records[key]=values
    if records.keys()!=expected:raise ValueError('missing formal/warmup trials')
    return report,records


def publish(path,output,static=False):
    report,records=load_cohort(path,static=static); samples=report['arguments']['samples']; rows=[]
    for mode,pattern in matrix():
        values=[records[r,mode,pattern] for r in range(samples)]
        for metric in values[0]:
            rows.append(dict(benchmark_id='B-VM-MEMORY',cohort=path.parent.name,mode=mode,
                vm_memory_mib=report['arguments'].get('memory_mib',256),
                pattern=pattern,metric=metric,unit='MiB' if metric.endswith('_mib') else 'ms',
                **(dict(n=1,value=values[0][metric]) if static else
                   distribution([value[metric] for value in values]))))
    comparisons=[]
    for mode,pattern in matrix():
        if mode=='default':continue
        metrics=[f"idle-{report['arguments']['wait']}_mib",'next_task_ms','idle_cpu_ms','peak_mib']
        physical=f"idle-{report['arguments']['wait']}_physical_mib"
        if physical in records[0,mode,pattern]:metrics.append(physical)
        for metric in metrics:
            diffs=[records[r,mode,pattern][metric]-records[r,'default',pattern][metric] for r in range(samples)]
            if static:
                comparisons.append(dict(cohort=path.parent.name,mode=mode,pattern=pattern,reference='default',
                    metric=metric,n=1,observed_difference=diffs[0]))
                continue
            rng=random.Random(7700)
            boot=sorted(statistics.median(rng.choices(diffs,k=samples)) for _ in range(5000))
            comparisons.append(dict(cohort=path.parent.name,mode=mode,pattern=pattern,reference='default',
                metric=metric,n=samples,median_paired_difference=statistics.median(diffs),
                ci95_low=boot[124],ci95_high=boot[4874]))
    output.mkdir(parents=True,exist_ok=True)
    write_csv(output/'memory-choices.csv',rows)
    write_csv(output/'memory-choices-comparisons.csv',comparisons)
    provenance=[dict(field=key,value=json.dumps(report[key],sort_keys=True)) for key in
                ('schema','benchmark_id','role','host','budget','arguments','binary_sha256',
                 'harness_sha256','helper_sha256','input_verification','host_verification')]
    provenance += [dict(field='report_sha256',value=digest(path)),
                   dict(field='resident_physical_metric',value='sum of product-process PSS; shared pages proportional; excludes unmapped file cache and kernel memory; full cgroup charge retained separately'),
                   dict(field='interference_policy',value=report.get('interference_policy','reject foreign VM/build activity')),
                   dict(field='build_receipt_sha256',value=digest(path.parent/'build-receipt.json')),
                   dict(field='input_manifest_sha256',value=digest(path.parent/'input-manifest.json')),
                   dict(field='statistics',value='single static observation; no quantiles or confidence intervals; 5-second window' if static else '30+ fresh paired samples; 5000 paired bootstrap resamples; P95 descriptive; separated clusters replace P50; no P99')]
    if static:
        affected=sum(any(job.get('kind')=='build/test' for observation in
            json.loads((path.parent/str(i)/'guard.json').read_text()) for job in observation['jobs'])
            for i in range(len(report['attempts'])))
        provenance.append(dict(field='conditions_with_background_builds',value=f"{affected}/{len(report['attempts'])}"))
    write_csv(output/'memory-choices-provenance.csv',provenance)
    return rows


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--static',action='store_true')
    args=parser.parse_args();publish(args.report,args.output,static=args.static)
