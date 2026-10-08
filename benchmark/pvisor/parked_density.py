#!/usr/bin/env python3
"""Recoverable parked-state capacity under pressure, separate from active density.

Benchmark: B-DENSITY (benchmark/README.md#b-density), role user-facing.
Motivation: waiting Agent states consume resources until they can be recovered.
Conclusion sought: validated parked/recovered capacity and cost, including
backing/cache, compared with paused processes and containers under one budget.
Design: identical static 64 MiB/Git worker, repeated/random payload, shared
stdin barrier, current raw/compressed Job snapshots versus SIGSTOP/Podman pause;
two-core 2 GiB zero-swap cgroup, fresh batches, one recovery slot, all failures.
Not active concurrency, SDK offload, automatic cold paging or portable OCI
checkpoint equivalence. Prepared common caches may be charged outside the group.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess
import sys
import threading
import time
import traceback

from density import cgroup, save
from reference_baselines import digest, validate_bundle_execution, verified_build_receipt
from v1.oci import verify_prepared
from vm_memory import memory
from retained_snapshot_archive import archive_completed_snapshots


def verify_result(ready, result, pattern, seed):
    if ready.get('kind')!=pattern or ready.get('seed')!=seed or ready.get('bytes')!=64*1024**2:
        raise ValueError('wrong parked memory fixture')
    if not ready.get('token') or not ready.get('checksum') or not ready.get('pid'):
        raise ValueError('missing original execution identity')
    if any(result.get(key)!=ready.get(key) for key in ('kind','seed','bytes','token','pid','checksum')):
        raise ValueError('recovery changed execution identity or full memory')
    if result.get('integrity')!='passed' or result.get('changes')!=4 or result.get('first_scan_ms',-1)<0:
        raise ValueError('incomplete recovered work')


def launch(argv, work, root, env):
    stderr=(root/'stderr.log').open('w')
    process=subprocess.Popen(argv,cwd=work,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,
        stderr=stderr,text=True,bufsize=1,start_new_session=True)
    record=dict(process=process,stderr=stderr,root=root,markers={},reader_error=None)
    def read():
        try:
            with (root/'stdout.log').open('w') as log:
                for line in process.stdout:
                    log.write(line);log.flush()
                    for prefix,key in (('PVISOR_PARKED_READY ','ready'),('PVISOR_PARKED_RESULT ','result')):
                        if line.startswith(prefix):
                            if key in record['markers']:raise ValueError('duplicate execution marker')
                            record['markers'][key]=json.loads(line[len(prefix):])
        except Exception as error:record['reader_error']=str(error)
    reader=threading.Thread(target=read,daemon=True);reader.start();record['reader']=reader
    return record


def wait_marker(record,key,timeout=90):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        if record['reader_error']:raise ValueError(record['reader_error'])
        if key in record['markers']:return record['markers'][key]
        if record['process'].poll() is not None and not record['reader'].is_alive():break
        time.sleep(.005)
    raise RuntimeError('missing '+key+' execution marker; retained stdout/stderr')


def command(argv, root, name, timeout=180, env=None):
    started=time.perf_counter_ns()
    result=subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout,env=env)
    elapsed=(time.perf_counter_ns()-started)/1e6
    (root/(name+'.stdout')).write_bytes(result.stdout);(root/(name+'.stderr')).write_bytes(result.stderr)
    save(root/(name+'.json'),dict(argv=argv,exit_code=result.returncode))
    if result.returncode:raise RuntimeError(name+' failed; retained command output')
    return result,elapsed


def inside_budget(pid, group):
    actual=Path(f'/proc/{pid}/cgroup').read_text().strip().split('::',1)[1]
    expected='/'+str(group.relative_to('/sys/fs/cgroup'))
    if actual!=expected and not actual.startswith(expected+'/'):raise ValueError('payload/helper escaped total budget')


def inspect_podman(podman, cid):
    return json.loads(subprocess.check_output(podman+['inspect',cid],text=True))[0]['State']


def internal(config):
    root=Path(config['root']);group=cgroup();budget=config['budget_bytes']
    quota,period=map(int,(group/'cpu.max').read_text().split())
    if int((group/'memory.max').read_text())!=budget or (group/'memory.swap.max').read_text().strip()!='0' or quota/period!=2:
        raise ValueError('total CPU/memory/swap budget missing')
    if os.sched_getaffinity(0)!=set(map(int,config['cpu_affinity'].split(','))):raise ValueError('CPU affinity missing')
    env=os.environ.copy()|dict(PVISOR_FS_PROFILE='0',PVISOR_STARTUP_TIMING='0',GIT_CONFIG_GLOBAL='/dev/null',GIT_CONFIG_NOSYSTEM='1',LC_ALL='C')
    env.pop('PVISOR_TEST_ALLOW_NO_USERNS',None)
    fixture=root/'fixture';(fixture/'files').mkdir(parents=True)
    for i in range(64):(fixture/'files'/f'f{i:03}').write_text(f'old-{i}\n')
    for argv in (['git','init','-q'],['git','config','gc.auto','0'],['git','config','core.hooksPath','/dev/null'],['git','add','files'],
        ['git','-c','user.name=benchmark','-c','user.email=benchmark@invalid','commit','-qm','fixture']):
        subprocess.run(argv,cwd=fixture,env=env,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    podman=['podman','--root',config['podman_root'],'--runroot',config['podman_runroot'],'--storage-driver','overlay','--cgroup-manager','cgroupfs']
    jobs=[];samples=[];stop=threading.Event();monitor_errors=[]
    def monitor():
        try:
            while not stop.wait(.05):
                point=memory(group);samples.append(point);save(root/'last-memory.json',point)
        except Exception as error:monitor_errors.append(str(error))
    thread=threading.Thread(target=monitor,daemon=True);thread.start();before=memory(group)
    started=time.perf_counter_ns();backend=config['backend'];snapshot=backend in ('snapshot-raw','snapshot-compressed')
    try:
        for i in range(config['concurrency']):
            jobroot=root/f'j{i}';jobroot.mkdir();work=jobroot/'work';stage=jobroot/'stage'
            subprocess.run(['cp','-a','--reflink=auto',str(fixture),str(work)],check=True)
            seed=20261006+config['trial']*128+i
            payload=[config['worker'] if backend=='native-paused' else '/bench/parked-memory-probe',config['pattern'],str(seed),
                '0,1' if snapshot else config['cpu_affinity'],'64']
            job=dict(root=jobroot,work=work,stage=stage,seed=seed,records=[]);jobs.append(job)
            jobenv=env|dict(PVISOR_RUN_HOME=str(jobroot/'runs'),XDG_CONFIG_HOME=str(jobroot/'config'))
            if backend=='native-paused':argv=payload
            elif backend=='podman-paused':
                argv=podman+['run','--rm','-i','--pull','never','--cgroups','enabled','--network','none',
                    '--cgroup-parent','/'+str(group.relative_to('/sys/fs/cgroup')),
                    '--cidfile',str(jobroot/'cid'),'--conmon-pidfile',str(jobroot/'conmon.pid'),'-v',str(work)+':/work:Z','-w','/work',
                    '--entrypoint',payload[0],config['podman_image'],*payload[1:]]
            else:
                argv=[config['binary'],'run','--no-agent-defaults','--overlaynet','off','--stdio','inherit','--timeout','1200s',
                    '--vm','--rootfs',config['rootfs'],'--vm-library-dir',config['firmware'],'--cpu','2','--memory','256MiB','--stage',str(stage),'--',*payload]
            record=launch(argv,work,jobroot,jobenv);job['records'].append(record)
            ready=wait_marker(record,'ready');job['ready']=ready
            if ready['kind']!=config['pattern'] or ready['seed']!=seed or ready['bytes']!=64*1024**2:raise ValueError('wrong ready payload')
            park_started=time.perf_counter_ns()
            if snapshot:
                _,park_ms=command([config['binary'],'suspend',str(stage),'--ram-storage',backend.removeprefix('snapshot-'),'--json'],jobroot,'suspend',env=jobenv)
                if record['process'].wait(timeout=30)!=0:raise ValueError('captured launcher did not stop cleanly')
                state=json.loads((stage/'execution-job.json').read_text())
                if state['state']!='suspended':raise ValueError('missing confirmed suspended Job')
                job['suspended_state']=state['state']
            elif backend=='podman-paused':
                cid=(jobroot/'cid').read_text().strip();job['cid']=cid
                state=inspect_podman(podman,cid)
                inside_budget(state['Pid'],group);inside_budget(int((jobroot/'conmon.pid').read_text()),group)
                _,park_ms=command(podman+['pause',cid],jobroot,'pause')
                if not inspect_podman(podman,cid)['Paused']:raise ValueError('container was not frozen')
            else:
                inside_budget(record['process'].pid,group);os.killpg(record['process'].pid,signal.SIGSTOP)
                deadline=time.monotonic()+5
                while time.monotonic()<deadline:
                    status=Path(f'/proc/{record["process"].pid}/status').read_text()
                    if any(line.startswith('State:') and '\tT ' in line for line in status.splitlines()):break
                    time.sleep(.005)
                else:raise ValueError('native process did not stop')
                park_ms=(time.perf_counter_ns()-park_started)/1e6
            job['park_ms']=park_ms
        tokens=[job['ready']['token'] for job in jobs]
        if len(tokens)!=len(set(tokens)):raise ValueError('duplicate original execution identity')
        creation_ms=(time.perf_counter_ns()-started)/1e6
        time.sleep(2)
        for job in jobs:
            if snapshot:
                if json.loads((job['stage']/'execution-job.json').read_text())['state']!='suspended':raise ValueError('Job left parked barrier')
            elif backend=='podman-paused':
                state=inspect_podman(podman,job['cid'])
                if not state['Paused'] or state['Pid']<=0:raise ValueError('container left parked barrier')
                inside_budget(state['Pid'],group)
            else:
                process=job['records'][0]['process']
                if process.poll() is not None:raise ValueError('paused process died before barrier')
                status=Path(f'/proc/{process.pid}/status').read_text()
                if not any(line.startswith('State:') and '\tT ' in line for line in status.splitlines()):raise ValueError('native process left parked barrier')
        parked=memory(group);barrier_ms=(time.perf_counter_ns()-started)/1e6
        outcomes=[];restore_started=time.perf_counter_ns()
        for job in jobs:
            resumed_started=time.perf_counter_ns();record=job['records'][0]
            if snapshot:
                resume_root=job['root']/'resumed';resume_root.mkdir()
                record=launch([config['binary'],'resume',str(job['stage'])],job['work'],resume_root,
                    env|dict(PVISOR_RUN_HOME=str(job['root']/'runs'),XDG_CONFIG_HOME=str(job['root']/'config')))
                job['records'].append(record)
            elif backend=='podman-paused':command(podman+['unpause',job['cid']],job['root'],'unpause')
            else:os.killpg(record['process'].pid,signal.SIGCONT)
            record['process'].stdin.write('GO '+job['ready']['token']+'\n');record['process'].stdin.flush();record['process'].stdin.close()
            result=wait_marker(record,'result',180)
            if record['process'].wait(timeout=30)!=0:raise ValueError('recovered task exited nonzero')
            record['reader'].join(timeout=5)
            if record['reader'].is_alive() or record['reader_error']:raise ValueError('incomplete or duplicate final output')
            verify_result(job['ready'],result,config['pattern'],job['seed'])
            recovered_ms=(time.perf_counter_ns()-resumed_started)/1e6
            if snapshot:
                state=json.loads((job['stage']/'execution-job.json').read_text());active=Path(state['active_stage'])
                if state['state']!='terminal' or not active.resolve().is_relative_to(job['stage'].resolve()):raise ValueError('invalid recovered Job state/path')
                bundle=json.loads((active/'run-bundle.json').read_text());validate_bundle_execution(bundle,'pvisor-vm','rootless_process')
                overlay=json.loads((active/'run.json').read_text())['overlay'];upper=Path(overlay['upper']['upper_dir'])
                if not upper.resolve().is_relative_to(job['stage'].resolve()):raise ValueError('recovered upper outside owned Job')
            for i in range(64):
                old=f'old-{i}\n';changed=f'new-{i}\n' if i<4 else old
                if (job['work']/'files'/f'f{i:03}').read_text()!=(old if snapshot else changed):raise ValueError('host workspace content changed incorrectly')
                if snapshot and i<4 and (upper/'files'/f'f{i:03}').read_text()!=changed:raise ValueError('recovered staged edit missing')
            outcomes.append(dict(correctness='passed',ready=job['ready'],result=result,recovery_ms=recovered_ms,park_ms=job['park_ms'],logs=str(job['root'])))
        after=memory(group)
        if monitor_errors:raise ValueError('memory monitor failed: '+str(monitor_errors))
        if after['events'].get('oom',0) or after['events'].get('oom_kill',0):raise ValueError('OOM invalidates reliable parked capacity')
        return dict(correctness='passed',attempted=config['concurrency'],parked=len(jobs),completed=len(outcomes),failed=0,
            creation_and_park_ms=creation_ms,parked_barrier_ms=barrier_ms,recovery_batch_ms=(time.perf_counter_ns()-restore_started)/1e6,
            before=before,parked_memory=parked,after=after,samples=samples,outcomes=outcomes,
            budget_bytes=budget,cgroup=str(group),recovery_slots=1)
    except Exception as error:
        return dict(correctness='failed',error=str(error),traceback=traceback.format_exc(),attempted=config['concurrency'],
            created=len(jobs),ready=sum('ready' in job for job in jobs),parked=sum('park_ms' in job for job in jobs),
            completed=len(outcomes) if 'outcomes' in locals() else 0,outcomes=outcomes if 'outcomes' in locals() else [],
            before=before,after=memory(group),samples=samples,budget_bytes=budget,cgroup=str(group))
    finally:
        stop.set();thread.join()
        for job in jobs:
            for record in job['records']:
                if record['process'].poll() is None:
                    os.killpg(record['process'].pid,signal.SIGKILL);record['process'].wait(timeout=30)
                record['reader'].join(timeout=5);record['stderr'].close()
            if backend=='podman-paused' and (job['root']/'cid').is_file():
                cid=(job['root']/'cid').read_text().strip()
                subprocess.run(podman+['rm','--force',cid],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30)


def main():
    if sys.argv[1:2]==['--internal-worker']:
        config=json.loads(Path(sys.argv[2]).read_text())
        try:result=internal(config)
        except Exception as error:result=dict(correctness='failed',error=str(error),traceback=traceback.format_exc())
        save(Path(config['result']),result);raise SystemExit(0 if result['correctness']=='passed' else 1)
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','build-receipt','worker','worker-receipt','firmware','rootfs','input-manifest','podman-root','output'):
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--podman-image',required=True)
    parser.add_argument('--backends',default='native-paused,podman-paused,snapshot-raw,snapshot-compressed')
    parser.add_argument('--patterns',default='repeated,random');parser.add_argument('--concurrencies',default='1,2,4,8,16,32,64,128')
    parser.add_argument('--samples',type=int,default=5);parser.add_argument('--budget-mib',type=int,default=2048);parser.add_argument('--cpu-affinity',default='0,1')
    parser.add_argument('--archive-completed-snapshots',action='store_true',help='losslessly retain verified successful snapshot trees after measured service exits')
    args=parser.parse_args();backends=args.backends.split(',');patterns=args.patterns.split(',');levels=list(map(int,args.concurrencies.split(',')))
    if (set(backends)-{'native-paused','podman-paused','snapshot-raw','snapshot-compressed'} or set(patterns)-{'repeated','random'}
            or len(set(backends))!=len(backends) or len(set(patterns))!=len(patterns) or len(set(levels))!=len(levels)
            or not levels or min(levels)<1 or max(levels)>128 or args.samples<1 or args.budget_mib!=2048):parser.error('invalid registered conditions/budget')
    for key,value in vars(args).items():
        if isinstance(value,Path):setattr(args,key,value.resolve())
    receipt=verified_build_receipt(args.build_receipt,args.binary);inputs=json.loads(args.input_manifest.read_text());worker_receipt=json.loads(args.worker_receipt.read_text())
    verify_prepared(args.rootfs,inputs)
    if args.podman_root!=Path(inputs['podman_info']['store']['graphRoot']).resolve():parser.error('Podman store differs from private preparation')
    if digest('/usr/bin/git')!=inputs['host_git_sha256']:parser.error('native Git differs from prepared guest/OCI tool')
    if worker_receipt.get('source_sha256')!=digest(Path(__file__).with_name('parked_memory_probe.rs')) or worker_receipt.get('target')!='x86_64-unknown-linux-musl':
        parser.error('static worker source/target differs from this frozen harness')
    if digest(args.worker)!=worker_receipt['worker_sha256'] or digest(args.rootfs/'bench/parked-memory-probe')!=worker_receipt['worker_sha256']:
        parser.error('native/guest static worker bytes differ')
    if worker_receipt!=inputs['parked_worker_build'] or inputs['podman_image']!=args.podman_image:parser.error('prepared worker/image receipt differs')
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir()
    for source,name in ((args.binary,'pvisor'),(args.worker,'parked-memory-probe')):shutil.copy2(source,args.output/'bin'/name)
    for source,name in ((args.build_receipt,'build-receipt.json'),(args.build_receipt.parent/'source-manifest.json','source-manifest.json'),
            (args.input_manifest,'input-manifest.json'),(args.worker_receipt,'worker-receipt.json')):shutil.copy2(source,args.output/name)
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    report=dict(benchmark_id='B-DENSITY',mechanism='parked current Job snapshots versus paused native/OCI process states',
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),host_kernel=os.uname().release,binary_build=receipt,worker_build=worker_receipt,
        firmware_sha256=digest(args.firmware/'libkrunfw.so.5'),input_manifest_sha256=digest(args.input_manifest),
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(budget='fixed two-core 2 GiB zero-swap parent, descendants included; payload/conmon membership checked',
            parked='sequential admission and parking, followed by common two-second parked barrier; not simultaneous running capacity',
            recovery='exact stdin token releases saved execution; one recovery slot, full 64 MiB checksum, Git edits and all contents',
            timing='creation includes private-view copying and every park call; per-task recovery includes release/launcher through checked guest output and exit, excluding subsequent host content validation',
            recovery_batch_timing='first release through all checked recoveries; includes between-task host content validation and final resource observation',
            controls='SIGSTOP and OCI pause retain processes; Job snapshots stop VM and preserve execution; portability/security differ',
            cache='warm prepared common cache may be charged outside; private backing/cache included; no net whole-machine claim',
            failures='no timing exclusions; retain failed/unknown/OOM; no sleep fallback for unsupported stdin restoration'),rows=[],failures=[])
    report['protocol']['artifact_retention']='all original bytes retained; optional verified tar+zstd archive after measured service; failed/unknown batches untouched'
    save(args.output/'report.json',report);rng=random.Random(20261006)
    for trial in range(args.samples):
        cases=[(backend,pattern,count) for backend in backends for pattern in patterns for count in levels];rng.shuffle(cases)
        for backend,pattern,count in cases:
            root=args.output/str(len(report['rows'])+len(report['failures']));root.mkdir()
            config=dict(root=str(root),result=str(root/'result.json'),binary=str(args.output/'bin/pvisor'),worker=str(args.output/'bin/parked-memory-probe'),
                rootfs=str(args.rootfs),firmware=str(args.firmware),podman_root=str(args.podman_root),podman_runroot=inputs['podman_runroot'],
                podman_image=args.podman_image,backend=backend,pattern=pattern,concurrency=count,trial=trial,
                budget_bytes=args.budget_mib*1024**2,cpu_affinity=args.cpu_affinity)
            save(root/'config.json',config);print(trial,backend,pattern,count,flush=True)
            cmd=['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit',f'pvisor-parked-density-{os.getpid()}-{root.name}',
                '--property=Delegate=yes','--property=CPUAccounting=yes','--property=MemoryAccounting=yes','--property=MemoryMax=2147483648',
                '--property=MemorySwapMax=0','--property=CPUQuota=200%','--property=CPUAffinity='+args.cpu_affinity.replace(',',' '),
                '--property=TasksMax=8192','--property=OOMPolicy=continue','--property=TimeoutStopSec=10',
                sys.executable,str(args.output/'harness/parked_density.py'),'--internal-worker',str(root/'config.json')]
            with (root/'service.stdout').open('wb') as stdout,(root/'service.stderr').open('wb') as stderr:
                result=subprocess.run(cmd,stdout=stdout,stderr=stderr)
            value=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='lost reporter; task outcomes unknown',service_exit=result.returncode)
            value.update(backend=backend,pattern=pattern,concurrency=count,trial=trial,logs=str(root))
            report['rows' if value['correctness']=='passed' else 'failures'].append(value);save(args.output/'report.json',report)
            if args.archive_completed_snapshots:
                retention=archive_completed_snapshots(root,value,result.returncode)
                if retention is not None:value['snapshot_retention']=retention;save(args.output/'report.json',report)
    verify_prepared(args.rootfs,inputs)
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
