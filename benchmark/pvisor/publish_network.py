#!/usr/bin/env python3
"""Publish complete B-NETWORK paired batches with retained output evidence."""
import argparse
import hashlib
import json
import math
from functools import lru_cache
from pathlib import Path
import statistics

from publication import distribution, write_csv
from publish_density import verify_harness
from publish_reference_campaign import paired_comparison
from reference_baselines import digest, verified_build_receipt

BACKENDS=('native','host','vm','podman','container')
MODES=('small','bulk','stream','deny')


def positive(value):
    if not isinstance(value,(float,int)) or isinstance(value,bool) or not math.isfinite(value) or value<=0:
        raise ValueError('invalid network timing')


@lru_cache(maxsize=3)
def expected_content(mode):
    content=b'x'*1024 if mode=='small' else b'x'*(32*1024**2) if mode=='bulk' else b'data: token\n\n'*10
    return len(content),hashlib.sha256(content).hexdigest()


def validate_payload(mode, check):
    if mode=='deny':
        if check!={'direct_socket_blocked':True}:raise ValueError('direct socket deny failed')
        return
    values=check['requests'] if mode=='small' else [check]
    if mode=='small' and (len(values)!=256 or check['concurrency']!=8):raise ValueError('incorrect request batch')
    size,expected=expected_content(mode)
    for value in values:
        if value['bytes']!=size or value['sha256']!=expected:raise ValueError('incorrect network response content')
        for metric in ('elapsed_ms','connect_ms','first_byte_ms'):positive(value[metric])
        if not value['connect_ms']<=value['first_byte_ms']<=value['elapsed_ms']:raise ValueError('inconsistent request timings')


def verify_outputs(report, directory):
    directory=directory.resolve();sources={};identities=set()
    worker=directory/'harness/v1/network_worker.py'
    for row in report['rows']:
        root=Path(row['logs']).resolve()
        if not root.is_relative_to(directory):raise ValueError('network evidence outside cohort')
        commands=list((root/'commands').glob('*/command.json'))
        if len(commands)!=1:raise ValueError('missing or ambiguous retained network invocation')
        command=commands[0];details=json.loads(command.read_text())
        if details.get('exit_code')!=0 or details.get('timed_out') or details.get('wall_ms')!=row['wall_ms']:
            raise ValueError('network invocation differs from successful sample')
        retained=[command,command.parent/'command.stdout',command.parent/'command.stderr',root/'workspace/worker.py']
        if digest(root/'workspace/worker.py')!=digest(worker):raise ValueError('network fixture worker changed')
        if row['backend'] in ('native','podman'):
            stdout=(command.parent/'command.stdout').read_text()
        else:
            bundles=list((root/'runs').glob('*/run-bundle.json'))
            if (root/'stage/run-bundle.json').is_file():bundles.append(root/'stage/run-bundle.json')
            if len(bundles)!=1:raise ValueError('missing independent network Run')
            bundle=json.loads(bundles[0].read_text());run=bundle['run']
            expected={'host':'rootless_process','vm':'virtual_machine','container':'container'}[row['backend']]
            if run['state']!='completed' or run['exit_code']!=0 or run['executor']['isolation']!=expected:
                raise ValueError('network Run lacks expected completed boundary')
            identity=(run['run_id'],run['attempt_id'])
            if not all(identity) or identity in identities:raise ValueError('duplicate or missing network Run identity')
            identities.add(identity)
            if bundle['safety']!=row['safety']:raise ValueError('network safety differs from retained Run')
            if row['backend']=='vm' and not bundle['safety']['filesystem_changes_staged']:raise ValueError('missing VM staging evidence')
            if row['workload']=='deny' and not bundle['safety']['network_non_bypassable']:raise ValueError('missing enforced network deny boundary')
            stdout=run['output']['stdout'];retained.append(bundles[0])
        value=json.loads(stdout.strip())
        if (value.get('mode')!=row['workload'] or value.get('worker_ms')!=row['worker_ms']
                or value.get('check')!=row['check'] or value.get('cpu_affinity')!=row['cpu_affinity']):
            raise ValueError('network report differs from retained output')
        for path in retained:
            if not path.resolve().is_relative_to(directory):raise ValueError('network evidence symlink outside cohort')
            sources[str(path.relative_to(directory))]=digest(path)
    return sources


