"""Disk admission/monitor helper for B-FS-ENG / B-FS-DIAG, outside task timers."""
import argparse
from functools import partial
import json
import os
from pathlib import Path
import shutil
import signal
import sys
import threading
import time

ADMISSION_BYTES=5<<30
CRITICAL_BYTES=1<<30
INTERVAL=0.2


def guarded(original,log,*args,**kwargs):
    def observe(event):
        free=shutil.disk_usage(log.parent).free
        with log.open('a') as stream:
            stream.write(json.dumps({'event':event,'unix_ns':time.time_ns(),'free_bytes':free})+'\n')
        return free
    if observe('admission')<ADMISSION_BYTES:raise RuntimeError('less than 5 GiB disk admission reserve')
    stop=threading.Event();low=threading.Event()
    def monitor():
        while not stop.is_set():
            try:free=observe('active')
            except OSError:free=0
            if free<CRITICAL_BYTES:
                low.set();os.kill(os.getpid(),signal.SIGINT);return
            stop.wait(INTERVAL)
    worker=threading.Thread(target=monitor,daemon=True);worker.start()
    try:result=original(*args,**kwargs)
    finally:stop.set();worker.join()
    if low.is_set():raise RuntimeError('critical disk reserve reached')
    observe('completed')
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prepared',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--samples',type=int,default=30);parser.add_argument('--warmups',type=int,default=3);parser.add_argument('--profiles',action='store_true')
    args=parser.parse_args();args.prepared=args.prepared.resolve();args.output=args.output.resolve()
    if '.data' not in args.output.parts or args.output.exists() or args.samples<1 or args.warmups<0:parser.error('requires valid fresh .data campaign')
    sys.path.insert(0,str(args.prepared/'harness'))
    import guest_lowering_writeback as bench
    evidence=args.output.parent/(args.output.name+'-guard');evidence.mkdir(exist_ok=False)
    shutil.copy2(__file__,evidence/'driver.py')
    record={'driver_sha256':bench.base.sha(evidence/'driver.py'),'prepared_receipt_sha256':bench.base.sha(args.prepared/'receipt.json'),
            'admission_bytes':ADMISSION_BYTES,'critical_bytes':CRITICAL_BYTES,'interval_seconds':INTERVAL,
            'response':'abort entire campaign on admission failure or critical reserve; owned child cleanup uses existing runner; no sample retry or pooling',
            'timing':'admission before task timer; read-only disk sampler runs outside VM process throughout each task and archival'}
    bench.base.save(evidence/'receipt.json',record)
    bench.run_one=partial(guarded,bench.run_one,evidence/'disk.jsonl')
    try:bench.campaign(args)
    finally:
        path=args.output/'report.json'
        if path.exists():
            report=json.loads(path.read_text());report['disk_guard']={'evidence':str(evidence),**record};bench.base.save(path,report)

if __name__=='__main__':main()
