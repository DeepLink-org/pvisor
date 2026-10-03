import concurrent.futures
import json
import os
from pathlib import Path
import resource
import threading
import time


def snapshot(roots):
    processes={}
    for path in Path('/proc').iterdir():
        if not path.name.isdigit():continue
        try:
            fields=(path/'stat').read_text().rsplit(') ',1)[1].split()
            rss=next(int(line.split()[1]) for line in (path/'status').read_text().splitlines() if line.startswith('VmRSS:'))
            processes[int(path.name)]=(int(fields[1]),rss)
        except (OSError,ValueError,StopIteration):continue
    owned=set(roots)
    while True:
        additions={pid for pid,(parent,_) in processes.items() if parent in owned}-owned
        if not additions:break
        owned.update(additions)
    return sum(processes[pid][1] for pid in owned if pid in processes),len(owned&processes.keys())


def run(ctx):
    backends=['native','host','staged','safe','vm']
    if ctx.image:backends+=['podman','container']
    # Workers all hold for 1s after recording ready; this is an occupancy probe.
    payload=['/bin/sh','-c','printf PVISOR_DENSITY_READY; sleep 1']
    original_run=ctx.run
    for backend in backends:
        for concurrency in (1,8,32,128):
            available=next(int(line.split()[1]) for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemAvailable:'))
            if backend=='vm' and concurrency*256*1024>available*0.65:
                ctx.capabilities[f'density/{backend}/{concurrency}']={'state':'not-measured','reason':'memory guard: 256 MiB estimated RSS/VM exceeds 65% of available host memory'}
                ctx.save();continue
            for trial in range(min(ctx.args.samples,5)):
                print(f'density {backend} C={concurrency}: {trial}',flush=True)
                # Native Popen tracking uses one monkeypatch local to this sequential suite.
                import subprocess
                Popen=subprocess.Popen
                roots=set();lock=threading.Lock();stop=threading.Event();peaks=[0,0]

                def tracked(*args,**kwargs):
                    process=Popen(*args,**kwargs)
                    with lock:roots.add(process.pid)
                    return process

                def monitor():
                    while not stop.is_set():
                        with lock:live=set(roots)
                        rss,count=snapshot(live)
                        peaks[0]=max(peaks[0],rss);peaks[1]=max(peaks[1],count)
                        stop.wait(0.02)

                def job(i):
                    root=ctx.fresh(f'density-{backend}-{concurrency}-{i}')
                    work=root/'workspace';work.mkdir()
                    stage=root/'stage';runs=root/'runs'
                    command=ctx.command(backend,work,stage,payload)
                    if backend=='vm':command[command.index('--memory')+1]='128MiB'
                    env={'PVISOR_RUN_HOME':str(runs),'XDG_CONFIG_HOME':str(root/'config')}
                    wall,stdout,_=original_run(command,cwd=work,env=env,timeout=120)
                    bundle=ctx.validate_bundle(backend,runs,stage)
                    if bundle:stdout=bundle['run']['output']['stdout']
                    assert stdout=='PVISOR_DENSITY_READY'
                    return wall

                before=resource.getrusage(resource.RUSAGE_CHILDREN)
                start=time.perf_counter_ns()
                sampler=threading.Thread(target=monitor,daemon=True)
                subprocess.Popen=tracked
                sampler.start()
                try:
                    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
                        latencies=list(pool.map(job,range(concurrency)))
                finally:
                    subprocess.Popen=Popen
                    stop.set();sampler.join()
                wall=(time.perf_counter_ns()-start)/1e6
                after=resource.getrusage(resource.RUSAGE_CHILDREN)
                ctx.record(dict(suite='density',workload='hold-1s',backend=backend,concurrency=concurrency,trial=trial,
                    wall_ms=wall,job_wall_ms=latencies,peak_tree_rss_kib=peaks[0],peak_tree_processes=peaks[1],
                    child_cpu_ms=((after.ru_utime+after.ru_stime)-(before.ru_utime+before.ru_stime))*1000,
                    completed=len(latencies),correctness='passed',memory_sampling_interval_ms=20))
