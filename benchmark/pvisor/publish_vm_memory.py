#!/usr/bin/env python3
"""Derived B-VM-MEMORY tables; preserve mechanisms, builds and cohorts separately."""
import argparse
import json
import math
from pathlib import Path

from publication import distribution, write_csv
from publish_reference_campaign import paired_comparison
from reference_baselines import digest
from live_vm_memory import validate_report
from vm_memory import validate_restore


def evidence_file(root, path):
    root=root.resolve();path=Path(path)
    if not path.is_absolute():path=root/path
    if not path.resolve().is_relative_to(root) or not path.is_file():
        raise ValueError('missing or outside-cohort memory evidence')
    return path


def validate_row_evidence(root, report, row, live):
    directory=evidence_file(root,Path(row['logs'])/'result.json').parent
    retained=json.loads((directory/'result.json').read_text())
    expected=row if live else {key:value for key,value in row.items() if key!='logs'}
    if retained!=expected:raise ValueError('retained memory result differs from aggregate')
    config_path=evidence_file(root,directory/'config.json');config=json.loads(config_path.read_text())
    args=report['arguments']
    if (Path(config['root']).resolve()!=directory.resolve()
            or Path(config['result']).resolve()!=(directory/'result.json').resolve()
            or config['trial']!=row['trial']
            or config['budget_bytes']!=int(args['budget_mib'])*1024**2
            or config['cpu_affinity']!=args['cpu_affinity']):
        raise ValueError('retained memory config differs from measured condition')
    checked=[directory/'result.json',config_path]
    if live:
        if (config['pattern']!=row['pattern'] or config['compressed']!=row['compressed']
                or config['seed']!=20261006+row['trial']+int(args['warmups'])):
            raise ValueError('retained SDK memory condition differs')
        proof=evidence_file(root,directory/'vm/raw.json')
        if json.loads(proof.read_text())!=row['report']:
            raise ValueError('retained SDK proof differs from aggregate')
        checked.append(proof)
    else:
        if (config['kind']!=row['kind'] or config['storage']!=row['storage']
                or config['payload_bytes']!=64*1024**2
                or row['ready'].get('bytes')!=64*1024**2
                or row['restored'].get('bytes')!=64*1024**2):
            raise ValueError('retained snapshot memory condition differs')
        for filename,prefix,key in (('run.stdout','PVISOR_MEMORY_READY ','ready'),
                ('resume.stdout','PVISOR_MEMORY_RESULT ','restored')):
            output=evidence_file(root,directory/filename)
            markers=[json.loads(line[len(prefix):]) for line in output.read_text().splitlines() if line.startswith(prefix)]
            if markers!=[row[key]]:raise ValueError('retained snapshot output differs from aggregate')
            checked.append(output)
        for filename in ('suspend.stdout','suspend.json'):
            output=evidence_file(root,directory/filename)
            if json.loads(output.read_text()).get('state')!='suspended':
                raise ValueError('retained snapshot suspension not confirmed')
            checked.append(output)
    return dict(trial=row['trial'],pattern=row['pattern'] if live else row['kind'],
        storage=('compressed' if row['compressed'] else 'raw') if live else row['storage'],
        files=[dict(path=str(path.relative_to(root)),sha256=digest(path)) for path in checked])


def validate_publication_eligibility(root):
    sidecar=root/'publication-eligibility.json'
    if sidecar.exists():
        eligibility=json.loads(evidence_file(root,sidecar).read_text())
        if eligibility.get('eligible_for_public_causal_comparison') is not True:
            raise ValueError('cohort ineligible for public causal comparison')


