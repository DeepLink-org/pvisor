"""Target-file-only cache operations. No global cache or source eviction."""
import ctypes,mmap,os,stat
from pathlib import Path
PAGE=os.sysconf('SC_PAGE_SIZE')
LIB=ctypes.CDLL(None,use_errno=True);LIB.mincore.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_void_p];LIB.mincore.restype=ctypes.c_int

def resident(fd,size):
 if not size:return (0,0)
 n=(size+PAGE-1)//PAGE
 with mmap.mmap(fd,size,access=mmap.ACCESS_COPY) as mapping:
  buf=ctypes.c_char.from_buffer(mapping);addr=ctypes.addressof(buf);vec=(ctypes.c_ubyte*n)()
  rc=LIB.mincore(addr,size,vec);del buf
  if rc:raise OSError(ctypes.get_errno(),'mincore')
  return (sum(v&1 for v in vec),n)

def regular_files(root):
 root=Path(root);assert root.is_dir() and not root.is_symlink()
 seen=set();result=[]
 for p in sorted(root.rglob('*')):
  s=p.lstat()
  if not stat.S_ISREG(s.st_mode):continue
  key=(s.st_dev,s.st_ino)
  if key in seen:continue
  seen.add(key);result.append(p)
 return result

def target_stats(root,operation):
 assert operation in ('resident','evict','warm')
 files=regular_files(root);assert files
 for p in files:
  fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
  try:
   if operation=='evict':os.fsync(fd);os.posix_fadvise(fd,0,0,os.POSIX_FADV_DONTNEED)
   elif operation=='warm':
    while os.read(fd,4*1024*1024):pass
  finally:os.close(fd)
 # Separate scan verifies the complete target after all actions, including early files.
 cached=total=0
 for p in files:
  fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW)
  try:r,n=resident(fd,os.fstat(fd).st_size);cached+=r;total+=n
  finally:os.close(fd)
 assert total>0
 return dict(files=len(files),resident_pages=cached,total_pages=total,resident_fraction=cached/total,page_bytes=PAGE,total_page_bytes=total*PAGE)

def require_cold(s):assert s['resident_pages']==0,('target still resident; no workload allowed',s)

import json
import re


def locate(root, url):
    if url == '/v2/':
        return None, 'application/json', None
    match = re.fullmatch(r'/v2/([a-z0-9-]+)/(manifests|blobs)/(sha256:[0-9a-f]{64})', url)
    if not match:
        raise FileNotFoundError(url)
    task, kind, digest = match.groups()
    records = json.loads((root / 'registry.json').read_text())
    record = records[task]
    choices = [record['manifest']] if kind == 'manifests' else [record['config'], record['layer']]
    descriptor = next((x for x in choices if x['digest'] == digest), None)
    if descriptor is None:
        raise FileNotFoundError(url)
    path = root / 'blobs/sha256' / digest.split(':')[1]
    assert path.stat().st_size == descriptor['size']
    return path, descriptor['mediaType'], digest
