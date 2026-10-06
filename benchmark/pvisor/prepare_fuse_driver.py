#!/usr/bin/env python3
"""Offline, frozen-source build helper for B-FS-DIAG's transport control."""
import argparse
import datetime as dt
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tomllib

from reference_baselines import digest


def source_inventory(root):
    records=[]
    for path in sorted(root.rglob('*')):
        if path.is_symlink():raise ValueError('driver source contains a symlink')
        if path.is_file():records.append(dict(path=path.relative_to(root).as_posix(),sha256=digest(path)))
    return records


def verify_driver_receipt(receipt_path, binary):
    receipt=json.loads(receipt_path.read_text())
    if receipt.get('state')!='passed':raise ValueError('driver build is not complete')
    if digest(binary)!=receipt['driver_sha256']:raise ValueError('driver binary differs from build receipt')
    manifest_path=receipt_path.parent/'source-manifest.json'
    if digest(manifest_path)!=receipt['source_manifest_sha256']:raise ValueError('driver source manifest differs')
    records=json.loads(manifest_path.read_text())
    known=set()
    for row in records:
        path=PurePosixPath(row['path'])
        if path.is_absolute() or '..' in path.parts or str(path)!=row['path'] or row['path'] in known:
            raise ValueError('invalid driver source path')
        known.add(row['path'])
    if source_inventory(receipt_path.parent/'source')!=records:raise ValueError('driver frozen source differs')
    return receipt


def main():
    repo=Path(__file__).resolve().parents[2]
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--fuser-source',type=Path,default=repo/'vendor/fuser')
    parser.add_argument('--product-lock',type=Path,default=repo/'Cargo.lock')
    parser.add_argument('--product-manifest',type=Path,default=repo/'Cargo.toml')
    args=parser.parse_args()
    for name in ('output','fuser_source','product_lock','product_manifest'):
        setattr(args,name,getattr(args,name).resolve())
    if '.data' not in args.output.parts:parser.error('raw build output must be under .data')
    lock=tomllib.loads(args.product_lock.read_text())
    libc=[p['version'] for p in lock['package'] if p['name']=='libc']
    if len(libc)!=1:raise ValueError('product lock does not identify one libc version')
    profile=tomllib.loads(args.product_manifest.read_text())['profile']['release']
    expected=dict(profile);expected.pop('build-override',None)
    args.output.mkdir(parents=True,exist_ok=False)
    source=args.output/'source';source.mkdir()
    shutil.copytree(args.fuser_source,source/'vendor/fuser')
    shutil.copy2(Path(__file__).with_name('filesystem_fuse_passthrough.rs'),source/'driver.rs')
    shutil.copy2(args.product_lock,source/'product-Cargo.lock')
    shutil.copy2(args.product_lock,source/'Cargo.lock')
    manifest='[package]\nname = "pvisor-bench-fuse"\nversion = "0.1.0"\nedition = "2024"\n\n[workspace]\n\n[[bin]]\nname = "fuse-passthrough"\npath = "driver.rs"\n\n[dependencies]\nfuser = { path = "vendor/fuser", default-features = false, features = ["abi-7-31"] }\nlibc = "='+libc[0]+'"\n\n[profile.release]\n'
    for key,value in expected.items():manifest+=key+' = '+json.dumps(value)+'\n'
    manifest+='\n[profile.release.build-override]\nstrip = "none"\n'
    (source/'Cargo.toml').write_text(manifest)
    record=dict(benchmark_id='B-FS-DIAG',role='preparation',state='building',recorded_at=dt.datetime.now(dt.timezone.utc).isoformat(),
                profile=expected,features=['abi-7-31'],default_features=False,
                product_lock_sha256=digest(args.product_lock),product_manifest_sha256=digest(args.product_manifest),
                fuser_source=source_inventory(args.fuser_source),commands=[],
                rustc=subprocess.check_output(['rustc','-Vv'],text=True),cargo=subprocess.check_output(['cargo','-V'],text=True))
    def save():(args.output/'build-receipt.json').write_text(json.dumps(record,indent=2)+'\n')
    save()
    generate=['cargo','generate-lockfile','--offline','--manifest-path',str(source/'Cargo.toml')]
    print('Freeze offline driver dependency resolution',flush=True)
    with (args.output/'lock.log').open('wb') as log:result=subprocess.run(generate,stdout=log,stderr=subprocess.STDOUT)
    record['commands'].append(dict(argv=generate,exit_code=result.returncode));save()
    if result.returncode:raise SystemExit(result.returncode)
    before=source_inventory(source)
    (args.output/'source-manifest.json').write_text(json.dumps(before,indent=2)+'\n')
    record['source_manifest_sha256']=digest(args.output/'source-manifest.json');save()
    argv=['cargo','build','--locked','--offline','--release','--jobs','2','--manifest-path',str(source/'Cargo.toml'),'--target-dir',str(args.output/'target')]
    print('Build standalone release FUSE control',flush=True)
    with (args.output/'build.log').open('wb') as log:result=subprocess.run(argv,stdout=log,stderr=subprocess.STDOUT)
    record['commands'].append(dict(argv=argv,exit_code=result.returncode));save()
    if result.returncode:raise SystemExit(result.returncode)
    if source_inventory(source)!=before:raise ValueError('driver source changed during compilation')
    (args.output/'bin').mkdir();binary=args.output/'bin/fuse-passthrough'
    shutil.copy2(args.output/'target/release/fuse-passthrough',binary)
    record.update(state='passed',driver_sha256=digest(binary),driver_source_sha256=digest(source/'driver.rs'),
                  cargo_lock_sha256=digest(source/'Cargo.lock'))
    save();verify_driver_receipt(args.output/'build-receipt.json',binary)
    print('Frozen driver build passed',record['driver_sha256'],flush=True)


if __name__=='__main__':main()
