#!/usr/bin/env python3
"""Bounded real-KVM image/compiler soak; preserve every command and failure."""

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

IMAGES = {
    "alpine": "alpine:3.20",
    "busybox": "busybox:1.37",
    "ubuntu": "ubuntu:24.04",
    "debian": "debian:bookworm-slim",
    "python": "python:3.12-slim",
    "node": "node:22-alpine",
    "gcc": "gcc:14",
    "go": "golang:1.25-alpine",
    "rust": "rust:1.90-slim",
}


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


class CommandFailure(RuntimeError):
    """A failed command with its raw evidence retained for ownership decisions."""

    def __init__(self, directory, code, expected, timed_out):
        self.directory = directory
        self.code = code
        super().__init__(f"{directory.name}: exit={code}, expected={expected}, timeout={timed_out}")


class Soak:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        self.store = self.output / "store"
        self.store.mkdir(mode=0o700)
        self.env = dict(os.environ, PVISOR_DOCKER_HOME=str(self.store))
        self.started = time.monotonic()
        self.deadline = self.started + args.seconds
        self.lock = threading.Lock()
        self.serial = 0
        self.failures = []
        self.counts = {}
        self.images = {}
        self.stop = threading.Event()
        save(
            self.output / "manifest.json",
            {
                "started_unix": time.time(),
                "seconds": args.seconds,
                "binary": str(args.binary),
                "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                "images": IMAGES,
                "seed": args.seed,
            },
        )

    def call(
        self, lane, command, *, timeout=120, data=None, env=None, expected=0, missing_ok=False
    ):
        with self.lock:
            serial = self.serial
            self.serial += 1
        directory = self.output / f"cmd-{serial:06d}-{lane}"
        directory.mkdir()
        save(directory / "command.json", {"argv": list(map(str, command)), "timeout": timeout})
        started = time.monotonic()
        with (directory / "stdout").open("wb") as out, (directory / "stderr").open("wb") as err:
            child = subprocess.Popen(
                list(map(str, command)),
                env=env or self.env,
                stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                stdout=out,
                stderr=err,
                start_new_session=True,
            )
            timed_out = False
            try:
                child.communicate(data, timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=15)
        save(
            directory / "exit.json",
            {
                "code": child.returncode,
                "timed_out": timed_out,
                "seconds": time.monotonic() - started,
            },
        )
        missing = (
            missing_ok
            and child.returncode == 1
            and b"no such container:" in (directory / "stderr").read_bytes().lower()
        )
        if timed_out or (child.returncode != expected and not missing):
            raise CommandFailure(directory, child.returncode, expected, timed_out)
        return (directory / "stdout").read_bytes()

    def pv(self, lane, *arguments, **kwargs):
        return self.call(lane, [self.args.binary, *arguments], **kwargs)

    def record(self, lane, error=None):
        with self.lock:
            self.counts[lane] = self.counts.get(lane, 0) + 1
            if error is not None:
                self.failures.append({"lane": lane, "error": repr(error), "unix": time.time()})
            state = {
                "elapsed": time.monotonic() - self.started,
                "commands": self.serial,
                "counts": dict(self.counts),
                "failures": list(self.failures),
            }
            save(self.output / "status.json", state)
            print(json.dumps(state), flush=True)

    def alive(self):
        return time.monotonic() < self.deadline and not self.stop.is_set()

    def run_image(self, lane, key, command, expected, *, memory="512m", cpus="1", mounts=()):
        name = f"soak-{lane}-{uuid.uuid4().hex}"
        failed = True
        try:
            out = self.pv(
                lane,
                "run",
                "--rm",
                "--name",
                name,
                "--cpus",
                cpus,
                "--memory",
                memory,
                *mounts,
                self.images[key],
                *command,
                timeout=240,
            )
            if out != expected:
                raise AssertionError(f"{key}: output {out[:500]!r}, expected {expected[:500]!r}")
            failed = False
        except CommandFailure as error:
            conflict = f"pvisor: container name already in use: {name}".encode()
            if error.code == 125 and (error.directory / "stderr").read_bytes().strip() == conflict:
                # Creation never acquired this name. Never remove that instance.
                failed = False
            raise
        finally:
            # A failed launch or timeout may retain the exact owned container.
            if failed:
                self.pv(lane, "rm", "-f", name, missing_ok=True)

    def diversity(self):
        probes = {
            "alpine": (["sh", "-c", "test $(id -u) = 0; printf image-ok"], b"image-ok"),
            "busybox": (
                ["sh", "-c", "printf abc | sha256sum | cut -d' ' -f1"],
                hashlib.sha256(b"abc").hexdigest().encode() + b"\n",
            ),
            "ubuntu": (["sh", "-c", "test -r /etc/os-release; printf image-ok"], b"image-ok"),
            "debian": (["sh", "-c", "test -r /etc/debian_version; printf image-ok"], b"image-ok"),
            "python": (
                [
                    "python",
                    "-c",
                    "import hashlib,sqlite3,zlib; d=bytes(range(256))*4096; assert zlib.decompress(zlib.compress(d))==d; c=sqlite3.connect(':memory:'); c.execute('create table t(x)'); c.executemany('insert into t values(?)',[(i,) for i in range(10000)]); assert c.execute('select sum(x) from t').fetchone()[0]==49995000; print(hashlib.sha256(d).hexdigest())",
                ],
                hashlib.sha256(bytes(range(256)) * 4096).hexdigest().encode() + b"\n",
            ),
            "node": (
                [
                    "node",
                    "-e",
                    "const c=require('crypto'),z=require('zlib');let b=Buffer.alloc(1048576);for(let i=0;i<b.length;i++)b[i]=i%256;if(!z.inflateSync(z.deflateSync(b)).equals(b))process.exit(7);console.log(c.createHash('sha256').update(b).digest('hex'))",
                ],
                hashlib.sha256(bytes(range(256)) * 4096).hexdigest().encode() + b"\n",
            ),
        }
        iteration = 0
        while self.alive():
            key = list(probes)[iteration % len(probes)]
            iteration += 1
            if key not in self.images:
                continue
            try:
                self.run_image("image", key, *probes[key])
                self.record("image-" + key)
            except Exception as error:
                self.record("image-" + key, error)
                return  # Retain first failure in this lane for investigation.

    def compile(self):
        context = self.output / "compile"
        context.mkdir()
        (context / "main.c").write_text(
            '#include <stdio.h>\n#include <stdint.h>\nint main(void){uint64_t s=0;for(uint64_t i=0;i<10000000;i++)s+=i;printf("%llu\\n",(unsigned long long)s);return s!=49999995000000ULL;}\n'
        )
        (context / "main.cpp").write_text(
            '#include <algorithm>\n#include <numeric>\n#include <vector>\n#include <iostream>\nint main(){std::vector<long long> v(1000000);std::iota(v.begin(),v.end(),0);std::reverse(v.begin(),v.end());std::sort(v.begin(),v.end());auto s=std::accumulate(v.begin(),v.end(),0LL);std::cout<<s<<"\\n";return s!=499999500000LL;}\n'
        )
        (context / "main.rs").write_text(
            '#[test] fn sum(){assert_eq!((0..1000000u64).sum::<u64>(),499999500000);} fn main(){println!("{}",(0..1000000u64).sum::<u64>());}\n'
        )
        (context / "go.mod").write_text("module soak\ngo 1.24\n")
        (context / "main_test.go").write_text(
            'package soak\nimport("testing";"crypto/sha256";"fmt")\nfunc TestWorkers(t *testing.T){ch:=make(chan [32]byte,8);for j:=0;j<8;j++{go func(){var h [32]byte;for i:=0;i<50000;i++{h=sha256.Sum256([]byte(fmt.Sprint(i)))};ch<-h}()};want:=sha256.Sum256([]byte("49999"));for j:=0;j<8;j++{if <-ch!=want{t.Fatal("digest")}}}\n'
        )
        commands = {
            "gcc": 'set -eu; cd /work; rm -f c1 c2 c3 c4 cpp1 cpp2 cpp3 cpp4; pids=""; for n in 1 2 3 4; do gcc -O2 -Wall -Werror main.c -o c$n & pids="$pids $!"; g++ -O2 -Wall -Werror main.cpp -o cpp$n & pids="$pids $!"; done; for pid in $pids; do wait "$pid"; done; for n in 1 2 3 4; do ./c$n; ./cpp$n; done',
            "rust": 'set -eu; cd /work; rm -f rs1 rs2 rs3 rs4 rstest; pids=""; for n in 1 2 3 4; do rustc -O main.rs -o rs$n & pids="$pids $!"; done; for pid in $pids; do wait "$pid"; done; rustc --test main.rs -o rstest; ./rstest --quiet >/dev/null; for n in 1 2 3 4; do ./rs$n; done',
            "go": "set -eu; cd /work; GOTOOLCHAIN=local GOPROXY=off GOMAXPROCS=2 go test -count=4 -parallel=4 -timeout=180s >/dev/null; printf go-ok",
        }
        expected = {
            "gcc": b"49999995000000\n499999500000\n" * 4,
            "rust": b"499999500000\n" * 4,
            "go": b"go-ok",
        }
        while self.alive():
            for key in commands:
                if key not in self.images or not self.alive():
                    continue
                try:
                    self.run_image(
                        "compile",
                        key,
                        ["sh", "-c", commands[key]],
                        expected[key],
                        memory="1536m",
                        cpus="2",
                        mounts=("-v", f"{context}:/work"),
                    )
                    self.record("compile-" + key)
                except Exception as error:
                    self.record("compile-" + key, error)
                    return

    def applications(self):
        iteration = 0
        while self.alive():
            dest = self.output / f"app-{iteration:04d}"
            try:
                self.call(
                    "app",
                    [
                        sys.executable,
                        "tests/podman/application_chaos.py",
                        "--binary",
                        self.args.binary,
                        "--http-binary",
                        self.args.http_binary,
                        "--image",
                        self.images["alpine"],
                        "--output",
                        dest,
                        "--seed",
                        str(self.args.seed + iteration),
                        "--rounds",
                        "120",
                        "--seconds",
                        "1200",
                        "--product-slice",
                        self.args.product_slice,
                    ],
                    timeout=1300,
                )
                self.call(
                    "audit", [sys.executable, "tests/podman/audit_application_chaos.py", dest]
                )
                self.record("app-120-rounds")
            except Exception as error:
                self.record("application", error)
                return
            iteration += 1

    def contracts(self):
        iteration = 0
        env = dict(
            self.env, PVISOR_BINARY=str(self.args.binary), PVISOR_TEST_IMAGE=self.images["alpine"]
        )
        files = sorted(Path("tests/podman/e2e").glob("*_test.go"))
        while self.alive():
            try:
                self.call(
                    "contracts",
                    ["go", "test", "-json", "-count=1", "-timeout=20m", *files],
                    timeout=1250,
                    env=env,
                )
                self.record("go-contract-suite")
            except Exception as error:
                self.record("go-contract-suite", error)
                return
            iteration += 1

    def main(self):
        if not callable(getattr(os, "pidfd_open", None)) or not callable(
            getattr(signal, "pidfd_send_signal", None)
        ):
            raise RuntimeError("soak requires Python with pidfd support; use /usr/bin/python3")
        for key, tag in IMAGES.items():
            reference = "mirror.gcr.io/library/" + tag
            try:
                self.pv("pull", "pull", reference, timeout=600)
                # Read the pull receipt's resolved digest rather than letting a tag drift.
                receipt = sorted(self.output.glob("cmd-*-pull"))[-1]
                match = re.search(
                    rb"Digest: (sha256:[0-9a-f]{64})", (receipt / "stdout").read_bytes()
                )
                if match is None:
                    raise AssertionError("pull did not report immutable digest")
                self.images[key] = reference.split(":")[0] + "@" + match[1].decode()
                self.record("pull-" + key)
            except Exception as error:
                self.record("pull-" + key, error)
        save(self.output / "images.json", self.images)
        if "alpine" not in self.images:
            raise RuntimeError("Alpine fixture unavailable")
        # Pin references are explicitly pulled into the catalog before launch.
        for key, reference in self.images.items():
            self.pv("pin", "pull", reference, timeout=600)
        self.started = time.monotonic()
        self.deadline = self.started + self.args.seconds
        save(self.output / "load-start.json", {"unix": time.time(), "seconds": self.args.seconds})
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            futures = [
                pool.submit(f)
                for f in (self.diversity, self.compile, self.applications, self.contracts)
            ]
            while self.alive() and not all(f.done() for f in futures):
                self.record("heartbeat")
                self.stop.wait(45)
            for future in futures:
                future.result()
        self.record("finished")
        save(
            self.output / "outcome.json",
            {
                "passed": not self.failures,
                "elapsed": time.monotonic() - self.started,
                "failures": self.failures,
                "counts": self.counts,
                "commands": self.serial,
            },
        )
        if self.failures:
            raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=lambda s: Path(s).resolve(strict=True), required=True)
    parser.add_argument("--http-binary", type=lambda s: Path(s).resolve(strict=True), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=21600)
    parser.add_argument("--seed", type=int, default=11025)
    parser.add_argument("--product-slice", required=True)
    arguments = parser.parse_args()
    if not 1 <= arguments.seconds <= 86400:
        parser.error("seconds must be in 1..86400")
    Soak(arguments).main()
