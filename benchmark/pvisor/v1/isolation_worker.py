import json
from pathlib import Path
import socket
import sys

outside,lower=sys.argv[1:3]
result={}
for name,path in [('absolute',outside+'/secret'),('symlink','escape/secret'),('proc-root','/proc/self/root'+outside+'/secret'),('traversal','../outside/secret')]:
    try:result[name+'-read']=Path(path).read_text()=='benchmark-secret'
    except OSError:result[name+'-read']=False
for name,path in [('absolute',outside+'/written'),('symlink','escape/symlink-written'),('lower-alias',lower+'/alias-written')]:
    try:Path(path).write_text('benchmark-write');result[name+'-write']=True
    except OSError:result[name+'-write']=False
try:
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(1);stream.connect(outside+'/socket');result['unix-socket']=stream.recv(32)==b'benchmark-socket'
except OSError:result['unix-socket']=False
Path('staged-marker').write_text('benchmark-write')
print(json.dumps(result))
