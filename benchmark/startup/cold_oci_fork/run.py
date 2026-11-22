import concurrent.futures
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
import time

# Benchmark: B-STARTUP-ENG. Linux KVM, cold local OCI to first command and four live children.
from fixture import locate, target_stats, require_cold

source = Path(os.environ['BENCH_OCI_FIXTURE']).resolve(strict=True)
binary = Path(os.environ['BENCH_PVISOR']).resolve(strict=True)
scratch = Path(os.environ['BENCH_SCRATCH']).resolve(strict=True)
command = 'import json,os;print(json.dumps({"ready":True,"cpus":os.cpu_count()}))'
digest = json.loads((source/'registry.json').read_text())['vuls-666']['manifest']['digest']
print(json.dumps(dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),manifest=digest)),flush=True)
for trial, early in enumerate([True, False, False, True, True, False], 1):
    with tempfile.TemporaryDirectory(prefix='oci-cold-',dir=scratch) as temp:
        root=Path(temp)
        trial_lock=(scratch/'bench.lock').open('a')
        fcntl.flock(trial_lock,fcntl.LOCK_EX)
        raw=root/'raw'
        shutil.copytree(source,raw)
        for blob in (raw/'blobs/sha256').iterdir():
            with blob.open('rb') as f:
                assert hashlib.file_digest(f,'sha256').hexdigest()==blob.name
        cold=target_stats(raw,'evict')
        require_cold(cold)
        subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=127.0.0.1','-addext','subjectAltName=IP:127.0.0.1','-keyout',str(root/'key'),'-out',str(root/'cert')],check=True,capture_output=True)
        def openssl(*args):
            subprocess.run(['openssl', *args], cwd=root, capture_output=True, check=True)
        openssl('req','-new','-newkey','rsa:2048','-nodes','-subj','/CN=127.0.0.1','-keyout','leaf.key','-out','leaf.csr')
        (root/'ext').write_text('subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\n')
        openssl('x509','-req','-in','leaf.csr','-CA','cert','-CAkey','key','-CAcreateserial','-days','1','-extfile','ext','-out','leaf.pem')
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                try:
                    path,media,tag=locate(raw,self.path)
                except (KeyError,FileNotFoundError):
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header('Content-Length', str(path.stat().st_size if path else 2))
                self.send_header('Content-Type',media)
                self.end_headers()
                if path:
                    with path.open('rb') as f: shutil.copyfileobj(f,self.wfile,1024*1024)
                else: self.wfile.write(b'{}')
        env=dict(os.environ,LD_LIBRARY_PATH=str(binary.parent),PVISOR_DOCKER_HOME=str(root/'docker'),PVISOR_IMAGE_STORE=str(root/'store'),SSL_CERT_FILE=str(root/'cert'))
        env['PVISOR_IMAGE_BATCH_DIRECTORY_SYNC'] = str(int(early))
        env['PVISOR_IMAGE_PUBLISH_WORKERS'] = '8'
        first_env = dict(env, PVISOR_DOCKER_HOME=str(root/'first-catalog')) if early else env
        pub_env = dict(env, PVISOR_BENCH_EARLY_DIR=str(root)) if early else env
        def cli(environment,*args):
            return subprocess.check_output([binary,*args],text=True,env=environment,timeout=180).strip()
        def boot(environment,name):
            cli(environment,'run','-d','--name',name,*(['--branchable'] if name=='first' else []),'--network','none','--cpus','2','--memory','7g','--entrypoint','/usr/bin/python3',ref,'-c','import time; time.sleep(500)')
        def guest(environment,name,code):
            return cli(environment,'exec',name,'/usr/bin/python3','-c',code)
        lease=(root/'lease').open('x')
        if early: fcntl.flock(lease,fcntl.LOCK_SH)
        start=time.monotonic()
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(root/'leaf.pem',root/'leaf.key')
        server.socket=ctx.wrap_socket(server.socket,server_side=True)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        ref=f'127.0.0.1:{server.server_port}/vuls-666@{digest}'
        events={key:threading.Event() for key in ('extracted','published')}
        clocks={}
        pull=subprocess.Popen([binary,'pull',ref],env=pub_env,text=True,stderr=subprocess.PIPE,stdout=subprocess.DEVNULL)
        def observe():
            for line in pull.stderr:
                if line.startswith('BENCH '):
                    key=line.strip().split()[1]
                    clocks[key]=time.monotonic()-start
                    events[key].set()
                elif not line.startswith(('METER','WORKER')):
                    print(line.rstrip(),file=sys.stderr,flush=True)
        reader=threading.Thread(target=observe,daemon=True)
        reader.start()
        def wait_event(key):
            deadline=time.monotonic()+400
            while not events[key].wait(.1):
                if pull.poll() is not None or time.monotonic()>deadline:
                    raise RuntimeError(f'no {key} receipt, pull state {pull.poll()}')
        started=[]
        try:
            if early:
                wait_event('extracted')
                assert not (root/'docker/images.json').exists()
            else:
                assert pull.wait(timeout=400)==0
                reader.join()
            clocks['launch']=time.monotonic()-start
            boot(first_env,'first')
            started.append((first_env,'first'))
            assert json.loads(guest(first_env,'first',command))=={'ready':True,'cpus':2}
            clocks['response']=time.monotonic()-start
            if early: assert not events['published'].is_set()
            marker='/root/.bench-branch-proof'
            mark_code=f'from pathlib import Path;p=Path({marker!r});assert not p.exists();p.write_text("first");print(p.read_text())'
            assert guest(first_env,'first',mark_code)=='first'
            if early:
                lower=Path(json.loads((root/'first-catalog/bench-lower.json').read_text()))
                assert not (lower/marker.lstrip('/')).exists()
            names=[f'child{i}' for i in range(4)]
            check=f'from pathlib import Path;import os;assert os.cpu_count()==2;print(Path({marker!r}).read_text())'
            split=time.monotonic()
            cli(first_env,'container','branch','first','--names',*names)
            returned=time.monotonic()
            started.extend((first_env,n) for n in names)
            def verify(name):
                assert guest(first_env,name,check)=='first'
                return time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(4) as pool:
                completions=list(pool.map(verify,names))
            clocks.update(split=split-start,call_s=returned-split,first_child_s=min(completions)-split,
                          fork_s=max(completions)-split,cold_to_four_s=max(completions)-start)
            if early: assert not events['published'].is_set()
            assert guest(first_env,names[0],f'from pathlib import Path;p=Path({marker!r});p.write_text("child");print(p.read_text())')=='child'
            assert guest(first_env,'first',check)=='first'
            assert all(guest(first_env,n,check)=='first' for n in names[1:])
            wait_event('published')
            assert json.loads((root/'docker/images.json').read_text())[0]['digest']==digest
            # Marker survives publication in the first VM, is absent from a fresh pool-backed VM.
            assert guest(first_env,'first',f'print(open({marker!r}).read())')=='first'
            boot(env,'from-pool')
            started.append((env,'from-pool'))
            assert guest(env,'from-pool',f'import os;assert not os.path.lexists({marker!r});'+command)==json.dumps({'ready':True,'cpus':2})
        finally:
            try:
                for e,name in reversed(started): cli(e,'rm','-f',name)
            finally:
                lease.close()
                try:
                    assert pull.wait(timeout=400)==0
                finally:
                    reader.join(timeout=5)
                    server.shutdown()
                    server.server_close()
        print(json.dumps(dict(trial=trial,early=early,clocks=clocks,cold=cold,guest_write_isolated=True,published_reused=True)),flush=True)
        trial_lock.close()
