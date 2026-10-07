#!/usr/bin/env python3
"""B-VM-MEMORY, user-facing: when do shared baselines and KSM save memory?
Fresh 1/2/4-VM groups, identical sealed bytes with shared/independent RAM inodes,
identical/unique contents, 0/25/100% writes and checked independent lifetimes.
KSM scanner is read-only. Shared-baseline and dynamic-private phases are distinct.
Four cores/2 GiB/swap0, 2 vCPU/512 MiB each, external resource and interference
observer; 3 warmups/30 randomized paired rounds, two 30-second scan windows.
"""
import argparse
import json
from pathlib import Path
import random
import os
import stat
import shutil
import sys

from memory_savings import run, digest, inventory, host_identity
from memory_scale import validate_report, smaps_text

ARMS = {
    'independent':dict(mode='baseline',dedup=False,independent_inodes=True),
    'shared':dict(mode='baseline',dedup=False,independent_inodes=False),
    'ksm-off':dict(mode='ksm',dedup=False,independent_inodes=False),
    'ksm-on':dict(mode='ksm',dedup=True,independent_inodes=False),
}
STRATEGIES = {
    'unshared': dict(mode='fresh',dedup=False,independent_inodes=False),
    'snapshot-cow': dict(mode='baseline',dedup=False,independent_inodes=False),
    'ksm': dict(mode='fresh',dedup=True,independent_inodes=False),
}
PATTERNS=('repeated','random-shared','random-unique')


def strategy_arms(pool=False):
    return {**STRATEGIES, **({'daemon-pool':dict(mode='pool',dedup=False,independent_inodes=False)} if pool else {})}


def scan_seconds(args,arm):
    return args.ksm_scan_seconds if getattr(args,'ksm_scan_seconds',None) is not None and arm=='ksm' else args.scan_seconds


def matrix(strategies=False,pool=False):
    return [(n,pattern,arm) for n in (1,2,4) for pattern in PATTERNS for arm in (strategy_arms(pool) if strategies else ARMS)]


def validate(raw,condition,args,binary,worker):
    config={**condition, 'rootfs':str(args.rootfs),'firmware':str(args.firmware),
            'output':str(worker),'example':str(binary),'example_sha256':digest(binary),
            'cgroup':raw.get('before',{}).get('cgroup')}
    if raw.get('conditions',{}).get('cpus')!=2:
        raise ValueError('wrong public two-vCPU profile')
    result=validate_report(raw,config)
    if condition.get('memory_mib')==512:
        for phase in raw['phases']:
            processes=phase['accounting']['processes']
            if not processes or any(smaps_text(p,worker) is None or 'Pss' not in p['smaps_totals_bytes'] for p in processes):
                raise ValueError('missing complete resident physical memory')
            for process in processes:
                observed=sum(int(line.split()[1])*1024 for line in smaps_text(process,worker).splitlines() if line.startswith('Pss:'))
                if observed!=process['smaps_totals_bytes']['Pss']:raise ValueError('resident PSS evidence mismatch')
    if result.get('scanner_state') != '1':
        # Validate the actual read-only sysfs evidence, regardless of helper keys.
        if raw['before']['ksm']['run'].get('raw','').strip()!='1':
            raise ValueError('KSM scanner is not enabled')
    return result


