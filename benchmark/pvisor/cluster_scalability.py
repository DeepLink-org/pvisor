#!/usr/bin/env python3
"""Validated useful-task throughput under one fixed Controller/Worker budget.

Benchmark: B-CLUSTER (benchmark/README.md#b-cluster), role user-facing.
Motivation: users need completed work as Worker count changes, without
silently adding host resources or treating guest ready rate as throughput.
Conclusion sought: completed verified tasks/s at fixed total CPU/memory;
complete charged cgroup memory and separate durable-history restart costs.
Design: owned systemd slice with fixed total quota/affinity/memory/no swap;
1/2/4 one-slot Workers; same twelve Python/Git tasks; randomized rounds;
full checksum/token/task result checks; all failures retained.
"""
import argparse
import copy
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import random
import shutil
import signal
import statistics
import subprocess
import threading
import time
import traceback

from density import sample_memory
from reference_baselines import digest, verified_build_receipt

ROOT=Path(__file__).resolve().parents[2]
MIB=1024**2
spec=importlib.util.spec_from_file_location('quickstart',ROOT/'scripts/cluster-quickstart.py')
qs=importlib.util.module_from_spec(spec);spec.loader.exec_module(qs)
EXPECTED=hashlib.sha256(b'x'*(32*MIB)).hexdigest()


def validate_result(record, token, node):
    if record.get('phase')!='succeeded':raise ValueError('task did not succeed')
    if record.get('lease',{}).get('key',{}).get('worker_id')!='qs-'+node:raise ValueError('task ran on the wrong Worker')
    output=record.get('result',{}).get('output',{}).get('stdout','')
    markers=[json.loads(line.removeprefix('CLUSTER_RESULT ')) for line in output.splitlines() if line.startswith('CLUSTER_RESULT ')]
    if len(markers)!=1:raise ValueError('missing unique useful-task result')
    result=markers[0]
    if result.get('token')!=token or result.get('bytes')!=32*MIB or result.get('checksum')!=EXPECTED or result.get('changes')!=4 or result.get('integrity')!='passed':
        raise ValueError('task returned incorrect work or memory content')
    return result


class BudgetSession(qs.Session):
    def __init__(self,state,slice_name,affinity):
        super().__init__(state);self.slice_name=slice_name;self.affinity=affinity

    def launch(self,role):
        binary=Path(self.config['bin_dir'])
        if role=='controller':
            argv=[str(binary/'pvisor-cluster'),'serve','--listen',self.config['url'].removeprefix('http://'),
                '--journal',str(self.state/'journal'),'--lease-ms','30000','--quotas',str(self.state/'quotas.json'),
                '--max-journal-bytes',str(64*MIB),'--max-artifact-bytes',str(128*MIB),
                '--artifact-limits',str(self.state/'artifact-limits.json')]
            maximum,quota=256*MIB,'25%'
        else:
            node=role[-1]
            argv=[str(binary/'pvisor-worker'),'--id','qs-'+node,'--state',str(self.state/role),
                '--backend',self.config['backend'],'--config',str(self.state/'worker.toml'),
                '--slots','1','--memory-bytes',str(128*MIB),'--cpu-millis','500','--poll-ms','200',
                '--label','quickstart-node='+node]
            maximum,quota=512*MIB,'50%'
        command=['systemd-run','--user','--quiet','--service-type=exec','--unit='+self.unit(role),
            '--slice='+self.slice_name,'--property=MemoryAccounting=yes','--property=CPUAccounting=yes',
            '--property=MemoryMax='+str(maximum),'--property=MemorySwapMax=0','--property=CPUQuota='+quota,
            '--property=CPUAffinity='+self.affinity.replace(',',' '),'--property=TasksMax=512',
            '--property=TimeoutStopSec=10','--property=WorkingDirectory='+str(self.state/'workspace')]
        command += ['--setenv='+k+'='+self.env[k] for k in ('PVISOR_CLUSTER_URL','PVISOR_CLUSTER_TOKEN',
            'PVISOR_CLUSTER_WORKER_TOKEN','TOKIO_WORKER_THREADS','NO_PROXY') if k in self.env]
        qs.execute(command+argv)


