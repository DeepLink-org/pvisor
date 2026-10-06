#!/usr/bin/env python3
"""Fresh-VM SDK offload with charged physical memory and full restore checks.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role user-facing.
Motivation: assess physical savings of parked Agent VMs including backing cache.
Conclusion sought: complete active/offloaded/restored cgroup memory, CPU and
verified first-read costs; repeatable/random data and raw/compressed backing.
Design: fresh VM per condition, randomized paired rounds, identical two-core
2 GiB owned cgroups, same SDK build and prepared tools, full memory validation.
Predeclared host gate: 30 s quiet admission, external 0.5 s VM/build guard;
any detected sample interference rejects the condition and stops the campaign.
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
import time
import traceback
import uuid

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


HOST_GUARD_PROTOCOL=dict(enabled=True,quiet_seconds=30,admission_deadline_seconds=180,sample_interval_seconds=.5,
    observer='coordinator outside measured service cgroup; no additional worker memory observer',
    detection='visible same-user KVM FDs and build/test commands; exclude only owned service cgroup and descendants',
    rejection='any detected VM/build during service rejects condition and stops campaign; no partial pooling or replacement',
    limitation='inaccessible process/FD inspection is logged, not proof of host-wide quiet; short jobs between polls may be missed')


def host_jobs(unit=None):
    """Read-only same-user guard; never exempt the coordinator's parent cgroup."""
    jobs,errors=[],[]
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():continue
        try:
            if proc.stat().st_uid!=os.getuid():continue
            if unit is not None:
                try:membership=proc.joinpath('cgroup').read_text()
                except (FileNotFoundError,ProcessLookupError):continue
                except OSError as error:
                    errors.append(dict(pid=int(proc.name),error=str(error)));membership=''
                if any(line.startswith('0::') and unit+'.service' in line[3:].split('/')
                    for line in membership.splitlines()):continue
            try:args=proc.joinpath('cmdline').read_bytes().split(b'\0')
            except (FileNotFoundError,ProcessLookupError):continue
            except OSError as error:
                errors.append(dict(pid=int(proc.name),error=str(error)));args=[]
            if args and Path(os.fsdecode(args[0])).name in ('cargo','rustc','cargo-nextest','make','ninja','cmake','cc','gcc','g++','clang','clang++'):
                jobs.append(dict(pid=int(proc.name),kind='build/test',args=[os.fsdecode(a) for a in args]))
            for fd in proc.joinpath('fd').iterdir():
                try:target=os.readlink(fd)
                except FileNotFoundError:continue
                except OSError as error:
                    errors.append(dict(pid=int(proc.name),fd=fd.name,error=str(error)));continue
                if target in ('/dev/kvm','anon_inode:kvm-vm','anon_inode:kvm-vcpu','anon_inode:[kvm-vm]','anon_inode:[kvm-vcpu]'):
                    jobs.append(dict(pid=int(proc.name),kind='other VM',args=[os.fsdecode(a) for a in args]));break
        except (FileNotFoundError,ProcessLookupError):continue
        except OSError as error:errors.append(dict(pid=int(proc.name),error=str(error)))
    return dict(time_ns=time.time_ns(),jobs=jobs,inspection_errors=errors)


def wait_for_quiet(logs,quiet_seconds=30,deadline_seconds=180):
    """Admission only; reset the continuous quiet window on competing work."""
    deadline=time.monotonic()+deadline_seconds;quiet_since=None
    with (logs/'prelaunch-wait.jsonl').open('w') as log:
        while True:
            row=host_jobs();log.write(json.dumps(row)+'\n');log.flush()
            now=time.monotonic()
            if row['jobs']:quiet_since=None
            elif quiet_since is None:quiet_since=now
            if now>deadline:return False
            if quiet_since is not None and now-quiet_since>=quiet_seconds:return True
            if now>=deadline:return False
            time.sleep(min(1,deadline-now))


def settle_unit(unit, logs):
    """Stop only our owned unit and fail closed if its final state is unknown."""
    proof=dict(unit=unit,unit_quiescent=False)
    try:
        stopped=subprocess.run(['systemctl','--user','stop',unit],capture_output=True,timeout=15)
        (logs/'stop.stdout').write_bytes(stopped.stdout);(logs/'stop.stderr').write_bytes(stopped.stderr)
        proof['stop_returncode']=stopped.returncode
        shown=subprocess.run(['systemctl','--user','show',unit,'--property=ActiveState','--property=LoadState'],
            capture_output=True,timeout=10)
        (logs/'unit-final.txt').write_bytes(shown.stdout+shown.stderr)
        state=dict(line.split('=',1) for line in shown.stdout.decode(errors='replace').splitlines() if '=' in line)
        proof.update(show_returncode=shown.returncode,state=state)
        proof['unit_quiescent']=shown.returncode==0 and (state.get('ActiveState') in ('inactive','failed') or state.get('LoadState')=='not-found')
    except (OSError,subprocess.TimeoutExpired) as error:
        proof['error']=str(error)
        (logs/'cleanup-error.txt').write_text(str(error))
    (logs/'service-quiescence.json').write_text(json.dumps(proof,indent=2)+'\n')
    return proof['unit_quiescent']


