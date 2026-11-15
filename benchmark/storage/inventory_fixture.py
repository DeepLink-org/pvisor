"""Deterministic inode inventory fixture for capacity contract tests.

This is not an experiment driver. Active storage accounting is implemented in
Rust; this independent oracle checks hardlink and no-follow semantics.
"""

SCANNER = r"""
import os,json,sys,stat
root=sys.argv[1];seen=set();rows=[]
def walk(path):
 s=os.lstat(path);key=(s.st_dev,s.st_ino)
 if key in seen:return
 seen.add(key)
 rows.append(dict(path=os.path.relpath(path,root),apparent=s.st_size,
                  allocated=s.st_blocks*512,mode=s.st_mode,inode=s.st_ino,device=s.st_dev))
 if stat.S_ISDIR(s.st_mode):
  with os.scandir(path) as entries:
   for entry in sorted(entries,key=lambda e:e.name):walk(entry.path)
walk(root)
print(json.dumps(dict(apparent=sum(r['apparent'] for r in rows),
 allocated=sum(r['allocated'] for r in rows),inodes=len(rows),rows=rows)))
"""
