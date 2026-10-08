#!/usr/bin/env python3
"""Compare strict/checkpoint stage durability using one pinned binary.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
With --profiles: B-FS-DIAG (benchmark/README.md#b-fs-diag), role diagnostic;
instrumented records do not establish elapsed-time gains.
Motivation: quantify what per-mutation persistence costs users, so the
default durability policy is chosen on evidence.
Conclusion sought: per-workload and whole-task difference between strict and
checkpoint, including the completion seal cost, with confidence intervals.
Design: one binary, fresh stage per job, modes shuffled per round, >=30
samples; completion includes sealing; correctness gates unchanged.
"""
import argparse
import csv
import json
import os
import random
import shutil
import traceback
import datetime as dt
from pathlib import Path
from types import SimpleNamespace

from reference_baselines import digest, run_trial, verified_build_receipt
from reference_inputs import verify_reference_inputs
from filesystem_diagnostic import summarize_trial, write_counter_csv
from publication import distribution
from publish_reference_campaign import paired_comparison

WORKLOADS = ("metadata", "read", "write", "git", "rg", "cargo", "npm")


def validate_completion(stage, durability):
    journal = stage / "preimages"
    if (journal / "durability-v1").read_bytes() != f"pvisor.stage.{durability}/1\n".encode():
        raise ValueError("wrong stage durability policy")
    if (journal / "sealed-v1").read_bytes() != b"pvisor.stage.sealed/1\n":
        raise ValueError("job exited without durable stage completion")


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','build-receipt','assets','firmware','output'):
        parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--backend',choices=('pvisor-staged','pvisor-vm'),default='pvisor-staged')
    parser.add_argument('--samples',type=int,default=30)
    parser.add_argument('--warmups',type=int,default=3)
    parser.add_argument('--cpu-affinity',default='0,1')
    parser.add_argument('--tool-scratch',choices=('executor','workspace'),default='executor')
    parser.add_argument('--profiles',action='store_true')
    parser.add_argument('--resource-budget',type=Path)
    parser.add_argument('--budget-memory-mib',type=int,default=16384)
    parser.add_argument('--budget-cpu-placement',choices=('affinity','cpuset'),default='affinity')
    parser.add_argument('--resource-observation',choices=('off','sampled'),default='sampled')
    args=parser.parse_args()
    if args.samples<1 or args.warmups<0:parser.error('invalid samples/warmups')
    for name in ('binary','build_receipt','assets','firmware','output'):
        setattr(args,name,getattr(args,name).resolve())
    if '.data' not in args.output.parts:parser.error('raw outputs must stay under .data')
    build=verified_build_receipt(args.build_receipt,args.binary)
    inputs_before=verify_reference_inputs(args.assets)
    firmware_before=digest(args.firmware/'libkrunfw.so.5')
    args.output.mkdir(parents=True,exist_ok=False)
    firmware=args.output/'firmware';firmware.mkdir()
    shutil.copy2(args.firmware/'libkrunfw.so.5',firmware/'libkrunfw.so.5')
    source=args.output/'binary';shutil.copy2(args.binary,source)
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json')
    shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    os.environ['PVISOR_FS_PROFILE']='1' if args.profiles else '0'
    os.environ.pop('PVISOR_VM_FS_WORKERS',None)
    metadata={'assets':json.loads((args.assets/'assets.json').read_text())}
    cells=('native','strict','checkpoint');configurations={}
    for cell in cells:
        root=args.output/cell;(root/'bin').mkdir(parents=True);shutil.copy2(source,root/'bin/pvisor')
        configurations[cell]=SimpleNamespace(assets=args.assets,output=root,firmware=firmware,
            cpu_affinity=args.cpu_affinity,memory_mib=16384,staged_isolation='rootless_process',
            host_isolation='rootless_process',docker_root_pid=None,stage_durability=None if cell=='native' else cell,
            tool_scratch=args.tool_scratch,diagnostic_timing=args.profiles,diagnostic_stderr_file=args.profiles,
            resource_budget=args.resource_budget,budget_memory_mib=args.budget_memory_mib,
            budget_cpu_placement=args.budget_cpu_placement,resource_observation=args.resource_observation,
            backends='native,'+args.backend)
    rows,failures,preflights,warmup_rows=[],[],{},[]
    report=dict(schema='pvisor-stage-durability/v2',state='running',
        benchmark_id='B-FS-DIAG' if args.profiles else 'B-FS-ENG',role='diagnostic' if args.profiles else 'engineering A/B',
        recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),binary_build=build,binary_sha256=digest(source),
        input_verification=inputs_before,firmware_sha256=firmware_before,
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in sorted((args.output/'harness').rglob('*')) if p.is_file()},
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},
        protocol=dict(order='seed20261005 random cells within each paired round',samples=args.samples,warmups=args.warmups,
            cache='fresh task-local '+args.tool_scratch+' cache; host page cache warm; no task cache reuse',
            completion='CLI exit including durable stage seal; preparation/validation excluded',profiles=args.profiles,
            resources='private parent if supplied; sampled affinity observations cannot prove full descendant lifetimes',
            stderr_capture='regular-file diagnostic' if args.profiles else 'pipe',exclusions='no timing exclusions; all failures retained'),
        samples=rows,failures=failures,preflights=preflights,warmup_rows=warmup_rows)
    def save():
        temporary=args.output/'report.tmp';temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(args.output/'report.json')
    def execute(cell,index):
        row=run_trial(configurations[cell],metadata,'native' if cell=='native' else args.backend,'filesystem',index)
        if cell!='native':validate_completion(Path(row['logs'])/'stage',cell)
        row['durability']=cell
        if args.profiles:
            row=summarize_trial(row,Path(row['logs']))
            if cell!='native' and (not row['filesystem'] or row['profile_coverage']['partial_instances']):
                raise ValueError('incomplete final durability profiles')
        return row
    save()
    for cell in cells:
        try:preflights[cell]=dict(state='passed',row=execute(cell,-100))
        except Exception as error:
            preflights[cell]=dict(state='failed',reason=str(error));failures.append(dict(phase='preflight',durability=cell,error=str(error),traceback=traceback.format_exc()))
        save();print('Durability preflight',cell,preflights[cell]['state'],flush=True)
    rng=random.Random(20261005)
    for index in (range(-args.warmups,args.samples) if not failures else ()):
        order=list(cells);rng.shuffle(order)
        for cell in order:
            try:row=execute(cell,index)
            except Exception as error:
                failures.append(dict(phase='trial',durability=cell,trial=index,error=str(error),traceback=traceback.format_exc()));save();continue
            if index>=0:rows.append(row)
            else:warmup_rows.append(row)
            save();print('Durability round',index,cell,flush=True)
    try:
        inputs_after=verify_reference_inputs(args.assets)
        if inputs_after!=inputs_before:raise ValueError('shared prepared inputs changed')
        if verified_build_receipt(args.build_receipt,args.binary)!=build or digest(source)!=build['pvisor_sha256']:
            raise ValueError('CLI build identity changed')
        if any(digest(configurations[cell].output/'bin/pvisor')!=build['pvisor_sha256'] for cell in cells):
            raise ValueError('frozen cell CLI changed')
        if digest(args.firmware/'libkrunfw.so.5')!=firmware_before or digest(firmware/'libkrunfw.so.5')!=firmware_before:
            raise ValueError('firmware identity changed')
        report['input_final_verification']=dict(state='passed',**inputs_after)
    except Exception as error:
        report['input_final_verification']=dict(state='failed',error=str(error));failures.append(dict(phase='final-input-verification',error=str(error),traceback=traceback.format_exc()))
    report['state']='passed' if not failures and len(rows)==3*args.samples else 'failed'
    if args.profiles:write_counter_csv(rows,args.output/'counter-summary.csv')
    save()
    if report['state']!='passed':raise SystemExit(1)
    summary={}
    for cell in cells:
        selected=[r for r in rows if r['durability']==cell]
        values={op:[r['result']['filesystem'][op]['worker_ms'] for r in selected] for op in WORKLOADS}
        values['completion_ms']=[r['completion_ms'] for r in selected]
        summary[cell]={op:distribution(timings) for op,timings in values.items()}
    report['summary']=summary
    if not args.profiles and args.samples>=30:
        report['comparisons']=[]
        for control in ('native','strict'):
            for operation in (*WORKLOADS,'completion_ms'):
                def values(cell):
                    return [dict(trial=r['trial'],value=r['completion_ms'] if operation=='completion_ms' else r['result']['filesystem'][operation]['worker_ms']) for r in rows if r['durability']==cell]
                report['comparisons'].append(dict(candidate='checkpoint',control=control,operation=operation,**paired_comparison(values('checkpoint'),values(control))))
    save()
    with (args.output/'summary.tsv').open('w') as file:
        writer=csv.writer(file,delimiter='\t');writer.writerow(('durability','operation','n','p50_ms','p95_reference_ms','distribution','low_n','low_p50','high_n','high_p50'))
        for cell,operations in summary.items():
            for operation,metrics in operations.items():
                writer.writerow((cell,operation,metrics['n'],metrics['p50'],metrics['p95_reference'],metrics['distribution'],metrics['low_n'],metrics['low_p50'],metrics['high_n'],metrics['high_p50']))


if __name__=='__main__':main()
