#!/usr/bin/env python3
"""Prepare small, pinned Python/Git inputs and a private Podman image for B-DENSITY."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from reference_baselines import digest, verified_build_receipt
from run_all import ldd_paths, prepare_rootfs


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('output','binary','build-receipt'):parser.add_argument('--'+name,type=Path,required=True)
    parser.add_argument('--parked-worker',type=Path)
    parser.add_argument('--parked-worker-receipt',type=Path)
    args=parser.parse_args()
    for name in ('output','binary','build_receipt'):setattr(args,name,getattr(args,name).resolve())
    receipt=verified_build_receipt(args.build_receipt,args.binary)
    args.output.mkdir(parents=True,exist_ok=False);root=args.output/'rootfs';root.mkdir()
    prepare_rootfs(root,args.binary)
    sources=set()
    for name in ('python3','git'):
        path=Path('/usr/bin')/name
        sources.add(path);sources.update(ldd_paths(path))
    stdlib=Path(subprocess.check_output(['/usr/bin/python3','-c','import sysconfig; print(sysconfig.get_path("stdlib"))'],text=True).strip())
    shutil.copytree(stdlib,root/stdlib.relative_to('/'),
        ignore=shutil.ignore_patterns('site-packages','__pycache__','test','tests','ensurepip'))
    for extension in stdlib.rglob('*.so'):
        if 'site-packages' not in extension.parts:sources.update(ldd_paths(extension))
    for path in sorted(sources):
        target=root/path.relative_to('/');target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(path,target,follow_symlinks=True)
    (root/'tmp').chmod(0o1777);(root/'bench').mkdir();(root/'work').mkdir()
    shutil.copy2(Path(__file__).with_name('density_worker.py'),root/'bench/density-worker.py')
    parked_receipt=None
    if args.parked_worker or args.parked_worker_receipt:
        if not args.parked_worker or not args.parked_worker_receipt:parser.error('parked worker requires its source/build receipt')
        parked_receipt=json.loads(args.parked_worker_receipt.read_text())
        if digest(args.parked_worker)!=parked_receipt['worker_sha256'] or parked_receipt['target']!='x86_64-unknown-linux-musl':
            parser.error('parked worker build differs from static Linux receipt')
        headers=subprocess.check_output(['readelf','-l',str(args.parked_worker)],text=True)
        if 'INTERP' in headers:parser.error('parked worker must be static in every environment')
        shutil.copy2(args.parked_worker,root/'bench/parked-memory-probe')
        shutil.copy2(args.parked_worker_receipt,args.output/'parked-worker-receipt.json')
    tar=args.output/'rootfs.tar'
    subprocess.run(['tar','-C',str(root),'-cf',str(tar),'.'],check=True)
    data=args.output/'podman-store';data.mkdir()
    runtime=tempfile.mkdtemp(prefix='pvd-env-',dir='/tmp')
    command=['podman','--root',str(data),'--runroot',runtime,'--storage-driver','overlay']
    image=subprocess.check_output(command+['import',str(tar)],text=True).strip()
    info=json.loads(subprocess.check_output(command+['info','--format','json'],text=True))
    inspection=json.loads(subprocess.check_output(command+['image','inspect',image],text=True))
    manifest=[]
    for path in sorted(root.rglob('*')):
        entry=dict(path=str(path.relative_to(root)),mode=path.lstat().st_mode)
        if path.is_symlink():entry.update(kind='symlink',target=str(path.readlink()))
        elif path.is_file():entry.update(kind='file',sha256=digest(path),size=path.stat().st_size)
        else:entry['kind']='directory'
        manifest.append(entry)
    report=dict(benchmark_id='B-DENSITY',role='preparation',binary_build=receipt,
        rootfs_manifest=manifest,rootfs_tar_sha256=digest(tar),podman_image=image,podman_runroot=runtime,
        podman_inspection=inspection,podman_info=info,
        podman_version=subprocess.check_output(['podman','--version'],text=True).strip(),
        host_python_sha256=digest('/usr/bin/python3'),host_git_sha256=digest('/usr/bin/git'),
        parked_worker_build=parked_receipt,
        commands=dict(import_image=command+['import',str(tar)]))
    (args.output/'input-manifest.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(dict(rootfs=str(root),podman_root=str(data),podman_image=image,
        input_manifest=str(args.output/'input-manifest.json'))))


if __name__=='__main__':main()
