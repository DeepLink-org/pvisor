#!/usr/bin/env python3
"""Rebuild two firmware configs from identical frozen kernel/patch/bundle sources."""
import argparse
import datetime as dt
import json
import sys
from pathlib import Path
import shutil
import subprocess

from reference_baselines import digest


def config_values(path):
    result={}
    for line in path.read_text().splitlines():
        if line.startswith('CONFIG_') and '=' in line:
            k,v=line.split('=',1);result[k]=v
        elif line.startswith('# CONFIG_') and line.endswith(' is not set'):
            result[line[2:].removesuffix(' is not set')]='n'
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--libkrunfw-root',type=Path,required=True)
    parser.add_argument('--baseline-config-ref',required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--jobs',type=int,default=8)
    parser.add_argument('--resume',action='store_true',help='continue a failed build from the same verified frozen inputs')
    args=parser.parse_args();source=args.libkrunfw_root.resolve();out=args.output.resolve()
    if args.jobs<1:parser.error('jobs must be positive')
    def git(*command):return subprocess.check_output(['git','-C',str(source),*command],text=True)
    baseline_ref=git('rev-parse',args.baseline_config_ref).strip()
    baseline=git('show',baseline_ref+':config-libkrunfw_x86_64')
    frozen=out/'source'
    tarball=source/'tarballs/linux-6.12.109.tar.xz'
    if not tarball.is_file():raise ValueError('prepared Linux 6.12.109 source tarball required; no implicit download')
    if args.resume:
        manifest=json.loads((out/'source-manifest.json').read_text())
        if {str(p.relative_to(frozen)) for p in frozen.rglob('*') if p.is_file()}!={r['path'] for r in manifest}:raise ValueError('frozen source inventory changed')
        for row in manifest:
            if digest(frozen/row['path'])!=row['sha256']:raise ValueError('frozen source bytes changed')
        if digest(tarball)!=digest(frozen/'tarballs'/tarball.name):raise ValueError('kernel input differs from frozen source')
    else:
        out.mkdir(parents=True,exist_ok=False);frozen.mkdir()
        for relative in git('ls-files').splitlines():
            path=source/relative
            if not path.is_file():continue
            target=frozen/relative;target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,target)
        (frozen/'tarballs').mkdir(exist_ok=True);shutil.copy2(tarball,frozen/'tarballs'/tarball.name)
        manifest=[dict(path=str(p.relative_to(frozen)),sha256=digest(p)) for p in sorted(frozen.rglob('*')) if p.is_file()]
        (out/'source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    report=dict(role='preparation',benchmark_id='B-KERNEL-ENG',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        source_head=git('rev-parse','HEAD').strip(),source_status=git('status','--porcelain'),
        source_manifest_sha256=digest(out/'source-manifest.json'),kernel_tarball_sha256=digest(tarball),
        baseline_config_ref=baseline_ref,baseline_config_path='config-libkrunfw_x86_64',
        kernel_version='6.12.109',packaging='identical current compact bundle for both configurations',
        packaging_python=sys.executable,pyelftools_version=__import__('elftools').__version__,
        gcc=subprocess.check_output(['gcc','--version'],text=True).splitlines()[0],variants={})
    for name in ('baseline','candidate'):
        build=out/name
        expected=baseline if name=='baseline' else (frozen/'config-libkrunfw_x86_64').read_text()
        if build.exists():
            if not args.resume or (build/'config-libkrunfw_x86_64').read_text()!=expected:raise ValueError('existing variant config differs')
        else:
            shutil.copytree(frozen,build)
            (build/'config-libkrunfw_x86_64').write_text(expected)
        argv=['make','-j'+str(args.jobs)]
        print('BUILD',name,flush=True)
        with (out/(name+'-build.log')).open('ab' if args.resume else 'wb') as log:subprocess.run(argv,cwd=build,stdout=log,stderr=subprocess.STDOUT,check=True)
        cfg=build/'linux-6.12.109/.config';values=config_values(cfg)
        for required in ('CONFIG_SMP','CONFIG_KVM_GUEST','CONFIG_CPU_MITIGATIONS','CONFIG_SECCOMP','CONFIG_SECCOMP_FILTER','CONFIG_NAMESPACES','CONFIG_FUSE_FS','CONFIG_VIRTIO_FS','CONFIG_VIRTIO_MMIO','CONFIG_NET','CONFIG_INET'):
            if values.get(required)!='y':raise ValueError(name+' omits required '+required)
        firmware=build/'libkrunfw.so.5.6.2'
        # Product selects the ABI soname; retain exact newly built bytes.
        if not (build/'libkrunfw.so.5').exists():(build/'libkrunfw.so.5').symlink_to(firmware.name)
        report['variants'][name]=dict(directory=str(build),command=argv,
            config_sha256=digest(cfg),input_config_sha256=digest(build/'config-libkrunfw_x86_64'),
            firmware_sha256=digest(firmware),firmware_bytes=firmware.stat().st_size,
            vmlinux_sha256=digest(build/'linux-6.12.109/vmlinux'),bundle_c_sha256=digest(build/'kernel.c'),config=values)
        (out/'build-receipt.json').write_text(json.dumps(report,indent=2)+'\n')
    a,b=(report['variants'][n]['config'] for n in ('baseline','candidate'))
    report['config_changes']={k:dict(baseline=a.get(k,'absent'),candidate=b.get(k,'absent')) for k in sorted(a.keys()|b.keys()) if a.get(k)!=b.get(k)}
    (out/'build-receipt.json').write_text(json.dumps(report,indent=2)+'\n')


if __name__=='__main__':main()
