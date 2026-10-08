"""Proxy and VM network latency and throughput against native.

Benchmark: B-NETWORK (benchmark/README.md#b-network), role user-facing.
Motivation: Agents make many small requests and bulk downloads; users need
the latency and throughput cost of network policy and VM networking.
Conclusion sought: ms added per small request by the proxy, VM bulk
throughput as a fraction of native, and whether either matters next to model
response time.
Design: local HTTP server, small-request latency and ~32 MiB transfers for
native, host proxy and VM; no public network; not model API latency.
"""

import json
import random
import shutil
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from .common import checked


class Origin(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        stream = self.path == "/stream"
        data = b"data: token\n\n" if stream else b"x" * 1024
        count = 10 if stream else (32768 if self.path == "/bulk" else 1)
        self.send_response(200)
        self.send_header("Content-Length", str(len(data) * count))
        self.end_headers()
        for _ in range(count):
            self.wfile.write(data)
            if stream:
                self.wfile.flush()
                time.sleep(0.002)

    def log_message(self, *args):
        pass


def run_trial(ctx, target, mode, backend, trial):
    root=ctx.fresh(f"net-{mode}-{backend}");work=root/'workspace';work.mkdir()
    shutil.copy2(Path(__file__).with_name('network_worker.py'),work/'worker.py')
    stage=root/'stage';runs=root/'runs'
    options=['--overlaynet-deny-all'] if mode=='deny' else ['--overlaynet-allow',target]
    affinity='0,1' if backend=='vm' else ctx.args.cpu_affinity
    command=ctx.command(backend,work,stage,['/usr/bin/python3','worker.py',mode,'http://'+target,affinity],
        network=('auto' if backend=='vm' else 'proxy',*options))
    wall,stdout,_=ctx.run(command,cwd=work,env={'PVISOR_RUN_HOME':str(runs),'XDG_CONFIG_HOME':str(root/'config')})
    bundle=ctx.validate_bundle(backend,runs,stage,expected_isolation='rootless_process' if backend=='host' and mode=='deny' else None)
    if bundle:stdout=bundle['run']['output']['stdout']
    value=json.loads(stdout.strip().splitlines()[-1])
    if value['mode']!=mode or value['cpu_affinity']!=sorted(map(int,affinity.split(','))):raise ValueError('wrong network condition or CPU budget')
    row=dict(suite='network',workload=mode,backend=backend,trial=trial,wall_ms=wall,worker_ms=value['worker_ms'],
        check=value['check'],correctness='passed',logs=str(root),cpu_affinity=value['cpu_affinity'])
    if bundle:
        row['safety']=bundle['safety']
        if mode=='deny' and not bundle['safety']['network_non_bypassable']:raise ValueError('deny boundary bypassable')
    if mode=='deny' and value['check']!={'direct_socket_blocked':True}:raise ValueError('direct socket bypassed deny-all')
    if trial>=0:ctx.record(row)


def run(ctx):
    addresses=json.loads(checked(['ip','-4','-json','addr','show']).stdout)
    host=next(a['local'] for item in addresses for a in item['addr_info'] if a['scope']=='global' and not a['local'].startswith('198.18.'))
    ThreadingHTTPServer.request_queue_size=128;server=ThreadingHTTPServer((host,0),Origin)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    target=f'{host}:{server.server_port}'
    modes=ctx.args.network_modes.split(',');backends=ctx.args.network_backends.split(',')
    if len(set(modes))!=len(modes) or set(modes)-{'small','bulk','stream','deny'} or len(set(backends))!=len(backends) or set(backends)-{'native','host','vm','podman','container'}:raise ValueError('invalid network conditions')
    conditions=[(mode,backend) for mode in modes for backend in (['host','vm'] if mode=='deny' else backends)]
    ctx.metadata['network_origin']=dict(address=target,location='same host; no Internet',bytes=32*1024**2)
    ctx.metadata['network_protocol']=dict(order='seeded shuffled all mode/backend conditions each paired round',samples=ctx.args.samples,warmups=ctx.args.warmups,
        cpu='two requested host CPUs; VM has two vCPU; each payload installs and checks affinity; local HTTP origin outside payload budget',
        controls='native, host policy proxy, VM policy, rootless Podman host-network, pVisor OCI host-network',
        budget='CPU controlled; memory not identically capped; not a resource-capacity ranking',
        small='256 fresh TCP requests, eight threads; per-request measurements within a batch are correlated',
        stream='ten 13-byte events, 2 ms server delay; checks complete payload; first-byte and full transfer separate',
        deny='host and VM direct-socket negative controls; successful allow probes are positive controls',
        exclusions='no timing exclusions; failed preflights unavailable, measured failures retained; no public-network or model-latency claim')
    ctx.save();available=[];rng=random.Random(ctx.args.seed)
    try:
        for mode,backend in conditions:
            key='network/'+mode+'/'+backend
            try:
                run_trial(ctx,target,mode,backend,-100);ctx.capabilities[key]=dict(state='available');available.append((mode,backend))
            except Exception as error:ctx.capabilities[key]=dict(state='failed',phase='preflight',reason=str(error),traceback=traceback.format_exc())
            ctx.save()
        for trial in range(-ctx.args.warmups,ctx.args.samples):
            cases=available.copy();rng.shuffle(cases)
            for mode,backend in cases:
                key='network/'+mode+'/'+backend
                try:run_trial(ctx,target,mode,backend,trial)
                except Exception as error:
                    failure=ctx.capabilities.setdefault(key,dict(state='failed'));failure['state']='failed'
                    failure.setdefault('failures',[]).append(dict(trial=trial,error=str(error),traceback=traceback.format_exc()));ctx.save()
                    if trial<0:raise
            if trial>=0 and (trial+1)%5==0:print(f'network: {trial+1}/{ctx.args.samples}',flush=True)
    finally:
        server.shutdown();server.server_close();thread.join()