def summarize(report):
    args=report['cli_arguments'];n=int(args['samples'])
    if (report.get('benchmark_id')!='B-NETWORK' or n<30 or not report.get('prepared_inputs_unchanged')
            or set(args['network_backends'].split(','))!=set(BACKENDS)
            or set(args['network_modes'].split(','))!=set(MODES)):
        raise ValueError('requires complete current network controls and unchanged prepared inputs')
    conditions=[(m,b) for m in MODES for b in (('host','vm') if m=='deny' else BACKENDS)]
    expected={(m,b,t) for m,b in conditions for t in range(n)}
    keys=[(r['workload'],r['backend'],r['trial']) for r in report['rows']]
    if len(keys)!=len(set(keys)) or set(keys)!=expected:raise ValueError('missing or duplicate network batches')
    for mode,backend in conditions:
        capability=report['capabilities']['network/'+mode+'/'+backend]
        if capability['state']!='available' or capability.get('failures'):raise ValueError('network capability or measured failure')
    affinity=sorted(map(int,args['cpu_affinity'].split(',')))
    if len(affinity)!=2 or len(set(affinity))!=2:raise ValueError('requires two distinct requested CPUs')
    for row in report['rows']:
        if row.get('correctness')!='passed' or row['cpu_affinity']!=([0,1] if row['backend']=='vm' else affinity):raise ValueError('incorrect network batch or CPU affinity')
        positive(row['wall_ms']);positive(row['worker_ms']);validate_payload(row['workload'],row['check'])
    records=[];comparisons=[]
    for mode,backend in conditions:
        selected=[r for r in report['rows'] if (r['workload'],r['backend'])==(mode,backend)]
        metrics={'job_wall_ms':lambda r:r['wall_ms'],'worker_ms':lambda r:r['worker_ms']}
        if mode=='small':metrics['batch_median_request_ms']=lambda r:statistics.median(v['elapsed_ms'] for v in r['check']['requests'])
        elif mode in ('bulk','stream'):
            metrics['transfer_ms']=lambda r:r['check']['elapsed_ms']
            metrics['first_byte_ms']=lambda r:r['check']['first_byte_ms']
            if mode=='bulk':metrics['throughput_mib_s']=lambda r:32/(r['check']['elapsed_ms']/1000)
        for metric,extract in metrics.items():
            records.append(dict(workload=mode,backend=backend,metric=metric,unit='MiB/s' if metric=='throughput_mib_s' else 'ms',
                planned_batches=n,validated_batches=n,failed_batches=0,warmups=args['warmups'],**distribution([extract(r) for r in selected]),
                aggregation='small request median within each 256-request batch; distribution across independent batches' if metric=='batch_median_request_ms' else 'one value per fresh verified batch',
                statistics='P95 descriptive, no P99; min/max observed; no model/network-service latency claim'))
            if mode!='deny' and backend!='native' and metric not in ('throughput_mib_s',):
                control=[r for r in report['rows'] if (r['workload'],r['backend'])==(mode,'native')]
                comparisons.append(dict(workload=mode,backend=backend,control='native',metric=metric,unit='ms',
                    **paired_comparison([dict(trial=r['trial'],value=extract(r)) for r in selected],
                                        [dict(trial=r['trial'],value=extract(r)) for r in control]),
                    confidence_method='5000 paired-round bootstrap resamples, seed 20261006; percentile 95% CI'))
    return records,comparisons


def publish(path, output):
    report=json.loads(path.read_text());receipt=verified_build_receipt(path.parent/'build-receipt.json',path.parent/'bin/pvisor')
    if receipt!=report['binary_build'] or receipt['pvisor_sha256']!=report['binary_sha256']:raise ValueError('network build receipt mismatch')
    verify_harness(report,path.parent)
    if digest(path.parent/'input-manifest.json')!=report['input_manifest_sha256']:raise ValueError('network input receipt mismatch')
    sources=verify_outputs(report,path.parent);records,comparisons=summarize(report);sha=digest(path)
    identity=dict(cohort=path.parent.name,report_sha256=sha,binary_sha256=receipt['pvisor_sha256'],source_manifest_sha256=receipt['source_manifest_sha256'],
        cpu_affinity=report['cli_arguments']['cpu_affinity'],memory_scope='CPU controlled; memory not identically capped; local origin outside payload budget')
    for row in records+comparisons:row.update(identity)
    audit=path.parent/'network-output-evidence-audit.json';audit.write_text(json.dumps(dict(report_sha256=sha,retained_evidence_sha256=sources),indent=2)+'\n')
    provenance=[dict(field=key,value=json.dumps(value,sort_keys=True) if isinstance(value,(dict,list)) else value)
        for key,value in report.items() if key not in ('rows','source_status')]
    provenance.extend([dict(field='report_sha256',value=sha),dict(field='output_evidence_audit_sha256',value=digest(audit))])
    output.mkdir(parents=True,exist_ok=True)
    for name,rows in [('network-summary.csv',records),('network-comparisons.csv',comparisons),('network-provenance.csv',provenance)]:write_csv(output/name,rows)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();publish(args.report,args.output)
