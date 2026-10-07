#!/usr/bin/env python3
"""B-VM-MEMORY: publish complete user sharing cohorts, never engineering runs."""
import argparse
import json
from pathlib import Path
import random
import statistics
from types import SimpleNamespace

from memory_sharing import ARMS,STRATEGIES,strategy_arms,matrix,validate
from memory_savings import digest,validate_observation
from publication import distribution,write_csv


def load_cohort(path,static=False):
    root=path.parent;report=json.loads(path.read_text());args=report['arguments']
    planned=[c for c in matrix(args.get("strategies",False),bool(args.get("pool_daemon"))) if not static or c[0]==4]
    if (report.get('schema')!='pvisor-memory-sharing-cohort/v1' or report.get('role')!='user-facing'
            or report.get('benchmark_id')!='B-VM-MEMORY'
            or report.get('budget')!=dict(cpu_cores=4,memory_max=2147483648,swap_max=0)
            or not report.get('complete') or args['preflight']
            or (static and (not args.get('static') or args['samples']!=1 or args['warmups']!=0 or args['scan_seconds']!=2))
            or (not static and (args['samples']<30 or args['warmups']<3 or args['scan_seconds']!=30))
            or (not static and args.get('static',False))
            or len(report['selected'])!=len(planned) or set(map(tuple,report['selected']))!=set(planned)
            or report.get('host_verification')!={'kernel':True,'cpu_model':True,'ksm':True,'host_cp_sha256':True}
            or report.get('input_verification')!={'rootfs':True,'firmware':True}):
        raise ValueError('requires complete 36-condition public sharing cohort')
    receipt=json.loads((root/'build-receipt.json').read_text());binary=root/'probe'
    if digest(binary)!=report['binary_sha256'] or receipt['example_sha256']!=report['binary_sha256']:
        raise ValueError('binary receipt mismatch')
    if digest(root/'source-manifest.json')!=receipt['source_manifest_sha256']:
        raise ValueError('source receipt mismatch')
    if set(report.get('harnesses',{}))!={'memory_sharing.py','memory_savings.py',
                                        'memory_scale.py','linux_cold_runtime.py'}:
        raise ValueError('incomplete harness inventory')
    for file,expected in report['harnesses'].items():
        if digest(root/file)!=expected:raise ValueError('harness mismatch')
    if args.get('pool_daemon'):
        pool=report.get('pool',{})
        if pool.get('binary_sha256')!=digest(root/'pool-daemon') or pool.get('build_receipt_sha256')!=digest(root/'pool-build-receipt.json') or pool.get('source_manifest_sha256')!=digest(root/'pool-source-manifest.json'):
            raise ValueError('daemon pool provenance mismatch')
    expected={(r,n,p,a) for r in range(-args['warmups'],args['samples']) for n,p,a in planned}
    records={};inputs=SimpleNamespace(rootfs=Path(args['rootfs']),firmware=Path(args['firmware']))
    for index,row in enumerate(report['attempts']):
        c=row['condition'];key=(row['round'],c['vms'],c['pattern'],row['arm'])
        if key not in expected or key in records or row['warmup']!=(row['round']<0):
            raise ValueError('unplanned/duplicate trial')
        wanted=dict(vms=c['vms'],pattern=c['pattern'],**(strategy_arms(bool(args.get("pool_daemon"))) if args.get("strategies") else ARMS)[row['arm']],cpus=2,
                    seed=20261006,settle_ms=500,ksm_wait_seconds=args['scan_seconds'])
        if row['arm']=='daemon-pool':wanted['pool_daemon']=args['pool_daemon']
        if 'memory_mib' in args:wanted['memory_mib']=args['memory_mib']
        if c!=wanted:raise ValueError('condition does not match planned arm')
        if row['status']!='successful' or row['returncode']!=0 or not row['unit_quiescent']:
            raise ValueError('failed trial')
        trial=root/str(index);raw_path=trial/'w/raw.json'
        if digest(raw_path)!=row['raw_sha256']:raise ValueError('raw mismatch')
        if digest(trial/'retired-runtime-manifest.json')!=row['retired_manifest_sha256']:
            raise ValueError('runtime inventory mismatch')
        raw=json.loads(raw_path.read_text())
        # Conditions bind the original command paths; retained artifacts may be relocated.
        validate(raw,c,inputs,binary,Path(raw['conditions']['output']))
        monitor=json.loads((trial/'monitor.json').read_text())
        validate_observation(monitor,json.loads((trial/'guard.json').read_text()),allow_builds=static)
        if any(x['group']!=raw['before']['cgroup'] for x in monitor):raise ValueError('observer group mismatch')
        values={'peak_mib':max([x['memory_peak'] for x in monitor]+
                              [int(raw['after']['counters']['memory.peak']['raw'])])/1024**2}
        prior=raw['before']['counters']['cpu.stat']['raw']
        prior_cpu=int(next(line.split()[1] for line in prior.splitlines() if line.startswith('usage_usec ')))
        for phase in raw['phases']:
            counters=phase['accounting']['counters'];name=phase['name']
            values[name+'_mib']=int(counters['memory.current']['raw'])/1024**2
            processes=phase['accounting']['processes']
            if processes and all('Pss' in p['smaps_totals_bytes'] for p in processes):
                values[name+'_physical_mib']=sum(p['smaps_totals_bytes']['Pss'] for p in processes)/1024**2
            stat={line.split()[0]:int(line.split()[1]) for line in counters['memory.stat']['raw'].splitlines()}
            for field in ('anon','file','kernel'):values[name+'_'+field+'_mib']=stat[field]/1024**2
            cpu=int(next(line.split()[1] for line in counters['cpu.stat']['raw'].splitlines() if line.startswith('usage_usec ')))
            values[name+'_cpu_ms']=(cpu-prior_cpu)/1000;prior_cpu=cpu
        for check_name,metric,percent in (
                ('ready_full_digest','ready_per_vm_scan_ms',None),
                ('dynamic_private_after_wait_full_digest','dynamic_private_per_vm_scan_ms',None),
                ('cow_full_digest','cow25_per_vm_checked_write_ms',25),
                ('cow_full_digest','cow100_per_vm_checked_write_ms',100)):
            scans=[x['evidence']['read_ms'] for x in raw['checks']
                   if x['name']==check_name and
                   (percent is None or x['evidence']['percent']==percent)]
            if scans:values[metric]=statistics.median(scans)
        records[key]=values
    if records.keys()!=expected:raise ValueError('missing formal/warmup trial')
    return report,records