def validate_host_attempts(report):
    # Legacy cohorts retain their original provenance, not today's guard protocol.
    if 'host_guard' not in report.get('protocol',{}):return False
    if report['protocol']['host_guard'].get('enabled') is not True or 'stopped' in report:
        raise ValueError('host guard disabled or campaign stopped')
    n=int(report['arguments']['samples']);warmups=int(report['arguments']['warmups'])
    attempts=report.get('attempts',[])
    identity=lambda row:(row['pattern'],row['compressed'],row['trial'])
    expected={(pattern,compressed,trial) for pattern in ('repeated','random')
        for compressed in (False,True) for trial in range(-warmups,n)}
    actual=[identity(attempt) for attempt in attempts]
    if warmups<0 or len(actual)!=len(set(actual)) or set(actual)!=expected:
        raise ValueError('incomplete or duplicate guarded memory attempts, including warmups')
    rows={identity(row):row for row in report['rows']}
    for attempt in attempts:
        if (attempt.get('correctness')!='passed' or attempt.get('host_admitted') is not True
                or attempt.get('unit_quiescent') is not True or attempt.get('deadline') is not False
                or attempt.get('returncode')!=0 or attempt.get('host_interference')!=[]
                or attempt.get('host_guard_errors')!=[]):
            raise ValueError('host admission/interference/guard or service failure')
        result=attempt.get('result',{})
        if (any(result.get(key)!=attempt[key] for key in ('pattern','compressed','trial','logs'))
                or result.get('correctness')!='passed'
                or (attempt['trial']>=0 and result!=rows.get(identity(attempt)))):
            raise ValueError('guarded attempt result differs from memory rows')
        checked=validate_report(result['report'],dict(pattern=attempt['pattern'],compressed=attempt['compressed'],
            seed=20261006+attempt['trial']+warmups,cgroup=result['active']['cgroup']))
        if any(result.get(key)!=value for key,value in checked.items()):
            raise ValueError('guarded attempt metrics differ from SDK integrity report')
    return True


def validate_host_evidence(root, attempt):
    directory=evidence_file(root,Path(attempt['logs'])/'service-result.json').parent
    checked=[]
    def read(name, lines=False):
        path=evidence_file(root,directory/name);checked.append(path)
        return [json.loads(line) for line in path.read_text().splitlines()] if lines else json.loads(path.read_text())
    retained=read('service-result.json')
    expected={key:value for key,value in attempt.items() if key not in ('host_admitted','trial','pattern','compressed')}
    if retained!=expected:raise ValueError('retained service result differs from guarded attempt')
    launch=read('service-launch.json')
    if any(launch.get(key)!=attempt.get(key) for key in ('unit','command','started_at','timeout_seconds','deadline','logs')):
        raise ValueError('retained service launch differs from guarded attempt')
    admission=read('prelaunch-wait.jsonl',True)
    quiet=[]
    for sample in admission:
        if 'jobs' not in sample or not valid_number(sample.get('time_ns')):
            raise ValueError('missing host admission proof')
        if sample['jobs']:quiet=[]
        else:quiet.append(sample['time_ns'])
    if (len(quiet)<2 or quiet!=sorted(quiet) or quiet[-1]-quiet[0]<30*10**9
            or admission[-1]['time_ns']-admission[0]['time_ns']>180*10**9):
        raise ValueError('retained host admission lacks complete quiet window')
    guard=read('host-guard.jsonl',True)
    if (len(guard)<2 or any(sample.get('jobs')!=[] or 'guard_error' in sample
            or not valid_number(sample.get('time_ns')) for sample in guard)
            or [sample['time_ns'] for sample in guard]!=sorted(sample['time_ns'] for sample in guard)):
        raise ValueError('retained host guard interference/failure or missing proof')
    proof=read('service-quiescence.json');state=proof.get('state',{})
    final=evidence_file(root,directory/'unit-final.txt');checked.append(final)
    final_state=dict(line.split('=',1) for line in final.read_text().splitlines() if '=' in line)
    if (proof.get('unit')!=attempt['unit'] or proof.get('unit_quiescent') is not True
            or proof.get('show_returncode')!=0 or state!=final_state
            or not (state.get('ActiveState') in ('inactive','failed') or state.get('LoadState')=='not-found')):
        raise ValueError('retained service quiescence not proven')
    return [dict(path=str(path.relative_to(root)),sha256=digest(path)) for path in checked]


def validate_retained_evidence(root, report, live):
    root=root.resolve();harness=root/'harness'
    validate_publication_eligibility(root)
    if digest(evidence_file(root,'rootfs-manifest.json'))!=report['rootfs_manifest_sha256']:
        raise ValueError('retained memory input manifest hash mismatch')
    if json.loads(evidence_file(root,'build-receipt.json').read_text())!=report['binary_build']:
        raise ValueError('retained memory build receipt differs from report')
    inventory=report['harness_sha256']
    actual={str(path.relative_to(harness)) for path in harness.rglob('*.py')}
    if actual!=set(inventory):raise ValueError('incomplete retained memory harness inventory')
    for name,sha in inventory.items():
        if digest(evidence_file(root,harness/name))!=sha:raise ValueError('retained memory harness hash mismatch')
    if live and validate_host_attempts(report):
        evidence=[]
        for attempt in report['attempts']:
            row=validate_row_evidence(root,report,attempt['result'],True)
            row['files'].extend(validate_host_evidence(root,attempt));evidence.append(row)
        return evidence
    return [validate_row_evidence(root,report,row,live) for row in report['rows']]


