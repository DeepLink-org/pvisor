#!/usr/bin/env python3
"""Identical guest loader/binary/libraries from virtio-fs versus anonymous RAM.

Benchmark: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic.
Motivation: distinguish guest exec and loader cost from filesystem requests.
Conclusion sought: complete request counts, child CPU/faults and diagnostic
exec latency with files or verified anonymous-RAM copies; no user ranking.
Design: fresh VM per condition, identical bytes/args/fds and copy preparation;
original versus independent-copy sources separate original-inode prewarming
from first tool mapping; common Python libraries remain warm in both cases.
rootfs/firmware/source pinned, random paired order, complete final counters.
Guest tmpfs remains noexec; executable memfd availability is checked, not forced.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import shutil
import signal
import struct
import subprocess
import traceback

from filesystem_diagnostic import summarize_trial, write_counter_csv
from reference_baselines import digest, verified_build_receipt, validate_bundle_execution
from run_all import ldd_paths

PAYLOAD=r'''
import hashlib,json,os,resource,subprocess,sys,time
from pathlib import Path
config=json.loads(Path('input.json').read_text())
fds={}
for item in config['files']:
    data=Path(item['copy_path'] if config['preparation_source']=='duplicate' else item['path']).read_bytes()
    assert hashlib.sha256(data).hexdigest()==item['sha256']
    fd=os.memfd_create('pvisor-exec-probe',os.MFD_CLOEXEC|getattr(os,'MFD_EXEC',0))
    assert os.write(fd,data)==len(data)
    os.fchmod(fd,0o700)
    fds[item['path']]=fd
# Both arms do exactly the same copies, hash checks and fd inheritance.
ram=config['case']=='anonymous-ram'
def location(path):return f'/proc/self/fd/{fds[path]}' if ram else path
env=os.environ.copy()
for name in ('LD_PRELOAD','LD_LIBRARY_PATH','LD_AUDIT'):env.pop(name,None)
env['LD_PRELOAD']=':'.join(location(path) for path in config['libraries'])
argv=[location(config['interpreter']),'--inhibit-cache','--library-path',config['library_path'],location(config['tool']),'--version']
rows=[]
for trial in range(config['calls']):
    before=resource.getrusage(resource.RUSAGE_CHILDREN);started=time.perf_counter_ns()
    result=subprocess.run(argv,env=env,pass_fds=tuple(fds.values()),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    elapsed=(time.perf_counter_ns()-started)/1e6;after=resource.getrusage(resource.RUSAGE_CHILDREN)
    assert result.returncode==0 and result.stdout.decode()==config['expected_stdout'], result.stderr.decode(errors='replace')
    rows.append(dict(call=trial,first_exec=trial==0,exec_ms=elapsed,child_user_ms=(after.ru_utime-before.ru_utime)*1000,
        child_system_ms=(after.ru_stime-before.ru_stime)*1000,minor_faults=after.ru_minflt-before.ru_minflt,
        major_faults=after.ru_majflt-before.ru_majflt,voluntary_switches=after.ru_nvcsw-before.ru_nvcsw,
        involuntary_switches=after.ru_nivcsw-before.ru_nivcsw))
for fd in fds.values():os.close(fd)
print('EXEC_PROBE_RESULT '+json.dumps(dict(case=config['case'],preparation_source=config['preparation_source'],calls=config['calls'],correctness='passed',rows=rows)),flush=True)
'''


def prepare_copies(work, inputs):
    for item in inputs:
        original=Path(item['path']);copied=work/item['copy_path']
        copied.parent.mkdir(exist_ok=True)
        shutil.copyfile(original,copied)
        original_stat,copied_stat=original.stat(),copied.stat()
        if digest(copied)!=item['sha256'] or (original_stat.st_dev,original_stat.st_ino)==(copied_stat.st_dev,copied_stat.st_ino):
            raise ValueError('copy preparation requires identical bytes in a separate inode')


def run_owned_probe(command, work, env, root, timeout=150):
    process=subprocess.Popen(command,cwd=work,env=env,stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,start_new_session=True)
    timed_out=False
    try:
        stdout,stderr=process.communicate(timeout=timeout)
    except BaseException as error:
        try:os.killpg(process.pid,signal.SIGKILL)
        except ProcessLookupError:pass
        stdout,stderr=process.communicate()
        timed_out=isinstance(error,subprocess.TimeoutExpired)
        (root/'stdout.log').write_bytes(stdout);(root/'stderr.log').write_bytes(stderr)
        (root/'command.json').write_text(json.dumps(dict(argv=command,exit=process.returncode,timed_out=timed_out))+'\n')
        raise
    (root/'stdout.log').write_bytes(stdout);(root/'stderr.log').write_bytes(stderr)
    (root/'command.json').write_text(json.dumps(dict(argv=command,exit=process.returncode,timed_out=False))+'\n')
    return subprocess.CompletedProcess(command,process.returncode,stdout,stderr)


def elf_interpreter(path):
    data=path.read_bytes()
    if data[:6]!=b'\x7fELF\x02\x01' or len(data)<64:raise ValueError('requires a dynamic little-endian ELF64 tool')
    offset=struct.unpack_from('<Q',data,32)[0];size,count=struct.unpack_from('<HH',data,54)
    for i in range(count):
        entry=offset+i*size
        if entry+56>len(data):raise ValueError('invalid ELF program headers')
        if struct.unpack_from('<I',data,entry)[0]==3:
            start=struct.unpack_from('<Q',data,entry+8)[0];length=struct.unpack_from('<Q',data,entry+32)[0]
            raw=data[start:start+length]
            if not raw or not raw.endswith(b'\0'):raise ValueError('invalid interpreter entry')
            return Path(raw[:-1].decode())
    raise ValueError('tool has no dynamic interpreter')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('assets','binary','build-receipt','firmware','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--tool',type=Path,default=Path('/usr/bin/rg'));parser.add_argument('--samples',type=int,default=3)
    parser.add_argument('--calls',type=int,default=50);parser.add_argument('--cpu-affinity',default='0,1')
    parser.add_argument('--preparation-sources',default='original,duplicate')
    args=parser.parse_args()
    if args.samples<1 or not 1<=args.calls<=500:parser.error('invalid repetitions/calls')
    sources=args.preparation_sources.split(',')
    if not sources or len(sources)!=len(set(sources)) or set(sources)-{'original','duplicate'}:parser.error('invalid preparation sources')
    for name in ('assets','binary','build_receipt','firmware','output','tool'):setattr(args,name,getattr(args,name).resolve())
    receipt=verified_build_receipt(args.build_receipt,args.binary)
    interpreter=elf_interpreter(args.tool)
    libraries=sorted(p for p in ldd_paths(args.tool) if p.resolve()!=interpreter.resolve())
    files=[args.tool,interpreter,*libraries]
    inputs=[]
    for index,path in enumerate(files):
        counterpart=args.assets/'rootfs'/path.relative_to('/')
        if digest(path)!=digest(counterpart):raise ValueError('host and prepared guest tool/library bytes differ')
        inputs.append(dict(path=str(path),copy_path=f'copies/{index}',sha256=digest(path)))
    config=dict(tool=str(args.tool),interpreter=str(interpreter),libraries=list(map(str,libraries)),files=inputs,
        library_path=':'.join(sorted({str(p.parent) for p in libraries})),calls=args.calls,
        expected_stdout=subprocess.check_output([str(args.tool),'--version'],text=True))
    args.output.mkdir(parents=True,exist_ok=False);(args.output/'bin').mkdir();shutil.copy2(args.binary,args.output/'bin/pvisor')
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json')
    shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    report=dict(benchmark_id='B-FS-DIAG',role='diagnostic',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),binary_build=receipt,
        host_kernel=os.uname().release,firmware_sha256=digest(args.firmware/'libkrunfw.so.5'),input_manifest_sha256=digest(args.assets/'input-manifest.json'),
        inputs=config,payload_sha256=__import__('hashlib').sha256(PAYLOAD.encode()).hexdigest(),harness_sha256=digest(Path(__file__)),
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(resources='same host affinity, two guest vCPU, 1 GiB guest RAM',
            order='seeded paired cases in independent fresh VMs',preparation='both arms copy/hash identical files into memfds before timed exec; pass same fd set',
            cache='warm host inputs, fresh guest; original preparation warms original inodes; duplicate preparation reads distinct workspace inodes; common Python loader/libraries remain warm; first call and subsequent calls separate; not completely cold startup',
            security='noexec tmpfs unchanged; unavailable executable memfd reported as failure, no policy relaxation',
            counters='independent instrumented runs only; include identical preparation and all exec calls; inclusive spans not additive; final coverage required'),rows=[],failures=[])
    def save():
        temporary=args.output/'report.tmp';temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(args.output/'report.json')
    save();rng=random.Random(20261006)
    for trial in range(args.samples):
        cases=[(source,case) for source in sources for case in ('virtiofs-files','anonymous-ram')];rng.shuffle(cases)
        for source,case in cases:
            root=args.output/'trials'/f'{trial}-{source}-{case}';work=root/'workspace';work.mkdir(parents=True)
            trial_config=config|dict(case=case,preparation_source=source)
            (work/'input.json').write_text(json.dumps(trial_config)+'\n')
            command=['taskset','--cpu-list',args.cpu_affinity,str(args.output/'bin/pvisor'),'run','--no-agent-defaults','--overlaynet','off',
                '--stdio','inherit','--timeout','120s','--vm','--rootfs',str(args.assets/'rootfs'),'--vm-library-dir',str(args.firmware),
                '--cpu','2','--memory','1GiB','--stage',str(root/'stage'),'--','/usr/bin/python3','-c',PAYLOAD]
            env=os.environ.copy()|{'PVISOR_FS_PROFILE':'1','PVISOR_STARTUP_TIMING':'0','PVISOR_RUN_HOME':str(root/'runs'),'XDG_CONFIG_HOME':str(root/'config')}
            env.pop('PVISOR_TEST_ALLOW_NO_USERNS',None)
            env.pop('PVISOR_VM_FS_WORKERS',None)
            for key in list(env):
                if key.upper() in ('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY'):env.pop(key)
            try:
                prepare_copies(work,inputs)
                result=run_owned_probe(command,work,env,root)
                if result.returncode:raise RuntimeError('guest exec probe failed; retained stdout/stderr')
                values=[json.loads(line.removeprefix('EXEC_PROBE_RESULT ')) for line in result.stdout.decode().splitlines() if line.startswith('EXEC_PROBE_RESULT ')]
                if len(values)!=1 or values[0]['case']!=case or values[0]['preparation_source']!=source or values[0]['calls']!=args.calls or len(values[0]['rows'])!=args.calls or values[0]['correctness']!='passed':raise ValueError('incomplete or wrong exec probe')
                if [row.get('call') for row in values[0]['rows']]!=list(range(args.calls)) or any(row.get('first_exec')!=(index==0) for index,row in enumerate(values[0]['rows'])):raise ValueError('first and repeated exec evidence incomplete')
                bundle=json.loads((root/'stage/run-bundle.json').read_text());validate_bundle_execution(bundle,'pvisor-vm')
                if (work/'input.json').read_text()!=json.dumps(trial_config)+'\n' or any(digest(work/item['copy_path'])!=item['sha256'] for item in inputs):raise ValueError('diagnostic changed host input')
                row=summarize_trial(dict(backend='pvisor-vm',mode=f'{source}-{case}',preparation_source=source,trial=trial,result=values[0],correctness='passed',logs=str(root)),root)
                if not row['filesystem'] or row['profile_coverage']['partial_instances']:raise ValueError('missing complete final filesystem counter coverage')
                report['rows'].append(row)
            except Exception as error:report['failures'].append(dict(case=case,preparation_source=source,trial=trial,error=str(error),traceback=traceback.format_exc(),logs=str(root)))
            save();print(trial,source,case,flush=True)
    write_counter_csv(report['rows'],args.output/'counter-summary.csv')
    if report['failures']:raise SystemExit(1)


if __name__=='__main__':main()
