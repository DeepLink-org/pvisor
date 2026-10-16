#!/usr/bin/env python3
"""Guest directory-image/lazy-open switch, measured against ordinary Linux caching.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: decide whether guest directory images and deferred opens help actual work.
Conclusion sought: per-workload paired differences, costs and unsupported-path boundaries.
Design: one frozen release/firmware, 2 vCPU/4096 MiB, 3 warmups/30 seeded pairs;
SDK Immutable and normal CLI separate; full correctness and unchanged inputs.
--profiles instead serves B-FS-DIAG; its timings never enter performance samples.
"""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import sys
import time

from kernel_cache_runner import source_files
from filesystem_exec_probe import run_owned_probe
from filesystem_diagnostic import collect_profiles, VM_PROFILE_COMPONENTS
from publication import distribution

ROOT=Path(__file__).resolve().parents[2]
HERE=Path(__file__).resolve().parent
PHASES=('stat_cold','stat_hot','deep_cold','deep_hot','scan_cold','scan_hot',
        'read_cold','read_hot','open_stat_hot','open_read_hot','sparse_cold','sparse_hot','stat_expired',
        'wide_pair','wide_random','wide_stride','wide_dense')
MODES=('empty','stat-prime','stat','expired','open-prime','open','read-prime','openread','sparse','pair','random','stride','dense','unused')

def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def save(path,value):path.write_text(json.dumps(value,indent=2,sort_keys=True)+'\n')
def inventory(root):
    result={}
    for path in sorted(root.rglob('*')):
        relative=str(path.relative_to(root));st=path.lstat()
        result[relative]={'mode':st.st_mode,'link':os.readlink(path)} if path.is_symlink() else (
            {'mode':st.st_mode,'sha256':sha(path)} if path.is_file() else {'mode':st.st_mode})
    return result

def create_fixture(root):
    root.mkdir()
    for group in ('flat','deep','scan','read'):
        for i in range(2048):
            path=root/group/f'd{i//64:02}'
            if group=='deep':
                for level in range(7):path/=f'l{level}'
            path.mkdir(parents=True,exist_ok=True)
            (path/f'f{i:04}').write_bytes(bytes([i%251])*(32768 if group=='read' else 8))
    for group in ('wide','wide_pair','wide_random','wide_stride','wide_dense'):
        (root/group).mkdir()
        for i in range(2048):(root/group/f'f{i:04}').write_bytes(bytes([i%251])*8)

def prepare(args):
    out=args.output;out.mkdir(parents=True,exist_ok=False)
    snapshot=out/'source';snapshot.mkdir()
    files={}
    for name in source_files():
        source=ROOT/name;target=snapshot/name;target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(source,target);files[name]=sha(source)
    save(out/'source-manifest.json',files)
    harness=out/'harness';harness.mkdir()
    for name in ('guest_cache.py','guest_cache_adaptive.py','guest_cache_driver.rs','guest_cache_guest.rs','test_guest_cache.py','test_guest_cache_adaptive.py',
                 'kernel_cache_runner.py','filesystem_exec_probe.py','filesystem_diagnostic.py','publication.py'):
        shutil.copy2(HERE/name,harness/name)
    for module in list(sys.modules.values()):
        path=Path(getattr(module,'__file__','') or '/')
        if path.suffix=='.py' and path.is_relative_to(HERE):
            target=harness/path.relative_to(HERE);target.parent.mkdir(parents=True,exist_ok=True)
            shutil.copy2(path,target)
    driver=out/'driver';driver.mkdir();shutil.copy2(harness/'guest_cache_driver.rs',driver/'main.rs')
    manifest=f'''[package]
name="guest-cache-bench"
version="0.1.0"
edition="2021"
[workspace]
[[bin]]
name="guest-cache-bench"
path="main.rs"
[dependencies]
pvisor-vm={{path={json.dumps(str(snapshot/'crates/pvisor-vm'))},features=["net"]}}
pvisor-overlay-core={{path={json.dumps(str(snapshot/'crates/pvisor-overlay-core'))}}}
serde_json="1"
env_logger="0.11"
[patch.crates-io]
fuser={{path={json.dumps(str(snapshot/'vendor/fuser'))}}}
[profile.release]
opt-level="z"
lto="thin"
strip="symbols"
codegen-units=8
panic="abort"
[profile.release.package.pvisor-vm]
opt-level=3
[profile.release.package.pvisor-overlay-core]
opt-level=3
'''
    (driver/'Cargo.toml').write_text(manifest);shutil.copy2(snapshot/'Cargo.lock',driver/'Cargo.lock')
    command=['cargo','build','--offline','--release','--manifest-path',str(driver/'Cargo.toml'),
             '--target-dir',str(ROOT/'target'),'-j','4']
    with (out/'build.log').open('w') as stream:subprocess.run(command,cwd=ROOT,stdout=stream,stderr=subprocess.STDOUT,check=True)
    shutil.copy2(ROOT/'target/release/guest-cache-bench',out/'sdk')
    guest=out/'guest'
    subprocess.run(['rustc','--edition=2021','--target','aarch64-unknown-linux-musl','-C','linker=rust-lld','-O',
                    str(harness/'guest_cache_guest.rs'),'-o',str(guest)],check=True)
    shutil.copy2(args.binary,out/'pvisor');shutil.copy2(args.firmware,out/'libkrunfw.5.dylib')
    for binary in ('sdk','pvisor'):
        subprocess.run(['codesign','--force','--sign','-','--entitlements',str(ROOT/'crates/pvisor/macos-hypervisor.entitlements'),str(out/binary)],check=True)
    shutil.copytree(args.rootfs,out/'rootfs',symlinks=True)
    create_fixture(out/'fixture');shutil.copy2(guest,out/'fixture/cache-probe')
    save(out/'input-manifest.json',inventory(out/'fixture'))
    save(out/'rootfs-manifest.json',inventory(out/'rootfs'))
    receipt={'command':command,'source_manifest':sha(out/'source-manifest.json'),
             'rustc':subprocess.check_output(['rustc','-vV'],text=True),
             'host':subprocess.check_output(['sw_vers'],text=True),
             'files':{name:sha(out/name) for name in ['sdk','pvisor','guest','libkrunfw.5.dylib']},
             'harness':{str(p.relative_to(harness)):sha(p) for p in harness.rglob('*') if p.is_file()}}
    save(out/'receipt.json',receipt);print('PREPARED',out,flush=True)

