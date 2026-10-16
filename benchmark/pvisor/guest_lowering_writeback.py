#!/usr/bin/env python3
"""Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), engineering factorial A/B.
Motivation: quantify metadata lowering and writeback without hiding synchronization.
Conclusion sought: per-workload guest-loop, sync, combined and task benefits/costs.
Design: frozen four-arm guest-cache/writeback switches; SDK Immutable and CLI
Mutable separately; 3 warmups/30 shuffled rounds, 2 vCPU/4096 MiB/HVF, checked
bytes and retained stages. --profiles serves independent B-FS-DIAG requests.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import tarfile
import time

import guest_cache as base
from retained_snapshot_archive import inventory as stage_inventory, validate_archive

HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[1]
CASES={'defer':(16,256<<10),'tiny':(200,4096),'boundary':(256,64<<10),
       'pressure':(24,1<<20),'overwrite':(16,4096),'stream':(1,8<<20),'fsync':(16,4096)}
ARMS={0:(0,0),1:(1,0),2:(0,1),3:(1,1)}
METRICS=tuple('meta.'+p for p in base.PHASES)+tuple(f'{c}.{m}_ms' for c in CASES for m in ('write','sync','total'))+('completion_ms',)
PROFILE_MODES=('empty','meta-stat','meta-open-prime','meta-open',*CASES,'overwrite-prime')
CONTRASTS={'lowering':(0,1),'writeback':(1,3),'combined':(0,3),'writeback_cache_off':(0,2),'lowering_writeback_on':(2,3)}


def parse(stdout,mode):
    lines=stdout.decode().splitlines()
    if lines.count('WB_PASS '+mode)!=1:raise ValueError('missing/duplicate checked completion')
    writes=[json.loads(s.removeprefix('WB_RESULT ')) for s in lines if s.startswith('WB_RESULT ')]
    expected=list(CASES) if mode=='all' else [mode] if mode in CASES else []
    if [x.get('case') for x in writes]!=expected:raise ValueError('missing/duplicate write case')
    result={}
    if mode=='all':result.update({'meta.'+k:v for k,v in base.results(stdout,'all').items()})
    elif mode.startswith('meta-'):base.results(stdout,mode.removeprefix('meta-'))
    elif any(s.startswith('CACHE_RESULT ') for s in lines):raise ValueError('unexpected metadata timing')
    for row in writes:
        for metric in ('write','sync','total'):
            value=row[metric+'_ms']
            if type(value) not in (int,float) or not math.isfinite(value) or value<0:raise ValueError('invalid timing')
            result[row['case']+'.'+metric+'_ms']=value
        if abs(row['write_ms']+row['sync_ms']-row['total_ms'])>0.000003:raise ValueError('noncontiguous timing boundaries')
    return result


def check_upper(upper,mode):
    cases=list(CASES) if mode=='all' else [mode] if mode in CASES else ['overwrite'] if mode=='overwrite-prime' else []
    actual={}
    for case in cases:
        count,size=CASES[case]
        directory=upper/'write-output'/case
        if {p.name for p in directory.iterdir()}!={f'f{i:04}' for i in range(count)}:raise ValueError('wrong final file set')
        for i in range(count):
            expected=bytes([(i+(63 if case=='overwrite' and mode!='overwrite-prime' else 0))%251])*size
            path=directory/f'f{i:04}'
            if path.read_bytes()!=expected:raise ValueError('host bytes differ: '+str(path))
            actual[str(path.relative_to(upper))]={'bytes':size,'sha256':hashlib.sha256(expected).hexdigest()}
    return actual


def archive_stage(out):
    trees=[out/n for n in ('stage','runs','config') if (out/n).is_dir()]
    expected=stage_inventory(out,trees)
    target=out/'stage-artifacts.tar.zst'
    with target.open('xb') as stream:
        process=subprocess.Popen(['zstd','-q','-1','-T1','-c'],stdin=subprocess.PIPE,stdout=stream,stderr=subprocess.PIPE)
        try:
            with tarfile.open(fileobj=process.stdin,mode='w|',format=tarfile.PAX_FORMAT,dereference=False) as tar:
                def metadata(member):
                    record=expected[member.name]
                    member.pax_headers['PVISOR.mtime_ns']=str(record['mtime_ns'])
                    member.pax_headers['PVISOR.xattrs']=json.dumps(record['xattrs'],sort_keys=True)
                    return member
                for name in sorted(expected):tar.add(out/name,arcname=name,recursive=False,filter=metadata)
            process.stdin.close();error=process.stderr.read();code=process.wait()
            if code:raise ValueError('archive compression failed: '+error.decode(errors='replace'))
        finally:
            if process.poll() is None:process.kill();process.wait()
            process.stderr.close()
        stream.flush();os.fsync(stream.fileno())
    validate_archive(target,expected)
    if stage_inventory(out,trees)!=expected:raise ValueError('stage changed during archival')
    receipt={'archive':target.name,'sha256':base.sha(target),'members':expected,'excluded_from_timing':True,
             'validation':'all file bytes, links, modes, uid/gid, nanosecond times, xattrs and complete zstd frame'}
    base.save(out/'stage-archive.json',receipt)
    for tree in trees:shutil.rmtree(tree)
    return {'sha256':receipt['sha256'],'bytes':target.stat().st_size,'members':len(expected)}


def verify(prepared):
    base.verify(prepared)
    r=json.loads((prepared/'receipt.json').read_text())
    if any(base.sha(prepared/'harness'/name)!=digest for name,digest in r['harness'].items()):raise ValueError('changed harness')
    if base.sha(prepared/'source-manifest.json')!=r['source_manifest']:raise ValueError('changed host source receipt')
    manifest=json.loads((prepared/'source-manifest.json').read_text())
    if any(base.sha(prepared/'source'/name)!=digest for name,digest in manifest.items()):raise ValueError('changed host source')
    for filename in ('firmware-source-manifest.json','current-firmware-manifest.json'):
        source='firmware-source' if filename.startswith('firmware-source') else 'current-firmware'
        m=json.loads((prepared/filename).read_text())
        if any(base.sha(prepared/source/name)!=digest for name,digest in m.items()):raise ValueError('changed firmware source')


def prepare(args):
    old=args.reuse.resolve();out=args.output.resolve();base.verify(old)
    r=json.loads((old/'receipt.json').read_text())
    changes=[n for n,d in json.loads((old/'source-manifest.json').read_text()).items() if base.sha(ROOT/n)!=d]
    allowed={'crates/pvisor-vm/README.md','crates/pvisor-vm/src/probes/guest_metadata_case.rs','crates/pvisor-vm/src/probes/guest_metadata_guest.rs'}
    if set(changes)-allowed:raise ValueError('frozen host production code differs: '+str(changes))
    for name in ('pvisor','libkrunfw.5.dylib'):
        if base.sha(ROOT/'target/release'/name)!=base.sha(old/name):raise ValueError('release differs from verified reuse: '+name)
    equivalence=json.loads(args.equivalence.read_text())
    if equivalence['state']!='passed':raise ValueError('unverified patch consolidation')
    oldpatch=old/'firmware-source/fw/patches'
    for name in equivalence['old_followups']:
        if (args.equivalence.parent/'old'/name).read_bytes()!=(oldpatch/name).read_bytes():raise ValueError('wrong consolidated reference')
    old_source=args.equivalence.parent/'expected';new_source=args.equivalence.parent/'merged'
    if any(base.sha(old_source/n)!=d or base.sha(new_source/n)!=d for n,d in equivalence['file_sha256'].items()):raise ValueError('consolidation source changed')
    # Compare the applied tree against the current patch in the frozen pre-followup tree.
    check=out.parent/(out.name+'-patch-check');shutil.copytree(args.equivalence.parent/'base',check)
    try:
        subprocess.run(['patch','--batch','--forward','--fuzz=0','-p1','-d',str(check),'-i',str(ROOT/'fw/patches'/equivalence['new_followup'])],check=True,stdout=subprocess.DEVNULL)
        if any(base.sha(check/n)!=d for n,d in equivalence['file_sha256'].items()):raise ValueError('maintained merged patch differs')
    finally:shutil.rmtree(check)
    out.mkdir(parents=True,exist_ok=False)
    for name in ('sdk','pvisor','libkrunfw.5.dylib','libkrunfw.SOURCE','source-manifest.json','rootfs-manifest.json'):shutil.copy2(old/name,out/name)
    (out/'source').symlink_to((old/'source').resolve(),target_is_directory=True)
    (out/'rootfs').symlink_to((old/'rootfs').resolve(),target_is_directory=True)
    shutil.copytree(old/'firmware-source',out/'firmware-source')
    shutil.copy2(old/'firmware-source-manifest.json',out/'firmware-source-manifest.json')
    shutil.copytree(ROOT/'fw',out/'current-firmware',ignore=shutil.ignore_patterns('__pycache__'))
    base.save(out/'current-firmware-manifest.json',{str(p.relative_to(out/'current-firmware')):base.sha(p) for p in (out/'current-firmware').rglob('*') if p.is_file()})
    base.save(out/'patch-equivalence.json',equivalence)
    harness=out/'harness';shutil.copytree(old/'harness',harness,ignore=shutil.ignore_patterns('__pycache__'))
    for name in ('guest_lowering_writeback.py','guest_writeback_bench.rs','test_guest_lowering_writeback.py','retained_snapshot_archive.py'):
        shutil.copy2(HERE/name,harness/name)
    command=['rustc','--edition=2021','--target','aarch64-unknown-linux-musl','-C','linker=rust-lld','-O',str(harness/'guest_writeback_bench.rs'),'-o',str(out/'guest')]
    subprocess.run(command,check=True)
    fixture=out/'fixture';fixture.mkdir()
    subprocess.run(['/bin/cp','-cR',str((old/'fixture').resolve()),str(fixture/'meta')],check=True)
    shutil.copy2(out/'guest',fixture/'cache-probe');shutil.copy2(old/'guest',fixture/'metadata-probe')
    base.save(out/'input-manifest.json',base.inventory(fixture))
    r.update(files={name:base.sha(out/name) for name in ('sdk','pvisor','guest','libkrunfw.5.dylib','libkrunfw.SOURCE','source-manifest.json','input-manifest.json','rootfs-manifest.json','firmware-source-manifest.json','current-firmware-manifest.json','patch-equivalence.json')},
        harness={str(p.relative_to(harness)):base.sha(p) for p in harness.rglob('*') if p.is_file()},
        guest_build=command,reused_from=str(old),host_source_delta=changes,
        firmware_equivalence='current 0044 produces byte-identical kernel source to measured old 0044+0045; original artifact/source receipt retained')
    base.save(out/'receipt.json',r);verify(out);print('PREPARED',out,flush=True)


def run_one(p,out,cohort,arm,mode,profiles):
    out.mkdir(parents=True,exist_ok=False);cache,wb=ARMS[arm]
    env={k:os.environ[k] for k in ('PATH','HOME','TMPDIR') if k in os.environ}
    env.update(PVISOR_VM_GUEST_CACHE=str(cache),PVISOR_VM_FS_WRITEBACK=str(wb),PVISOR_FS_PROFILE=str(int(profiles)),
        PVISOR_STARTUP_TIMING='0',PVISOR_PERSISTENCE_TIMING='0',PVISOR_RUN_HOME=str(out/'runs'),XDG_CONFIG_HOME=str(out/'config'))
    if cohort=='sdk':
        (out/'stage').mkdir()
        command=[str(p/'sdk'),str((p/'rootfs').resolve()),str(p/'libkrunfw.5.dylib'),str(p/'guest'),str((p/'fixture').resolve()),str(out/'stage'),mode]
        upper=out/'stage/input-upper'
    else:
        command=[str(p/'pvisor'),'run','--no-agent-defaults','--safe','--executor','vm','--rootfs',str((p/'rootfs').resolve()),
            '--vm-library-dir',str(p),'--cpu','2','--memory','4096MiB','--timeout','60s','--stage',str(out/'stage'),'--','./cache-probe','.',mode]
        upper=out/'stage/upper'
    started=time.perf_counter_ns();result=base.run_owned_probe(command,p/'fixture',env,out,timeout=75);elapsed=(time.perf_counter_ns()-started)/1e6
    if result.returncode:raise RuntimeError('guest failed: '+str(out))
    metrics=parse(result.stdout,mode);metrics['completion_ms']=elapsed
    bdi=[s for s in result.stdout.decode().splitlines() if s.startswith('WB_BDI ') and 'strict_limit=' in s]
    if not bdi or (wb and not any('max_ratio=100 ' in s and 'strict_limit=1' in s and int(s.split('min_bytes=')[1].split()[0])>8<<20 for s in bdi)):raise ValueError('missing actual writeback BDI negotiation')
    if cohort=='cli':
        from reference_baselines import validate_bundle_execution
        bundle=out/'stage/run-bundle.json';validate_bundle_execution(json.loads(bundle.read_text()),'pvisor-vm');shutil.copy2(bundle,out/'run-bundle.json')
    host=check_upper(upper,mode);base.save(out/'host-byte-checks.json',host)
    counters=base.collect_profiles(result.stderr.decode().splitlines(), require_complete=True,
                                  expected_components=base.VM_PROFILE_COMPONENTS) if profiles else []
    archive=archive_stage(out)
    return {'metrics':metrics,'profiles':counters,'bdi':bdi,'host_files_checked':len(host),'archive':archive,'correctness':'passed'}


def analyze(report):
    if report.get('state')!='passed' or report['failures']:raise ValueError('incomplete campaign')
    rows=[r for r in report['rows'] if r['round']>=0]
    expected={(i,c,a) for i in range(report['samples']) for c in report['cohorts'] for a in ARMS}
    keys=[(x['round'],x['cohort'],x['arm']) for x in rows]
    if len(keys)!=len(set(keys)) or set(keys)!=expected:raise ValueError('incomplete/duplicate factorial cohort')
    if any(set(x['metrics'])!=set(METRICS) for x in rows):raise ValueError('incomplete timing metrics')
    rng=random.Random(20261019);indices=[[rng.randrange(report['samples']) for _ in range(report['samples'])] for _ in range(5000)]
    result=[]
    for c in report['cohorts']:
        by_key={(x['round'],x['arm']):x for x in rows if x['cohort']==c}
        for metric in METRICS:
            values={a:[by_key[i,a]['metrics'][metric] for i in range(report['samples'])] for a in ARMS}
            ds={a:base.distribution(v) for a,v in values.items()}
            for label,(off,on) in CONTRASTS.items():
                row={'cohort':c,'metric':metric,'contrast':label,'off_arm':off,'on_arm':on,'off':ds[off],'on':ds[on]}
                if ds[off]['distribution']==ds[on]['distribution']=='unsplit':
                    diffs=sorted(statistics.median(values[on][i] for i in ix)-statistics.median(values[off][i] for i in ix) for ix in indices)
                    row['difference_ci95_ms']=[diffs[125],diffs[4874]]
                result.append(row)
    return result


def campaign(args):
    p=args.prepared;verify(p);out=args.output;out.mkdir(parents=True,exist_ok=False)
    cohorts=['sdk','cli'];plan=[];rng=random.Random(20261019)
    if args.profiles:plan=[(0,c,a,m) for c in cohorts for a in (1,3) for m in PROFILE_MODES]
    else:
        for i in range(-args.warmups,args.samples):
            conditions=[(c,a) for c in cohorts for a in ARMS];rng.shuffle(conditions)
            plan.extend((i,c,a,'all') for c,a in conditions)
    r={'benchmark_id':'B-FS-DIAG' if args.profiles else 'B-FS-ENG','profiles':args.profiles,'prepared':str(p),'prepared_receipt':json.loads((p/'receipt.json').read_text()),
        'arms':ARMS,'cohorts':cohorts,'samples':args.samples,'warmups':args.warmups,'seed':20261019,
        'scope':'warm host; fresh VM/upper; 2 vCPU/4096 MiB/HVF; no host affinity/cgroup or host-wide quiet guarantee',
        'archive':'after completion and guest/host byte validation; lossless compressed stage with per-member verification; failed cases never compacted',
        'plan':plan,'rows':[],'failures':[],'no_exclusions':True}
    base.save(out/'report.json',r)
    try:
        for i,c,a,m in plan:
            result=run_one(p,out/f'{i+args.warmups:02}-{c}-{a}-{m}',c,a,m,args.profiles)
            r['rows'].append({'round':i,'cohort':c,'arm':a,'mode':m,**result});base.save(out/'report.json',r);print(i,c,a,m,flush=True)
        verify(p);r['state']='passed';base.save(out/'report.json',r)
        if not args.profiles:base.save(out/'summary.json',analyze(r))
    except BaseException as error:
        r['state']='failed';r['failures'].append(repr(error));raise
    finally:base.save(out/'report.json',r)


def main():
    parser=argparse.ArgumentParser(description=__doc__);sub=parser.add_subparsers(dest='action',required=True)
    prep=sub.add_parser('prepare');prep.add_argument('--reuse',type=Path,required=True);prep.add_argument('--equivalence',type=Path,required=True);prep.add_argument('--output',type=Path,required=True)
    run=sub.add_parser('run');run.add_argument('--prepared',type=Path,required=True);run.add_argument('--output',type=Path,required=True)
    run.add_argument('--samples',type=int,default=30);run.add_argument('--warmups',type=int,default=3);run.add_argument('--profiles',action='store_true')
    args=parser.parse_args()
    for name in ('reuse','equivalence','prepared','output'):
        if getattr(args,name,None) is not None:setattr(args,name,getattr(args,name).resolve())
    if '.data' not in args.output.parts:parser.error('output must be a fresh .data directory')
    if args.action=='prepare':prepare(args)
    elif args.samples<1 or args.warmups<0:parser.error('invalid sample count')
    else:campaign(args)

if __name__=='__main__':main()