def validate_cohort(report):
    if report.get('benchmark_id')!='B-VM-MEMORY' or int(report['arguments']['samples'])<30 or report.get('failures'):
        raise ValueError('requires complete successful B-VM-MEMORY cohort with >=30 samples')
    live=report.get('mechanism','').startswith('current SDK')
    if live:validate_host_attempts(report)
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
        else:
            validate_restore(row['ready'],row['restored'])
            if row['ready'].get('bytes')!=64*1024**2:raise ValueError('snapshot proof lacks complete private data')
        phases=('active','offloaded','restored') if live else ('active','suspended','after')
        for phase in phases:
            value=row[phase]
            if value['current_bytes']<=0 or value['events'].get('oom',0) or value['events'].get('oom_kill',0):raise ValueError('invalid cgroup memory/OOM evidence')
            if any(name not in value['stat'] for name in ('anon','file','kernel')) or 'usage_usec' not in value['cpu']:
                raise ValueError('incomplete physical-memory or CPU accounting')
            if any(not valid_number(number) for number in [value['current_bytes'],value['cpu']['usage_usec'],
                    *(value['stat'][name] for name in ('anon','file','kernel'))]):
                raise ValueError('nonfinite or negative physical-memory/CPU metric')
        timing=('offload_ms','offload_resume_ms','baseline_read_ms','restored_read_ms') if live else ('suspend_ms','resume_completion_ms')
        optional=('allocated_bytes','backed_bytes','pause_ms','resume_ms','wake_heartbeat_ms','resident_before_bytes',
            'resident_after_bytes') if live else ('retained_job_bytes','retained_job_allocated_bytes')
        if any(not valid_number(row[key]) for key in timing) or any(not valid_number(row[key]) for key in optional if key in row):
            raise ValueError('nonfinite or negative memory/timing metric')
        if not live and any(not valid_number(row[key][field]) for key,field in (('ready','warm_scan_ms'),('restored','restored_scan_ms'))):
            raise ValueError('nonfinite or negative guest scan metric')
        usage=[row[phase]['cpu']['usage_usec'] for phase in phases]
        if usage!=sorted(usage):raise ValueError('nonmonotonic phase CPU accounting')
        for sample in row.get('samples',[]):
            if sample['events'].get('oom',0) or sample['events'].get('oom_kill',0):
                raise ValueError('resource monitor recorded an OOM')
    return live


def valid_number(value):
    return isinstance(value,(int,float)) and not isinstance(value,bool) and math.isfinite(value) and value>=0


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
        validate_publication_eligibility(path.parent)
        report=json.loads(path.read_text());live=validate_cohort(report)
        mechanism='sdk-live-offload' if live else 'execution-snapshot'
        if mechanism in seen:raise ValueError('multiple cohorts for one mechanism must not be pooled')
        seen.add(mechanism);receipt=report['binary_build']
        if digest(path.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:raise ValueError('source receipt mismatch')
        binary=path.parent/'bin'/('vm_live_memory_bench' if live else 'pvisor')
        binary_sha=receipt['example_sha256'] if live else receipt['pvisor_sha256']
        if digest(binary)!=binary_sha:raise ValueError('binary receipt mismatch')
        evidence=validate_retained_evidence(path.parent,report,live)
        audit=path.parent/'memory-output-evidence-audit.json'
        audit.write_text(json.dumps(dict(report_sha256=digest(path),rows=evidence),indent=2)+'\n')
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
            dict(cohort=path.parent.name,field='memory_output_evidence_audit_sha256',value=digest(audit)),
            dict(cohort=path.parent.name,field='statistics',value='separate mechanisms/cohorts; complete 30+ paired rounds; no failures or timing exclusions; separated clusters replace P50; P95 descriptive; no P99; CPU phase deltas include background/control work')])
    output.mkdir(parents=True,exist_ok=True)
    write_csv(output/'memory-summary.csv',summary);write_csv(output/'memory-comparisons.csv',comparisons);write_csv(output/'memory-provenance.csv',provenance)
    return summary,comparisons


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--reports',nargs='+',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.reports,args.output)
