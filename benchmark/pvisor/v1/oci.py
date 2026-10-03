"""Prepare a local OCI rootfs; no image download or third-party service."""
from pathlib import Path
import shutil

from run_all import prepare_rootfs, ldd_paths
from .common import checked, digest


def prepare(ctx):
    root=ctx.output/'rootfs'
    root.mkdir()
    copied=prepare_rootfs(root,ctx.binary)
    sources=set()
    for name in ('python3','git','rg','node-24','cc','as','ld'):
        path=Path('/usr/bin')/name
        if path.exists():
            sources.add(path)
            sources.update(ldd_paths(path))
    for binary in (ctx.toolchain/'bin/cargo',ctx.toolchain/'bin/rustc'):
        sources.update(ldd_paths(binary))
    sources.update(Path('/usr/lib64').glob('*crt*.o'))
    for name in ('libutil.a','librt.a','libpthread.a','libdl.a'):
        sources.add(Path('/usr/lib64')/name)
    sources.update(Path('/usr/lib64').glob('libc*.so'))
    sources.update(Path('/usr/lib64').glob('libc_nonshared.a'))
    for name in ('libgcc_s.so.1','libutil.so.1','librt.so.1','libpthread.so.0','libm.so','libm.so.6','libmvec.so.1'):
        sources.add(Path('/usr/lib64')/name)
    for path in sources:
        target=root/path.relative_to('/')
        target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(path,target,follow_symlinks=True)
    for path in (Path('/usr/lib64/python3.14'),Path('/usr/lib/node_modules_24')):
        shutil.copytree(path,root/path.relative_to('/'),ignore=shutil.ignore_patterns('site-packages','__pycache__'))
    for path in (Path('/usr/lib/gcc'),Path('/usr/libexec/gcc')):
        shutil.copytree(path,root/path.relative_to('/'),symlinks=True,ignore=shutil.ignore_patterns('32'))
    (root/'usr/bin/node').symlink_to('node-24')
    (root/'usr/bin/npm').symlink_to('../lib/node_modules_24/npm/bin/npm-cli.js')
    (root/'tmp').chmod(0o1777)
    (root/'root').mkdir()
    tar=ctx.output/'rootfs.tar'
    checked(['tar','-C',str(root),'-cf',str(tar),'.'])
    ctx.metadata['oci_rootfs_tar_sha256']=digest(tar)
    ctx.rootfs=root
    try:
        ctx.image=checked(['podman','import',str(tar)],timeout=120).stdout.strip()
        ctx.metadata['podman_image']=ctx.image
        ctx.metadata['podman_version']=checked(['podman','--version']).stdout.strip()
        ctx.metadata['crun_version']=checked(['crun','--version']).stdout.splitlines()[0]
    except RuntimeError as error:
        ctx.capabilities['podman']={'state':'unavailable','reason':str(error)}
    ctx.save()


def prepare_shell(ctx):
    root=ctx.output/'rootfs';root.mkdir()
    prepare_rootfs(root,ctx.binary)
    (root/'tmp').chmod(0o1777)
    tar=ctx.output/'rootfs.tar'
    checked(['tar','-C',str(root),'-cf',str(tar),'.'])
    ctx.rootfs=root
    ctx.image=checked(['podman','import',str(tar)],timeout=120).stdout.strip()
    ctx.metadata.update(oci_shape='shell',oci_rootfs_tar_sha256=digest(tar),podman_image=ctx.image,
                        rootfs_file_bytes=sum(p.stat().st_size for p in root.rglob('*') if p.is_file()))
    ctx.save()
