#!/usr/bin/env python3
"""Publish B-DENSITY capacity without hiding failed or unknown task outcomes."""
import argparse
from collections import Counter
import json
from pathlib import Path

from publication import distribution, write_csv
from reference_baselines import digest, validate_bundle_execution


def validated_completed(row, useful):
    """Only retained, integrity-checked results count as known completions."""
    outcomes=row.get('outcomes',[])
    if len(outcomes)>row['concurrency']:raise ValueError('too many task outcomes')
    tokens=[];completed=0
    for outcome in outcomes:
        if outcome.get('correctness')!='passed':continue
        result=outcome['result']
        if (result.get('integrity')!='passed' or result.get('bytes')!=(32*1024**2 if useful else 0)
                or result.get('changes')!=(4 if useful else 0) or not result.get('token')):
            raise ValueError('unverified task counted as completed')
        tokens.append(outcome.get('execution_identity',result['token']));completed+=1
    if len(tokens)!=len(set(tokens)):raise ValueError('duplicate completed-task token')
    if 'completed' in row and row['completed']!=completed:raise ValueError('completion count differs from retained outcomes')
    if row['correctness']=='passed' and (completed!=row['concurrency'] or not row.get('all_ready')
            or row.get('ready')!=row['concurrency'] or row.get('failed')!=0):
        raise ValueError('incomplete successful batch')
    return completed,len(outcomes)-completed,row['concurrency']-len(outcomes)


def verify_execution_identities(report, directory):
    """Bind staged completions to their independent retained Run and output."""
    directory=directory.resolve();sources={};identities=set()
    for row in report['rows']+report['failures']:
        for outcome in row.get('outcomes',[]):outcome.pop('execution_identity',None)
        if row['backend'] not in ('staged','vm'):continue
        for outcome in row.get('outcomes',[]):
            if outcome.get('correctness')!='passed':continue
            root=Path(outcome['logs']).resolve()
            if not root.is_relative_to(directory):raise ValueError('execution evidence outside cohort')
            stdout=root/'stdout.log';bundle_path=root/'stage/run-bundle.json'
            if any(not path.resolve().is_relative_to(directory) for path in (stdout,bundle_path)):
                raise ValueError('execution evidence outside cohort')
            markers={}
            for prefix,key in [('PVISOR_DENSITY_READY ','ready'),('PVISOR_DENSITY_RESULT ','result')]:
                values=[json.loads(line.removeprefix(prefix)) for line in stdout.read_text().splitlines() if line.startswith(prefix)]
                if len(values)!=1:raise ValueError('missing or duplicate retained worker marker')
                markers[key]=values[0]
            if markers['result']!=outcome['result'] or any(markers['ready'].get(key)!=markers['result'].get(key) for key in ('token','bytes','checksum')):
                raise ValueError('retained output does not match completion')
            bundle=json.loads(bundle_path.read_text())
            validate_bundle_execution(bundle,'pvisor-vm' if row['backend']=='vm' else 'pvisor-staged','rootless_process')
            run=bundle['run']
            if run.get('state')!='completed' or run.get('exit_code')!=0 or not run.get('run_id') or not run.get('attempt_id'):
                raise ValueError('missing successful independent Run identity')
            identity=(run['run_id'],run['attempt_id'])
            if identity in identities:raise ValueError('duplicate retained execution identity')
            identities.add(identity);outcome['execution_identity']='/'.join(identity)
            for path in (stdout,bundle_path):sources[str(path.relative_to(directory))]=digest(path)
    return sources


def has_oom(row):
    values=[row[name] for name in ('before','barrier','after') if name in row]+row.get('samples',[])
    if 'last_sampler' in row:values.append(row['last_sampler'])
    return any(value['events'].get('oom',0) or value['events'].get('oom_kill',0) for value in values)


def recover_last_samples(report, directory):
    """Failed reporters may retain a last point, never a complete final result."""
    sources={};directory=directory.resolve()
    for row in report['failures']:
        if 'after' in row:continue
        root=Path(row['logs']).resolve()
        if not root.is_relative_to(directory):raise ValueError('sampler evidence outside retained cohort')
        sample=root/'last-memory.json'
        if sample.is_file():
            value=json.loads(sample.read_text())
            if not all(key in value for key in ('current_bytes','peak_bytes','events','stat','cpu')):
                raise ValueError('incomplete retained sampler point')
            row['last_sampler']=value;sources[str(sample.relative_to(directory))]=digest(sample)
    return sources


def verify_harness(report, directory):
    root=(directory/'harness').resolve()
    expected=report.get('harness_sha256',{})
    actual={str(path.relative_to(root)):path for path in root.rglob('*.py')}
    if not expected or set(actual)!=set(expected):
        raise ValueError('missing or different retained harness inventory')
    for name,path in actual.items():
        if not path.resolve().is_relative_to(root) or digest(path)!=expected[name]:
            raise ValueError('retained harness provenance mismatch')