def results(stdout, mode):
    lines=stdout.decode().splitlines()
    if lines.count('CACHE_PASS '+mode)!=1:raise ValueError('missing or duplicate success marker')
    rows=[json.loads(line.removeprefix('CACHE_RESULT ')) for line in lines if line.startswith('CACHE_RESULT ')]
    if mode!='all':
        if rows:raise ValueError('unexpected timed phases in counter-only mode')
        return {}
    if [r.get('phase') for r in rows]!=list(PHASES):raise ValueError('missing or reordered phases')
    if any(type(r['ms']) not in (float,int) or not math.isfinite(r['ms']) or r['ms']<0 for r in rows):raise ValueError('invalid time')
    return {r['phase']:r['ms'] for r in rows}

def verify(prepared):
    receipt=json.loads((prepared/'receipt.json').read_text())
    for name,digest in receipt['files'].items():
        if sha(prepared/name)!=digest:raise ValueError('changed artifact '+name)
    if inventory(prepared/'fixture')!=json.loads((prepared/'input-manifest.json').read_text()):raise ValueError('changed fixture')
    if inventory(prepared/'rootfs')!=json.loads((prepared/'rootfs-manifest.json').read_text()):raise ValueError('changed rootfs')

def run_one(prepared, out, cohort, arm, mode, profiles, firmware=None):
    firmware=firmware or prepared/'libkrunfw.5.dylib'
    if cohort=='cli' and firmware.name!='libkrunfw.5.dylib':
        raise ValueError('CLI firmware directory must contain the selected libkrunfw.5.dylib')
    out.mkdir(parents=True,exist_ok=False)
    env={k:os.environ[k] for k in ('PATH','HOME','TMPDIR') if k in os.environ}
    env.update(PVISOR_VM_GUEST_CACHE=str(arm),PVISOR_VM_FS_WRITEBACK='0',PVISOR_FS_PROFILE='1' if profiles else '0',
               PVISOR_STARTUP_TIMING='0',PVISOR_PERSISTENCE_TIMING='0',PVISOR_RUN_HOME=str(out/'runs'),XDG_CONFIG_HOME=str(out/'config'))
    if cohort=='sdk':
        stage=out/'stage';stage.mkdir()
        command=[str(prepared/'sdk'),str((prepared/'rootfs').resolve()),str(firmware),str(prepared/'guest'),str((prepared/'fixture').resolve()),str(stage),mode]
    else:
        command=[str(prepared/'pvisor'),'run','--no-agent-defaults','--safe','--executor','vm','--rootfs',str((prepared/'rootfs').resolve()),
                 '--vm-library-dir',str(firmware.parent),'--cpu','2','--memory','4096MiB','--timeout','45s',
                 '--stage',str(out/'stage'),'--','./cache-probe','.',mode]
    result=run_owned_probe(command,prepared/'fixture',env,out,timeout=60)
    if result.returncode:raise RuntimeError(f'guest failed {result.returncode}: {out}')
    times=results(result.stdout,mode)
    if cohort=='cli':
        bundle=json.loads((out/'stage/run-bundle.json').read_text())
        # The existing validator checks completed VM process evidence.
        from reference_baselines import validate_bundle_execution
        validate_bundle_execution(bundle,'pvisor-vm')
    counters=[]
    if profiles:
        counters=collect_profiles(result.stderr.decode().splitlines(), require_complete=True,
                                  expected_components=VM_PROFILE_COMPONENTS)
    return {'times':times,'profiles':counters,'correctness':'passed'}

