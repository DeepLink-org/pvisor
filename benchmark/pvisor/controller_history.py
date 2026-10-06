#!/usr/bin/env python3
"""Measure retained Controller history in independently budgeted processes.

Benchmark: B-CLUSTER (benchmark/README.md#b-cluster), role user-facing.
Motivation: retained records consume memory and increase restart waiting.
Conclusion sought: current query and warm-WAL replay costs, with whole-cgroup
physical peak and CPU at 1,000 through 1,000,000 retained records.
Design: frozen release scheduler_load v4, one size per fresh process/cgroup,
two host CPUs, 16 GiB and zero swap, NVMe TMPDIR; three independent processes
per size and thirty query batches within each. No HTTP/execution throughput,
industry ranking, stable retained-state footprint or restart-tail claim.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time

from density import cgroup, save
from reference_baselines import digest
from vm_memory import memory


def validate_result(report, size, samples, affinity):
    if (report.get('schema')!='pvisor-controller-history-load/v4' or report.get('benchmark_id')!='B-CLUSTER'
            or report.get('build_has_debug_assertions') is not False or not report.get('source_identity_unchanged_during_measurement')
            or len(report.get('rows',[]))!=1):raise ValueError('incorrect history implementation or source evidence')
    # /proc formats adjacent CPUs as a range; compare sets rather than text.
    actual=set()
    for part in report['affinity'].split(','):
        ends=list(map(int,part.split('-')));actual.update(range(ends[0],ends[-1]+1))
    if actual!=set(map(int,affinity.split(','))):raise ValueError('history CPU affinity differs')
    row=report['rows'][0]
    if (row['tasks']!=size or row['ready_tasks_before_poll']!=1 or row['cancelled_history']!=size-1
            or row['indexed_counts']['n']!=samples or not row['replay_fencing_verified']
            or row['current_polls_until_assignment']!=1
            or row['final_counts']!={'cancelled':size-1,'failed':1}):
        raise ValueError('incomplete retained history or durable replay verification')
    return row


def internal(config):
    root=Path(config['root']);group=cgroup();quota,period=map(int,(group/'cpu.max').read_text().split())
    if (int((group/'memory.max').read_text())!=config['memory_bytes'] or quota/period!=2
            or (group/'memory.swap.max').read_text().strip()!='0'
            or os.sched_getaffinity(0)!=set(map(int,config['cpu_affinity'].split(',')))):
        raise ValueError('history total resource controls differ')
    tmp=root/'wal-tmp';tmp.mkdir();env=os.environ.copy()|dict(TMPDIR=str(tmp))
    samples=[];stop=threading.Event();errors=[]
    def monitor():
        try:
            while not stop.wait(.05):
                point=memory(group);samples.append(point);save(root/'last-memory.json',point)
        except Exception as error:errors.append(str(error))
    thread=threading.Thread(target=monitor,daemon=True);thread.start();before=memory(group)
    argv=[config['example'],'--tasks',str(config['size']),'--ready','1','--samples',str(config['query_samples']),
          '--indexed-batch','100','--output',str(root/'example-report.json')]
    started=time.perf_counter_ns()
    try:
        with (root/'example.stdout').open('wb') as stdout,(root/'example.stderr').open('wb') as stderr:
            result=subprocess.run(argv,env=env,stdout=stdout,stderr=stderr,timeout=3600)
        after=memory(group)
        if result.returncode:raise ValueError('history example failed; retained stderr')
        report=json.loads((root/'example-report.json').read_text())
        validate_result(report,config['size'],config['query_samples'],config['cpu_affinity'])
        if errors or any(p['events'].get('oom',0) or p['events'].get('oom_kill',0) for p in [before,after,*samples]):
            raise ValueError('monitor failure or OOM invalidates complete history observation')
        return dict(correctness='passed',example_report=report,before=before,after=after,samples=samples,
            wall_ms=(time.perf_counter_ns()-started)/1e6,cgroup=str(group),memory_bytes=config['memory_bytes'],
            physical_scope='whole lifecycle peak including history creation, record-validation temporaries, WAL/cache and warm replay; not stable retained-state footprint')
    finally:stop.set();thread.join()


def main():
    if sys.argv[1:2]==['--internal-worker']:
        config=json.loads(Path(sys.argv[2]).read_text())
        try:row=internal(config)
        except Exception as error:row=dict(correctness='failed',error=str(error))
        save(Path(config['root'])/'result.json',row);raise SystemExit(0 if row['correctness']=='passed' else 1)
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('example','build-receipt','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--sizes',default='1000,10000,100000,1000000');parser.add_argument('--repetitions',type=int,default=3)
    parser.add_argument('--query-samples',type=int,default=30);parser.add_argument('--cpu-affinity',default='0,1')
    parser.add_argument('--memory-mib',type=int,default=16384)
    args=parser.parse_args();sizes=list(map(int,args.sizes.split(',')))
    if not sizes or len(sizes)!=len(set(sizes)) or min(sizes)<2 or args.repetitions<1 or args.query_samples<30 or args.memory_mib<1024:
        parser.error('invalid history conditions')
    args.example=args.example.resolve();args.build_receipt=args.build_receipt.resolve();args.output=args.output.resolve()
    storage=subprocess.check_output(['findmnt','--noheadings','--output','SOURCE,FSTYPE','--target',str(args.output.parent)],text=True).strip()
    if storage.split()[-1] in ('tmpfs','ramfs'):parser.error('history WAL requires disk storage, not tmpfs/ramfs')
    receipt=json.loads(args.build_receipt.read_text())
    if digest(args.example)!=receipt['binaries']['scheduler_load']['sha256'] or digest(args.build_receipt.parent/'source-manifest.json')!=receipt['source_manifest_sha256']:
        parser.error('history source/binary build receipt differs')
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir();shutil.copy2(args.example,args.output/'bin/scheduler_load')
    for source,name in [(args.build_receipt,'build-receipt.json'),(args.build_receipt.parent/'source-manifest.json','source-manifest.json')]:shutil.copy2(source,args.output/name)
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    report=dict(benchmark_id='B-CLUSTER',mechanism='current Controller retained history, no task execution',
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),host_kernel=os.uname().release,binary_build=receipt,
        storage=storage,
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        protocol=dict(cpu='two-core whole-cgroup quota and affinity',swap=0,
            storage='WAL TMPDIR on output filesystem, not /tmp tmpfs; fsync and warm replay',
            memory='whole lifecycle physical peak; process RSS only secondary; not stationary retained-state footprint',
            statistics='three fresh processes per size by default; query batches correlated within process; restart/memory no P95/P99',
            scope='synthetic cancelled history plus one ready task completed with a deliberate failed terminal receipt for fencing/replay verification; durable typed API, not task execution or HTTP/VM/Kubernetes/Ray throughput'),rows=[])
    save(args.output/'report.json',report)
    for trial in range(args.repetitions):
        for size in sizes:
            root=args.output/f'{size}-{trial}';root.mkdir()
            config=dict(root=str(root),example=str(args.output/'bin/scheduler_load'),size=size,query_samples=args.query_samples,
                memory_bytes=args.memory_mib*1024**2,cpu_affinity=args.cpu_affinity)
            save(root/'config.json',config);print(trial,size,flush=True)
            argv=['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit',f'pvisor-history-{os.getpid()}-{size}-{trial}',
                '--property=MemoryAccounting=yes',f'--property=MemoryMax={config["memory_bytes"]}','--property=MemorySwapMax=0',
                '--property=CPUQuota=200%','--property=CPUAffinity='+args.cpu_affinity.replace(',',' '),
                sys.executable,str(args.output/'harness/controller_history.py'),'--internal-worker',str(root/'config.json')]
            with (root/'service.stdout').open('wb') as stdout,(root/'service.stderr').open('wb') as stderr:result=subprocess.run(argv,stdout=stdout,stderr=stderr)
            value=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='lost reporter',service_exit=result.returncode)
            value.update(size=size,trial=trial,logs=str(root));report['rows'].append(value);save(args.output/'report.json',report)
    if any(r['correctness']!='passed' for r in report['rows']):raise SystemExit(1)


if __name__=='__main__':main()