def run_condition(args, report, size, trial, index):
    root=args.output/'trials'/str(index);root.mkdir(parents=True)
    state=args.state/str(index)
    qs.prepare(state,'vm',args.bin_dir,args.firmware_dir)
    # Prepared Python/Git inputs are independent of the quickstart shell rootfs.
    shutil.copytree(args.rootfs,state/'rootfs',dirs_exist_ok=True)
    shutil.copy2(Path(__file__).with_name('cluster_worker.py'),state/'rootfs/bench/cluster-worker.py')
    qs.write_json(state/'quotas.json',{'quickstart':{'slots':size,'memory_bytes':size*128*MIB,'cpu_millis':size*500}})
    slice_name=f'pvisor-bench-{os.getpid()}-{index}.slice'
    session=BudgetSession(state,slice_name,args.cpu_affinity);roles=['controller',*[f'worker-{n}' for n in 'abcd'[:size]]]
    row=dict(workers=size,trial=trial,planned=args.tasks,submitted=0,completed=0,failed=0,correctness='failed',logs=str(root),state=str(state),samples=[],tasks=[])
    report['rows'].append(row)
    stop=threading.Event();sampler=None
    try:
        qs.execute(['systemctl','--user','set-property','--runtime',slice_name,'MemoryAccounting=yes','CPUAccounting=yes',
            f'MemoryMax={args.budget_mib*MIB}','MemorySwapMax=0','CPUQuota=200%'])
        for role in roles:
            session.launch(role)
            if role=='controller':session.until(lambda:session.api('/health'),timeout=20)
        session.until(lambda:len(session.ctl('workers'))==size,timeout=30)
        raw=qs.execute(['systemctl','--user','show',slice_name,'-p','ControlGroup','--value']).strip()
        group=Path('/sys/fs/cgroup')/raw.lstrip('/')
        if int((group/'memory.max').read_text())!=args.budget_mib*MIB or (group/'memory.swap.max').read_text().strip()!='0':raise ValueError('total memory/swap budget missing')
        quota,period=map(int,(group/'cpu.max').read_text().split())
        if quota/period!=2:raise ValueError('fixed total two-core quota missing')
        row['controls']=dict(slice=slice_name,cgroup=str(group),memory_max=args.budget_mib*MIB,cpu_max=[quota,period],swap_max=0)
        row['service_controls']={}
        for role in roles:
            path=qs.execute(['systemctl','--user','show',session.unit(role),'-p','ControlGroup','--value']).strip()
            child=Path('/sys/fs/cgroup')/path.lstrip('/')
            if not child.is_relative_to(group):raise ValueError('service escaped total resource slice')
            maximum=int((child/'memory.max').read_text());q,p=map(int,(child/'cpu.max').read_text().split())
            if maximum!=(256 if role=='controller' else 512)*MIB or q/p!=(.25 if role=='controller' else .5):raise ValueError('per-service caps differ')
            if (child/'memory.swap.max').read_text().strip()!='0':raise ValueError('per-service swap allowed')
            for pid in (child/'cgroup.procs').read_text().split():
                if os.sched_getaffinity(int(pid))!=set(map(int,args.cpu_affinity.split(','))):raise ValueError('service affinity differs')
            row['service_controls'][role]=dict(cgroup=str(child),memory_max=maximum,cpu_max=[q,p])
        row['before']=sample_memory(group)
        def monitor():
            try:
                while not stop.wait(.05):row['samples'].append(sample_memory(group))
            except Exception as error:row['monitor_error']=str(error)
        sampler=threading.Thread(target=monitor,daemon=True);sampler.start()
        base=json.loads((state/'inputs/hello.json').read_text());started=time.perf_counter_ns()
        for i in range(args.tasks):
            node='abcd'[i%size];token=f'cluster-{index}-{i}'
            task=copy.deepcopy(base);task['id']=task['run']['run_id']=token;task['labels']['quickstart-node']=node
            task['run']['invocation']['program']='/usr/bin/python3'
            task['run']['invocation']['args']=['/bench/cluster-worker.py',token]
            task['run']['runtime']['resource_limits']['cpu_time_ms']=10000
            task['retain_artifacts']={'version':1,'trace':True,'workspace_upper':True}
            path=state/'inputs'/f'{token}.json';qs.write_json(path,task)
            session.ctl('submit',str(path));row['submitted']+=1;row['tasks'].append(dict(id=token,node=node))
        for task in row['tasks']:
            record=session.wait(task['id']);qs.write_json(root/(task['id']+'.json'),record)
            try:task['result']=validate_result(record,task['id'],task['node']);row['completed']+=1
            except Exception as error:task['error']=str(error);row['failed']+=1
        row['completion_ms']=(time.perf_counter_ns()-started)/1e6
        row['after']=sample_memory(group)
        if row.get('monitor_error'):raise ValueError('resource monitor failed: '+row['monitor_error'])
        if row['after']['events'].get('oom') or row['after']['events'].get('oom_kill'):raise ValueError('OOM invalidates reliable throughput')
        row['validated_per_second']=row['completed']/(row['completion_ms']/1000)
        if row['completed']!=args.tasks or row['failed']:raise ValueError('batch did not complete all useful tasks')
        row['correctness']='passed'
    except Exception as error:row.update(error=str(error),traceback=traceback.format_exc())
    finally:
        stop.set()
        if sampler:sampler.join()
        for role in reversed(roles):session.stop(role)
        qs.execute(['systemctl','--user','stop',slice_name])
        qs.write_json(root/'result.json',row)
    return row


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('state','output','firmware-dir','bin-dir','build-receipt','rootfs','input-manifest'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--sizes',default='1,2,4');parser.add_argument('--repetitions',type=int,default=30)
    parser.add_argument('--warmups',type=int,default=3);parser.add_argument('--tasks',type=int,default=12)
    parser.add_argument('--budget-mib',type=int,default=2048);parser.add_argument('--cpu-affinity',default='0,1')
    args=parser.parse_args();sizes=list(map(int,args.sizes.split(',')))
    if not sizes or set(sizes)-{1,2,4} or len(sizes)!=len(set(sizes)) or args.repetitions<1 or args.warmups<0 or args.tasks<max(sizes) or args.budget_mib<1024:parser.error('invalid conditions')
    for k in ('state','output','firmware_dir','bin_dir','build_receipt','rootfs','input_manifest'):setattr(args,k,getattr(args,k).resolve())
    if args.state.exists() or args.output.exists():parser.error('state/output must be new directories')
    receipt=json.loads(args.build_receipt.read_text())
    for name in ('pvisor-cluster','pvisor-worker'):
        if digest(args.bin_dir/name)!=receipt['binaries'][name]['sha256']:parser.error('binary differs from build receipt')
    if digest(args.build_receipt.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:parser.error('source manifest differs from build receipt')
    inputs=json.loads(args.input_manifest.read_text())
    if {str(p.relative_to(args.rootfs)) for p in args.rootfs.rglob('*')}!={r['path'] for r in inputs['rootfs_manifest']}:parser.error('rootfs inventory differs')
    for r in inputs['rootfs_manifest']:
        p=args.rootfs/r['path']
        if p.lstat().st_mode!=r['mode'] or (r['kind']=='file' and digest(p)!=r['sha256']) or (r['kind']=='symlink' and str(p.readlink())!=r['target']):parser.error('rootfs content/mode differs')
    args.state.mkdir();args.output.mkdir();(args.output/'bin').mkdir()
    for name in ('pvisor-cluster','pvisor-worker'):shutil.copy2(args.bin_dir/name,args.output/'bin'/name)
    args.bin_dir=args.output/'bin'
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json');shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copy2(args.input_manifest,args.output/'input-manifest.json');shutil.copy2(__file__,args.output/'cluster_scalability.py')
    shutil.copy2(Path(__file__).with_name('cluster_worker.py'),args.output/'cluster_worker.py');shutil.copy2(ROOT/'scripts/cluster-quickstart.py',args.output/'cluster-quickstart.py')
    report=dict(benchmark_id='B-CLUSTER',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),binary_build=receipt,
        source_manifest_sha256=receipt['source_manifest_sha256'],host_kernel=os.uname().release,
        input_manifest_sha256=digest(args.input_manifest),worker_sha256=digest(args.output/'cluster_worker.py'),
        harness_sha256=digest(args.output/'cluster_scalability.py'),quickstart_sha256=digest(args.output/'cluster-quickstart.py'),
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(total='one owned slice, 2-core quota, CPUs '+args.cpu_affinity+', fixed memory.max, zero swap',
            service_caps='Controller 256 MiB/25%; each one-slot Worker 512 MiB/50%; shared total slice always enforced',
            task='same task count at every Worker count; each Python/Git task checks 32 MiB eight times, edits four of 64 files and validates Git status',
            measurement='submit through all validated completions; no guest sleep or ready-rate throughput substitution',
            scope='single-host KVM, prepared Python/Git rootfs; no model inference or multi-host claim',
            cache='warm prepared shared inputs; no cache eviction; shared prepared caches may be charged outside owned slice',
            ordering='seeded randomized Worker-count conditions per round; no timing exclusions'),rows=[])
    rng=random.Random(20261006)
    for trial in range(-args.warmups,args.repetitions):
        conditions=sizes.copy();rng.shuffle(conditions)
        for size in conditions:
            row=run_condition(args,report,size,trial,len(report['rows']));qs.write_json(args.output/'report.json',report)
            print(trial,size,row['correctness'],row.get('completion_ms'),flush=True)
            if row['correctness']!='passed' and trial<0:raise SystemExit(1)
    if any(r['correctness']!='passed' for r in report['rows']):raise SystemExit(1)


if __name__=='__main__':main()
