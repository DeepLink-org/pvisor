#!/usr/bin/env python3
"""Matched apply implementation experiment; counters run independently.

Benchmark: B-APPLY-ENG (benchmark/README.md#b-apply-eng), role engineering A/B.
Motivation: determine whether the directory index reduces apply waiting.
Conclusion sought: paired median differences/intervals with unchanged complete
results and conflict refusal. No user-facing cross-product ranking.
Design: two release binaries, same frozen parent/dependencies/compiler,
1,000/10,000 files, apply/pre-apply conflict, 30 rounds and three warmups,
randomized order and fresh stage/target preparation outside operation timing.
With --profile: B-APPLY-DIAG (benchmark/README.md#b-apply-diag), diagnostic role;
retain inclusive counters separately, never use instrumented time as a gain.
"""
import argparse
import json
from pathlib import Path
import random
import shutil
import subprocess
from types import SimpleNamespace

from publication import distribution, write_csv
from publish_reference_campaign import paired_comparison
from reference_baselines import digest, verified_build_receipt
from v1.apply import staged_trial
from v1.common import Context

APPLY='crates/pvisor-overlay-core/src/apply.rs'


def cli_build_command(command):
    if command[:2]!=['cargo','build']:raise ValueError('requires cargo build CLI receipt')
    forbidden={'--example','--examples','--test','--tests','--bench','--benches','--all-targets','--lib'}
    if any(part.split('=',1)[0] in forbidden for part in command):
        raise ValueError('joint CLI/example/test builds can unify unrelated dependency features')
    packages=[command[i+1] for i,part in enumerate(command[:-1]) if part in ('-p','--package')]
    binaries=[command[i+1] for i,part in enumerate(command[:-1]) if part=='--bin']
    if packages!=['pvisor'] or binaries!=['pvisor']:
        raise ValueError('requires one package and the pvisor CLI target only')
    normalized=[];index=0
    while index<len(command):
        part=command[index]
        if part=='--target-dir':
            if index+1==len(command):raise ValueError('missing target directory')
            index+=2;continue
        if part.startswith('--target-dir='):index+=1;continue
        if part=='--features':
            if index+1==len(command):raise ValueError('missing features')
            feature=command[index+1]
            normalized.extend([part,'gateway' if feature=='pvisor/gateway' else feature]);index+=2;continue
        normalized.append(part);index+=1
    return normalized


