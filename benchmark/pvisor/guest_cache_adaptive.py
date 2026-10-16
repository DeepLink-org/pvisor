#!/usr/bin/env python3
"""Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), engineering A/B.
Motivation: recover dense lookup performance without unbounded sparse prefetch.
Conclusion sought: paired candidate differences against eager and scan-only kernels.
Design: frozen CLI/SDK/guest, three firmware arms, 3 warmups/30 shuffled rounds;
2 vCPU/4096 MiB, fresh VM/upper, verified inputs, separate B-FS-DIAG profiles.
"""
import argparse
import json
from pathlib import Path
import random
import guest_cache as bench

LABELS={0:'eager',1:'scan_only',2:'adaptive'}

def analyze(report):
    rows=[x for x in report['rows'] if x['round']>=0]
    expected={(i,c,a) for i in range(report['samples']) for c in report['cohorts'] for a in LABELS}
    keys=[(x['round'],x['cohort'],x['arm']) for x in rows]
    if len(keys)!=len(set(keys)) or set(keys)!=expected:raise ValueError('incomplete or duplicate three-arm cohort')
    result=[]
    for reference in (0,1):
        paired={**report,'rows':[{**x,'arm':int(x['arm']==2)} for x in rows if x['arm'] in (reference,2)]}
        result.extend({**x,'reference':LABELS[reference]} for x in bench.analyze(paired))
    return result

def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('prepared','eager','scan','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--samples',type=int,default=30);p.add_argument('--warmups',type=int,default=3)
    p.add_argument('--profiles',action='store_true')
    a=p.parse_args()
    if a.samples<1 or a.warmups<0 or '.data' not in a.output.parts:p.error('invalid campaign')
    prepared=a.prepared.resolve();out=a.output.resolve()
    firmware={0:a.eager.resolve(),1:a.scan.resolve(),2:prepared/'libkrunfw.5.dylib'}
    digests={k:bench.sha(v) for k,v in firmware.items()}
    bench.verify(prepared);out.mkdir(parents=True,exist_ok=False)
    r={'benchmark_id':'B-FS-DIAG' if a.profiles else 'B-FS-ENG','profiles':a.profiles,
       'cohorts':['sdk','cli'],'samples':a.samples,'warmups':a.warmups,'seed':20261018,
       'arms':LABELS,'prepared':str(prepared),'prepared_receipt':json.loads((prepared/'receipt.json').read_text()),
       'firmware':{k:{'path':str(v),'sha256':digests[k]} for k,v in firmware.items()},
       'resources':'HVF 2 vCPU/4096 MiB; no host affinity or cgroup claim',
       'cache':'warm host; fresh VM and upper; guest cache=1; writeback=0',
       'rows':[],'failures':[],'no_exclusions':True}
    plan=[];rng=random.Random(r['seed'])
    if a.profiles:
        plan=[(0,c,k,m) for c in r['cohorts'] for k in LABELS for m in (*bench.MODES,'all')]
    else:
        for i in range(-a.warmups,a.samples):
            conditions=[(c,k) for c in r['cohorts'] for k in LABELS];rng.shuffle(conditions)
            plan.extend((i,c,k,'all') for c,k in conditions)
    r['plan']=plan;bench.save(out/'report.json',r)
    try:
        for i,c,k,m in plan:
            value=bench.run_one(prepared,out/f'{i+a.warmups:02}-{c}-{LABELS[k]}-{m}',c,1,m,a.profiles,firmware[k])
            r['rows'].append({'round':i,'cohort':c,'arm':k,'mode':m,**value})
            bench.save(out/'report.json',r);print(i,c,LABELS[k],m,flush=True)
        bench.verify(prepared)
        if any(bench.sha(v)!=digests[k] for k,v in firmware.items()):raise ValueError('firmware changed')
        if not a.profiles:bench.save(out/'summary.json',analyze(r))
        r['state']='passed'
    except Exception as error:
        r['state']='failed';r['failures'].append(str(error));raise
    finally:bench.save(out/'report.json',r)

if __name__=='__main__':main()
