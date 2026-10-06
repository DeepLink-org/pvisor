#!/usr/bin/env python3
"""Paired current-product firmware comparison, with rebuilt source provenance.

Benchmark: B-KERNEL-ENG (benchmark/README.md#b-kernel-eng), role engineering A/B.
Motivation: determine whether trimmed firmware lowers usable startup cost
without treating missing workspace/tool capabilities as a performance win.
Conclusion sought: median readiness/completion differences and 95% paired
bootstrap intervals, with explicit kernel configuration capability changes.
Design: same binary/input/source/patches/bundle scheme, rebuilt configs only;
randomized paired rounds, three warmups, thirty samples, full correctness.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import random
import shutil
import traceback
from types import SimpleNamespace

from publish_reference_campaign import paired_comparison
from reference_baselines import digest, run_trial, verified_build_receipt


def verify_firmware(receipt, name, directory):
    value=receipt['variants'][name]
    if digest(directory/'libkrunfw.so.5')!=value['firmware_sha256']:raise ValueError('firmware bytes differ from build receipt')
    if digest(directory/'linux-6.12.109/.config')!=value['config_sha256']:raise ValueError('built kernel config differs from receipt')
    if digest(directory/'linux-6.12.109/vmlinux')!=value['vmlinux_sha256']:raise ValueError('kernel bytes differ from receipt')
    if receipt.get('packaging')!='identical current compact bundle for both configurations' or not receipt.get('source_manifest_sha256') or not receipt.get('kernel_tarball_sha256'):
        raise ValueError('matching source and packaging provenance missing')
    return value


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('assets','binary','build-receipt','firmware-receipt','baseline','candidate','output'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--modes',default='ready,filesystem,tools');parser.add_argument('--samples',type=int,default=30)
    parser.add_argument('--warmups',type=int,default=3);parser.add_argument('--cpu-affinity',default='0,1')
    parser.add_argument('--memory-mib',type=int,default=16384)
    args=parser.parse_args();modes=args.modes.split(',')
    if args.samples<1 or args.warmups<0 or not modes or set(modes)-{'ready','filesystem','tools'} or len(modes)!=len(set(modes)):parser.error('invalid conditions')
    for key in ('assets','binary','build_receipt','firmware_receipt','baseline','candidate','output'):setattr(args,key,getattr(args,key).resolve())
    binary_receipt=verified_build_receipt(args.build_receipt,args.binary)
    firmware_receipt=json.loads(args.firmware_receipt.read_text())
    if digest(args.firmware_receipt.parent/'source-manifest.json')!=firmware_receipt['source_manifest_sha256']:parser.error('firmware source manifest differs from build receipt')
    for name in ('baseline','candidate'):verify_firmware(firmware_receipt,name,getattr(args,name))
    if firmware_receipt['variants']['baseline']['firmware_sha256']==firmware_receipt['variants']['candidate']['firmware_sha256']:parser.error('configs produced identical firmware')
    args.output.mkdir(parents=True,exist_ok=False)
    shutil.copy2(args.build_receipt,args.output/'build-receipt.json');shutil.copy2(args.build_receipt.parent/'source-manifest.json',args.output/'source-manifest.json')
    shutil.copy2(args.firmware_receipt,args.output/'firmware-build-receipt.json')
    shutil.copy2(args.firmware_receipt.parent/'source-manifest.json',args.output/'firmware-source-manifest.json')
    shutil.copytree(Path(__file__).parent,args.output/'harness',ignore=shutil.ignore_patterns('.data','__pycache__','.pytest_cache'))
    cases={}
    for name in ('baseline','candidate'):
        out=args.output/('a' if name=='baseline' else 'b');(out/'bin').mkdir(parents=True)
        shutil.copy2(args.binary,out/'bin/pvisor');(out/'firmware').mkdir()
        shutil.copy2(getattr(args,name)/'libkrunfw.so.5',out/'firmware/libkrunfw.so.5')
        cases[name]=SimpleNamespace(**(vars(args)|dict(output=out,firmware=out/'firmware',host_isolation='rootless_process',staged_isolation='rootless_process',docker_root_pid=None)))
    assets=json.loads((args.assets/'assets.json').read_text())
    report=dict(benchmark_id='B-KERNEL-ENG',role='engineering A/B',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),host_kernel=os.uname().release,
        binary_build=binary_receipt,firmware_build=firmware_receipt,input_manifest_sha256=digest(args.assets/'input-manifest.json'),
        harness_sha256={str(p.relative_to(args.output/'harness')):digest(p) for p in (args.output/'harness').rglob('*.py')},
        arguments={k:str(v) if isinstance(v,Path) else v for k,v in vars(args).items()},assets=assets,
        protocol=dict(order='seeded randomized mode/config conditions each paired round',cpu='same host affinity and 2 vCPU',
            memory='128 MiB for shell-ready; same declared tool memory for both configs',cache='warm; no global cache eviction',
            correctness='same tool results, installed VM isolation, untouched host originals and complete retained changes',
            scope='configuration A/B, not pure VMM/security ranking; network and restoration require independent capability tests',
            exclusions='no timing exclusions; failed conditions retained; instrumented profiles excluded'),rows=[],failures=[],capabilities={},comparisons=[])
    def save():
        temp=args.output/'report.tmp';temp.write_text(json.dumps(report,indent=2)+'\n');temp.replace(args.output/'report.json')
    os.environ['PVISOR_FS_PROFILE']='0';os.environ['PVISOR_STARTUP_TIMING']='0';save()
    for mode in modes:
        for name in cases:
            try:
                run_trial(cases[name],dict(assets=assets),'pvisor-vm',mode,-100)
                report['capabilities'][name+'/'+mode]=dict(state='available')
            except Exception as error:report['capabilities'][name+'/'+mode]=dict(state='failed-preflight',error=str(error),traceback=traceback.format_exc())
            save()
    rng=random.Random(20261006)
    for trial in range(-args.warmups,args.samples):
        conditions=[(mode,name) for mode in modes for name in cases if report['capabilities'][name+'/'+mode]['state']=='available'];rng.shuffle(conditions)
        for mode,name in conditions:
            print(trial,mode,name,flush=True)
            try:
                row=run_trial(cases[name],dict(assets=assets),'pvisor-vm',mode,trial)
                if trial>=0:report['rows'].append(row|dict(variant=name,benchmark_id='B-KERNEL-ENG'))
            except Exception as error:
                report['failures'].append(dict(mode=mode,variant=name,trial=trial,error=str(error),traceback=traceback.format_exc()))
                if trial<0:save();raise SystemExit(1)
            save()
    if args.samples>=30:
        for mode in modes:
            for metric in ('ready_ms','completion_ms'):
                values={name:[dict(trial=r['trial'],value=r[metric]) for r in report['rows'] if r['mode']==mode and r['variant']==name] for name in cases}
                if all(len(v)==args.samples for v in values.values()):
                    report['comparisons'].append(dict(mode=mode,metric=metric,unit='ms',candidate='candidate',control='baseline',**paired_comparison(values['candidate'],values['baseline'])))
        save()
    if report['failures'] or any(c['state']!='available' for c in report['capabilities'].values()):raise SystemExit(1)


if __name__=='__main__':main()
