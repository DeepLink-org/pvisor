"""Shared firecracker ksm contracts for active JSON benchmarks.

Extracted from the retired runners; no historical campaign entry point.
"""

import hashlib
import os
import selectors
import struct
import time
from pathlib import Path

PATTERNS = ("repeated", "random-shared", "random-unique")

SIZE = 64 * 1024 * 1024


PAGE = 4096


SEED = 20261006


MASK = (1 << 64) - 1


WORKER = r"""
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
"""


def page(pattern, instance, index, percent, size=SIZE):
    mutated = index < (size // PAGE) * percent // 100
    identity = instance | (1 << 32) if mutated else instance if pattern == "random-unique" else 0
    if not mutated and pattern == "repeated":
        return bytes(range(256)) * 16
    x = SEED ^ ((identity * 0xD1B54A32D192ED03) & MASK) ^ ((index * 0x9E3779B97F4A7C15) & MASK)
    words = []
    for _ in range(512):
        x = (x * 6364136223846793005 + 1442695040888963407) & MASK
        words.append(x)
    return struct.pack("<512Q", *words)


def expected(pattern, instance, percent):
    h = hashlib.sha256()
    for index in range(SIZE // PAGE):
        h.update(page(pattern, instance, index, percent))
    return h.hexdigest()


class Guest:
    def __init__(self, process, identity, log):
        self.process, self.identity, self.log = process, identity, log
        self.buffer = b""
        self.pending = []
        os.set_blocking(process.stdout.fileno(), False)

    def ack(self, token, percent, wanted, timeout=40):
        deadline = time.monotonic() + timeout
        sel = selectors.DefaultSelector()
        sel.register(self.process.stdout, selectors.EVENT_READ)
        try:
            while time.monotonic() < deadline:
                while self.pending:
                    line = self.pending.pop(0).strip().decode(errors="replace")
                    if not line.startswith("F4K "):
                        continue
                    fields = line.split()
                    if fields != ["F4K", token, str(self.identity), str(percent), wanted]:
                        raise ValueError("unexpected/full-digest mismatch: " + line)
                    return dict(token=token, instance=self.identity, percent=percent, sha256=wanted)
                if self.process.poll() is not None:
                    raise RuntimeError("guest process exited before acknowledgement")
                for _key, _events in sel.select(0.1):
                    b = os.read(self.process.stdout.fileno(), 65536)
                    if not b:
                        raise RuntimeError("serial EOF")
                    self.log.write(b)
                    self.log.flush()
                    self.buffer += b
                    lines = self.buffer.split(b"\n")
                    self.buffer = lines.pop()
                    self.pending.extend(lines)
            raise TimeoutError("guest acknowledgement deadline: " + token)
        finally:
            sel.close()

    def command(self, token, op, percent, wanted):
        self.process.stdin.write(f"{token} {op} {percent}\n".encode())
        self.process.stdin.flush()
        return self.ack(token, percent, wanted)


def group_path():
    line = next(
        x for x in Path("/proc/self/cgroup").read_text().splitlines() if x.startswith("0::")
    )
    return Path("/sys/fs/cgroup") / line[3:].lstrip("/")
