#!/usr/bin/env python3
"""Derived B-VM-MEMORY tables; preserve mechanisms, builds and cohorts separately."""
import argparse
import json
from pathlib import Path

from publication import distribution, write_csv
from publish_reference_campaign import paired_comparison
from reference_baselines import digest
from live_vm_memory import validate_report
from vm_memory import validate_restore


def validate_cohort(report):
    if report.get('benchmark_id')!='B-VM-MEMORY' or int(report['arguments']['samples'])<30 or report.get('failures'):
        raise ValueError('requires complete successful B-VM-MEMORY cohort with >=30 samples')
    live=report.get('mechanism','').startswith('current SDK')
    n=int(report['arguments']['samples'])
    actual=[(row['pattern'] if live else row['kind'],row['compressed'] if live else row['storage']=='compressed',row['trial']) for row in report['rows']]
    expected={(kind,compressed,trial) for kind in ('repeated','random') for compressed in (False,True) for trial in range(n)}
    if len(actual)!=len(set(actual)) or set(actual)!=expected:raise ValueError('incomplete or duplicate memory conditions')
    for row in report['rows']:
        if row.get('correctness')!='passed':raise ValueError('incorrect memory row')
        if live:
            proof=row['report']
            checked=validate_report(proof,dict(pattern=row['pattern'],compressed=row['compressed'],seed=20261006+row['trial']+int(report['arguments']['warmups']),cgroup=row['active']['cgroup']))
            if any(row.get(key)!=value for key,value in checked.items()):
                raise ValueError('published metrics differ from the SDK integrity report')
        else:validate_restore(row['ready'],row['restored'])
        phases=('active','offloaded','restored') if live else ('active','suspended','after')
        for phase in phases:
            value=row[phase]
            if value['current_bytes']<=0 or value['events'].get('oom',0) or value['events'].get('oom_kill',0):raise ValueError('invalid cgroup memory/OOM evidence')
            if any(name not in value['stat'] for name in ('anon','file','kernel')) or 'usage_usec' not in value['cpu']:
                raise ValueError('incomplete physical-memory or CPU accounting')
        usage=[row[phase]['cpu']['usage_usec'] for phase in phases]
        if usage!=sorted(usage):raise ValueError('nonmonotonic phase CPU accounting')
        for sample in row.get('samples',[]):
            if sample['events'].get('oom',0) or sample['events'].get('oom_kill',0):
                raise ValueError('resource monitor recorded an OOM')
    return live


def metric_values(row, live):
    if live:
        return dict(active_memory_mib=row['active']['current_bytes']/1024**2,
            parked_memory_mib=row['offloaded']['current_bytes']/1024**2,
            restored_memory_mib=row['restored']['current_bytes']/1024**2,
            backing_allocated_mib=row['allocated_bytes']/1024**2,
            offload_ms=row['offload_ms'],resume_ack_ms=row['offload_resume_ms'],
            heartbeat_ms=row['wake_heartbeat_ms'],warm_scan_ms=row['baseline_read_ms'],first_restored_scan_ms=row['restored_read_ms'],
            active_to_parked_cpu_ms=(row['offloaded']['cpu']['usage_usec']-row['active']['cpu']['usage_usec'])/1000,
            parked_to_restored_cpu_ms=(row['restored']['cpu']['usage_usec']-row['offloaded']['cpu']['usage_usec'])/1000,
            parked_anon_mib=row['offloaded']['stat']['anon']/1024**2,
            parked_file_mib=row['offloaded']['stat']['file']/1024**2,
            parked_kernel_mib=row['offloaded']['stat']['kernel']/1024**2)
    return dict(active_memory_mib=row['active']['current_bytes']/1024**2,
        parked_memory_mib=row['suspended']['current_bytes']/1024**2,
        after_completion_memory_mib=row['after']['current_bytes']/1024**2,
        retained_job_allocated_mib=row['retained_job_allocated_bytes']/1024**2,
        suspend_ms=row['suspend_ms'],resume_completion_ms=row['resume_completion_ms'],
        warm_scan_ms=row['ready']['warm_scan_ms'],first_restored_scan_ms=row['restored']['restored_scan_ms'],
        active_to_parked_cpu_ms=(row['suspended']['cpu']['usage_usec']-row['active']['cpu']['usage_usec'])/1000,
        parked_to_completed_cpu_ms=(row['after']['cpu']['usage_usec']-row['suspended']['cpu']['usage_usec'])/1000,
        parked_anon_mib=row['suspended']['stat']['anon']/1024**2,
        parked_file_mib=row['suspended']['stat']['file']/1024**2,
        parked_kernel_mib=row['suspended']['stat']['kernel']/1024**2)


