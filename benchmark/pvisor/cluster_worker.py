"""Fixed useful Python/Git task for B-CLUSTER, with verifiable returned results."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

started=time.perf_counter_ns()
token=sys.argv[1]
root=Path('/tmp')/token
root.mkdir()
(root/'files').mkdir()
for i in range(64):(root/'files'/f'f{i:03d}').write_text(f'old-{i}\n')
for command in (['git','init','-q'],['git','config','core.hooksPath','/dev/null'],
                ['git','config','gc.auto','0'],['git','add','files'],
                ['git','-c','user.name=Benchmark','-c','user.email=bench@example.invalid','commit','-qm','fixture']):
    subprocess.run([command[0],'-c','safe.directory='+str(root),*command[1:]],cwd=root,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
data=bytearray(b'x'*(32*1024*1024))
expected=hashlib.sha256(data).hexdigest()
for _ in range(8):
    assert hashlib.sha256(data).hexdigest()==expected
for i in range(4):(root/'files'/f'f{i:03d}').write_text(f'new-{i}\n')
changes=subprocess.check_output(['git','-c','safe.directory='+str(root),'status','--porcelain'],cwd=root,text=True).splitlines()
assert sorted(line[3:] for line in changes)==[f'files/f{i:03d}' for i in range(4)]
for i in range(64):
    assert (root/'files'/f'f{i:03d}').read_text()==(f'new-{i}\n' if i<4 else f'old-{i}\n')
print('CLUSTER_RESULT '+json.dumps(dict(token=token,bytes=len(data),checksum=expected,changes=4,
    integrity='passed',worker_ms=(time.perf_counter_ns()-started)/1e6)),flush=True)
