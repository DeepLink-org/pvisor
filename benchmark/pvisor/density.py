#!/usr/bin/env python3
"""Fixed-budget simultaneous occupancy and validated useful-task throughput.

Benchmark: B-DENSITY (benchmark/README.md#b-density), role user-facing.
Motivation: users need reliable concurrent capacity, including complete VM
backing and runtime costs, rather than summed RSS or configured guest RAM.
Conclusion sought: ready/completed/failed tasks, barrier occupancy, whole owned
cgroup memory/CPU, and throughput under a fixed budget for idle/useful cases.
Design: fresh owned cgroup per batch; CPU quota/affinity, memory.max and zero
swap; all ready jobs hold before release; Python/Git edits and full checksum;
Podman with cgroups disabled must remain in the same enclosing cgroup.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
import traceback

from reference_baselines import digest, validate_bundle_execution, verified_build_receipt
from vm_memory import memory


def save(path, value):
    temp=path.with_suffix('.tmp');temp.write_text(json.dumps(value,indent=2)+'\n');temp.replace(path)


def cgroup():
    relative=Path('/proc/self/cgroup').read_text().strip().split('::',1)[1]
    return Path('/sys/fs/cgroup')/relative.lstrip('/')


def sample_memory(group):
    value = memory(group)
    rss = []
    pids=set()
    for membership in [group/'cgroup.procs', *group.rglob('cgroup.procs')]:
        try:pids.update(membership.read_text().split())
        except FileNotFoundError:continue
    for pid in pids:
        try:
            status = Path(f'/proc/{pid}/status').read_text().splitlines()
            rss.append(int(next(line.split()[1] for line in status if line.startswith('VmRSS:'))))
        except (OSError, StopIteration):
            continue
    return value | dict(process_rss_sum_kib=sum(rss), observed_processes=len(rss),
                        rss_scope='owned cgroup processes; shared pages may be double-counted; not primary physical memory')


def verify_result(ready, result, useful):
    if result.get('integrity')!='passed' or result.get('changes')!=(4 if useful else 0):
        raise ValueError('useful task or integrity check failed')
    if any(result.get(k)!=ready.get(k) for k in ('token','bytes','checksum')):
        raise ValueError('worker result differs from its ready state')
    if ready.get('bytes') != (32 * 1024 * 1024 if useful else 0):
        raise ValueError('worker did not touch the requested private payload')


def internal(config):
    root=Path(config['root']);group=cgroup();budget=config['budget_bytes']
    if int((group/'memory.max').read_text())!=budget or (group/'memory.swap.max').read_text().strip()!='0':
        raise ValueError('memory/swap controls missing')
    quota,period=map(int,(group/'cpu.max').read_text().split())
    cpus=set(map(int,config['cpu_affinity'].split(',')))
    if quota/period!=2 or os.sched_getaffinity(0)!=cpus:raise ValueError('two-core controls missing')
    env=os.environ.copy()|{'PVISOR_FS_PROFILE':'0','PVISOR_STARTUP_TIMING':'0',
        'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1','LC_ALL':'C'}
    env.pop('PVISOR_TEST_ALLOW_NO_USERNS',None)
    count=config['concurrency'];backend=config['backend'];useful=config['workload']=='useful'
    fixture=root/'fixture';(fixture/'files').mkdir(parents=True)
    for i in range(64):(fixture/'files'/f'f{i:03d}').write_text(f'old-{i}\n')
    for argv in (['git','init','-q'],['git','config','core.hooksPath','/dev/null'],
                 ['git','config','gc.auto','0'],['git','add','files'],
                 ['git','-c','user.name=Benchmark','-c','user.email=bench@example.invalid','commit','-qm','fixture']):
        subprocess.run(argv,cwd=fixture,env=env,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    jobs=[]
    podman=['podman','--root',config['podman_root'],'--runroot',config['podman_runroot'],
            '--storage-driver','overlay','--cgroup-manager','cgroupfs'] if backend=='podman' else []
    for i in range(count):
        jobroot=root/f'j{i}';jobroot.mkdir();work=jobroot/'work';stage=jobroot/'stage'
        subprocess.run(['cp','-a','--reflink=auto',str(fixture),str(work)],check=True)
        expected_cpus='0,1' if backend=='vm' else config['cpu_affinity']
        worker=config['worker'] if backend in ('native','staged') else '/bench/density-worker.py'
        payload=['/usr/bin/python3',worker,'32' if useful else '0',expected_cpus]
        if backend=='native':argv=payload
        elif backend=='podman':
            argv=podman+['run','--rm','-i','--pull','never','--cgroups','disabled','--network','none',
                '--cidfile',str(jobroot/'cid'),'--conmon-pidfile',str(jobroot/'conmon.pid'),'-v',str(work)+':/work:Z','-w','/work',
                '--entrypoint','/usr/bin/python3',config['podman_image'],*payload[1:]]
        else:
            argv=[config['binary'],'run','--no-agent-defaults','--overlaynet','off',
                  '--stdio','inherit','--timeout','240s','--stage',str(stage)]
            if backend=='vm':argv+=['--vm','--rootfs',config['rootfs'],'--vm-library-dir',config['firmware'],
                                   '--cpu','2','--memory','256MiB']
            argv+=['--',*payload]
        jobs.append(dict(root=jobroot,work=work,stage=stage,argv=argv,ready=None,result=None,error=None))
    before=sample_memory(group);stop=threading.Event();samples=[]
    def monitor():
        while not stop.wait(.05):
            current=sample_memory(group);samples.append(current)
            save(root/'last-memory.json',current)
    monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
    start=time.perf_counter_ns()
    def read(job):
        try:
            with (job['root']/'stdout.log').open('w') as log:
                for line in job['process'].stdout:
                    log.write(line);log.flush()
                    for prefix,key in [('PVISOR_DENSITY_READY ','ready'),('PVISOR_DENSITY_RESULT ','result')]:
                        if line.startswith(prefix):
                            if job[key] is not None:raise ValueError('duplicate worker marker')
                            job[key]=json.loads(line[len(prefix):])
                            if key=='ready':job['ready_ms']=(time.perf_counter_ns()-start)/1e6
        except Exception as error:job['error']=str(error)
    try:
        for job in jobs:
            job['stderr']=(job['root']/'stderr.log').open('w')
            jobenv=env|{'PVISOR_RUN_HOME':str(job['root']/'runs'),'XDG_CONFIG_HOME':str(job['root']/'config')}
            job['process']=subprocess.Popen(job['argv'],cwd=job['work'],env=jobenv,
                stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=job['stderr'],text=True,bufsize=1,start_new_session=True)
            job['reader']=threading.Thread(target=read,args=(job,),daemon=True);job['reader'].start()
        deadline=time.monotonic()+config['ready_timeout']
        while time.monotonic()<deadline:
            if all(j['ready'] is not None or j['error'] or j['process'].poll() is not None for j in jobs):break
            time.sleep(.01)
        ready=[j for j in jobs if j['ready'] is not None and j['process'].poll() is None and not j['error']]
        barrier=sample_memory(group)
        # An OCI runtime that moves its payload outside the budget invalidates
        # this condition, even if its task output is otherwise correct.
        for job in ready:
            pid=job['process'].pid
            if backend=='podman':
                cid=(job['root']/'cid').read_text().strip()
                pid=int(subprocess.check_output(podman+['inspect','--format','{{.State.Pid}}',cid],text=True))
            pids=[pid]
            if backend=='podman':pids.append(int((job['root']/'conmon.pid').read_text().strip()))
            expected='/'+str(group.relative_to('/sys/fs/cgroup'))
            for member in pids:
                actual=Path(f'/proc/{member}/cgroup').read_text().strip().split('::',1)[1]
                if actual!=expected and not actual.startswith(expected+'/'):
                    raise ValueError('runtime payload or conmon escaped the enclosing resource budget')
        release=time.perf_counter_ns()
        for job in ready:
            job['process'].stdin.write('GO\n');job['process'].stdin.flush();job['process'].stdin.close()
        outcomes=[]
        for job in jobs:
            process=job['process']
            if job not in ready and process.poll() is None:os.killpg(process.pid,signal.SIGKILL)
            try:code=process.wait(timeout=120)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid,signal.SIGKILL);code=process.wait();job['error']='completion timeout'
            job['reader'].join(timeout=5)
            try:
                if code or job['error']:raise ValueError(job['error'] or f'exit {code}')
                if job['ready'] is None or job['result'] is None:raise ValueError('missing ready/result markers')
                verify_result(job['ready'],job['result'],useful)
                for i in range(64):
                    original=f'old-{i}\n';changed=f'new-{i}\n' if useful and i<4 else original
                    expected=original if backend in ('staged','vm') else changed
                    if (job['work']/'files'/f'f{i:03d}').read_text()!=expected:raise ValueError('host workspace content mismatch')
                    if backend in ('staged','vm') and useful and i<4:
                        if (job['stage']/'upper/files'/f'f{i:03d}').read_text()!=changed:raise ValueError('retained staged content mismatch')
                if backend in ('staged','vm'):
                    bundle=json.loads((job['stage']/'run-bundle.json').read_text())
                    validate_bundle_execution(bundle,'pvisor-vm' if backend=='vm' else 'pvisor-staged','rootless_process')
                outcomes.append(dict(correctness='passed',ready_ms=job['ready_ms'],result=job['result'],logs=str(job['root'])))
            except Exception as error:outcomes.append(dict(correctness='failed',error=str(error),logs=str(job['root'])))
        elapsed=(time.perf_counter_ns()-start)/1e6;completed=sum(o['correctness']=='passed' for o in outcomes)
        return dict(backend=backend,workload=config['workload'],concurrency=count,trial=config['trial'],
            attempted=count,ready=len(ready),completed=completed,failed=count-completed,
            all_ready=len(ready)==count,wall_ms=elapsed,ready_barrier_ms=(release-start)/1e6,
            completed_per_second=completed/(elapsed/1000),before=before,barrier=barrier,after=sample_memory(group),
            samples=samples,outcomes=outcomes,budget_bytes=budget,cgroup=str(group),
            correctness='passed' if completed==count and len(ready)==count else 'failed',
            memory_scope='whole owned cgroup including all VM backing and charged cache; common preprepared tool/image cache may be charged outside it')
    finally:
        stop.set();monitor_thread.join()
        for job in jobs:
            if 'process' in job and job['process'].poll() is None:
                os.killpg(job['process'].pid,signal.SIGKILL);job['process'].wait()
            if 'stderr' in job:job['stderr'].close()
            if backend=='podman' and (job['root']/'cid').is_file():
                cid=(job['root']/'cid').read_text().strip()
                subprocess.run(podman+['rm','--force',cid],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30)


def main():
    if sys.argv[1:2]==['--internal-worker']:
        config=json.loads(Path(sys.argv[2]).read_text())
        try:result=internal(config)
        except Exception as error:result=dict(correctness='failed',error=str(error),traceback=traceback.format_exc())
        save(Path(config['result']),result);raise SystemExit(0 if result['correctness']=='passed' else 1)
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','build-receipt','firmware','rootfs','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--input-manifest',type=Path,required=True)
    parser.add_argument('--podman-root',type=Path);parser.add_argument('--podman-image')
    parser.add_argument('--backends',default='native,staged,vm,podman');parser.add_argument('--workloads',default='idle,useful')
    parser.add_argument('--concurrencies',default='1,2,4,8,16,32,64,128');parser.add_argument('--samples',type=int,default=5)
    parser.add_argument('--cpu-affinity',default='0,1');parser.add_argument('--budget-mib',type=int,default=4096)
    parser.add_argument('--ready-timeout',type=int,default=180)
    args=parser.parse_args();backends=args.backends.split(',');workloads=args.workloads.split(',');levels=list(map(int,args.concurrencies.split(',')))
    if not backends or not workloads or set(backends)-{'native','staged','vm','podman'} or set(workloads)-{'idle','useful'} or len(backends)!=len(set(backends)) or len(workloads)!=len(set(workloads)) or not levels or len(levels)!=len(set(levels)) or min(levels)<1 or max(levels)>128 or args.samples<1 or args.budget_mib<512 or args.ready_timeout<1:parser.error('invalid conditions or budget')
    if 'podman' in backends and (not args.podman_root or not args.podman_image or not re.fullmatch(r'(sha256:)?[0-9a-f]{64}',args.podman_image)):parser.error('Podman requires a prepared private store and immutable image ID')
    for key in ('binary','build_receipt','firmware','rootfs','output','podman_root','input_manifest'):
        if getattr(args,key,None):setattr(args,key,getattr(args,key).resolve())
    receipt=verified_build_receipt(args.build_receipt,args.binary)
    inputs=json.loads(args.input_manifest.read_text())
    if inputs['host_python_sha256']!=digest('/usr/bin/python3') or inputs['host_git_sha256']!=digest('/usr/bin/git'):parser.error('host tools changed since density preparation')
    if 'podman' in backends and inputs['podman_image']!=args.podman_image:parser.error('Podman image differs from prepared inputs')
    if {str(p.relative_to(args.rootfs)) for p in args.rootfs.rglob('*')} != {e['path'] for e in inputs['rootfs_manifest']}:parser.error('rootfs inventory differs from prepared input')
    for entry in inputs['rootfs_manifest']:
        path=args.rootfs/entry['path']
        if path.lstat().st_mode!=entry['mode']:parser.error('rootfs mode differs from prepared input')
        if entry['kind']=='file' and digest(path)!=entry['sha256']:parser.error('rootfs content differs from prepared input')
        if entry['kind']=='symlink' and str(path.readlink())!=entry['target']:parser.error('rootfs symlink differs from prepared input')
    if digest(args.rootfs/'bench/density-worker.py')!=digest(Path(__file__).with_name('density_worker.py')):parser.error('rootfs worker does not match this harness')
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir();shutil.copy2(args.binary,args.output/'bin/pvisor')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json')
    shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copy2(args.input_manifest,args.output/'input-manifest.json')
    report=dict(benchmark_id='B-DENSITY',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),binary_build=receipt,
        pvisor_sha256=digest(args.output/'bin/pvisor'),worker_sha256=digest(args.rootfs/'bench/density-worker.py'),
        firmware_sha256=digest(args.firmware/'libkrunfw.so.5'),host_kernel=os.uname().release,
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        input_manifest_sha256=digest(args.input_manifest),podman_preparation=inputs['podman_info'],
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(resources='fresh owned systemd cgroup per batch, two-core quota and affinity, fixed memory.max and no swap',
            ready='all surviving jobs hold at stdin barrier before release; all-ready required for reliable occupancy',
            useful='32 MiB touched/checksummed private data; four edits among 64 committed files; actual git status and full contents checked',
            idle='same Python environment, no private payload or edits, held at same barrier',
            cache='preprepared shared rootfs/image cache; cgroup counts charged cache and complete private backing, not all host shared caches',
            scope='fixed cgroup-budget capacity; not whole-machine net compression saving',
            failures='retain failed/partial batches and OOM; never count configured RAM or missing results as completed work',
            timing='batch launch through completion and host correctness verification; includes readiness barrier and membership checks',
            order='seeded randomized backend/workload/concurrency conditions each round; no timing exclusions'),rows=[],failures=[])
    save(args.output/'report.json',report);rng=random.Random(20261006)
    for trial in range(args.samples):
        conditions=[(b,w,n) for b in backends for w in workloads for n in levels];rng.shuffle(conditions)
        for backend,workload,count in conditions:
            index=len(report['rows'])+len(report['failures']);root=args.output/str(index);root.mkdir()
            config=dict(root=str(root),result=str(root/'result.json'),binary=str(args.output/'bin/pvisor'),
                worker=str(args.output/'harness/density_worker.py'),rootfs=str(args.rootfs),firmware=str(args.firmware),
                podman_root=str(args.podman_root or ''),podman_image=args.podman_image,
                podman_runroot=inputs['podman_runroot'],
                backend=backend,workload=workload,concurrency=count,trial=trial,
                budget_bytes=args.budget_mib*1024*1024,cpu_affinity=args.cpu_affinity,ready_timeout=args.ready_timeout)
            save(root/'config.json',config)
            print(trial,backend,workload,count,flush=True)
            with (root/'service.stdout').open('wb') as stdout,(root/'service.stderr').open('wb') as stderr:
                process=subprocess.run(['systemd-run','--user','--quiet','--wait','--pipe','--collect',
                    '--unit',f'pvisor-density-{os.getpid()}-{index}', '--property=MemoryAccounting=yes','--property=CPUAccounting=yes',
                    f'--property=MemoryMax={config["budget_bytes"]}','--property=MemorySwapMax=0','--property=CPUQuota=200%',
                    f'--property=CPUAffinity={args.cpu_affinity}','--property=OOMPolicy=continue',
                    '--property=TasksMax=8192','--property=TimeoutStopSec=10',
                    sys.executable,str(args.output/'harness/density.py'),'--internal-worker',str(root/'config.json')],
                    stdout=stdout,stderr=stderr)
            result=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='service lost before complete result',service_exit=process.returncode)
            result.update(backend=backend,workload=workload,concurrency=count,trial=trial,logs=str(root))
            report['rows' if result['correctness']=='passed' else 'failures'].append(result);save(args.output/'report.json',report)
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
