#!/usr/bin/env python3
"""Paired firmware boot benchmark using the existing signed guest_init runner.

Benchmark: B-STARTUP (benchmark/README.md#b-startup), role diagnostic.
Motivation: locate which boot stage dominates VM ready time.
Conclusion sought: per-stage boot time for two firmware builds; feeds design
analysis only, never the user startup table.
Design: paired runs on one host, identical guest payload and runner.

Ready is process spawn to the payload marker received by the host. Guest kernel
init is the dmesg timestamp of Run /init.krun, with early-clock limitations.
Completion includes the payload's sleep, dmesg collection, sync and shutdown.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import re
import subprocess
import threading
import time

from startup import percentile


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    for name in ('runner', 'rootfs', 'before', 'after', 'output'):
        ap.add_argument('--' + name, type=Path, required=True)
    ap.add_argument('--iterations', type=int, default=50)
    ap.add_argument('--warmup', type=int, default=5)
    args = ap.parse_args()
    assert args.iterations > 0 and args.warmup >= 0
    assert platform.system() == 'Darwin' and platform.machine() == 'arm64'
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    workspace = out / 'workspace'
    workspace.mkdir(exist_ok=True)
    configs = {}
    for mode, net in [('direct', 'off'), ('workspace', 'network')]:
        config = out / f'{mode}-{net}.config'
        config.write_text(json.dumps(dict(
            argv=['/bin/sh', '-c', '/payload; dmesg'], env={},
            cwd='/workspace' if mode == 'workspace' else '/',
            workspace='/workspace' if mode == 'workspace' else None,
            network={'address': [192, 0, 2, 2], 'gateway': [192, 0, 2, 1]}
            if net == 'network' else None)))
        configs[(mode, net)] = config
    rows = []
    rng = random.Random(20261003)
    load_before = os.getloadavg()
    for round_id in range(-args.warmup, args.iterations):
        modes = list(configs)
        rng.shuffle(modes)
        for mode, net in modes:
            cases = ['before', 'after']
            rng.shuffle(cases)
            for case in cases:
                firmware = getattr(args, case).resolve()
                env = dict(os.environ, DYLD_LIBRARY_PATH=str(firmware.parent))
                env.pop('DYLD_PRINT_LIBRARIES', None)
                started = time.perf_counter_ns()
                proc = subprocess.Popen([
                    str(args.runner.resolve()), 'rust', mode, net,
                    str(args.rootfs.resolve()), 'embedded', str(configs[(mode, net)]),
                    str(workspace)], env=env, stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                timing = {}
                stdout = []
                stderr = []

                def read_stdout():
                    for line in proc.stdout:
                        stdout.append(line)
                        if line.strip() == b'PVISOR_GUEST_READY':
                            timing['ready'] = time.perf_counter_ns()

                reader = threading.Thread(target=read_stdout)
                errors = threading.Thread(target=lambda: stderr.append(proc.stderr.read()))
                reader.start()
                errors.start()
                try:
                    code = proc.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                    raise
                finished = time.perf_counter_ns()
                reader.join()
                errors.join()
                text = b''.join(stdout).decode(errors='replace')
                label = f'{round_id}-{mode}-{net}-{case}'
                (out / f'{label}.log').write_text(text + b''.join(stderr).decode(errors='replace'))
                match = re.search(r'\[\s*([\d.]+)\] Run /init\.krun as init process', text)
                assert code == 0 and 'ready' in timing and match, (label, code, text)
                row = dict(round=round_id, case=case, mode=f'{mode}-{net}',
                           ready_ms=(timing['ready']-started)/1e6,
                           completion_ms=(finished-started)/1e6,
                           guest_kernel_init_ms=float(match[1])*1000)
                if round_id >= 0:
                    rows.append(row)
                    with (out / 'samples.jsonl').open('a') as f:
                        f.write(json.dumps(row) + '\n')
        print(f'round {round_id+1}/{args.iterations}', flush=True)
    summary = {}
    for mode in ('direct-off', 'workspace-network'):
        summary[mode] = {}
        for case in ('before', 'after'):
            samples = [r for r in rows if r['mode'] == mode and r['case'] == case]
            summary[mode][case] = {metric: dict(p50=percentile([r[metric] for r in samples], 50),
                                               p95=percentile([r[metric] for r in samples], 95))
                                  for metric in ('ready_ms', 'completion_ms', 'guest_kernel_init_ms')}
        deltas = {}
        for metric in ('ready_ms', 'guest_kernel_init_ms'):
            paired = []
            for i in range(args.iterations):
                pair = {r['case']: r[metric] for r in rows if r['mode'] == mode and r['round'] == i}
                paired.append(pair['before']-pair['after'])
            deltas[metric] = dict(paired_saved_ms_p50=percentile(paired, 50),
                                  positive_pairs=sum(v > 0 for v in paired), pairs=len(paired))
        summary[mode]['paired'] = deltas
    files = [args.runner, args.before, args.after, args.rootfs/'payload', Path(__file__)]
    result = dict(platform=platform.platform(), cpus=1, memory_mib=128,
                  iterations=args.iterations, warmup=args.warmup, seed=20261003,
                  load_before=load_before, load_after=os.getloadavg(), summary=summary,
                  source_sha256={str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in files},
                  scope='existing signed low-level runner; embedded Rust init; warm host caches; not pVisor CLI latency',
                  samples=rows)
    (out/'results.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == '__main__':
    main()