def publish(paths, output):
    summary=[];comparisons=[];provenance=[];seen=set()
    for path in paths:
        report=json.loads(path.read_text());live=validate_cohort(report)
        mechanism='sdk-live-offload' if live else 'execution-snapshot'
        if mechanism in seen:raise ValueError('multiple cohorts for one mechanism must not be pooled')
        seen.add(mechanism);receipt=report['binary_build']
        if digest(path.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:raise ValueError('source receipt mismatch')
        binary=path.parent/'bin'/('vm_live_memory_bench' if live else 'pvisor')
        binary_sha=receipt['example_sha256'] if live else receipt['pvisor_sha256']
        if digest(binary)!=binary_sha:raise ValueError('binary receipt mismatch')
        sha=digest(path);args=report['arguments'];n=int(args['samples'])
        values={}
        for pattern in ('repeated','random'):
            for compressed in (False,True):
                selected=sorted((row for row in report['rows'] if (row['pattern'] if live else row['kind'])==pattern and (row['compressed'] if live else row['storage']=='compressed')==compressed),key=lambda row:row['trial'])
                cells=[(row['trial'],metric_values(row,live)) for row in selected]
                values[pattern,compressed]={key:[dict(trial=trial,value=value[key]) for trial,value in cells] for key in cells[0][1]}
                for key,measured in values[pattern,compressed].items():
                    summary.append(dict(benchmark_id='B-VM-MEMORY',cohort=path.parent.name,mechanism=mechanism,pattern=pattern,
                        storage='compressed' if compressed else 'raw',metric=key,unit='MiB' if key.endswith('_mib') else 'ms',
                        planned=n,failed=0,warmups=args['warmups'],memory_budget_mib=args['budget_mib'],cpu_affinity=args['cpu_affinity'],
                        **distribution([r['value'] for r in measured]),report_sha256=sha,binary_sha256=binary_sha))
            for metric in values[pattern,False]:
                comparison=paired_comparison(values[pattern,True][metric],values[pattern,False][metric])
                comparisons.append(dict(cohort=path.parent.name,mechanism=mechanism,pattern=pattern,metric=metric,
                    unit='MiB' if metric.endswith('_mib') else 'ms',comparison='compressed minus raw',n=n,
                    difference=comparison['difference_ms'],ci95_low=comparison['ci95_low_ms'],ci95_high=comparison['ci95_high_ms'],
                    outcome='separated distribution; inspect clusters' if comparison['ci95_low_ms']=='' else
                        ('lower' if comparison['ci95_high_ms']<0 else 'higher' if comparison['ci95_low_ms']>0 else 'no detected difference'),
                    confidence_method='5000 paired-round bootstrap resamples, seed 20261006',report_sha256=sha))
        provenance.extend(dict(cohort=path.parent.name,field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value)
            for key,value in report.items() if key not in ('rows','failures'))
        provenance.extend([dict(cohort=path.parent.name,field='report_sha256',value=sha),dict(cohort=path.parent.name,field='raw_location',value=str(path)),
            dict(cohort=path.parent.name,field='statistics',value='separate mechanisms/cohorts; complete 30+ paired rounds; no failures or timing exclusions; separated clusters replace P50; P95 descriptive; no P99; CPU phase deltas include background/control work')])
    output.mkdir(parents=True,exist_ok=True)
    write_csv(output/'memory-summary.csv',summary);write_csv(output/'memory-comparisons.csv',comparisons);write_csv(output/'memory-provenance.csv',provenance)
    return summary,comparisons


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--reports',nargs='+',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.reports,args.output)
