#!/usr/bin/env python3
"""Historical retired-snapshot harness; use an archived binary.

Real KVM continuation, RAM/FD preservation, private forks and snapshot latency.

Needs a prepared static guest binary built from snapshot_guest.rs. Uses a new
output directory and a private copy of the CLI to pin compatibility identity.
"""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import shutil
import subprocess
import time

parser = argparse.ArgumentParser(
    description="Real KVM full snapshot correctness and latency benchmark"
)
parser.add_argument("--output", type=pathlib.Path, required=True)
parser.add_argument("--binary", type=pathlib.Path, required=True)
parser.add_argument("--guest", type=pathlib.Path, required=True)
parser.add_argument("--samples", type=int, default=10)
parser.add_argument("--warmups", type=int, default=2)
parser.add_argument("--memory", type=int, default=256)
parser.add_argument("--cpus", type=int, default=2)
args = parser.parse_args()
legacy_help = subprocess.run([str(args.binary.resolve()), "snapshot", "--help"], capture_output=True, timeout=10)
if legacy_help.returncode != 0:
    parser.error("this historical harness requires an archived binary exposing the retired snapshot command")
assert args.samples > 0 and args.warmups >= 0
out = args.output.resolve()
out.mkdir()
pvisor = out / "pvisor"
shutil.copy2(args.binary.resolve(), pvisor)
env = dict(os.environ)
reports = []


def command(*args):
    t = time.perf_counter()
    p = subprocess.run(
        [str(pvisor), "snapshot", *map(str, args)],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    print(args[0], p.returncode, p.stdout[-500:], p.stderr[-1000:], flush=True)
    assert p.returncode == 0, (p.stdout, p.stderr)
    return p.stdout, (time.perf_counter() - t) * 1000


procs = []


def start(args, name):
    log = (out / (name + ".log")).open("w")
    t = time.perf_counter()
    p = subprocess.Popen(
        [str(pvisor), "snapshot", *map(str, args)],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    procs.append(p)
    return p, t


def wait_file(path, p, old=None):
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        if p.poll() is not None:
            raise RuntimeError(f"VM exited {p.returncode}: {path}")
        if path.exists():
            s = path.read_text()
            if s and s != old:
                return s
        time.sleep(0.005)
    raise TimeoutError(str(path))


try:
    for trial in range(args.warmups + args.samples):
        for storage in ["raw", "compressed"]:
            base = out / ("snapshot-" + storage + f"-trial{trial}")
            base.mkdir()
            source = base / "input"
            source.mkdir()
            (source / "dev").mkdir()
            shutil.copy2(args.guest.resolve(), source / "init.krun")
            store = base / "store"
            p, t = start(
                [
                    "--store",
                    store,
                    "run",
                    "--name",
                    "source",
                    "--rootfs",
                    source,
                    "--native-init",
                    "--ram-storage",
                    storage,
                    "--memory",
                    str(args.memory),
                    "--cpus",
                    str(args.cpus),
                ],
                f"snapshot-{storage}-trial{trial}-source",
            )
            root = store / "runs/source/rootfs"
            wait_file(root / "ready", p)
            ready_ms = (time.perf_counter() - t) * 1000
            time.sleep(0.25)
            (root / "request.tmp").write_text("before")
            (root / "request.tmp").replace(root / "request")
            ack = wait_file(root / "ack", p)
            saved_n = int(ack.split()[1])
            print("guest ack", ack, flush=True)
            stdout, save_ms = command("--store", store, "save", "source")
            p.wait(timeout=15)
            ids = command("--store", store, "list")[0].splitlines()
            print("ids", ids, flush=True)
            objects = list((store / "objects").iterdir())
            assert len(objects) == 1
            sid = objects[0].name
            # Remove both original trees; snapshot must be fully independent.
            shutil.rmtree(source)
            shutil.rmtree(root)
            manifest = json.loads((objects[0] / "manifest.json").read_text())
            print("snapshot saved", storage, sid, save_ms, flush=True)
            alloc = sum(f.stat().st_blocks * 512 for f in objects[0].rglob("*") if f.is_file())
            restored = []
            for index in range(2):
                name = f"fork{index}"
                rp, t = start(
                    ["--store", store, "restore", sid, "--name", name],
                    f"snapshot-{storage}-trial{trial}-{name}",
                )
                rr = store / "runs" / name / "rootfs"
                old = (objects[0] / "rootfs/heartbeat").read_text()
                wait_file(rr / "heartbeat", rp, old)
                restore_ms = (time.perf_counter() - t) * 1000
                (rr / "request.tmp").write_text(name)
                (rr / "request.tmp").replace(rr / "request")
                ack = wait_file(rr / "ack", rp, (objects[0] / "rootfs/ack").read_text())
                assert ack.split()[0] == name and int(ack.split()[1]) > saved_n
                restored.append(
                    dict(
                        name=name,
                        restore_heartbeat_ms=restore_ms,
                        read_check_ms=float(ack.split()[2]),
                        ack=ack,
                    )
                )
                print("restored", restored[-1], flush=True)
            # Mutate fork0, verify fork1 and immutable snapshot remain unchanged.
            (store / "runs/fork0/rootfs/open-file").write_text("private")
            assert (store / "runs/fork1/rootfs/open-file").read_text() == "abcdef"
            assert (objects[0] / "rootfs/open-file").read_text() == "abcdef"
            for rp in procs:
                if rp.poll() is None:
                    os.killpg(rp.pid, 15)
                    rp.wait(timeout=10)
            result = dict(
                correctness="passed",
                storage=storage,
                ready_ms=ready_ms,
                save_ms=save_ms,
                snapshot_id=sid,
                snapshot_ram_sha256=manifest["ram_sha256"],
                snapshot_allocated_bytes=alloc,
                restores=restored,
                trial=trial,
                warmup=trial < args.warmups,
            )
            (base / "result.json").write_text(json.dumps(result, indent=2))
            reports.append(result)
except Exception:
    print("FAILED; see logs", flush=True)
    raise
finally:
    for p in procs:
        if p.poll() is None:
            os.killpg(p.pid, 15)
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, 9)
                p.wait()
report = dict(
    schema="pvisor-kvm-snapshot-benchmark/v1",
    recorded_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
    os=platform.platform(),
    cpus=args.cpus,
    memory_mib=args.memory,
    samples=args.samples,
    warmups=args.warmups,
    guest_data_bytes=64 * 1024 * 1024,
    binary_sha256=hashlib.sha256(pvisor.read_bytes()).hexdigest(),
    guest_sha256=hashlib.sha256(args.guest.read_bytes()).hexdigest(),
    rows=reports,
)
(out / "raw.json").write_text(json.dumps(report, indent=2))
