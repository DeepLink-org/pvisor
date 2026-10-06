#!/usr/bin/env python3
"""Fresh-VM SDK offload with charged physical memory and full restore checks.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role user-facing.
Motivation: assess physical savings of parked Agent VMs including backing cache.
Conclusion sought: complete active/offloaded/restored cgroup memory, CPU and
verified first-read costs; repeatable/random data and raw/compressed backing.
Design: fresh VM per condition, randomized paired rounds, identical two-core
2 GiB owned cgroups, same SDK build and prepared tools, full memory validation.
Whole-VM offload is separate from snapshots and automatic cold-page paging.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import sys
import threading
import traceback

from reference_baselines import digest
from vm_memory import memory


def validate_report(report, config):
    if report.get('schema') != 'pvisor-live-offload/v1' or report.get('correctness') != 'passed':
        raise ValueError('missing successful full guest integrity proof')
    for name, expected in (('pattern',config['pattern']),('seed',config['seed']),('compressed',config['compressed']),
        ('samples',1),('warmups',0),('guest_data_bytes',64*1024**2),('memory_mib',256),('cpus',2),
        ('settle_ms',2000),('long_pause_seconds',0),('stress',False),('cancel_while',None)):
        if report.get(name) != expected:raise ValueError('wrong live memory condition: '+name)
    rows=report.get('rows',[])
    if len(rows)!=1:raise ValueError('expected one fresh-VM measurement')
    row=rows[0]
    groups=[]
    for phase in ('active','offloaded','restored'):
        value=row[phase];groups.append(value['cgroup'])
        if value['current_bytes']<=0 or value['events'].get('oom',0) or value['events'].get('oom_kill',0):
            raise ValueError('physical-memory evidence invalid or OOM')
        if any(name not in value['stat'] for name in ('anon','file','kernel')) or 'usage_usec' not in value['cpu']:
            raise ValueError('incomplete backing/cache/CPU accounting')
    if len(set(groups))!=1 or groups[0]!=config['cgroup']:raise ValueError('VM escaped measured cgroup')
    usage=[row[phase]['cpu']['usage_usec'] for phase in ('active','offloaded','restored')]
    if usage!=sorted(usage):raise ValueError('nonmonotonic CPU accounting')
    if row['backed_bytes']<256*1024**2 or any(row[name]<0 for name in ('offload_ms','offload_resume_ms','baseline_read_ms','restored_read_ms')):
        raise ValueError('missing RAM or restore timing')
    return row


def internal(config):
    root=Path(config['root'])
    group=Path('/sys/fs/cgroup')/Path('/proc/self/cgroup').read_text().strip().split('::',1)[1].lstrip('/')
    if int((group/'memory.max').read_text())!=config['budget_bytes'] or (group/'memory.swap.max').read_text().strip()!='0':raise ValueError('memory/swap budget not installed')
    quota,period=map(int,(group/'cpu.max').read_text().split())
    if quota/period!=2 or os.sched_getaffinity(0)!=set(map(int,config['cpu_affinity'].split(','))):raise ValueError('two-core CPU controls not installed')
    config=config|dict(cgroup=str(group))
    samples=[];stop=threading.Event();errors=[]
    def monitor():
        try:
            while not stop.wait(.05):samples.append(memory(group))
        except Exception as error:errors.append(str(error))
    thread=threading.Thread(target=monitor,daemon=True);thread.start()
    try:
        cmd=[config['example'],'--rootfs',config['rootfs'],'--firmware',config['firmware'],'--output',str(root/'vm'),
             '--memory','256','--cpus','2','--samples','1','--warmups','0','--long-pause-seconds','0',
             '--pattern',config['pattern'],'--seed',str(config['seed']),'--settle-ms','2000']
        if config['compressed']:cmd.append('--compressed')
        env=os.environ.copy()|{'PVISOR_FS_PROFILE':'0','PVISOR_STARTUP_TIMING':'0','PVISOR_RUN_HOME':str(root/'runs'),'XDG_CONFIG_HOME':str(root/'config')}
        env.pop('PVISOR_TEST_ALLOW_NO_USERNS',None)
        with (root/'sdk.stdout').open('wb') as stdout,(root/'sdk.stderr').open('wb') as stderr:
            result=subprocess.run(cmd,stdout=stdout,stderr=stderr,env=env,timeout=180)
        if result.returncode:raise RuntimeError('SDK offload failed; retained sdk.stderr')
        report=json.loads((root/'vm/raw.json').read_text());row=validate_report(report,config)
        if errors:raise RuntimeError('resource monitor failed: '+str(errors))
        return row|dict(correctness='passed',trial=config['trial'],pattern=config['pattern'],compressed=config['compressed'],seed=config['seed'],samples=samples,report=report,logs=str(root))
    finally:stop.set();thread.join()


def main():
    if sys.argv[1:2]==['--internal-worker']:
        config=json.loads(Path(sys.argv[2]).read_text())
        try:result=internal(config)
        except Exception as error:result=dict(correctness='failed',error=str(error),traceback=traceback.format_exc())
        Path(config['result']).write_text(json.dumps(result,indent=2)+'\n');raise SystemExit(0 if result['correctness']=='passed' else 1)
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('example','build-receipt','rootfs','firmware','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--samples',type=int,default=30);parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--cpu-affinity',default='0,1');parser.add_argument('--budget-mib',type=int,default=2048)
    args=parser.parse_args()
    if args.samples<1 or args.warmups<0 or args.budget_mib<512:parser.error('invalid sample or budget')
    for name in ('example','build_receipt','rootfs','firmware','output'):setattr(args,name,getattr(args,name).resolve())
    receipt=json.loads(args.build_receipt.read_text())
    if digest(args.example)!=receipt['example_sha256'] or digest(args.build_receipt.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:parser.error('SDK build provenance mismatch')
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir()
    shutil.copy2(args.example,args.output/'bin/vm_live_memory_bench');example=args.output/'bin/vm_live_memory_bench'
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json');shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    inputs=[]
    for path in sorted(args.rootfs.rglob('*')):
        entry=dict(path=str(path.relative_to(args.rootfs)),mode=path.lstat().st_mode)
        if path.is_symlink():entry.update(kind='symlink',target=str(path.readlink()))
        elif path.is_file():entry.update(kind='file',sha256=digest(path),bytes=path.stat().st_size)
        else:entry['kind']='directory'
        inputs.append(entry)
    (args.output/'rootfs-manifest.json').write_text(json.dumps(inputs,indent=2)+'\n')
    report=dict(benchmark_id='B-VM-MEMORY',mechanism='current SDK whole-VM offload, raw/compressed live RAM backing',
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),host_kernel=os.uname().release,binary_build=receipt,
        firmware_sha256=digest(args.firmware/'libkrunfw.so.5'),rootfs_manifest_sha256=digest(args.output/'rootfs-manifest.json'),
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(physical_memory='complete owned cgroup; VM plus FUSE, backing/store/cache and collector',
            memory='256 MiB guest, 64 MiB verified private data, MemoryMax and swap zero verified',
            order='seeded shuffled four conditions in each paired round; fresh VM every time',cache='warm prepared tools; no global eviction',
            settle='2 s active and 2 s parked before point measurements; 50 ms full cgroup monitor',
            scope='whole-VM offload, separate from execution snapshot and automatic cold pager; not concurrent density',
            exclusions='no timing exclusions; failure and OOM retained; full immutable and mutable guest data verified'),rows=[],failures=[])
    def save():
        temporary=args.output/'report.tmp';temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(args.output/'report.json')
    save();rng=random.Random(20261006)
    for trial in range(-args.warmups,args.samples):
        cases=[(pattern,compressed) for pattern in ('repeated','random') for compressed in (False,True)];rng.shuffle(cases)
        for pattern,compressed in cases:
            root=args.output/'trials'/f'{trial+args.warmups}-{pattern}-{int(compressed)}';root.mkdir(parents=True)
            config=dict(root=str(root),result=str(root/'result.json'),example=str(example),rootfs=str(args.rootfs),firmware=str(args.firmware),
                pattern=pattern,compressed=compressed,trial=trial,seed=20261006+trial+args.warmups,budget_bytes=args.budget_mib*1024**2,cpu_affinity=args.cpu_affinity)
            cfg=root/'config.json';cfg.write_text(json.dumps(config)+'\n')
            unit=f'pvisor-live-memory-{os.getpid()}-{trial+args.warmups}-{pattern}-{int(compressed)}'
            cmd=['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit='+unit,'--property=MemoryAccounting=yes',
                '--property=CPUAccounting=yes',f'--property=MemoryMax={config["budget_bytes"]}','--property=MemorySwapMax=0',
                '--property=CPUQuota=200%','--property=CPUAffinity='+args.cpu_affinity.replace(',',' '),'--property=TasksMax=128',
                '--property=TimeoutStopSec=10',sys.executable,str(args.output/'harness/live_vm_memory.py'),'--internal-worker',str(cfg)]
            print(trial,pattern,compressed,flush=True)
            try:
                result=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=240)
                (root/'service.stdout').write_bytes(result.stdout);(root/'service.stderr').write_bytes(result.stderr)
                value=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='service produced no evidence')
                if result.returncode or value['correctness']!='passed':raise RuntimeError(json.dumps(value))
                if trial>=0:report['rows'].append(value)
            except Exception as error:
                report['failures'].append(dict(trial=trial,pattern=pattern,compressed=compressed,error=str(error),logs=str(root)))
                save()
                if trial<0:raise SystemExit(1)
            save()
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
