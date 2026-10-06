#!/usr/bin/env python3
"""Measure charged physical memory and verified execution restore with current Jobs.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role user-facing.
Motivation: dormant Agent states save resources only if backing/cache and
compression overhead are included and resumption preserves useful state.
Conclusion sought: active/suspended/restore cgroup memory, snapshot storage,
CPU and suspend/restore cost for repeated and incompressible private memory.
Design: fresh Linux VM per raw/compressed condition; bounded owned cgroup,
identical payload and resources; full guest checksum/token after restore.
Execution-snapshot compression is distinct from live cold-page compression.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import selectors
import shutil
import signal
import subprocess
import sys
import threading
import time
import traceback

from reference_baselines import digest, verified_build_receipt


def counters(path):
    return {key:int(value) for key,value in (line.split() for line in path.read_text().splitlines())}


def memory(group):
    return dict(current_bytes=int((group/'memory.current').read_text()),
                peak_bytes=int((group/'memory.peak').read_text()),stat=counters(group/'memory.stat'),
                events=counters(group/'memory.events'),cpu=counters(group/'cpu.stat'))


def validate_restore(ready, result):
    if result.get('integrity') != 'passed' or any(result.get(k) != ready.get(k) for k in ('kind','bytes','token','checksum')):
        raise ValueError('restore did not preserve the complete guest memory and execution token')


def read_ready(process, timeout=60):
    # Unbuffered byte reads avoid losing already-buffered lines to select().
    selector=selectors.DefaultSelector();selector.register(process.stdout,selectors.EVENT_READ)
    buffer=bytearray();deadline=time.monotonic()+timeout
    try:
        while time.monotonic()<deadline:
            if not selector.select(timeout=min(1,max(0,deadline-time.monotonic()))):
                if process.poll() is not None:break
                continue
            chunk=os.read(process.stdout.fileno(),65536)
            if not chunk:break
            buffer.extend(chunk)
            lines=bytes(buffer).splitlines()
            for line in lines:
                if line.startswith(b'PVISOR_MEMORY_READY '):
                    return json.loads(line.removeprefix(b'PVISOR_MEMORY_READY ')),bytes(buffer)
    finally:selector.close()
    raise RuntimeError('VM exited or timed out before allocating verified private memory')


def internal(config):
    root=Path(config['root']);work=root/'workspace';stage=root/'stage'
    group_path=Path('/proc/self/cgroup').read_text().strip().split('::',1)[1]
    group=Path('/sys/fs/cgroup')/group_path.lstrip('/')
    if int((group/'memory.max').read_text()) != config['budget_bytes'] or (group/'memory.swap.max').read_text().strip()!='0':
        raise ValueError('requested physical-memory/swap limits not installed')
    quota,period=map(int,(group/'cpu.max').read_text().split())
    if quota/period != 2 or os.sched_getaffinity(0) != set(map(int,config['cpu_affinity'].split(','))):
        raise ValueError('requested two-core budget not installed')
    env=os.environ.copy()|{'PVISOR_STARTUP_TIMING':'0','PVISOR_FS_PROFILE':'0',
        'PVISOR_RUN_HOME':str(root/'runs'),'XDG_CONFIG_HOME':str(root/'config')}
    env.pop('PVISOR_TEST_ALLOW_NO_USERNS',None)
    binary=config['binary'];process=None;stop=threading.Event();samples=[]
    def monitor():
        while not stop.wait(.05):samples.append(memory(group))
    monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
    before=memory(group)
    try:
        argv=[binary,'run','--no-agent-defaults','--overlaynet','off','--stdio','inherit',
            '--timeout','120s','--vm','--rootfs',config['rootfs'],'--vm-library-dir',config['firmware'],
            '--cpu','2','--memory','256MiB','--stage',str(stage),'--','/bench/memory-probe',
            config['kind'],str(config['payload_bytes']),'8']
        with (root/'run.stderr').open('wb') as stderr:
            process=subprocess.Popen(argv,cwd=work,env=env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,
                stderr=stderr,start_new_session=True,bufsize=0)
            ready,initial=read_ready(process)
            active=memory(group)
            start=time.perf_counter_ns()
            suspension=subprocess.run([binary,'suspend',str(stage),'--ram-storage',config['storage'],'--json'],
                cwd=work,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
            suspend_ms=(time.perf_counter_ns()-start)/1e6
            (root/'suspend.stdout').write_bytes(suspension.stdout);(root/'suspend.stderr').write_bytes(suspension.stderr)
            if suspension.returncode:raise RuntimeError('suspend failed: '+suspension.stderr.decode(errors='replace')[-1000:])
            remainder,_=process.communicate(timeout=30)
            (root/'run.stdout').write_bytes(initial+remainder)
            if process.returncode:raise RuntimeError(f'suspended launcher exit {process.returncode}')
        suspended=memory(group)
        suspension_json=json.loads(suspension.stdout)
        if suspension_json.get('state')!='suspended':raise ValueError('native execution suspension not confirmed')
        (root/'suspend.json').write_text(json.dumps(suspension_json,indent=2)+'\n')
        start=time.perf_counter_ns()
        resumed=subprocess.run([binary,'resume',str(stage)],cwd=work,env=env,stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
        resume_completion_ms=(time.perf_counter_ns()-start)/1e6
        (root/'resume.stdout').write_bytes(resumed.stdout);(root/'resume.stderr').write_bytes(resumed.stderr)
        if resumed.returncode:raise RuntimeError('resume failed: '+resumed.stderr.decode(errors='replace')[-1000:])
        results=[json.loads(line.removeprefix('PVISOR_MEMORY_RESULT ')) for line in resumed.stdout.decode().splitlines() if line.startswith('PVISOR_MEMORY_RESULT ')]
        if len(results)!=1:raise ValueError('missing unique restored guest result')
        validate_restore(ready,results[0])
        after=memory(group)
        if after['events'].get('oom',0) or after['events'].get('oom_kill',0):raise ValueError('OOM invalidates single-VM restore measurement')
        files=[p for p in stage.rglob('*') if p.is_file() and not p.is_symlink()]
        return dict(kind=config['kind'],storage=config['storage'],trial=config['trial'],correctness='passed',
            ready=ready,restored=results[0],suspend_ms=suspend_ms,resume_completion_ms=resume_completion_ms,
            timing_note='resume completion includes remaining guest sleep; guest restored_scan_ms isolates full first scan, not host resume-ready time',
            before=before,active=active,suspended=suspended,after=after,samples=samples,
            retained_job_bytes=sum(p.stat().st_size for p in files),retained_job_allocated_bytes=sum(p.stat().st_blocks*512 for p in files),
            memory_scope='entire owned cgroup, including launcher, capture/restore, helpers and charged file/backing cache; not sum of RSS',
            cgroup=str(group),budget_bytes=config['budget_bytes'])
    finally:
        stop.set();monitor_thread.join()
        if process and process.poll() is None:
            os.killpg(process.pid,signal.SIGKILL);process.communicate()


def main():
    if sys.argv[1:2]==['--internal-worker']:
        config=json.loads(Path(sys.argv[2]).read_text())
        try:
            result=internal(config)
        except Exception as error:
            result=dict(correctness='failed',error=str(error),traceback=traceback.format_exc())
        Path(config['result']).write_text(json.dumps(result,indent=2)+'\n')
        raise SystemExit(0 if result['correctness']=='passed' else 1)
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','build-receipt','firmware','rootfs','output'):
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--samples',type=int,default=30);parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--cpu-affinity',default='0,1');parser.add_argument('--budget-mib',type=int,default=2048)
    parser.add_argument('--payload-mib',type=int,default=64)
    args=parser.parse_args()
    if args.samples<1 or args.warmups<0 or not 1<=args.payload_mib<=128 or args.budget_mib<512:
        parser.error('invalid sample, payload or memory budget')
    for name in ('binary','build_receipt','firmware','rootfs','output'):setattr(args,name,getattr(args,name).resolve())
    receipt=verified_build_receipt(args.build_receipt,args.binary)
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir()
    shutil.copy2(args.binary,args.output/'bin/pvisor');binary=args.output/'bin/pvisor'
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json')
    shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    rootfs_manifest=[]
    for path in sorted(args.rootfs.rglob('*')):
        entry=dict(path=str(path.relative_to(args.rootfs)),mode=path.lstat().st_mode)
        if path.is_symlink():entry.update(kind='symlink',target=str(path.readlink()))
        elif path.is_file():entry.update(kind='file',sha256=digest(path),size=path.stat().st_size)
        else:entry['kind']='directory'
        rootfs_manifest.append(entry)
    (args.output/'rootfs-manifest.json').write_text(json.dumps(rootfs_manifest,indent=2)+'\n')
    report=dict(benchmark_id='B-VM-MEMORY',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        rootfs_manifest_sha256=digest(args.output/'rootfs-manifest.json'),binary_build=receipt,binary_sha256=digest(binary),probe_sha256=digest(args.rootfs/'bench/memory-probe'),
        firmware_sha256=digest(args.firmware/'libkrunfw.so.5'),host_kernel=os.uname().release,
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(compression='execution snapshot storage; not a live cold-page pager',cache='warm; no global eviction',
            order='seeded randomized raw/compressed and repeated/random cases per round',cpu='2 vCPU, host affinity and 200% cgroup quota',
            memory='256 MiB guest, shared complete owned cgroup with explicit MemoryMax and no swap',
            exclusions='no timing exclusions; failures retained; correctness before publication'),rows=[],failures=[])
    rng=random.Random(20261006)
    def save():
        temporary=args.output/'report.tmp';temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(args.output/'report.json')
    save()
    for trial in range(-args.warmups,args.samples):
        cases=[(kind,storage) for kind in ('repeated','random') for storage in ('raw','compressed')];rng.shuffle(cases)
        for kind,storage in cases:
            root=args.output/'trials'/f'{trial+args.warmups}-{0 if kind == "repeated" else 1}-{storage[0]}';(root/'workspace').mkdir(parents=True)
            config=dict(root=str(root),result=str(root/'result.json'),binary=str(binary),rootfs=str(args.rootfs),firmware=str(args.firmware),
                kind=kind,storage=storage,trial=trial,payload_bytes=args.payload_mib*1024**2,budget_bytes=args.budget_mib*1024**2,cpu_affinity=args.cpu_affinity)
            cfg=root/'config.json';cfg.write_text(json.dumps(config)+'\n')
            unit=f'pvisor-memory-{os.getpid()}-{trial+args.warmups}-{kind}-{storage}'
            argv=['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit='+unit,
                '--property=MemoryAccounting=yes','--property=CPUAccounting=yes',f'--property=MemoryMax={config["budget_bytes"]}',
                '--property=MemorySwapMax=0','--property=CPUQuota=200%','--property=CPUAffinity='+args.cpu_affinity.replace(',',' '),
                '--property=TasksMax=128','--property=TimeoutStopSec=10',sys.executable,str(args.output/'harness/vm_memory.py'),'--internal-worker',str(cfg)]
            print(trial,kind,storage,flush=True)
            proc=subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=300)
            (root/'service.stdout').write_bytes(proc.stdout);(root/'service.stderr').write_bytes(proc.stderr)
            result=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='bounded service failed before producing evidence')
            if proc.returncode or result['correctness']!='passed':
                report['failures'].append(dict(trial=trial,kind=kind,storage=storage,logs=str(root),result=result));save()
                if trial<0:raise SystemExit(1)
            elif trial>=0:report['rows'].append(result|dict(logs=str(root)));save()
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