def validate_sources(left, right, left_manifest, right_manifest):
    if left['rustc']!=right['rustc'] or left['cargo']!=right['cargo']:raise ValueError('different compiler versions')
    for receipt in (left,right):
        if not all(option in receipt['command'] for option in ('--release','--locked','--offline')):raise ValueError('requires locked offline release builds')
        units=receipt.get('compiled_units',{})
        if not isinstance(units.get('cli'),dict) or not units.get('libraries'):
            raise ValueError('missing actual CLI/dependency feature and profile evidence')
        required={'rustc','features','declared_features','profile','rustflags','config','compile_kind'}
        if not required.issubset(units['cli']) or any(not required.issubset(unit) or not unit.get('name') for unit in units['libraries']):
            raise ValueError('incomplete compiled-unit evidence')
    if cli_build_command(left['command'])!=cli_build_command(right['command']):
        raise ValueError('different CLI build commands')
    if left['compiled_units']!=right['compiled_units']:
        raise ValueError('different actual dependency features, profile or rustflags')
    before={r['path']:r['sha256'] for r in left_manifest};after={r['path']:r['sha256'] for r in right_manifest}
    if len(before)!=len(left_manifest) or len(after)!=len(right_manifest):raise ValueError('duplicate source inventory entries')
    if set(before)!=set(after):raise ValueError('different source inventories')
    changed={p for p in before if before[p]!=after[p]}
    if changed!={APPLY}:raise ValueError('unrelated compiled-source change in apply comparison')
    if left['pvisor_sha256']==right['pvisor_sha256']:raise ValueError('identical apply binaries')
    return sorted(changed)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('baseline','candidate','baseline-receipt','candidate-receipt','firmware','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--sizes',default='1000,10000');parser.add_argument('--actions',default='apply,conflict')
    parser.add_argument('--samples',type=int,default=30);parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--cpu-affinity',default='0,1');parser.add_argument('--profile',action='store_true')
    parser.add_argument('--trace-syscalls',type=Path,help='frozen strace binary; diagnostic --profile only')
    parser.add_argument('--tracer-receipt',type=Path,help='source archive/compiler/binary receipt for --trace-syscalls')
    args=parser.parse_args();sizes=list(map(int,args.sizes.split(',')));actions=args.actions.split(',')
    if bool(args.trace_syscalls)!=bool(args.tracer_receipt) or (args.trace_syscalls and not args.profile):
        parser.error('syscall tracing requires --profile and both tracer binary/receipt')
    if not sizes or len(sizes)!=len(set(sizes)) or min(sizes)<1 or set(actions)-{'apply','conflict'} or len(actions)!=len(set(actions)) or not actions or args.samples<1 or args.warmups<0:
        parser.error('invalid apply experiment conditions')
    args.output=args.output.resolve()
    receipts={};manifests={}
    for variant in ('baseline','candidate'):
        receipt_path=getattr(args,variant+'_receipt').resolve()
        receipts[variant]=verified_build_receipt(receipt_path,getattr(args,variant).resolve())
        manifests[variant]=json.loads((receipt_path.parent/'source-manifest.json').read_text())
    changes=validate_sources(receipts['baseline'],receipts['candidate'],manifests['baseline'],manifests['candidate'])
    tracer=None;tracer_build=None
    if args.trace_syscalls:
        tracer_build=json.loads(args.tracer_receipt.read_text())
        if digest(args.trace_syscalls)!=tracer_build['strace_sha256'] or digest(Path(tracer_build['source_archive']))!=tracer_build['source_archive_sha256']:
            raise ValueError('syscall tracer source/binary receipt mismatch')
        if not tracer_build.get('compiler') or not tracer_build.get('commands'):
            raise ValueError('missing tracer compiler/build command evidence')
    args.output.mkdir(parents=True,exist_ok=False);contexts={}
    if tracer_build:
        (args.output/'bin').mkdir();tracer=args.output/'bin/strace';shutil.copy2(args.trace_syscalls,tracer)
        shutil.copy2(args.tracer_receipt,args.output/'tracer-build-receipt.json')
        shutil.copy2(tracer_build['source_archive'],args.output/'tracer-source.tar.xz')
        tracer_build=tracer_build|dict(version=subprocess.check_output([str(tracer),'--version'],text=True))
    for variant in ('baseline','candidate'):
        common=SimpleNamespace(output=args.output/variant,binary=getattr(args,variant),build_receipt=getattr(args,variant+'_receipt'),
            firmware=args.firmware,samples=args.samples,warmups=args.warmups,vm_memory='1GiB',cpu_affinity=args.cpu_affinity)
        ctx=Context(common);ctx.metadata['benchmark_id']='B-APPLY-DIAG' if args.profile else 'B-APPLY-ENG'
        ctx.metadata['role']='diagnostic' if args.profile else 'engineering A/B'
        ctx.env['PVISOR_FS_PROFILE']='1' if args.profile else '0';ctx.save();contexts[variant]=ctx
    report=dict(benchmark_id='B-APPLY-DIAG' if args.profile else 'B-APPLY-ENG',role='diagnostic' if args.profile else 'engineering A/B',
        profile_enabled=args.profile,source_delta=changes,binary_builds=receipts,syscall_tracer=tracer_build,
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(order='seeded randomized variant/size/action cases per paired round',
            fixture='fresh independent stage/target; all original/staged/final contents and committed ledger checked',
            timing='apply/refusal command only; preparation and correctness checks excluded',
            exclusions='no timing exclusions; failures retained and comparison refused',
            counters='inclusive nested spans; missing old counters unknown, not zero',
            syscalls='optional file/descriptor syscall classes for apply/refusal only; per-PID raw traces; traced elapsed time is not formal performance or kernel CPU'),rows=[],failures=[])
    def save():(args.output/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    save();rng=random.Random(20261006)
    for trial in range(-args.warmups,args.samples):
        cases=[(v,size,action) for v in contexts for size in sizes for action in actions];rng.shuffle(cases)
        for variant,size,action in cases:
            ctx=contexts[variant];print(trial,variant,size,action,flush=True)
            try:
                row=staged_trial(ctx,size,action,trial,syscall_tracer=tracer)
                if action=='conflict':
                    output=(Path(row['logs'])/'command.stderr').read_text()
                    if 'target changed after staging' not in output or 'refusing to overwrite concurrent changes' not in output:
                        raise ValueError('expected explicit pre-apply conflict refusal')
                if trial>=0:report['rows'].append(row|dict(variant=variant))
            except Exception as error:
                report['failures'].append(dict(variant=variant,files=size,action=action,trial=trial,error=str(error)));save()
                raise
            save()
    summary=[];comparisons=[]
    for size in sizes:
        for action in actions:
            def rows(variant):return [dict(trial=r['trial'],value=r['wall_ms']) for r in report['rows'] if (r['variant'],r['files'],r['workload'])==(variant,size,action)]
            for variant in contexts:summary.append(dict(files=size,operation=action,variant=variant,profile_enabled=args.profile,unit='ms',**distribution([r['value'] for r in rows(variant)])))
            if not args.profile and args.samples>=30:comparisons.append(dict(files=size,operation=action,**paired_comparison(rows('candidate'),rows('baseline'))))
    write_csv(args.output/'apply-ab-summary.csv',summary)
    if comparisons:write_csv(args.output/'apply-ab-comparisons.csv',comparisons)


if __name__=='__main__':main()
