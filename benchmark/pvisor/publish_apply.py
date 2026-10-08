#!/usr/bin/env python3
"""Publish complete B-APPLY cohorts and distinguish actual crash-window hits."""
import argparse
import json
import math
from pathlib import Path

from publication import distribution, write_csv
from publish_density import verify_harness
from publish_reference_campaign import paired_comparison
from reference_baselines import digest, verified_build_receipt


def summarize(report):
    if report.get('benchmark_id')!='B-APPLY':raise ValueError('not an apply report')
    protocol=report['apply_protocol'];actions=protocol['actions'];sizes=protocol['sizes']
    if set(actions)!={'apply','drop','conflict','copy','git-apply'} or len(actions)!=5:
        raise ValueError('missing apply operations or Git control')
    planned={int(k):int(v) for k,v in protocol['samples_per_size'].items()}
    if not sizes or min(sizes)<1 or len(sizes)!=len(set(sizes)) or set(sizes)!=set(planned) or any(v<1 for v in planned.values()):
        raise ValueError('invalid planned size conditions')
    expected={(size,action,trial) for size in sizes for action in actions for trial in range(planned[size])}
    timing=[row for row in report['rows'] if row['workload']!='crash-recovery']
    keys=[(row['files'],row['workload'],row['trial']) for row in timing]
    if len(keys)!=len(set(keys)) or set(keys)-expected:raise ValueError('duplicate or unexpected timing samples')
    failed=[]
    for name,capability in report.get('capabilities',{}).items():
        if not name.startswith('apply/'):continue
        _,size,action=name.split('/')
        for failure in capability.get('failures',[]):
            if failure['trial']>=0:failed.append((int(size),action,failure['trial']))
    if len(failed)!=len(set(failed)) or set(keys)&set(failed) or set(keys)|set(failed)!=expected:
        raise ValueError('incomplete or conflicting attempted conditions')
    if any(row.get('correctness')!='passed' or not math.isfinite(row['wall_ms']) or row['wall_ms']<=0 for row in timing):
        raise ValueError('incorrect or invalid timing sample')
    summary=[]
    for size in sizes:
        for action in actions:
            values=[row['wall_ms'] for row in timing if row['files']==size and row['workload']==action]
            stats=distribution(values) if values else {k:'' for k in distribution([0])}
            stats['n']=len(values)
            summary.append(dict(files=size,operation=action,unit='ms',planned_samples=planned[size],
                failed_samples=sum(key[:2]==(size,action) for key in failed),
                warmups=protocol['warmups_per_size'][str(size)],**stats,
                timing=protocol['timing'],statistics='separated clusters replace P50; P95 descriptive only at N>=30; no P99; min/max are observed ranges'))
    crash_protocol=report['crash_protocol'];states=crash_protocol['states'];count=crash_protocol['files']
    repetitions=min(int(report['cli_arguments']['samples']),3)
    if set(states)!={'prepared','target_applied','committed'} or len(states)!=3:
        raise ValueError('missing crash states')
    crashes=[row for row in report['rows'] if row['workload']=='crash-recovery']
    crash_keys=[(row['files'],row['requested_state'],row['trial']) for row in crashes]
    crash_expected={(count,state,trial) for state in states for trial in range(repetitions)}
    if len(crash_keys)!=len(set(crash_keys)) or set(crash_keys)!=crash_expected:
        raise ValueError('incomplete or duplicate crash probes')
    for row in crashes:
        if (row.get('correctness')!='passed' or type(row.get('kill_window_hit')) is not bool
                or row['kill_window_hit']!=(row['requested_state']==row['state_at_death'])
                or not math.isfinite(row['recovery_ms']) or row['recovery_ms']<=0):
            raise ValueError('invalid recovery or durable-state evidence')
    recovery=[]
    for state in states:
        selected=[row for row in crashes if row['requested_state']==state]
        hits=[row for row in selected if row['kill_window_hit']]
        stats=distribution([row['recovery_ms'] for row in hits]) if hits else {k:'' for k in distribution([0])}
        stats['n']=len(hits)
        recovery.append(dict(files=count,requested_state=state,planned_probes=repetitions,
            actual_window_hits=len(hits),missed_windows=len(selected)-len(hits),
            recovered_results=len(selected),unit='ms',**stats,
            scope='recovery timing only for actual requested-state hits; SIGKILL process interruption, not power loss or storage corruption'))
    return summary,recovery


def publish(path, output):
    report=json.loads(path.read_text())
    receipt=verified_build_receipt(path.parent/'build-receipt.json',path.parent/'bin/pvisor')
    if receipt!=report['binary_build'] or receipt['pvisor_sha256']!=report['binary_sha256']:
        raise ValueError('report disagrees with retained build receipt')
    verify_harness(report,path.parent)
    summary,recovery=summarize(report);report_sha=digest(path)
    identity=dict(cohort=path.parent.name,recorded_at=report['recorded_at'],report_sha256=report_sha,
        binary_sha256=receipt['pvisor_sha256'],source_manifest_sha256=receipt['source_manifest_sha256'],
        cpu_affinity=report['cli_arguments']['cpu_affinity'],memory_limit='no benchmark-specific host memory cap')
    for row in summary+recovery:row.update(identity)
    comparisons=[]
    for size in report['apply_protocol']['sizes']:
        candidate=[dict(trial=row['trial'],value=row['wall_ms']) for row in report['rows'] if row['files']==size and row['workload']=='apply']
        control=[dict(trial=row['trial'],value=row['wall_ms']) for row in report['rows'] if row['files']==size and row['workload']=='git-apply']
        complete=len(candidate)==len(control)==int(report['apply_protocol']['samples_per_size'][str(size)])
        result=paired_comparison(candidate,control) if complete and len(candidate)>=30 else dict(
            n=min(len(candidate),len(control)),difference_ms='',ci95_low_ms='',ci95_high_ms='',conclusion='incomplete or fewer than 30 pairs; descriptive levels only')
        comparisons.append(dict(files=size,candidate='pvisor apply',control='git apply',**result,
            method='5000 paired-round bootstrap, seed 20261006; CI only at >=30 complete pairs; operation semantics differ',**identity))
    provenance=[dict(field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value)
        for key,value in (identity|dict(benchmark_id='B-APPLY',host_platform=report['platform'],cpu=report['cpu'],
            protocol=report['apply_protocol'],crash_protocol=report['crash_protocol'],
            conflict_scope='this timing cohort tests external edits before apply; mid-apply external edits require a separate retained correctness cohort and audit')).items()]
    output.mkdir(parents=True,exist_ok=True)
    for name,rows in [('apply.csv',summary),('apply-recovery.csv',recovery),('apply-comparisons.csv',comparisons),('apply-provenance.csv',provenance)]:write_csv(output/name,rows)
    return summary,recovery,comparisons


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.report,args.output)
