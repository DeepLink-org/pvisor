#!/usr/bin/env python3
"""Firecracker guest-RAM diagnostic, not a product density claim.

Benchmark: B-MEMORY-SCALE (benchmark/README.md#b-memory-scale), engineering A/B.
Motivation: distinguish actual KSM merging from advice and process footprint.
Conclusion sought: four-VM RAM-VMA/cgroup observations with full integrity checks.
Design: one preflight per repeated/random-shared/random-unique x advice off/on;
256 MiB/1 vCPU each, 4 CPU/2 GiB/swap0 group, scanner read-only, 20s window.

The page generator and mutation IDs match vm_memory_scale.rs exactly. Unlike
pVisor this boots independent fresh VMs, without snapshot baseline inode sharing,
SDK offload, cold reclaim or heartbeat. A static PID-1 serial worker checks EVERY
byte against the generator and emits SHA256 for independent host verification.
Only an installed documented Firecracker merging option is admissible: this
version's absent API is UNSUPPORTED, never replaced by ptrace/LD_PRELOAD advice.
One sample is diagnostic only, not a formal five-round or statistical comparison.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import selectors
import shutil
import socket
import struct
import subprocess
import sys
import time
import traceback
import uuid

SIZE = 64 * 1024 * 1024
PAGE = 4096
SEED = 20261006
MASK = (1 << 64) - 1
PATTERNS = ('repeated', 'random-shared', 'random-unique')

# Self-contained SHA256 avoids guest libraries and dynamic linker requirements.
WORKER = r'''
use std::io::{self, BufRead, Write};
const N: usize = 64*1024*1024;
const K: [u32;64] = [
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
fn block(h: &mut [u32;8], b: &[u8]) {
 let mut w=[0u32;64];
 for i in 0..16 {w[i]=u32::from_be_bytes(b[i*4..i*4+4].try_into().unwrap());}
 for i in 16..64 {let x=w[i-15];let y=w[i-2];w[i]=w[i-16].wrapping_add(x.rotate_right(7)^x.rotate_right(18)^(x>>3)).wrapping_add(w[i-7]).wrapping_add(y.rotate_right(17)^y.rotate_right(19)^(y>>10));}
 let [mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut z]=*h;
 for i in 0..64 {let t=z.wrapping_add(e.rotate_right(6)^e.rotate_right(11)^e.rotate_right(25)).wrapping_add((e&f)^(!e&g)).wrapping_add(K[i]).wrapping_add(w[i]);let u=(a.rotate_right(2)^a.rotate_right(13)^a.rotate_right(22)).wrapping_add((a&b)^(a&c)^(b&c));z=g;g=f;f=e;e=d.wrapping_add(t);d=c;c=b;b=a;a=t.wrapping_add(u);}
 for (i,x) in [a,b,c,d,e,f,g,z].iter().enumerate(){h[i]=h[i].wrapping_add(*x);}
}
fn sha(data: &[u8])->String {
 let mut h=[0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
 let mut chunks=data.chunks_exact(64);for b in &mut chunks {block(&mut h,b);}
 let rem=chunks.remainder();let mut end=vec![0u8; if rem.len()<56 {64} else {128}];end[..rem.len()].copy_from_slice(rem);end[rem.len()]=128;let n=end.len();end[n-8..].copy_from_slice(&((data.len() as u64)*8).to_be_bytes());for b in end.chunks(64){block(&mut h,b);}
 h.iter().map(|x|format!("{:08x}",x)).collect()
}
fn page(pattern: &str, id:u64, p:usize, percent:usize)->[u8;4096] {
 let mutated=p<(N/4096)*percent/100;
 let instance=if mutated {id|(1<<32)} else if pattern=="random-unique" {id} else {0};
 let mut out=[0u8;4096];
 if !mutated && pattern=="repeated" {for i in 0..4096 {out[i]=(i%256) as u8;}}
 else {let mut x=20261006u64^instance.wrapping_mul(0xd1b54a32d192ed03)^(p as u64).wrapping_mul(0x9e3779b97f4a7c15);for word in out.chunks_exact_mut(8){x=x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);word.copy_from_slice(&x.to_le_bytes());}}
 out
}
fn verify(data:&[u8],pattern:&str,id:u64,percent:usize,token:&str) {
 for p in 0..N/4096 {assert_eq!(&data[p*4096..(p+1)*4096],&page(pattern,id,p,percent),"full-byte mismatch page {}",p);}
 println!("F4K {} {} {} {}",token,id,percent,sha(data));io::stdout().flush().unwrap();
}
fn main(){
 let a:Vec<String>=std::env::args().collect();
 if a.get(1).map(String::as_str)==Some("--sha-test") {println!("{}",sha(a[2].as_bytes()));return;}
 let native=a.get(1).map(String::as_str)==Some("--native");let offset=if native {2} else {1};
 let pattern=&a[offset];assert!(["repeated","random-shared","random-unique"].contains(&pattern.as_str()));let id:u64=a[offset+1].parse().unwrap();
 let mut data=vec![0u8;N];for p in 0..N/4096 {data[p*4096..(p+1)*4096].copy_from_slice(&page(pattern,id,p,0));}
 let mut percent=0;verify(&data,pattern,id,percent,"ready");
 for line in io::stdin().lock().lines(){let l=line.unwrap();let v:Vec<&str>=l.split_whitespace().collect();if v.len()!=3 {continue;}let token=v[0];let op=v[1];let requested:usize=v[2].parse().unwrap();
 if op=="mutate" {assert!([25,100].contains(&requested));assert!(requested>=percent);percent=requested;for p in 0..(N/4096)*percent/100 {data[p*4096..(p+1)*4096].copy_from_slice(&page(pattern,id,p,percent));}}
 else {assert!(["read","exit"].contains(&op));assert_eq!(requested,percent);}
 verify(&data,pattern,id,percent,token);if op=="exit" {if native{return;}loop{std::thread::sleep(std::time::Duration::from_secs(60));}}
 }
 panic!("serial EOF");
}
'''


def save(path, value):
    path = Path(path)
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(value, indent=2) + '\n')
    tmp.replace(path)


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for b in iter(lambda: f.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def page(pattern, instance, index, percent, size=SIZE):
    mutated = index < (size // PAGE) * percent // 100
    identity = instance | (1 << 32) if mutated else instance if pattern == 'random-unique' else 0
    if not mutated and pattern == 'repeated':
        return bytes(range(256)) * 16
    x = SEED ^ ((identity * 0xd1b54a32d192ed03) & MASK) ^ ((index * 0x9e3779b97f4a7c15) & MASK)
    words = []
    for _ in range(512):
        x = (x * 6364136223846793005 + 1442695040888963407) & MASK
        words.append(x)
    return struct.pack('<512Q', *words)


def expected(pattern, instance, percent):
    h = hashlib.sha256()
    for index in range(SIZE // PAGE):
        h.update(page(pattern, instance, index, percent))
    return h.hexdigest()


def vm_processes():
    found = []
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():
            continue
        try:
            comm = (p / 'comm').read_text().strip()
            cmd = (p / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
            # KVM descriptors also catch SDK runners with unrelated process names.
            kvm = any('kvm-vm' in os.readlink(f) for f in (p / 'fd').iterdir())
            if comm == 'firecracker' or comm.startswith('qemu-system') or comm.startswith('vm_memory') or kvm:
                found.append(dict(pid=int(p.name), comm=comm, command=cmd, kvm_vm=kvm))
        except FileNotFoundError:
            continue
        except PermissionError:
            # Other users' named VMMs still appear above; opaque fd tables are recorded separately.
            if comm == 'firecracker' or comm.startswith('qemu-system') or comm.startswith('vm_memory'):
                found.append(dict(pid=int(p.name), comm=comm, command=cmd, fd_access='denied'))
    return found


def ksm():
    result = {}
    for path in sorted(Path('/sys/kernel/mm/ksm').iterdir()):
        if path.is_file():
            try:
                result[path.name] = path.read_text()
            except OSError as e:
                result[path.name] = {'error': str(e)}
    return result


def scanner_guard():
    value = ksm()
    if any(value.get(k, '').strip() != v for k, v in
           [('run', '1'), ('pages_to_scan', '100'), ('sleep_millisecs', '20')]):
        raise ValueError('administrator KSM configuration differs from run=1/pages=100/sleep=20')
    return value


def ram_vmas(text):
    """Require exactly 256MiB anonymous rw RAM, not total process RSS.

    Small x86 guests are one contiguous anonymous mapping. This rejects ambiguous
    candidates and huge-page/split layouts rather than counting all process VMAs.
    Configured 256MiB RAM, successful boot and exact VMA length identify the
    candidate together; retained complete smaps allows audit. No claim is made
    that process RSS or arbitrary anonymous mappings are guest RAM.
    """
    blocks = re.split(r'(?=^[0-9a-f]+-[0-9a-f]+\s)', text, flags=re.M)
    candidates = []
    for block in blocks:
        lines = block.splitlines()
        if not lines:
            continue
        fields = lines[0].split()
        if len(fields) != 5 or fields[1] != 'rw-p' or fields[4] != '0':
            continue
        lo, hi = (int(x, 16) for x in fields[0].split('-'))
        if hi - lo != 256 * 1024 * 1024:
            continue
        values = {}
        for line in lines[1:]:
            if ':' in line:
                key, val = line.split(':', 1)
                values[key] = val.strip()
        if values.get('Size') != '262144 kB':
            raise ValueError('RAM mapping size disagreement')
        candidates.append(dict(header=lines[0], fields=values, start=lo, end=hi))
    if len(candidates) != 1:
        raise ValueError(f'expected exactly one anonymous 256MiB RAM VMA; found {len(candidates)}')
    return candidates


def api(sock, method, endpoint, body=None):
    data = json.dumps(body).encode() if body is not None else b''
    request = (f'{method} {endpoint} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {len(data)}\r\nConnection: close\r\n\r\n').encode() + data
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(5)
        s.connect(str(sock))
        s.sendall(request)
        chunks = []
        while True:
            b = s.recv(65536)
            if not b:
                break
            chunks.append(b)
            received = b''.join(chunks)
            if b'\r\n\r\n' in received:
                headers, payload = received.split(b'\r\n\r\n', 1)
                match = re.search(rb'(?im)^content-length:\s*(\d+)', headers)
                status_line = headers.split(b'\r\n', 1)[0]
                if (match and len(payload) >= int(match[1])) or b' 204 ' in status_line:
                    break
    raw = b''.join(chunks).decode()
    status = int(raw.split(' ', 2)[1])
    return dict(method=method, endpoint=endpoint, request=body, status=status, raw=raw)


def wait_socket(sock, process):
    deadline = time.monotonic() + 10
    while not sock.exists():
        if process.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError('Firecracker API failed to become ready')
        time.sleep(.02)


class Guest:
    def __init__(self, process, identity, log):
        self.process, self.identity, self.log = process, identity, log
        self.buffer = b''
        self.pending = []
        os.set_blocking(process.stdout.fileno(), False)

    def ack(self, token, percent, wanted, timeout=40):
        deadline = time.monotonic() + timeout
        sel = selectors.DefaultSelector()
        sel.register(self.process.stdout, selectors.EVENT_READ)
        try:
            while time.monotonic() < deadline:
                while self.pending:
                    line = self.pending.pop(0).strip().decode(errors='replace')
                    if not line.startswith('F4K '):
                        continue
                    fields = line.split()
                    if fields != ['F4K', token, str(self.identity), str(percent), wanted]:
                        raise ValueError('unexpected/full-digest mismatch: ' + line)
                    return dict(token=token, instance=self.identity, percent=percent, sha256=wanted)
                if self.process.poll() is not None:
                    raise RuntimeError('guest process exited before acknowledgement')
                for _key, _events in sel.select(.1):
                    b = os.read(self.process.stdout.fileno(), 65536)
                    if not b:
                        raise RuntimeError('serial EOF')
                    self.log.write(b)
                    self.log.flush()
                    self.buffer += b
                    lines = self.buffer.split(b'\n')
                    self.buffer = lines.pop()
                    self.pending.extend(lines)
            raise TimeoutError('guest acknowledgement deadline: ' + token)
        finally:
            sel.close()

    def command(self, token, op, percent, wanted):
        self.process.stdin.write(f'{token} {op} {percent}\n'.encode())
        self.process.stdin.flush()
        return self.ack(token, percent, wanted)


def group_path():
    line = next(x for x in Path('/proc/self/cgroup').read_text().splitlines() if x.startswith('0::'))
    return Path('/sys/fs/cgroup') / line[3:].lstrip('/')


def snapshot(root, label, guests, group):
    folder = root / label
    folder.mkdir()
    result = dict(monotonic=time.monotonic(), ksm=scanner_guard(), cgroup={}, processes=[])
    for name in ('memory.current', 'memory.peak', 'memory.stat', 'memory.events', 'memory.events.local',
                 'memory.swap.current', 'memory.max', 'memory.swap.max', 'cpu.max', 'cpu.stat',
                 'memory.pressure', 'cpu.pressure', 'io.stat', 'pids.current', 'cgroup.procs'):
        try:
            result['cgroup'][name] = (group / name).read_text()
        except OSError as e:
            result['cgroup'][name] = {'error': str(e)}
    for guest in guests:
        p = Path('/proc') / str(guest.process.pid)
        if guest.process.poll() is not None:
            raise RuntimeError('VM not live during snapshot')
        smaps = (p / 'smaps').read_text()
        (folder / f'{guest.identity}.smaps').write_text(smaps)
        stat = (p / 'stat').read_text()
        result['processes'].append(dict(id=guest.identity, pid=guest.process.pid,
            ram=ram_vmas(smaps), stat=stat, cgroup=(p / 'cgroup').read_text(),
            status=(p / 'status').read_text(), smaps_rollup=(p / 'smaps_rollup').read_text()))
    save(folder / 'snapshot.json', result)
    return result


def internal(config):
    root = Path(config['root'])
    result = dict(status='failed', config=config, checks=[], phases=[], cleanup=[])
    guests, handles = [], []
    try:
        before = vm_processes()
        result['other_vms_before'] = before
        if before:
            raise RuntimeError('other benchmark/KVM VMs exist; cap cannot be assured')
        group = group_path()
        result['group'] = str(group)
        controls = {name: (group / name).read_text().strip() for name in ('memory.max', 'memory.swap.max', 'cpu.max')}
        result['controls'] = controls
        quota, period = controls['cpu.max'].split()
        if controls['memory.max'] != '2147483648' or controls['memory.swap.max'] != '0' or quota == 'max' or int(quota) != 4 * int(period):
            raise ValueError('group budget not installed')
        result['ksm_before'] = scanner_guard()
        for identity in range(1, 5):
            if len(vm_processes()) != len(guests):
                raise RuntimeError('unowned VM appeared; abort admission')
            cfg = dict(config['vm_config'])
            cfg['boot-source'] = dict(cfg['boot-source'], boot_args=cfg['boot-source']['boot_args'] + f' -- {config["pattern"]} {identity}')
            path = root / f'vm{identity}.json'
            save(path, cfg)
            command = ['/usr/bin/firecracker', '--enable-pci', '--no-api', '--config-file', str(path)]
            err = (root / f'vm{identity}.stderr').open('wb')
            log = (root / f'vm{identity}.serial').open('wb')
            handles.extend([err, log])
            process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err)
            guest = Guest(process, identity, log)
            guests.append(guest)
            result.setdefault('commands', []).append(command)
            result['checks'].append(guest.ack('ready', 0, config['hashes'][f'{identity}:0']))
        states = {i: 0 for i in range(1, 5)}
        def check_all(token):
            for g in guests:
                percent = states[g.identity]
                result['checks'].append(g.command(token, 'read', percent, config['hashes'][f'{g.identity}:{percent}']))
        result['phases'].append(snapshot(root, 'ready', guests, group))
        start = time.monotonic()
        time.sleep(20)
        result['observation_seconds'] = time.monotonic() - start
        check_all('after-window')
        result['phases'].append(snapshot(root, 'observed', guests, group))
        if any('mg' in p['ram'][0]['fields'].get('VmFlags', '').split() for p in result['phases'][-1]['processes']):
            raise ValueError('off condition unexpectedly has mergeable RAM')
        for percent in (25, 100):
            for guest in guests:
                result['checks'].append(guest.command(f'mutate{percent}-{guest.identity}', 'mutate', percent, config['hashes'][f'{guest.identity}:{percent}']))
                states[guest.identity] = percent
                check_all(f'peer{percent}-{guest.identity}')
            result['phases'].append(snapshot(root, f'mutated{percent}', guests, group))
        while guests:
            exiting = guests.pop(0)
            result['checks'].append(exiting.command(f'exit-{exiting.identity}', 'exit', 100, config['hashes'][f'{exiting.identity}:100']))
            exiting.process.kill()
            result['cleanup'].append(dict(id=exiting.identity, pid=exiting.process.pid, returncode=exiting.process.wait(timeout=10), reaped=True))
            check_all(f'after-exit-{exiting.identity}')
        result['phases'].append(snapshot(root, 'all-reaped', [], group))
        for phase in result['phases']:
            events = phase['cgroup']['memory.events']
            if any(int(line.split()[1]) for line in events.splitlines() if line.split()[0] in ('oom', 'oom_kill', 'oom_group_kill')):
                raise ValueError('cgroup OOM; reject sample')
        result['status'] = 'passed'
    except Exception as e:
        result.update(error=str(e), traceback=traceback.format_exc())
    finally:
        for guest in guests:
            try:
                if guest.process.poll() is None:
                    guest.process.kill()
                rc = guest.process.wait(timeout=10)
                result['cleanup'].append(dict(id=guest.identity, pid=guest.process.pid, returncode=rc, reaped=True))
            except Exception as e:
                result['cleanup'].append(dict(id=guest.identity, error=str(e), reaped=False))
        for f in handles:
            f.close()
        result['ksm_after'] = ksm()
        save(root / 'result.json', result)
    return 0 if result['status'] == 'passed' else 1


def run_logged(command, root, name, timeout=120):
    completed = subprocess.run(command, capture_output=True, timeout=timeout)
    (root / (name + '.stdout')).write_bytes(completed.stdout)
    (root / (name + '.stderr')).write_bytes(completed.stderr)
    save(root / (name + '.command.json'), dict(command=command, returncode=completed.returncode))
    if completed.returncode:
        raise RuntimeError(f'{name} failed ({completed.returncode})')
    return completed.stdout


def prepare(root, assets):
    frozen = root / 'frozen'
    frozen.mkdir()
    for name in ('firecracker_ksm.py', 'test_firecracker_ksm.py', 'memory_scale.py', 'reference_baselines.py'):
        shutil.copy2(Path(__file__).parent / name, frozen / name)
    shutil.copy2(Path(__file__).resolve().parents[2] / 'crates/pvisor/examples/vm_memory_scale.rs', frozen / 'vm_memory_scale.rs')
    shutil.copy2('/usr/share/doc/firecracker/firecracker.yaml', frozen / 'firecracker.yaml')
    shutil.copy2('/usr/bin/firecracker', frozen / 'firecracker')
    source = frozen / 'worker.rs'
    source.write_text(WORKER)
    binary = frozen / 'worker'
    command = [shutil.which('rustc'), '--edition=2021', '--target', 'x86_64-unknown-linux-musl', '-C', 'opt-level=3', '-o', str(binary), str(source)]
    run_logged(command, root, 'build')
    elf = run_logged(['readelf', '-l', str(binary)], root, 'worker-elf').decode()
    if 'INTERP' in elf:
        raise ValueError('worker is not static')
    for value in ('', 'abc', 'a' * 1000):
        actual = subprocess.check_output([str(binary), '--sha-test', value], text=True).strip()
        if actual != hashlib.sha256(value.encode()).hexdigest():
            raise ValueError('SHA256 implementation selftest failed')
    tree = root / 'guest-root'
    tree.mkdir()
    (tree / 'dev').mkdir()
    shutil.copy2(binary, tree / 'worker')
    disk = root / 'guest.ext4'
    with disk.open('wb') as f:
        f.truncate(96 * 1024 * 1024)
    run_logged(['mkfs.ext4', '-q', '-F', '-d', str(tree), str(disk)], root, 'mkfs')
    config = (assets / 'kernel.config').read_text()
    if 'CONFIG_EXT4_FS=y' not in config or 'CONFIG_SERIAL_8250_CONSOLE=y' not in config or 'CONFIG_VIRTIO_PCI=y' not in config:
        raise ValueError('prepared kernel lacks ext4/serial/PCI support')
    receipt = dict(command=command, compiler=subprocess.check_output(['rustc', '--version'], text=True),
        host=subprocess.check_output(['uname', '-a'], text=True), affinity=sorted(os.sched_getaffinity(0)),
        hashes={str(p): digest(p) for p in list(frozen.iterdir()) + [disk, assets / 'vmlinux', assets / 'kernel.config']},
        argv=sys.argv, version=subprocess.check_output(['/usr/bin/firecracker', '--version'], text=True),
        differences=__doc__)
    save(root / 'receipt.json', receipt)
    return dict(**{'boot-source': dict(kernel_image_path=str(assets / 'vmlinux'), boot_args='console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/worker quiet'),
        'drives': [dict(drive_id='rootfs', path_on_host=str(disk), is_root_device=True, is_read_only=True)],
        'machine-config': dict(vcpu_count=1, mem_size_mib=256)})


def probe(root):
    """Live negative capability probe; no guest started, no RAM allocated."""
    sock = root / 'api.sock'
    with (root / 'api.stdout').open('wb') as out, (root / 'api.stderr').open('wb') as err:
        process = subprocess.Popen(['/usr/bin/firecracker', '--api-sock', str(sock)], stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        responses = []
        try:
            wait_socket(sock, process)
            responses.append(api(sock, 'GET', '/machine-config'))
            # Candidate spelling is an explicit NEGATIVE probe, never a claimed supported option.
            responses.append(api(sock, 'PUT', '/machine-config', dict(vcpu_count=1, mem_size_mib=256, memory_mergeable=True)))
        finally:
            process.kill()
            rc = process.wait(timeout=10)
            save(root / 'api-probe.json', dict(command=['/usr/bin/firecracker', '--api-sock', str(sock)],
                responses=responses, pid=process.pid, reaped=True, returncode=rc))
    if len(responses) != 2 or responses[1]['status'] != 400 or 'unknown field' not in responses[1]['raw']:
        raise ValueError('unexpected capability probe response; manual API audit required')
    return responses


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--assets', type=Path)
    parser.add_argument('--internal', type=Path)
    args = parser.parse_args()
    if args.internal:
        return internal(json.loads(args.internal.read_text()))
    if args.output is None or args.assets is None:
        parser.error('--output and --assets required')
    root = args.output.resolve()
    if not root.is_dir() or any(root.iterdir()):
        parser.error('--output must be an existing empty fresh directory')
    report = dict(benchmark='B-MEMORY-SCALE', role='engineering A/B diagnostic extension', samples=1,
                  formal=False, conditions=[], status='failed', started=time.time())
    try:
        report['vm_precheck'] = vm_processes()
        if report['vm_precheck']:
            raise RuntimeError('existing VMs; refuse to launch')
        report['ksm_before'] = scanner_guard()
        vm_config = prepare(root, args.assets.resolve())
        report['api_probe'] = probe(root)
        report['merging_support'] = 'unsupported: installed schema/CLI has no KSM option; API rejects unknown field memory_mergeable'
        # Independent host SHA256 predictions are computed before measurement.
        hashes = {}
        for pattern in PATTERNS:
            hashes[pattern] = {}
            for identity in range(1, 5):
                for percent in (0, 25, 100):
                    key = f'{identity}:{percent}'
                    if percent == 0 and pattern != 'random-unique' and identity > 1:
                        value = hashes[pattern]['1:0']
                    else:
                        value = expected(pattern, identity, percent)
                    hashes[pattern][key] = value
        save(root / 'expected-sha256.json', hashes)
        native_tests = []
        for pattern in PATTERNS:
            command = [str(root / 'frozen/worker'), '--native', pattern, '1']
            commands = b'n25 mutate 25\nn100 mutate 100\nbye exit 100\n'
            completed = subprocess.run(command, input=commands, capture_output=True, timeout=40)
            (root / (pattern + '.native.stdout')).write_bytes(completed.stdout)
            (root / (pattern + '.native.stderr')).write_bytes(completed.stderr)
            wanted = [f'F4K {token} 1 {percent} {hashes[pattern]["1:" + str(percent)]}'
                      for token, percent in [('ready', 0), ('n25', 25), ('n100', 100), ('bye', 100)]]
            if completed.returncode or completed.stdout.decode().splitlines() != wanted:
                raise ValueError('native static-worker full-byte/SHA verification failed: ' + pattern)
            native_tests.append(dict(command=command, stdin=commands.decode(), returncode=completed.returncode, status='passed'))
        save(root / 'native-worker-tests.json', native_tests)
        conditions = [(p, advice) for p in PATTERNS for advice in (False, True)]
        random.Random(SEED).shuffle(conditions)
        for index, (pattern, advice) in enumerate(conditions):
            folder = root / f'{index}-{pattern}-{"on" if advice else "off"}'
            folder.mkdir()
            record = dict(pattern=pattern, merging_requested=advice, status='unsupported' if advice else 'failed', folder=str(folder))
            report['conditions'].append(record)
            if advice:
                record['error'] = report['merging_support']
                record['vm_count'] = 0
                save(folder / 'result.json', record)
                continue
            if vm_processes():
                raise RuntimeError('VM process appeared between conditions')
            config = dict(root=str(folder), pattern=pattern, hashes=hashes[pattern], vm_config=vm_config)
            save(folder / 'config.json', config)
            unit = 'f4k-' + uuid.uuid4().hex
            command = ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect', '--unit=' + unit,
                '--property=MemoryAccounting=yes', '--property=CPUAccounting=yes', '--property=MemoryMax=2147483648',
                '--property=MemorySwapMax=0', '--property=CPUQuota=400%', '--property=TasksMax=128',
                '--property=RuntimeMaxSec=180', '--property=TimeoutStopSec=10', '--property=KillMode=control-group',
                sys.executable, str(root / 'frozen/firecracker_ksm.py'), '--internal', str(folder / 'config.json')]
            record['command'] = command
            try:
                run_logged(command, folder, 'service', timeout=200)
                record['result'] = json.loads((folder / 'result.json').read_text())
                record['status'] = record['result']['status']
            except Exception as e:
                record['error'] = str(e)
                if (folder / 'result.json').exists():
                    record['result'] = json.loads((folder / 'result.json').read_text())
            finally:
                completed = subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True, timeout=15)
                (folder / 'stop.txt').write_bytes(completed.stdout + completed.stderr)
                final = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ActiveState', '--property=LoadState'], capture_output=True, timeout=10)
                (folder / 'unit-final.txt').write_bytes(final.stdout + final.stderr)
                record['unit_quiescent'] = any(x in final.stdout for x in (b'ActiveState=inactive', b'ActiveState=failed', b'LoadState=not-found'))
                if not record['unit_quiescent'] or vm_processes():
                    raise RuntimeError('cleanup not proven; stop to preserve cap')
            save(root / 'report.json', report)
        report['status'] = 'incomplete-unsupported' if any(r['status'] == 'unsupported' for r in report['conditions']) else 'passed'
        if any(r['status'] == 'failed' for r in report['conditions']):
            report['status'] = 'failed'
    except Exception as e:
        report.update(error=str(e), traceback=traceback.format_exc())
    finally:
        report['ksm_after'] = ksm()
        report['vm_postcheck'] = vm_processes()
        report['ended'] = time.time()
        save(root / 'report.json', report)
    print(json.dumps({k: report.get(k) for k in ('status', 'error', 'merging_support')}, indent=2))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