def publish(path,output,static=False):
    report,records=load_cohort(path,static=static);samples=report['arguments']['samples'];rows=[];comparisons=[]
    for n,pattern,arm in report['selected']:
        values=[records[r,n,pattern,arm] for r in range(samples)]
        for metric in values[0]:
            rows.append(dict(benchmark_id='B-VM-MEMORY',cohort=path.parent.name,vms=n,pattern=pattern,
                vm_memory_mib=report['arguments'].get('memory_mib',256),
                arm=arm,metric=metric,unit='MiB' if metric.endswith('_mib') else 'ms',
                **(dict(n=1,value=values[0][metric]) if static else
                   distribution([v[metric] for v in values]))))
        reference=({'snapshot-cow':'unshared','ksm':'unshared','daemon-pool':'unshared'} if report['arguments'].get('strategies') else {'shared':'independent','ksm-on':'ksm-off'}).get(arm)
        if reference:
            metrics=('ready_mib','cow25_mib','cow100_mib') if arm in ('shared','snapshot-cow','ksm','daemon-pool') else (
                'dynamic_private_after_wait_mib','cow25_mib','cow100_mib','dynamic_private_after_wait_cpu_ms')
            metrics=list(metrics)
            metrics += [metric[:-4]+'_physical_mib' for metric in metrics.copy()
                        if metric.endswith('_mib') and metric[:-4]+'_physical_mib' in values[0]]
            for metric in metrics:
                differences=[records[r,n,pattern,arm][metric]-records[r,n,pattern,reference][metric] for r in range(samples)]
                if static:
                    physical=metric.endswith('_physical_mib')
                    baseline=records[0,n,pattern,reference][metric]
                    current=records[0,n,pattern,arm][metric]
                    if physical and baseline <= 0:raise ValueError('nonpositive resident baseline')
                    comparisons.append(dict(cohort=path.parent.name,vms=n,pattern=pattern,arm=arm,reference=reference,
                        metric=metric,n=1,observed_difference=differences[0],
                        reference_per_instance_mib=baseline/n if physical else '',
                        current_per_instance_mib=current/n if physical else '',
                        resident_saving_pct=100*(baseline-current)/baseline if physical else ''))
                    continue
                rng=random.Random(7800)
                boot=sorted(statistics.median(rng.choices(differences,k=samples)) for _ in range(5000))
                comparisons.append(dict(cohort=path.parent.name,vms=n,pattern=pattern,arm=arm,reference=reference,
                    metric=metric,n=samples,median_paired_difference=statistics.median(differences),
                    ci95_low=boot[124],ci95_high=boot[4874]))
    output.mkdir(parents=True,exist_ok=True)
    write_csv(output/'memory-sharing.csv',rows);write_csv(output/'memory-sharing-comparisons.csv',comparisons)
    provenance=[dict(field=k,value=json.dumps(report[k],sort_keys=True)) for k in
                ('schema','benchmark_id','role','host','budget','arguments','binary_sha256','harnesses','input_verification','host_verification')]
    provenance +=[dict(field='report_sha256',value=digest(path)),dict(field='build_receipt_sha256',value=digest(path.parent/'build-receipt.json')),
                 dict(field='resident_physical_metric',value='sum of product-process PSS; shared pages proportional; excludes unmapped file cache and kernel memory; full cgroup charge retained separately'),
                 dict(field='interference_policy',value=report.get('interference_policy','reject foreign VM/build activity')),
                 dict(field='input_manifest_sha256',value=digest(path.parent/'input-manifest.json')),
                 dict(field='statistics',value='single static group observation; 2-second scan windows; no quantiles or confidence intervals; per-VM duration is median within group' if static else 'group is independent sample; per-VM timing is median within each group, not complete group elapsed time; writes at 25/100% remain separate; 5000 paired bootstrap resamples; P95 descriptive; separated clusters replace P50; no P99')]
    if static:
        affected=sum(any(job.get('kind')=='build/test' for observation in
            json.loads((path.parent/str(i)/'guard.json').read_text()) for job in observation['jobs'])
            for i in range(len(report['attempts'])))
        provenance.append(dict(field='conditions_with_background_builds',value=f"{affected}/{len(report['attempts'])}"))
    write_csv(output/'memory-sharing-provenance.csv',provenance)
    return rows


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--report',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True);parser.add_argument('--static',action='store_true')
    args=parser.parse_args();publish(args.report,args.output,static=args.static)