def retire(worker,output):
    # Only after complete correctness and native/owned-unit reaping. Retain
    # aggregate proof and bytes/modes/hashes of disposable execution stores.
    retired_sockets=[]
    socket=worker/'pool'/'pool.sock'
    if socket.exists():
        raw=json.loads((worker/'raw.json').read_text())
        metadata=socket.lstat()
        if (raw.get('correctness')!='passed' or raw.get('cleanup',{}).get('all_reaped') is not True
                or raw.get('pool_cleanup',{}).get('reaped') is not True
                or not stat.S_ISSOCK(metadata.st_mode) or metadata.st_uid!=os.geteuid()):
            raise ValueError('pool socket retirement requires successful native/pool reaping')
        retired_sockets.append(dict(path='pool/pool.sock',mode=metadata.st_mode,kind='socket',retired_after_pool_reap=True))
        socket.unlink()
    manifest=inventory(worker)+retired_sockets
    (output/'retired-runtime-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    for child in worker.iterdir():
        if child.is_dir():shutil.rmtree(child)
        elif child.suffix=='.ram':child.unlink()
    return digest(output/'retired-runtime-manifest.json')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('example','build-receipt','rootfs','firmware','output'):
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--samples',type=int,default=30)
    parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--scan-seconds',type=int,default=30)
    parser.add_argument('--ksm-scan-seconds',type=int,choices=(2,60),help='static strategy comparison: override only the fresh KSM arm')
    parser.add_argument('--preflight',action='store_true')
    parser.add_argument('--arm',choices=(*ARMS,*STRATEGIES,'daemon-pool'),help='preflight only: restrict to one arm')
    parser.add_argument('--pattern',choices=PATTERNS,help='preflight only: restrict to one pattern')
    parser.add_argument('--quiet-seconds',type=int,default=30)
    parser.add_argument('--vms',type=int,choices=(1,2,4),help='restrict to one group size')
    parser.add_argument('--pool-daemon',type=Path,help='frozen daemon binary; include daemon-owned pool strategy')
    parser.add_argument('--pool-receipt',type=Path,help='matching daemon build receipt and source manifest')
    parser.add_argument('--strategies',action='store_true',help='compare independent fresh VMs, snapshot COW and fresh-RAM KSM')
    parser.add_argument('--static',action='store_true',help='one four-VM observation with 2-second scan windows')
    parser.add_argument('--memory-mib',type=int,choices=(256,512),default=512)
    parser.add_argument('--group-memory-max',type=int,choices=(2147483648,4294967296),default=2147483648)
    args=parser.parse_args()
    if args.ksm_scan_seconds is not None and (not args.static or not args.strategies):parser.error('KSM window override requires static strategies')
    if (args.arm or args.pattern) and not args.preflight:parser.error('arm/pattern subsets require preflight')
    if args.preflight:args.samples,args.warmups,args.scan_seconds=1,0,2
    if args.static:args.samples,args.warmups,args.scan_seconds,args.quiet_seconds,args.vms=1,0,2,0,4
    if args.samples<1 or args.warmups<0 or not 1<=args.scan_seconds<=30 or not 0<=args.quiet_seconds<=30:
        parser.error('invalid sampling protocol')
    for name in ('example','build_receipt','rootfs','firmware','output'):
        setattr(args,name,getattr(args,name).resolve())
    if bool(args.pool_daemon)!=bool(args.pool_receipt) or (args.pool_daemon and not args.strategies):parser.error('pool daemon requires strategies and matching receipt')
    args.output.mkdir();binary=args.output/'probe';shutil.copy2(args.example,binary)
    receipt=json.loads(args.build_receipt.read_text())
    if receipt['example_sha256']!=digest(binary):raise ValueError('binary receipt mismatch')
    for source,name in [(args.build_receipt,'build-receipt.json'),
                        (args.build_receipt.parent/'source-manifest.json','source-manifest.json')]:
        shutil.copy2(source,args.output/name)
    if digest(args.output/'source-manifest.json')!=receipt['source_manifest_sha256']:
        raise ValueError('source receipt mismatch')
    helpers={}
    for name in ('memory_sharing.py','memory_savings.py','memory_scale.py','linux_cold_runtime.py'):
        shutil.copy2(Path(__file__).with_name(name),args.output/name)
        helpers[name]=digest(args.output/name)
    pool_info=None
    if args.pool_daemon:
        source=args.pool_daemon.resolve();receipt_path=args.pool_receipt.resolve()
        pool_receipt=json.loads(receipt_path.read_text())
        if pool_receipt['binary_sha256']!=digest(source):raise ValueError('pool receipt mismatch')
        manifest=receipt_path.parent/'source-manifest.json'
        if pool_receipt['source_manifest_sha256']!=digest(manifest):raise ValueError('pool source receipt mismatch')
        shutil.copy2(source,args.output/'pool-daemon');shutil.copy2(receipt_path,args.output/'pool-build-receipt.json')
        shutil.copy2(manifest,args.output/'pool-source-manifest.json')
        args.pool_daemon=(args.output/'pool-daemon').resolve()
        pool_info=dict(binary_sha256=digest(args.pool_daemon),build_receipt_sha256=digest(args.output/'pool-build-receipt.json'),source_manifest_sha256=digest(args.output/'pool-source-manifest.json'))
    before={name:inventory(getattr(args,name)) for name in ('rootfs','firmware')}
    (args.output/'input-manifest.json').write_text(json.dumps(before,indent=2)+'\n')
    report=dict(schema='pvisor-memory-sharing-cohort/v1',benchmark_id='B-VM-MEMORY',role='user-facing',
                arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
                binary_sha256=digest(binary),harnesses=helpers,
                pool=pool_info,selected=[c for c in matrix(args.strategies,bool(args.pool_daemon)) if (args.vms is None or c[0]==args.vms) and (args.arm is None or c[2]==args.arm) and (args.pattern is None or c[1]==args.pattern)],
                host=host_identity(),
                interference_policy='record background builds, reject foreign VMs; static memory only' if args.static else 'reject visible foreign VM/build activity',
                budget=dict(cpu_cores=4,memory_max=args.group_memory_max,swap_max=0),attempts=[],complete=False)
    def save():(args.output/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    save()
    for round_id in range(-args.warmups,args.samples):
        cells=report['selected'].copy();random.Random(7800+round_id).shuffle(cells)
        for n,pattern,arm in cells:
            output=args.output/str(len(report['attempts']))
            condition=dict(vms=n,pattern=pattern,**(strategy_arms(bool(args.pool_daemon)) if args.strategies else ARMS)[arm],cpus=2,seed=20261006,
                           settle_ms=500,ksm_wait_seconds=scan_seconds(args,arm),memory_mib=args.memory_mib)
            if args.group_memory_max!=2147483648:condition['group_memory_max']=args.group_memory_max
            if arm=='daemon-pool':condition['pool_daemon']=str(args.pool_daemon)
            row=run(args,binary,output,condition,round_id,round_id<0,
                    validator=lambda raw,c:validate(raw,c,args,binary,output/'w'))
            row['arm']=arm
            if row['status']=='successful':
                row['retired_manifest_sha256']=retire(output/'w',output)
            report['attempts'].append(row);save()
            print(f'round={round_id} n={n} {pattern}/{arm}: {row["status"]}',flush=True)
            if row['status']!='successful':print(row.get('error'),flush=True);return 1
    report['input_verification']={name:before[name]==inventory(getattr(args,name)) for name in before}
    after_host=host_identity()
    report['host_verification']={name:report['host'][name]==after_host[name]
                                 for name in ('kernel','cpu_model','ksm','host_cp_sha256')}
    report['complete']=all(report['input_verification'].values()) and all(report['host_verification'].values());save()
    return 0 if report['complete'] else 1


if __name__=='__main__':sys.exit(main())
