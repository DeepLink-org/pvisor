"""Prepare a local OCI rootfs; no image download or third-party service."""

import shutil
from pathlib import Path

from run_all import ldd_paths, prepare_rootfs

from .common import checked, digest


def prepare(ctx):
    root = ctx.output / "rootfs"
    root.mkdir()
    prepare_rootfs(root, ctx.binary)
    sources = set()
    for name in ("python3", "git", "rg", "node-24", "cc", "as", "ld"):
        path = Path("/usr/bin") / name
        if path.exists():
            sources.add(path)
            sources.update(ldd_paths(path))
    for binary in (ctx.toolchain / "bin/cargo", ctx.toolchain / "bin/rustc"):
        sources.update(ldd_paths(binary))
    sources.update(Path("/usr/lib64").glob("*crt*.o"))
    for name in ("libutil.a", "librt.a", "libpthread.a", "libdl.a"):
        sources.add(Path("/usr/lib64") / name)
    sources.update(Path("/usr/lib64").glob("libc*.so"))
    sources.update(Path("/usr/lib64").glob("libc_nonshared.a"))
    for name in (
        "libgcc_s.so.1",
        "libutil.so.1",
        "librt.so.1",
        "libpthread.so.0",
        "libm.so",
        "libm.so.6",
        "libmvec.so.1",
    ):
        sources.add(Path("/usr/lib64") / name)
    for path in sources:
        target = root / path.relative_to("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target, follow_symlinks=True)
        # Fedora linker scripts use absolute /lib64 paths; this minimal
        # rootfs has a real /lib64 directory instead of Fedora's host symlink.
        if path.parent == Path("/usr/lib64"):
            alias = root / "lib64" / path.name
            alias.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, alias, follow_symlinks=True)
    for path in (Path("/usr/lib64/python3.14"), Path("/usr/lib/node_modules_24")):
        shutil.copytree(
            path,
            root / path.relative_to("/"),
            ignore=shutil.ignore_patterns("site-packages", "__pycache__"),
        )
    for path in (Path("/usr/lib/gcc"), Path("/usr/libexec/gcc")):
        shutil.copytree(
            path, root / path.relative_to("/"), symlinks=True, ignore=shutil.ignore_patterns("32")
        )
    (root / "usr/bin/node").symlink_to("node-24")
    (root / "usr/bin/npm").symlink_to("../lib/node_modules_24/npm/bin/npm-cli.js")
    (root / "tmp").chmod(0o1777)
    (root / "root").mkdir()
    tar = ctx.output / "rootfs.tar"
    checked(["tar", "-C", str(root), "-cf", str(tar), "."])
    ctx.metadata["oci_rootfs_tar_sha256"] = digest(tar)
    ctx.rootfs = root
    try:
        ctx.image = checked(["podman", "import", str(tar)], timeout=120).stdout.strip()
        ctx.metadata["podman_image"] = ctx.image
        ctx.metadata["podman_version"] = checked(["podman", "--version"]).stdout.strip()
        ctx.metadata["crun_version"] = checked(["crun", "--version"]).stdout.splitlines()[0]
    except RuntimeError as error:
        ctx.capabilities["podman"] = {"state": "unavailable", "reason": str(error)}
    ctx.save()


def prepare_shell(ctx):
    root = ctx.output / "rootfs"
    root.mkdir()
    prepare_rootfs(root, ctx.binary)
    (root / "tmp").chmod(0o1777)
    tar = ctx.output / "rootfs.tar"
    checked(["tar", "-C", str(root), "-cf", str(tar), "."])
    ctx.rootfs = root
    ctx.image = checked(["podman", "import", str(tar)], timeout=120).stdout.strip()
    ctx.metadata.update(
        oci_shape="shell",
        oci_rootfs_tar_sha256=digest(tar),
        podman_image=ctx.image,
        rootfs_file_bytes=sum(p.stat().st_size for p in root.rglob("*") if p.is_file()),
    )
    ctx.save()


def verify_prepared(root, manifest):
    entries=manifest['rootfs_manifest']
    if {str(p.relative_to(root)) for p in root.rglob('*')}!={r['path'] for r in entries}:
        raise ValueError('prepared rootfs inventory differs')
    for entry in entries:
        path=root/entry['path']
        if path.lstat().st_mode!=entry['mode'] or (entry['kind']=='file' and digest(path)!=entry['sha256']) or (entry['kind']=='symlink' and str(path.readlink())!=entry['target']):
            raise ValueError('prepared rootfs contents/modes differ')


def use_prepared(ctx):
    import json
    args=ctx.args
    if not args.input_manifest or not args.podman_root or not args.podman_image:
        raise ValueError('prepared rootfs requires input manifest and private Podman store/image')
    manifest=json.loads(args.input_manifest.read_text());root=args.prepared_rootfs.resolve()
    verify_prepared(root,manifest)
    if digest('/usr/bin/python3')!=manifest['host_python_sha256'] or digest('/usr/bin/git')!=manifest['host_git_sha256']:
        raise ValueError('host tools differ from prepared tools')
    runroot=(args.podman_runroot or Path(manifest['podman_runroot'])).resolve()
    if str(runroot)!=manifest['podman_runroot'] or args.podman_image!=manifest['podman_image']:
        raise ValueError('prepared private Podman image/runroot differs')
    ctx.rootfs=root;ctx.image=args.podman_image
    ctx.podman_options=['--root',str(args.podman_root.resolve()),'--runroot',str(runroot),'--storage-driver','overlay']
    inspected=checked(['podman',*ctx.podman_options,'image','inspect',ctx.image]).stdout
    if json.loads(inspected)[0]['Id'].removeprefix('sha256:')!=ctx.image.removeprefix('sha256:'):raise ValueError('Podman immutable image identity differs')
    shutil.copy2(args.input_manifest,ctx.output/'input-manifest.json')
    ctx.metadata.update(prepared_rootfs=str(root),input_manifest_sha256=digest(args.input_manifest),
        podman_image=ctx.image,podman_options=ctx.podman_options,oci_preparation='verified immutable existing offline Python/Git rootfs and private image; preparation excluded')
    ctx.save()