def analyze(report):
    rows=[r for r in report['rows'] if r['round']>=0];summary=[];rng=random.Random(20261017)
    for cohort in report['cohorts']:
        pairs={i:{r['arm']:r for r in rows if r['round']==i and r['cohort']==cohort} for i in range(report['samples'])}
        if any(set(pair)!={0,1} for pair in pairs.values()):raise ValueError('incomplete paired cohort')
        for phase in PHASES:
            values={arm:[pairs[i][arm]['times'][phase] for i in pairs] for arm in (0,1)}
            dist={arm:distribution(v) for arm,v in values.items()}
            record={'cohort':cohort,'phase':phase,'off':dist[0],'on':dist[1]}
            if all(d['distribution']=='unsplit' for d in dist.values()):
                diffs=[]
                for _ in range(5000):
                    idx=[rng.randrange(len(pairs)) for _ in pairs]
                    diffs.append(statistics.median(values[1][i] for i in idx)-statistics.median(values[0][i] for i in idx))
                diffs.sort();record['difference_ci95_ms']=[diffs[125],diffs[4874]]
            summary.append(record)
    return summary

def campaign(args):
    p=args.prepared;verify(p)
    baseline=args.baseline_firmware
    baseline_sha=sha(baseline) if baseline else None
    args.output.mkdir(parents=True,exist_ok=False)
    report={'benchmark_id':'B-FS-DIAG' if args.profiles else 'B-FS-ENG','profiles':args.profiles,
            'prepared':str(p),'cohorts':args.cohorts.split(','),'samples':args.samples,'warmups':args.warmups,
            'seed':20261017,'resources':'HVF 2 vCPU/4096 MiB; no host CPU affinity/cgroup claim',
            'cache':'warm host; fresh VM and upper; native FUSE 1s TTL; no forced drop_caches',
            'rows':[],'failures':[],'no_exclusions':True}
    report['arms']={0:'baseline firmware; guest_cache=1',1:'prepared firmware; guest_cache=1'} if baseline else {0:'guest_cache=0',1:'guest_cache=1'}
    if baseline:report['baseline_firmware']={'path':str(baseline),'sha256':baseline_sha}
    report['prepared_receipt']=json.loads((p/'receipt.json').read_text())
    plan=[];rng=random.Random(report['seed'])
    if args.profiles:
        plan=[(0,c,a,m) for c in report['cohorts'] for a in (0,1) for m in MODES]
    else:
        for i in range(-args.warmups,args.samples):
            conditions=[(c,a) for c in report['cohorts'] for a in (0,1)];rng.shuffle(conditions)
            plan.extend((i,c,a,'all') for c,a in conditions)
    report['plan']=plan;save(args.output/'report.json',report)
    try:
        for i,c,a,m in plan:
            d=args.output/f'{i+args.warmups:02}-{c}-{a}-{m}'
            firmware=baseline if baseline and a==0 else p/'libkrunfw.5.dylib'
            value=run_one(p,d,c,1 if baseline else a,m,args.profiles,firmware)
            report['rows'].append({'round':i,'cohort':c,'arm':a,'mode':m,**value})
            save(args.output/'report.json',report);print(i,c,a,m,flush=True)
        verify(p)
        if baseline and sha(baseline)!=baseline_sha:raise ValueError('changed baseline firmware')
        report['state']='passed'
        if not args.profiles:save(args.output/'summary.json',analyze(report))
    except Exception as error:
        report['state']='failed';report['failures'].append(str(error));raise
    finally:save(args.output/'report.json',report)

def main():
    parser=argparse.ArgumentParser(description=__doc__);sub=parser.add_subparsers(dest='action',required=True)
    prep=sub.add_parser('prepare')
    for name in ('binary','firmware','rootfs','output'):prep.add_argument('--'+name,type=Path,required=True)
    run=sub.add_parser('run')
    for name in ('prepared','output'):run.add_argument('--'+name,type=Path,required=True)
    run.add_argument('--samples',type=int,default=30);run.add_argument('--warmups',type=int,default=3)
    run.add_argument('--baseline-firmware',type=Path,help='compare firmware versions with guest cache enabled in both arms')
    run.add_argument('--cohorts',default='sdk,cli');run.add_argument('--profiles',action='store_true')
    args=parser.parse_args()
    for name in ('binary','firmware','rootfs','output','prepared','baseline_firmware'):
        if getattr(args,name,None) is not None:setattr(args,name,getattr(args,name).resolve())
    if '.data' not in args.output.parts:parser.error('output must be under .data')
    if args.action=='prepare':prepare(args)
    else:
        if args.samples<1 or args.warmups<0 or any(c not in ('sdk','cli') for c in args.cohorts.split(',')):parser.error('invalid campaign')
        campaign(args)
if __name__=='__main__':main()