def run_service(unit, config, harness):
    root=Path(config['root'])
    cmd=['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit='+unit,'--property=MemoryAccounting=yes',
        '--property=CPUAccounting=yes',f'--property=MemoryMax={config["budget_bytes"]}','--property=MemorySwapMax=0',
        '--property=CPUQuota=200%','--property=CPUAffinity='+config['cpu_affinity'].replace(',',' '),'--property=TasksMax=128',
        '--property=RuntimeMaxSec=220','--property=KillMode=control-group','--property=TimeoutStopSec=10',
        sys.executable,str(harness),'--internal-worker',str(root/'config.json')]
    record=dict(unit=unit,command=cmd,started_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        timeout_seconds=240,deadline=False,correctness='failed',logs=str(root))
    (root/'service-launch.json').write_text(json.dumps(record,indent=2)+'\n')
    stop=threading.Event();interference=[];guard_errors=[]
    guard=(root/'host-guard.jsonl').open('w')
    def check_host():
        try:
            row=host_jobs(unit);guard.write(json.dumps(row)+'\n');guard.flush()
            if row['jobs']:interference.append(row)
        except Exception as error:
            guard_errors.append(str(error))
            guard.write(json.dumps(dict(time_ns=time.time_ns(),guard_error=str(error)))+'\n');guard.flush()
    def monitor_host():
        while not stop.wait(.5):check_host()
    watcher=threading.Thread(target=monitor_host,daemon=True)
    try:
        check_host()
        if interference or guard_errors:raise RuntimeError('host interference/guard failure before service launch')
        watcher.start()
        result=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=240)
        record['returncode']=result.returncode
        (root/'service.stdout').write_bytes(result.stdout);(root/'service.stderr').write_bytes(result.stderr)
        value=json.loads((root/'result.json').read_text()) if (root/'result.json').is_file() else dict(correctness='failed',error='service produced no evidence')
        if result.returncode or value['correctness']!='passed':raise RuntimeError(json.dumps(value))
        record.update(correctness='passed',result=value)
    except subprocess.TimeoutExpired as error:
        (root/'service.stdout').write_bytes(error.stdout or b'');(root/'service.stderr').write_bytes(error.stderr or b'')
        record.update(error=str(error),deadline=True)
    except Exception as error:
        record['error']=str(error)
    finally:
        stop.set()
        if watcher.ident is not None:watcher.join()
        check_host();guard.close()
        record.update(host_interference=interference,host_guard_errors=guard_errors)
        if interference or guard_errors:
            record.update(correctness='failed',error='host interference or guard failure; reject condition and stop campaign')
        record['unit_quiescent']=settle_unit(unit,root)
        if not record['unit_quiescent']:
            record.update(correctness='failed',cleanup_error='owned unit cleanup not proven; stop campaign to preserve cap')
            record.setdefault('error',record['cleanup_error'])
        (root/'service-result.json').write_text(json.dumps(record,indent=2)+'\n')
    return record


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
            exclusions='no timing exclusions; failure and OOM retained; full immutable and mutable guest data verified',
            host_guard=HOST_GUARD_PROTOCOL),rows=[],failures=[],attempts=[])
    def save():
        temporary=args.output/'report.tmp';temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(args.output/'report.json')
    save();rng=random.Random(20261006)
    for trial in range(-args.warmups,args.samples):
        cases=[(pattern,compressed) for pattern in ('repeated','random') for compressed in (False,True)];rng.shuffle(cases)
        for case_index,(pattern,compressed) in enumerate(cases):
            root=args.output/'trials'/f'{trial+args.warmups}-{case_index}';root.mkdir(parents=True)
            config=dict(root=str(root),result=str(root/'result.json'),example=str(example),rootfs=str(args.rootfs),firmware=str(args.firmware),
                pattern=pattern,compressed=compressed,trial=trial,seed=20261006+trial+args.warmups,budget_bytes=args.budget_mib*1024**2,cpu_affinity=args.cpu_affinity)
            cfg=root/'config.json';cfg.write_text(json.dumps(config)+'\n')
            unit='pvisor-live-memory-'+uuid.uuid4().hex
            print(trial,pattern,compressed,flush=True)
            try:
                admitted=wait_for_quiet(root)
            except Exception as error:
                admitted=False
                (root/'admission-error.txt').write_text(str(error))
            if admitted:
                attempt=run_service(unit,config,args.output/'harness/live_vm_memory.py')
                attempt['host_admitted']=True
            else:
                attempt=dict(unit=unit,correctness='failed',unit_quiescent=True,host_admitted=False,logs=str(root),
                    error='180-second quiet admission deadline or guard failure; no VM launched')
            attempt.update(trial=trial,pattern=pattern,compressed=compressed)
            report['attempts'].append(attempt)
            if attempt['correctness']=='passed':
                if trial>=0:report['rows'].append(attempt['result'])
            else:
                report['failures'].append(attempt)
            save()
            if not attempt['host_admitted'] or attempt.get('host_interference') or attempt.get('host_guard_errors'):
                report['stopped']='host quiet/interference gate failed; remaining conditions unmeasured; cohort invalid, no partial pooling'
                save();raise SystemExit(1)
            if not attempt['unit_quiescent'] or (trial<0 and attempt['correctness']!='passed'):raise SystemExit(1)
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
