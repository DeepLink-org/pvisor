"""Lossless post-measurement retention of completed parked-capacity snapshots.

Logs, configuration, result proofs, Run Bundles and final upper changes stay
directly readable. Failed/unknown batches are never compacted. Archive contents
are checked independently before the original generated snapshot trees retire.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile


def file_sha(path):
    value=hashlib.sha256()
    with path.open('rb') as handle:
        for block in iter(lambda:handle.read(1024**2),b''):value.update(block)
    return value.hexdigest()


def owned(root,path):
    if not path.is_relative_to(root):raise ValueError('outside owned batch')
    current=path
    while current!=root:
        if current.is_symlink():raise ValueError('snapshot tree ownership includes symlink')
        current=current.parent
    if root.is_symlink():raise ValueError('batch root is a symlink')


def inventory(root,trees):
    records={}
    for tree in trees:
        owned(root,tree)
        paths=[tree]
        for directory,dirs,files in os.walk(tree,followlinks=False):
            paths.extend(Path(directory)/name for name in dirs+files)
        for path in sorted(set(paths)):
            info=path.lstat();name=str(path.relative_to(root))
            attributes={name:base64.b64encode(os.getxattr(path,name,follow_symlinks=False)).decode('ascii')
                for name in os.listxattr(path,follow_symlinks=False)}
            common=dict(mode=stat.S_IMODE(info.st_mode),mtime_ns=info.st_mtime_ns,uid=info.st_uid,gid=info.st_gid,xattrs=attributes)
            if stat.S_ISLNK(info.st_mode):record=dict(kind='symlink',target=os.readlink(path),**common)
            elif stat.S_ISDIR(info.st_mode):record=dict(kind='directory',**common)
            elif stat.S_ISREG(info.st_mode):record=dict(kind='file',size=info.st_size,sha256=file_sha(path),**common)
            else:raise ValueError('snapshot contains unsupported special file')
            records[name]=record
    return records


def validate_archive(path,expected,zstd='zstd'):
    process=subprocess.Popen([zstd,'-q','--long=29','-d','-c',str(path)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    actual={}
    try:
        with tarfile.open(fileobj=process.stdout,mode='r|') as archive:
            for member in archive:
                name=member.name
                if name not in expected or name in actual:raise ValueError('unexpected or duplicate archive member')
                record=expected[name]
                if (member.mode!=record['mode'] or abs(member.mtime-record['mtime_ns']/1e9)>1e-6
                        or member.uid!=record['uid'] or member.gid!=record['gid']
                        or member.pax_headers.get('PVISOR.mtime_ns')!=str(record['mtime_ns'])
                        or json.loads(member.pax_headers.get('PVISOR.xattrs','null'))!=record['xattrs']):
                    raise ValueError('archive metadata differs')
                if record['kind']=='directory':
                    if not member.isdir():raise ValueError('archive directory differs')
                elif record['kind']=='symlink':
                    if not member.issym() or member.linkname!=record['target']:raise ValueError('archive symlink differs')
                elif member.islnk():
                    previous=actual.get(member.linkname)
                    if previous is None or previous.get('sha256')!=record['sha256'] or previous.get('size')!=record['size']:
                        raise ValueError('archive hard link differs')
                else:
                    if not member.isfile() or member.size!=record['size']:raise ValueError('archive file size/type differs')
                    value=hashlib.sha256();handle=archive.extractfile(member)
                    for block in iter(lambda:handle.read(1024**2),b''):value.update(block)
                    if value.hexdigest()!=record['sha256']:raise ValueError('archive file content differs')
                actual[name]=record
        # Consume the compressor stream after tar's logical end so corruption in
        # the final compressed frame is checked by zstd before deletion.
        while process.stdout.read(1024**2):pass
        process.stdout.close();error=process.stderr.read();code=process.wait()
        if code or set(actual)!=set(expected):raise ValueError('incomplete or corrupt archive: '+error.decode(errors='replace'))
    finally:
        if process.poll() is None:process.kill();process.wait()
        process.stdout.close();process.stderr.close()


def archive_completed_snapshots(root,value,service_exit,zstd='zstd'):
    root=Path(root).absolute()
    if root.is_symlink():raise ValueError('batch root is a symlink')
    root=root.resolve()
    if value.get('correctness')!='passed' or service_exit!=0:return None
    if value.get('backend') not in ('snapshot-raw','snapshot-compressed'):return None
    count=value['concurrency']
    if (value.get('attempted')!=count or value.get('parked')!=count or value.get('completed')!=count
            or value.get('failed')!=0 or len(value.get('outcomes',[]))!=count):
        raise ValueError('incomplete supposedly successful snapshot batch')
    trees=[]
    for index,outcome in enumerate(value['outcomes']):
        job=root/f'j{index}';owned(root,job)
        if Path(outcome['logs']).resolve()!=job or outcome.get('correctness')!='passed':raise ValueError('wrong completed Job path')
        state_path=job/'stage/execution-job.json';owned(root,state_path)
        state=json.loads(state_path.read_text())
        if state['state']!='terminal':raise ValueError('snapshot Job is not terminal')
        active=Path(state['active_stage']).absolute();owned(root,active)
        if not active.is_relative_to(job/'stage') or not (active/'run-bundle.json').is_file():raise ValueError('missing terminal Run Bundle')
        ready=outcome['ready'];result=outcome['result']
        if (result.get('integrity')!='passed' or result.get('changes')!=4
                or ready.get('bytes')!=64*1024**2
                or any(result.get(key)!=ready.get(key) for key in ('token','checksum','pid','kind','seed','bytes'))):
            raise ValueError('missing complete recovered execution proof')
        stream=job/'resumed/stdout.log';owned(root,stream)
        markers=[json.loads(line.removeprefix('PVISOR_PARKED_RESULT ')) for line in stream.read_text().splitlines() if line.startswith('PVISOR_PARKED_RESULT ')]
        if markers!=[result]:raise ValueError('independent restored output differs')
        snapshots=job/'stage/execution-snapshots'
        if not snapshots.is_dir():raise ValueError('missing original snapshot tree')
        trees.append(snapshots)
        trees.extend(sorted((job/'stage/attempts').glob('*/execution-restore')))
    expected=inventory(root,trees)
    archive=root/'snapshot-artifacts.tar.zst';temporary=root/'snapshot-artifacts.tar.zst.partial'
    manifest=root/'snapshot-artifacts.json'
    if any(path.exists() for path in (archive,temporary,manifest)):raise ValueError('archive output already exists')
    with temporary.open('xb') as output:
        process=subprocess.Popen([zstd,'-q','-3','--long=29','-T1','-c'],stdin=subprocess.PIPE,stdout=output,stderr=subprocess.PIPE)
        try:
            with tarfile.open(fileobj=process.stdin,mode='w|',format=tarfile.PAX_FORMAT,dereference=False) as tar:
                def metadata(member):
                    record=expected[member.name]
                    member.pax_headers['PVISOR.mtime_ns']=str(record['mtime_ns'])
                    member.pax_headers['PVISOR.xattrs']=json.dumps(record['xattrs'],sort_keys=True)
                    return member
                for name in sorted(expected):tar.add(root/name,arcname=name,recursive=False,filter=metadata)
            process.stdin.close();error=process.stderr.read();code=process.wait()
            if code:raise ValueError('snapshot compression failed: '+error.decode(errors='replace'))
        finally:
            if process.poll() is None:process.kill();process.wait()
            process.stderr.close()
        output.flush();os.fsync(output.fileno())
    validate_archive(temporary,expected,zstd)
    if inventory(root,trees)!=expected:raise ValueError('snapshot changed during retention')
    temporary.replace(archive)
    receipt=dict(format='tar+pax+zstd',archive=archive.name,archive_sha256=file_sha(archive),members=expected,
        codec='zstd level3, one thread, maximum 512MiB matching window; outside measured service',
        roots=[str(tree.relative_to(root)) for tree in trees],
        timing='after terminated measured service; excluded from task timing, phase CPU and memory',
        recovery='extract into an empty scratch directory; member paths are relative to the batch root',
        verified='all member types, modes, numeric ownership, nanosecond timestamps, xattrs, file SHA256, links and zstd frame completion')
    with manifest.open('x') as handle:
        handle.write(json.dumps(receipt,indent=2)+'\n');handle.flush();os.fsync(handle.fileno())
    descriptor=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(descriptor)
    finally:os.close(descriptor)
    for tree in trees:
        owned(root,tree);shutil.rmtree(tree)
    return dict(manifest=manifest.name,manifest_sha256=file_sha(manifest),archive=archive.name,
        archive_sha256=receipt['archive_sha256'],members=len(expected),compressed_bytes=archive.stat().st_size)


def restore_archive(manifest_path,destination,zstd='zstd'):
    manifest_path=Path(manifest_path);destination=Path(destination).absolute()
    receipt=json.loads(manifest_path.read_text());archive=manifest_path.parent/receipt['archive'];expected=receipt['members']
    if archive.parent!=manifest_path.parent or file_sha(archive)!=receipt['archive_sha256']:
        raise ValueError('archive receipt mismatch')
    for name in expected:
        path=Path(name)
        if path.is_absolute() or '..' in path.parts:raise ValueError('unsafe archived member path')
        if any(expected.get(str(parent),{}).get('kind')=='symlink' for parent in path.parents):
            raise ValueError('archived member traverses symlink')
    validate_archive(archive,expected,zstd)
    if destination.exists() or destination.is_symlink():raise ValueError('restore requires a new empty destination')
    destination.mkdir()
    process=subprocess.Popen([zstd,'-q','--long=29','-d','-c',str(archive)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    try:
        with tarfile.open(fileobj=process.stdout,mode='r|') as tar:
            # All names, types, data, link targets and symlink ancestors have
            # already been checked, and the destination is a fresh directory.
            # Guest absolute symlink targets must remain literal guest links.
            tar.extractall(destination,filter='fully_trusted')
        while process.stdout.read(1024**2):pass
        process.stdout.close();error=process.stderr.read();code=process.wait()
        if code:raise ValueError('archive restore failed: '+error.decode(errors='replace'))
    finally:
        if process.poll() is None:process.kill();process.wait()
        process.stdout.close();process.stderr.close()
    for name in sorted(expected,key=lambda value:len(Path(value).parts),reverse=True):
        path=destination/name;record=expected[name]
        for key,value in record['xattrs'].items():os.setxattr(path,key,base64.b64decode(value),follow_symlinks=False)
        if record['kind']!='symlink':os.chmod(path,record['mode'])
        os.utime(path,ns=(record['mtime_ns'],record['mtime_ns']),follow_symlinks=False)
    trees=[destination/name for name in receipt['roots']]
    if inventory(destination,trees)!=expected:raise ValueError('restored inventory differs from originals')
    return destination


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--restore',type=Path,required=True);parser.add_argument('--destination',type=Path,required=True)
    args=parser.parse_args();print(restore_archive(args.restore,args.destination))