def summarize(report):
    if report.get('benchmark_id')!='B-DENSITY':raise ValueError('not a density report')
    args=report['arguments'];n=int(args['samples'])
    if n<5:raise ValueError('capacity publication requires five complete rounds')
    backends=args['backends'].split(',');workloads=args['workloads'].split(',')
    levels=list(map(int,args['concurrencies'].split(',')))
    expected={(b,w,c,t) for b in backends for w in workloads for c in levels for t in range(n)}
    rows=report['rows']+report['failures']
    keys=[(r['backend'],r['workload'],r['concurrency'],r['trial']) for r in rows]
    if len(keys)!=len(set(keys)) or set(keys)!=expected:raise ValueError('missing or duplicate attempted conditions')
    summary=[]
    for backend in backends:
        for workload in workloads:
            for count in levels:
                selected=[r for r in rows if (r['backend'],r['workload'],r['concurrency'])==(backend,workload,count)]
                completions=failures=unknown=oom=full=resource_unknown=0;timings=[];occupancy=[];peaks=[];reasons=Counter()
                for row in selected:
                    done,bad,missing=validated_completed(row,workload=='useful')
                    completions+=done;failures+=bad;unknown+=missing
                    oom_here=has_oom(row);oom+=int(oom_here)
                    resource_unknown+=int('after' not in row)
                    if row.get('error'):reasons[row['error']]+=1
                    if 'budget_bytes' in row and row['budget_bytes']!=int(args['budget_mib'])*1024**2:
                        raise ValueError('batch has a different memory budget')
                    complete=row['correctness']=='passed' and not oom_here
                    if complete:
                        if 'after' not in row or 'budget_bytes' not in row:
                            raise ValueError('successful batch lacks final resource evidence')
                        if row['wall_ms']<=0:raise ValueError('missing batch completion time')
                        full+=1;timings.append(row['wall_ms'])
                        occupancy.append(row['barrier']['current_bytes']/1024**2)
                    observations=[row[name] for name in ('before','barrier','after') if name in row]+row.get('samples',[])
                    if 'last_sampler' in row:observations.append(row['last_sampler'])
                    if observations:peaks.append(max(value['peak_bytes'] for value in observations)/1024**2)
                summary.append(dict(backend=backend,workload=workload,concurrency=count,planned_batches=n,
                    full_valid_no_oom_batches=full,failed_or_unknown_or_oom_batches=n-full,oom_batches=oom,
                    resource_evidence_unknown_batches=resource_unknown,failure_reasons=json.dumps(reasons,sort_keys=True),
                    attempted_tasks=count*n,validated_completed_tasks=completions,known_failed_tasks=failures,
                    unknown_tasks=unknown,observed_all_rounds_succeeded=full==n,
                    memory_budget_mib=args['budget_mib'],cpu_affinity=args['cpu_affinity'],swap_bytes=0,
                    full_batch_wall_distribution=json.dumps(distribution(timings),sort_keys=True) if timings else '',
                    barrier_memory_distribution_mib=json.dumps(distribution(occupancy),sort_keys=True) if occupancy else '',
                    observed_peak_min_mib=min(peaks) if peaks else '',observed_peak_max_mib=max(peaks) if peaks else '',
                    statistics=f'{n}-round occupancy/completion evidence; no tail-latency or sustained reliability guarantee; timing only full no-OOM batches'))
    return summary


def publish(path, output):
    report=json.loads(path.read_text());receipt=report['binary_build']
    if digest(path.parent/'bin/pvisor')!=receipt['pvisor_sha256']:raise ValueError('binary provenance mismatch')
    if digest(path.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:raise ValueError('source provenance mismatch')
    if digest(path.parent/'input-manifest.json')!=report['input_manifest_sha256']:raise ValueError('input provenance mismatch')
    verify_harness(report,path.parent)
    execution_sources=verify_execution_identities(report,path.parent)
    sampler_sources=recover_last_samples(report,path.parent)
    rows=summarize(report);sha=digest(path)
    for row in rows:row.update(cohort=path.parent.name,report_sha256=sha,binary_sha256=receipt['pvisor_sha256'])
    output.mkdir(parents=True,exist_ok=True);write_csv(output/'density-summary.csv',rows)
    provenance=[dict(cohort=path.parent.name,field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value)
                for key,value in report.items() if key not in ('rows','failures')]
    provenance.append(dict(cohort=path.parent.name,field='report_sha256',value=sha))
    provenance.append(dict(cohort=path.parent.name,field='retained_last_sampler_sha256',value=json.dumps(sampler_sources,sort_keys=True)))
    audit=path.parent/'density-execution-evidence-audit.json'
    audit.write_text(json.dumps(dict(report_sha256=sha,execution_evidence_sha256=execution_sources,
        scope='staged/VM completions matched to unique retained Run/Attempt IDs and exact ready/result output; timestamp token alone is not globally unique'),indent=2)+'\n')
    provenance.append(dict(cohort=path.parent.name,field='execution_evidence_audit_sha256',value=digest(audit)))
    write_csv(output/'density-provenance.csv',provenance)
    return rows


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.report,args.output)
