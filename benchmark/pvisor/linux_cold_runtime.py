#!/usr/bin/env python3
"""Benchmark: B-COLD-RUNTIME-ENG (benchmark/README.md#b-cold-runtime-eng), role engineering A/B.
Motivation: verify live Linux instance-local cold compression and kernel fault recovery.
Conclusion sought: full-group memory/CPU and RAM-VMA PSS with real discard/restore,
full payload integrity, mutations, device I/O and owned-process termination.
Design: four fresh sequential 1-VM off/on x repeated/random-unique n=1 cells,
256 MiB/1 vCPU, 64 MiB payload, fixed 20/35-second windows, 4 CPU/2 GiB/swap0.
No snapshot/offload, external service, host configuration changes or density claims.
Build/test and freeze BEFORE invoking this coordinator. Timing includes debug validation.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import sys
import traceback
import uuid
import threading
import time

REPO = Path(__file__).resolve().parents[2]
MEMORY_MAX = 2 * 1024**3
def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def save_json(path, value):
    path = Path(path)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)



def parser():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--example',type=Path,default=REPO/'target/debug/examples/vm_cold_runtime')
    p.add_argument('--build-receipt',type=Path,required=True)
    for name in ('rootfs','firmware','output'): p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--seed',type=int,default=20261006)
    p.add_argument('--wait1',type=int,default=20)
    p.add_argument('--wait2',type=int,default=35)
    return p


def validate_limits(args):
    if not 0 <= args.seed < 2**64: raise ValueError('seed must be u64')
    if not 20 <= args.wait1 <= 30 or not 35 <= args.wait2 <= 45:
        raise ValueError('fixed windows require wait1=20..30 and wait2=35..45')
    if len(os.fsencode(str(args.output.resolve()/'w/t3'))) > 70:
        raise ValueError('canonical worker output exceeds 70 bytes')


def worker_command(config):
    if type(config['cold']) is not bool or config['pattern'] not in ('repeated','random-unique'):
        raise ValueError('invalid condition')
    cmd=[config['example']]
    for name in ('rootfs','firmware','output','pattern','cold','seed','wait1','wait2'):
        value=config[name]
        cmd += ['--'+name,str(value).lower() if isinstance(value,bool) else str(value)]
    return cmd


def unit_command(unit,harness,config):
    return ['systemd-run','--user','--quiet','--wait','--pipe','--collect','--unit='+unit,
            '--property=MemoryAccounting=yes','--property=CPUAccounting=yes',
            '--property=MemoryMax=2147483648','--property=MemorySwapMax=0',
            '--property=CPUQuota=400%','--property=TasksMax=128',
            '--property=RuntimeMaxSec=200','--property=TimeoutStopSec=10',
            '--property=KillMode=control-group',sys.executable,str(harness),'--internal-worker',str(config)]


def parse_metrics(stderr):
    import re
    rows=[]
    for line in stderr.splitlines():
        if 'pvisor-cold-linux ' not in line: continue
        row={key:int(value) for key,value in re.findall(r'(\w+)=(\d+)',line)}
        required=('cold_bytes_current','discarded_bytes_total','restored_bytes_total',
                  'put_rejections_total','pool_encoded_bytes_current','pool_objects_current','pid')
        if any(key not in row for key in required): raise ValueError('incomplete cold metrics')
        if rows:
            if row['pid'] != rows[-1]['pid']: raise ValueError('multiple pager PIDs')
            for key in row:
                if key.endswith('_total') and row[key] < rows[-1].get(key,0):
                    raise ValueError('cumulative counter regression: '+key)
        rows.append(row)
    return rows


def ram_vmas(accounting, binary=None, cold=True):
    import re
    result=[]
    for process in accounting['processes']:
        if binary is not None and process.get('cmdline', {}).get('raw') != str(binary)+'\0':
            continue
        raw=process['smaps'].get('raw','')
        sections=re.split(r'(?m)^(?=[0-9a-f]+-[0-9a-f]+ )',raw)
        for section in sections:
            lines=section.splitlines()
            if not lines: continue
            fields=lines[0].split()
            anonymous = len(fields)==5 and fields[1]=='rw-p' and fields[4]=='0'
            file_backed = (len(fields)>=6 and fields[1]=='rw-s' and fields[4]!='0'
                           and any(str(fd['inode'])==fields[4] and str(fd['target'])==' '.join(fields[5:])
                                   for fd in process.get('large_file_fds', [])))
            if not (anonymous if cold else file_backed): continue
            lo,hi=(int(x,16) for x in fields[0].split('-'))
            # Guest RAM is anonymous and split by the x86 MMIO hole: require a
            # large region and preserve headers/raw counters, not process totals.
            if hi-lo < 128*1024**2: continue
            stats={line.split(':')[0]:int(line.split()[1])*1024 for line in lines[1:]
                   if line.startswith(('Size:','Pss:','Rss:','Anonymous:','KSM:'))}
            if stats.get('Size')!=hi-lo: raise ValueError('smaps size mismatch')
            result.append(dict(pid=process['pid'],header=lines[0],bytes=hi-lo,stats=stats,
                               identity='private anonymous' if cold else 'shared file inode matches runner FD'))
    return result


def interval_union(intervals):
    """Canonical union; overlap is invalid, adjacency may be coalesced."""
    result = []
    for start, end in sorted(intervals):
        if not 0 <= start < end <= 2**64:
            raise ValueError('invalid interval bounds')
        if result and start < result[-1][1]:
            raise ValueError('overlapping intervals')
        if result and start == result[-1][1]:
            result[-1] = (result[-1][0], end)
        else:
            result.append((start, end))
    return result


def parse_cold_layout(stderr):
    import re
    prefixes = {
        'pvisor-cold-linux-layout': {'host_page_bytes', 'block_bytes', 'eligible_mappings',
            'eligible_bytes_total', 'kernel_excluded', 'pss_attribution', 'pid'},
        'pvisor-cold-linux-region': {'host_start', 'length', 'guest_start', 'pid'},
        'pvisor-cold-linux-kernel-excluded': {'host_start', 'length', 'guest_start', 'reason', 'pid'},
    }
    parsed = {prefix: [] for prefix in prefixes}
    for line in stderr.splitlines():
        for prefix, keys in prefixes.items():
            if prefix not in line: continue
            if not line.startswith(prefix+' '): raise ValueError('malformed layout prefix')
            fields = {}
            for word in line[len(prefix)+1:].split():
                if word.count('=') != 1: raise ValueError('malformed layout field')
                key, value = word.split('=')
                if key in fields: raise ValueError('duplicate layout field')
                fields[key] = value
            if set(fields) != keys: raise ValueError('missing/unknown layout fields')
            parsed[prefix].append(fields)
    layouts = parsed['pvisor-cold-linux-layout']
    if len(layouts) != 1: raise ValueError('missing or ambiguous trusted layout')
    layout = layouts[0]
    def decimal(value):
        if not re.fullmatch(r'[0-9]+', value): raise ValueError('malformed decimal layout value')
        number = int(value)
        if number >= 2**64: raise ValueError('layout integer overflow')
        return number
    for key in ('host_page_bytes','block_bytes','eligible_mappings','eligible_bytes_total','pid'):
        layout[key] = decimal(layout[key])
    if (layout['host_page_bytes'] != 4096 or layout['block_bytes'] != 65536 or
            layout['kernel_excluded'] != 'true' or
            layout['pss_attribution'] != 'requires_exact_vma_union' or layout['pid'] == 0):
        raise ValueError('unsupported trusted layout contract')
    def region(fields):
        value = {'pid': decimal(fields['pid']), 'length': decimal(fields['length'])}
        for key in ('host_start','guest_start'):
            if not re.fullmatch(r'0x[0-9a-fA-F]+', fields[key]): raise ValueError('malformed layout address')
            value[key] = int(fields[key], 16)
        if value['pid'] != layout['pid']: raise ValueError('layout PID mismatch')
        for key in ('host_start','guest_start'):
            start, length = value[key], value['length']
            if start % 4096 or length % 4096 or not 0 < length or start+length > 2**64:
                raise ValueError('unaligned or overflowing region')
        return value
    regions = [region(fields) for fields in parsed['pvisor-cold-linux-region']]
    excluded = parsed['pvisor-cold-linux-kernel-excluded']
    if len(excluded) != 1 or excluded[0]['reason'] != 'trusted_raw_firmware':
        raise ValueError('missing or ambiguous trusted kernel exclusion')
    kernel = region(excluded[0])
    if not regions or len(regions) != layout['eligible_mappings'] or sum(r['length'] for r in regions) != layout['eligible_bytes_total']:
        raise ValueError('layout count/byte sum mismatch')
    for key in ('host_start','guest_start'):
        interval_union([(r[key], r[key]+r['length']) for r in regions])
        interval_union([(r[key], r[key]+r['length']) for r in regions]+[(kernel[key],kernel[key]+kernel['length'])])
    return dict(layout=layout, regions=regions, kernel_excluded=kernel,
                eligible_union=interval_union([(r['host_start'],r['host_start']+r['length']) for r in regions]))


def cold_ram_pss(accounting, binary, inventory):
    """Only whole VMAs with an exact interval union may be called isolated RAM."""
    import re
    pid = inventory['layout']['pid']
    processes = [p for p in accounting['processes'] if p['pid'] == pid]
    if len(processes) != 1 or processes[0].get('cmdline',{}).get('raw') != str(binary)+'\0':
        raise ValueError('trusted layout runner identity missing/ambiguous')
    raw = processes[0]['smaps'].get('raw')
    if not isinstance(raw, str): raise ValueError('runner smaps unavailable')
    wanted = inventory['eligible_union']
    selected, all_intervals = [], []
    for section in re.split(r'(?m)^(?=[0-9a-f]+-[0-9a-f]+ )', raw):
        lines = section.splitlines()
        if not lines: continue
        header = re.fullmatch(r'([0-9a-f]+)-([0-9a-f]+) ([r-][w-][x-][ps]) ([0-9a-f]+) ([0-9a-f]+:[0-9a-f]+) ([0-9]+)(?:\s+(.*))?', lines[0].rstrip())
        if not header: raise ValueError('malformed smaps header')
        start, end = int(header[1],16), int(header[2],16)
        all_intervals.append((start,end))
        if not any(start < hi and end > lo for lo,hi in wanted): continue
        if header[3] != 'rw-p' or header[6] != '0' or header[7]:
            raise ValueError('trusted eligible region intersects non-anonymous/private writable VMA')
        stats = {}
        for line in lines[1:]:
            key = line.split(':',1)[0]
            if key not in ('Size','Pss','Rss','Anonymous','KSM'): continue
            match = re.fullmatch(r'\w+:\s+([0-9]+) kB',line)
            if not match or key in stats: raise ValueError('malformed/duplicate smaps accounting')
            stats[key] = int(match[1])*1024
        if stats.get('Size') != end-start or 'Pss' not in stats or stats['Pss'] > end-start:
            raise ValueError('smaps size/PSS mismatch')
        selected.append(dict(pid=pid,start=start,end=end,header=lines[0],bytes=end-start,stats=stats))
    interval_union(all_intervals)
    union = interval_union([(v['start'],v['end']) for v in selected])
    for lo, hi in wanted:
        if not any(start <= lo and end >= hi for start,end in union):
            raise ValueError('trusted eligible union not fully covered by smaps')
    exact = union == wanted
    pss = sum(v['stats']['Pss'] for v in selected)
    return dict(eligible_bytes=inventory['layout']['eligible_bytes_total'],eligible_union=wanted,
                isolated_ram_pss_available=exact,isolated_ram_pss_bytes=pss if exact else None,
                isolated_ram_vmas=selected if exact else [],
                ram_envelope_pss_bytes=None if exact else pss,
                ram_envelope_vmas=[] if exact else selected,
                attribution='exact complete VMA union' if exact else 'unavailable: coalesced VMA envelope contains non-eligible bytes')


def validate_report(raw,config):
    if raw.get('schema')!='pvisor-cold-runtime/v1' or raw.get('correctness')!='passed':
        raise ValueError('worker correctness failed')
    if not raw.get('cleanup',{}).get('all_reaped'): raise ValueError('native reaping missing')
    if raw['conditions']['cold']!=config['cold'] or raw['conditions']['pattern']!=config['pattern']:
        raise ValueError('condition mismatch')
    profile=raw['profile']
    if any(profile.get(key)!=value for key,value in dict(memory_mib=256,cpus=1,payload_bytes=64*1024**2,
                                                         max_live_vms=1,overlaynet_mode='off',network_policy='no-network').items()):
        raise ValueError('VM profile mismatch')
    guests=raw['guests']
    if len(guests)!=1 or guests[0]['result']['state']!='completed' or guests[0]['result']['exit_code']!=0:
        raise ValueError('orderly native guest exit missing')
    if raw['source']['binary_sha256'] != config['example_sha256']:
        raise ValueError('executed binary does not match build receipt')
    phases=raw['phases']
    if [p['name'] for p in phases]!=['ready','cold1','restore1','mutation','cold2','restore2']:
        raise ValueError('missing phase')
    for cold_phase,ready_phase,window in ((1,0,config['wait1']),(4,3,config['wait2'])):
        if phases[cold_phase]['elapsed_ms']-phases[ready_phase]['elapsed_ms']<window*1000:
            raise ValueError('cold window shorter than declared fixed wait')
    stderr=Path(config['output']).joinpath('guest-1.stderr').read_text()+Path(config['stderr']).read_text()
    inventory=parse_cold_layout(stderr) if config['cold'] else None
    observations=[]
    for phase in phases:
        accounting=phase['accounting']; counters=accounting['counters']
        def field(name): return counters[name]['raw'].strip()
        if field('memory.max')!='2147483648' or field('memory.swap.max')!='0':
            raise ValueError('group budget mismatch')
        quota,period=map(int,field('cpu.max').split())
        if period<=0 or quota!=4*period: raise ValueError('CPU quota mismatch')
        events=dict(line.split() for line in field('memory.events').splitlines())
        if any(int(events.get(k,0)) for k in ('oom','oom_kill','max')): raise ValueError('memory budget event')
        if not phase.get('heartbeat_stable'): raise ValueError('missing stable barrier')
        if accounting.get('process_errors'):
            raise ValueError('incomplete cgroup process accounting')
        if config.get('cgroup') and accounting['cgroup'] != config['cgroup']:
            raise ValueError('phase accounting outside owned group')
        if config['cold']:
            attribution=cold_ram_pss(accounting,config['example'],inventory)
            vmas=attribution['isolated_ram_vmas']
        else:
            vmas=ram_vmas(accounting,config['example'],False)
            if len(vmas)!=1 or vmas[0]['bytes']!=256*1024**2:
                raise ValueError('RAM VMA identity ambiguous: '+repr(vmas))
            attribution=dict(isolated_ram_pss_available=True,
                             isolated_ram_pss_bytes=sum(v['stats']['Pss'] for v in vmas),
                             attribution='shared RAM VMA inode matches runner FD')
        stat={line.split()[0]:int(line.split()[1]) for line in field('memory.stat').splitlines()}
        observations.append(dict(phase=phase['name'],elapsed_ms=phase['elapsed_ms'],
            memory_current=int(field('memory.current')),memory_peak=int(field('memory.peak')),
            anon=stat['anon'],file=stat['file'],kernel=stat['kernel'],ram_vmas=vmas,ram_pss=attribution,
            cpu_stat=field('cpu.stat'),memory_pressure=field('memory.pressure'),
            cpu_pressure=field('cpu.pressure'),events=events))
    checks={c['name']:c for c in raw['checks']}
    for name,percent in [('ready',0),('restore1',0),('mutation',100),('restore2',100),('exit',100)]:
        ack=checks[name]['evidence']
        if not checks[name]['passed'] or ack['percent']!=percent or ack['device_io_bytes']!=64*1024**2:
            raise ValueError('guest state/device I/O mismatch')
        expected=next(d['digest'] for d in raw['expected_digests'] if d['percent']==percent)
        if ack['digest']!=expected: raise ValueError('oracle mismatch')
    for previous,current in zip(phases,phases[1:]):
        if current['heartbeat'][0]<=previous['heartbeat'][0]: raise ValueError('heartbeat not advancing')
    stderr=Path(config['output']).joinpath('guest-1.stderr').read_text()+Path(config['stderr']).read_text()
    metrics=parse_metrics(stderr)
    if config['cold'] and any(row['pid'] != inventory['layout']['pid'] for row in metrics):
        raise ValueError('metric PID differs from trusted inventory')
    if config['cold']:
        if not metrics or metrics[-1]['discarded_bytes_total']<=0 or metrics[-1]['restored_bytes_total']<=0:
            raise ValueError('on-mode actual discard AND restore required')
    elif metrics: raise ValueError('off-mode unexpectedly has cold pager')
    events_path=Path(config['stderr']).with_name('stream-events.jsonl')
    events=[json.loads(line) for line in events_path.read_text().splitlines()]
    telemetry=[]
    for event in events:
        parsed=parse_metrics(event['line'])
        if parsed: telemetry.append(dict(time_ns=event['time_ns'],metrics=parsed[0]))
        if event['stream']=='stdout':
            try: marker=json.loads(event['line'])
            except ValueError: continue
            if marker.get('phase'):
                sample=next(row for row in observations if row['phase']==marker['phase'])
                before=[row for row in telemetry if row['time_ns']<=event['time_ns']]
                sample['cold_metric_before_marker']=before[-1] if before else None
                if config['cold'] and (not before or event['time_ns']-before[-1]['time_ns']>5_000_000_000):
                    raise ValueError('missing or stale named-phase cold telemetry')
    if config['cold']:
        by_name={row['phase']:row['cold_metric_before_marker']['metrics'] for row in observations}
        for cycle,previous in ((1,'ready'),(2,'mutation')):
            cold=by_name[f'cold{cycle}']; restored=by_name[f'restore{cycle}']
            if cold['cold_bytes_current']<=0 or restored['restored_bytes_total']<=by_name[previous]['restored_bytes_total']:
                raise ValueError('each cold window requires actual cold RAM and subsequent restoration')
    return dict(phases=observations,cold_metrics=metrics,trusted_cold_layout=inventory,
        metrics_semantics='*_current gauges; *_total cumulative, never summed across rows; pool bytes exclude original RAM/metadata/scratch')


def inventory(root):
    entries = []
    for path in [root] + sorted(root.rglob('*')):
        row = dict(path=str(path.relative_to(root)), mode=path.lstat().st_mode)
        if path.is_symlink():
            row.update(kind='symlink', target=os.readlink(path))
            if path.is_file():
                row['resolved_sha256'] = digest(path)
        elif path.is_file():
            row.update(kind='file', bytes=path.stat().st_size, sha256=digest(path))
        elif path.is_dir():
            row['kind'] = 'directory'
        else:
            raise ValueError('unsupported input type: ' + str(path))
        entries.append(row)
    return entries


def git(*arguments):
    return subprocess.run(['git', '--no-pager', *arguments], cwd=REPO, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30).stdout


def freeze_build_receipt(path, binary, output):
    """Verify retained bytes against a supplied build record, not its source claims."""
    destination = output / 'build'
    destination.mkdir()
    shutil.copy2(path, destination / 'build-receipt.json')
    shutil.copy2(path.parent / 'source-manifest.json', destination / 'source-manifest.json')
    receipt = json.loads((destination / 'build-receipt.json').read_text())
    if not isinstance(receipt, dict):
        raise ValueError('build receipt must be a JSON object')
    if receipt.get('example_sha256') != digest(binary):
        raise ValueError('build receipt binary digest mismatch')
    if receipt.get('source_manifest_sha256') != digest(destination / 'source-manifest.json'):
        raise ValueError('build receipt source manifest digest mismatch')
    identity = receipt.get('source_identity')
    if (not isinstance(identity, dict) or not isinstance(identity.get('head'), str)
            or not identity['head'].strip() or type(identity.get('dirty')) is not bool):
        raise ValueError('build receipt requires source_identity head and boolean dirty')
    command = receipt.get('build_command')
    if not ((isinstance(command, str) and command.strip())
            or (isinstance(command, list) and command
                and all(isinstance(part, str) and part.strip() for part in command))):
        raise ValueError('build receipt requires a nonempty build_command string or argument list')
    return dict(status='verified supplied receipt', receipt=receipt,
                receipt_path=str(destination / 'build-receipt.json'),
                receipt_sha256=digest(destination / 'build-receipt.json'),
                source_manifest_path=str(destination / 'source-manifest.json'),
                source_manifest_sha256=digest(destination / 'source-manifest.json'),
                verification='binary and manifest hashes verified; build/source identity are parent assertions, not independently reproduced')


def freeze(args):
    """Keep current source separate from the optionally supplied build-time record."""
    root = args.output
    (root / 'bin').mkdir()
    binary = root / 'bin/vm_cold_runtime'
    shutil.copy2(args.example, binary)
    build_receipt = getattr(args, 'build_receipt', None)
    build_provenance = (freeze_build_receipt(build_receipt, binary, root) if build_receipt
                        else dict(status='unverified: no build receipt supplied'))
    (root / 'harness').mkdir()
    harness = root / 'harness/linux_cold_runtime.py'
    shutil.copy2(Path(__file__), harness)
    test = Path(__file__).with_name('test_linux_cold_runtime.py')
    if test.exists():
        shutil.copy2(test, root / 'harness/test_linux_cold_runtime.py')
    paths = git('ls-files', '-z', '--cached', '--others', '--exclude-standard').decode().split('\0')
    selected = sorted({p for p in paths if p and (p.startswith(('crates/', 'vendor/', '.cargo/'))
                      or p in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'))})
    source = []
    for relative in selected:
        path = REPO / relative
        if not path.exists() and not path.is_symlink():
            source.append(dict(path=relative, deleted=True))
            continue
        destination = root / 'source' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination, follow_symlinks=False)
        row = dict(path=relative, mode=path.lstat().st_mode)
        if path.is_symlink():
            row['symlink'] = os.readlink(path)
        else:
            row.update(sha256=digest(destination), bytes=destination.stat().st_size)
        source.append(row)
    example_source = 'crates/pvisor/examples/vm_cold_runtime.rs'
    if not any(row['path'] == example_source for row in source):
        raise ValueError('current Rust example missing from source manifest')
    save_json(root / 'source-manifest.json', source)
    if digest(root / 'source-manifest.json') != build_provenance['receipt']['source_manifest_sha256']:
        raise ValueError('current build sources differ from frozen build-time source manifest')
    for name, expected_hash in build_provenance['receipt'].get('harness_sha256', {}).items():
        if digest(root / 'harness' / name) != expected_hash:
            raise ValueError('harness changed since build/test receipt: '+name)
    (root / 'source-dirty.patch').write_bytes(git('diff', '--binary', 'HEAD'))
    status = git('status', '--porcelain=v1', '--untracked-files=all').decode()
    (root / 'source-status.txt').write_text(status)
    save_json(root / 'rootfs-manifest.json', inventory(args.rootfs))
    save_json(root / 'firmware-manifest.json', inventory(args.firmware))
    receipt = dict(example_sha256=digest(binary), example_source_sha256=digest(root / 'source' / example_source),
                   source_manifest_sha256=digest(root / 'source-manifest.json'),
                   source_head=git('rev-parse', 'HEAD').decode().strip(), dirty_source=bool(status),
                   source_status=status, dirty_patch_sha256=digest(root / 'source-dirty.patch'),
                   binary_source_relationship=build_provenance['status'],
                   build_provenance=build_provenance,
                   dependency_scope='repository crates/vendor/Cargo files/.cargo; external registry sources not frozen',
                   rootfs_manifest_sha256=digest(root / 'rootfs-manifest.json'),
                   firmware_manifest_sha256=digest(root / 'firmware-manifest.json'),
                   harness_sha256={p.name: digest(p) for p in sorted((root / 'harness').glob('*.py'))})
    save_json(root / 'source-receipt.json', receipt)
    return binary, harness, receipt


def host_preflight():
    """Read-only host observations; the worker remains the capability/budget gate."""
    observations = dict(cgroup_v2=Path('/sys/fs/cgroup/cgroup.controllers').exists(), devices={})
    for device in ('/dev/kvm', '/dev/fuse', '/dev/userfaultfd'):
        observations['devices'][device] = dict(exists=Path(device).exists(),
                                               read_write_access=os.access(device, os.R_OK | os.W_OK))
    observations['ksm'] = {}
    for name in ('run', 'pages_to_scan', 'sleep_millisecs', 'full_scans'):
        try:
            observations['ksm'][name] = dict(raw=(Path('/sys/kernel/mm/ksm') / name).read_text())
        except OSError as error:
            observations['ksm'][name] = dict(error=str(error))
    observations['policy'] = 'no host writes; unavailable capabilities fail the retained worker sample'
    return observations


def current_group():
    relative = next(line[3:] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::'))
    return str(Path('/sys/fs/cgroup') / relative.lstrip('/'))


def competing_jobs(group):
    """Read-only same-user process/FD guard; inaccessible processes are explicit."""
    jobs, errors = [], []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit(): continue
        try:
            if proc.stat().st_uid != os.getuid(): continue
            membership = proc.joinpath('cgroup').read_text()
            own = str(group).removeprefix('/sys/fs/cgroup')
            if any(line.removeprefix('0::').rstrip() == own or
                   line.removeprefix('0::').rstrip().startswith(own+'/')
                   for line in membership.splitlines()): continue
            args = proc.joinpath('cmdline').read_bytes().split(b'\0')
            if args and Path(os.fsdecode(args[0])).name in ('cargo','rustc','cargo-nextest'):
                jobs.append(dict(pid=int(proc.name),kind='build/test',args=[os.fsdecode(a) for a in args]))
            for fd in proc.joinpath('fd').iterdir():
                try: target = os.readlink(fd)
                except FileNotFoundError: continue
                if target in ('/dev/kvm','anon_inode:kvm-vm','anon_inode:kvm-vcpu'):
                    jobs.append(dict(pid=int(proc.name),kind='other VM',args=[os.fsdecode(a) for a in args]))
                    break
        except (FileNotFoundError, ProcessLookupError): continue
        except OSError as error: errors.append(dict(pid=int(proc.name),error=str(error)))
    return dict(time_ns=time.time_ns(),jobs=jobs,inspection_errors=errors)


def capture_worker(config, command, env):
    """Both the runner and live telemetry/host monitor are in the capped group."""
    stop = threading.Event()
    interference = []
    events_path = Path(config['stderr']).with_name('stream-events.jsonl')
    guard_path = Path(config['stderr']).with_name('host-guard.jsonl')
    lock = threading.Lock()
    with events_path.open('w') as events, guard_path.open('w') as guard:
        first = competing_jobs(config['cgroup'])
        guard.write(json.dumps(first)+'\n'); guard.flush()
        if first['jobs']:
            raise ValueError('competing VM/build before sample: '+repr(first))
        def monitor():
            while not stop.wait(1):
                row = competing_jobs(config['cgroup'])
                guard.write(json.dumps(row)+'\n'); guard.flush()
                if row['jobs']: interference.append(row)
        watcher = threading.Thread(target=monitor,daemon=True); watcher.start()
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        def reader(pipe, path, stream):
            with Path(path).open('wb') as log:
                for line in iter(pipe.readline, b''):
                    log.write(line); log.flush()
                    row = dict(time_ns=time.time_ns(),stream=stream,line=line.decode(errors='replace').rstrip('\n'))
                    with lock: events.write(json.dumps(row)+'\n'); events.flush()
            pipe.close()
        readers = [threading.Thread(target=reader,args=(process.stdout,config['stdout'],'stdout'),daemon=True),
                   threading.Thread(target=reader,args=(process.stderr,config['stderr'],'stderr'),daemon=True)]
        for thread in readers: thread.start()
        try: returncode = process.wait(timeout=185)
        finally:
            if process.poll() is None: process.kill(); process.wait(timeout=5)
            for thread in readers: thread.join(timeout=5)
            stop.set(); watcher.join(timeout=5)
        if any(thread.is_alive() for thread in readers): raise ValueError('worker output pipes not reaped')
        if interference: raise ValueError('host interference during sample; see host-guard.jsonl')
    return returncode


def internal(config):
    result = dict(correctness='failed', command=worker_command(config))
    config = config | dict(cgroup=current_group())
    result['cgroup'] = config['cgroup']
    env = os.environ.copy()
    env.update(PVISOR_FS_PROFILE='0', PVISOR_STARTUP_TIMING='0', PVISOR_EXPERIMENTAL_MEMORY_METRICS='1')
    env.pop('PVISOR_TEST_ALLOW_NO_USERNS', None)
    try:
        if env.get('PVISOR_EXPERIMENTAL_MEMORY_POOL'):
            raise ValueError('external memory pool forbidden')
        group = Path(config['cgroup'])
        if (group / 'pids.max').read_text().strip() != '128':
            raise ValueError('TasksMax=128 not installed')
        returncode = capture_worker(config, result['command'], env)
        result['returncode'] = returncode
        raw = json.loads((Path(config['output']) / 'raw.json').read_text())
        result['worker_error'] = raw.get('error')
        result['native_cleanup'] = raw.get('cleanup')
        result['observations'] = validate_report(raw, config)
        if returncode:
            raise ValueError('worker exited nonzero')
        result['correctness'] = 'passed'
    except Exception as error:
        result.update(error=str(error), traceback=traceback.format_exc())
    finally:
        save_json(config['result'], result)
    return result


def settle_unit(unit, logs):
    """Only touch our UUID-owned unit, and never wait indefinitely for cleanup."""
    try:
        stopped = subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True, timeout=15)
        (logs / 'stop.stdout').write_bytes(stopped.stdout)
        (logs / 'stop.stderr').write_bytes(stopped.stderr)
        shown = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ActiveState',
                                '--property=LoadState'], capture_output=True, timeout=10)
        (logs / 'unit-final.txt').write_bytes(shown.stdout + shown.stderr)
        return b'ActiveState=inactive' in shown.stdout or b'ActiveState=failed' in shown.stdout or b'LoadState=not-found' in shown.stdout
    except (OSError, subprocess.TimeoutExpired) as error:
        (logs / 'cleanup-error.txt').write_text(str(error))
        return False


def run_attempt(record, config, harness, logs):
    cfg = logs / 'config.json'
    save_json(cfg, config)
    cmd = unit_command(record['unit'], harness, cfg)
    record.update(command=cmd, worker_command=worker_command(config), config=str(cfg), status='failed')
    try:
        with (logs / 'service.stdout').open('wb') as stdout, (logs / 'service.stderr').open('wb') as stderr:
            completed = subprocess.run(cmd, stdout=stdout, stderr=stderr, timeout=220)
        record['returncode'] = completed.returncode
        result = json.loads(Path(config['result']).read_text())
        record['result'] = result
        if completed.returncode or result.get('correctness') != 'passed':
            raise ValueError('service/worker failed; inspect retained logs and raw.json')
        # Independently validate the persisted worker bytes, not just wrapper status.
        raw = json.loads((Path(config['output']) / 'raw.json').read_text())
        record['observations'] = validate_report(raw, config | dict(cgroup=result['cgroup']))
        record['status'] = 'successful'
    except Exception as error:
        record.update(error=str(error), deadline=isinstance(error, subprocess.TimeoutExpired))
    finally:
        record['unit_quiescent'] = settle_unit(record['unit'], logs)
        raw_path = Path(config['output']) / 'raw.json'
        if raw_path.exists():
            shutil.copy2(raw_path, logs / 'raw.json')
            record['raw_report'] = str(logs / 'raw.json')
        result_path = Path(config['result'])
        if result_path.exists():
            record['result_path'] = str(result_path)
        if not record['unit_quiescent']:
            record.update(status='failed', error='owned unit cleanup not proven; stop sweep to preserve cap')
    return record



def wait_for_quiet(logs, quiet_seconds=30, deadline_seconds=180):
    """Admission only; no VM yet. Reset the quiet window on any competing job."""
    deadline = time.monotonic()+deadline_seconds
    quiet_since = None
    rows = []
    while True:
        row = competing_jobs(current_group())
        rows.append(row)
        save_json(logs/'prelaunch-wait.json', rows)
        now = time.monotonic()
        if row['jobs']:
            quiet_since = None
        elif quiet_since is None:
            quiet_since = now
        elif now-quiet_since >= quiet_seconds:
            return True
        if now >= deadline:
            return False
        time.sleep(min(1,deadline-now))


def main(argv=None):
    p=parser(); args=p.parse_args(argv)
    try: validate_limits(args)
    except ValueError as error: p.error(str(error))
    for name in ('example','rootfs','firmware','output','build_receipt'):
        setattr(args,name,getattr(args,name).resolve())
    if args.output.exists(): p.error('output must be NEW')
    if not args.example.is_file() or not os.access(args.example,os.X_OK): p.error('build executable before samples')
    if not args.rootfs.is_dir() or not args.firmware.is_dir(): p.error('prepared input directories required')
    if any(source==args.output or source in args.output.parents for source in (args.rootfs,args.firmware)):
        p.error('output cannot modify inputs')
    args.output.mkdir(parents=True); (args.output/'w').mkdir()
    batch=uuid.uuid4().hex
    matrix=[dict(pattern=pattern,cold=cold) for pattern in ('repeated','random-unique') for cold in (False,True)]
    random.Random(args.seed).shuffle(matrix)
    report=dict(schema='pvisor-cold-runtime-coordinator/v1',benchmark_id='B-COLD-RUNTIME-ENG',
        role='engineering A/B',n_per_cell=1,host=dict(kernel=os.uname().release,cpu_affinity=sorted(os.sched_getaffinity(0))),
        budget=dict(cpu_cores=4,memory_max=MEMORY_MAX,swap_max=0,max_live_vms=1),
        windows=[args.wait1,args.wait2],attempts=[],
        boundaries=['debug SDK timing includes hashing/device verification and diagnostic pauses',
                    'host guard covers visible same-user KVM FDs and builds; inaccessible FDs are retained, not proof of absence',
                    'memory.peak includes allocator/codec scratch; phase snapshots do not isolate transient codec peak',
                    'named-phase cold metrics are latest live stderr observations, with a maximum five-second age',
                    'on-mode isolated RAM PSS requires trusted eligible union equal to complete VMA union; otherwise only envelope PSS is available',
                    'each launch follows at least 30 quiet seconds, bounded by a 180-second admission deadline',
                    'n=1 engineering preflight only; no confidence interval, density or tail latency claim'])
    for i,cell in enumerate(matrix):
        report['attempts'].append(dict(id=i,condition=cell,unit=f'pvisor-cold-runtime-{batch}-{i}.service',status='unmeasured'))
    def save(): save_json(args.output/'report.json',report)
    save()
    try:
        report['host_preflight']=host_preflight()
        binary,harness,receipt=freeze(args); report['provenance']=receipt; save()
        for record in report['attempts']:
            logs=args.output/'attempts'/str(record['id']); logs.mkdir(parents=True)
            config=record['condition']|dict(example=str(binary),rootfs=str(args.rootfs),firmware=str(args.firmware),
                output=str(args.output/'w'/f"t{record['id']}"),example_sha256=receipt['example_sha256'],
                seed=args.seed,wait1=args.wait1,wait2=args.wait2,
                stdout=str(logs/'worker.stdout'),stderr=str(logs/'worker.stderr'),result=str(logs/'result.json'))
            record['status']='attempting'; save()
            if not wait_for_quiet(logs):
                record.update(status='failed',error='180-second quiet admission deadline; no VM launched',unit_quiescent=True)
                report['stopped']='competing VM/build prevents 30-second quiet admission; remaining cells unmeasured'
                save()
                break
            run_attempt(record,config,harness,logs); save()
            if not record['unit_quiescent']: break
    except Exception as error: report.update(error=str(error),traceback=traceback.format_exc())
    finally: save()
    return int(bool(report.get('error')) or any(r['status']!='successful' for r in report['attempts']))


if __name__=='__main__':
    if len(sys.argv)==3 and sys.argv[1]=='--internal-worker':
        sys.exit(0 if internal(json.loads(Path(sys.argv[2]).read_text()))['correctness']=='passed' else 1)
    sys.exit(main())
