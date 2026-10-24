#!/usr/bin/env python3
"""B-FS-DIAG: retain the complete failing plan and locate guest ENOMEM.

This generated diagnostic payload emits guest memory/kernel logs only; none of
its timings are performance samples. Prepare outside the measurement process.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


DUMP = r"""
fn memory_dump(label: &str) {
    eprintln!("MEMORY_DIAGNOSTIC {label}");
    for name in ["/proc/meminfo", "/proc/buddyinfo", "/proc/vmstat"] {
        match std::fs::read_to_string(name) {
            Ok(value) => eprintln!("MEMORY_FILE {name}\n{value}"),
            Err(error) => eprintln!("MEMORY_FILE_ERROR {name}: {error}"),
        }
    }
    unsafe extern "C" { fn klogctl(action: i32, buf: *mut u8, len: i32) -> i32; }
    let mut buffer = [0u8; 65536];
    let count = unsafe { klogctl(3, buffer.as_mut_ptr(), buffer.len() as i32) };
    if count >= 0 {
        eprintln!("KERNEL_LOG\n{}", String::from_utf8_lossy(&buffer[..count as usize]));
    } else {
        eprintln!("KERNEL_LOG_ERROR {}", std::io::Error::last_os_error());
    }
}
"""


def prepare(args):
    source = args.source_batch.resolve()
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    shutil.copy2(__file__, batch / "harness/guest_writeback_memory.py")
    original = (source / "harness/curves_guest.rs").read_text()
    marker = "self.file.as_ref().unwrap().sync_all()?;"
    if original.count(marker) != 1:
        raise ValueError("diagnostic stream-sync insertion point changed")
    inserted = """if self.cell.id == "pressure-128m" { memory_dump("BEFORE_128M_SYNC"); }
                    self.file.as_ref().unwrap().sync_all().map_err(|error| {
                        memory_dump("SYNC_ERROR"); error
                    })?;"""
    payload = original.replace(marker, inserted) + DUMP
    (batch / "harness/original_guest.rs").write_text(original)
    (batch / "harness/memory_guest.rs").write_text(payload)
    subprocess.run(
        [
            "rustc",
            "--edition=2024",
            "-O",
            "--target",
            "x86_64-unknown-linux-musl",
            str(batch / "harness/memory_guest.rs"),
            "-o",
            str(batch / "payload"),
        ],
        check=True,
    )
    for old, new in [
        ("bin/subject", "subject"),
        ("plan-001.tsv", "plan"),
        ("bin/libkrunfw.so", "libkrunfw.so"),
    ]:
        shutil.copy2(source / old, batch / new)
    protocol = json.loads((source / "protocol.json").read_text())
    files = [p for p in batch.rglob("*") if p.is_file()]
    save(
        batch / "protocol.json",
        dict(
            role="B-FS-DIAG",
            source_batch=str(source),
            source_protocol_sha256=sha(source / "protocol.json"),
            rootfs=protocol["rootfs"],
            max_trials=5,
            rounds=3,
            environment=protocol["conditions"]["guest-wb"]["env"],
            artifacts={str(p.relative_to(batch)): sha(p) for p in files},
            scope="same complete plan001 and budgets; memory/log instrumentation; no performance inference",
        ),
    )
    save(
        batch / "in.json",
        {
            "guest-writeback-memory": "$ "
            + sys.executable
            + " "
            + str(batch / "harness/guest_writeback_memory.py")
            + " --measure "
            + str(batch)
        },
    )
    print(batch)


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    if (batch / "records.json").exists():
        raise ValueError("refusing to reuse a measured diagnostic batch")
    for name, expected in protocol["artifacts"].items():
        if sha(batch / name) != expected:
            raise ValueError("frozen diagnostic artifact changed: " + name)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_", "CURVES_"))}
    env.update(protocol["environment"])
    env.update(CURVES_PROFILE="0", PVISOR_FS_PROFILE="0", RUST_LOG="off")
    relative = next(
        line[3:]
        for line in Path("/proc/self/cgroup").read_text().splitlines()
        if line.startswith("0::")
    )
    group = Path("/sys/fs/cgroup") / relative.lstrip("/")
    budget = {
        name: (group / name).read_text().strip()
        for name in ("cpu.max", "memory.max", "memory.swap.max")
    }
    budget["affinity"] = sorted(os.sched_getaffinity(0))
    save(batch / "actual-budget.json", budget)
    if (
        budget["cpu.max"] != "200000 100000"
        or budget["memory.max"] != "2147483648"
        or budget["memory.swap.max"] != "0"
        or budget["affinity"] != [0, 1]
    ):
        raise ValueError("controlled diagnostic CPU/memory budget differs")
    records = []
    for index in range(protocol["max_trials"]):
        trial = batch / f"trial-{index:03}"
        (trial / "stage/upper/curve-work").mkdir(parents=True, exist_ok=False)
        (trial / "stage/work").mkdir()
        command = [
            str(batch / "subject"),
            protocol["rootfs"],
            str(batch / "libkrunfw.so"),
            str(batch / "payload"),
            str(batch / "plan"),
            str(trial / "stage"),
            "3",
        ]
        timed_out = False
        try:
            result = subprocess.run(command, env=env, capture_output=True, timeout=180)
        except subprocess.TimeoutExpired as error:
            timed_out = True
            result = subprocess.CompletedProcess(
                command, 124, stdout=error.stdout or b"", stderr=error.stderr or b""
            )
        (trial / "stdout.log").write_bytes(result.stdout)
        (trial / "stderr.log").write_bytes(result.stderr)
        records.append(
            dict(
                index=index,
                command=command,
                returncode=result.returncode,
                timed_out=timed_out,
                stdout_sha256=sha(trial / "stdout.log"),
                stderr_sha256=sha(trial / "stderr.log"),
            )
        )
        save(batch / "records.json", records)
        if result.returncode:
            break
    print(
        json.dumps(dict(trials=len(records), failures=sum(r["returncode"] != 0 for r in records)))
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path)
    parser.add_argument("--source-batch", type=Path)
    parser.add_argument("--measure", type=Path)
    parser.add_argument("--campaign", type=Path)
    args = parser.parse_args()
    if args.prepare:
        prepare(args)
    elif args.campaign:
        campaign(args.campaign.resolve())
    elif args.measure:
        batch = args.measure.resolve()
        command = [
            "systemd-run",
            "--user",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            "-p",
            "CPUQuota=200%",
            "-p",
            "AllowedCPUs=0,1",
            "-p",
            "MemoryMax=2G",
            "-p",
            "MemorySwapMax=0",
            "-p",
            "LimitNOFILE=65536",
            "taskset",
            "-c",
            "0,1",
            sys.executable,
            str(batch / "harness/guest_writeback_memory.py"),
            "--campaign",
            str(batch),
        ]
        save(batch / "campaign-command.json", command)
        result = subprocess.run(command, capture_output=True, timeout=1800)
        (batch / "campaign.stdout.log").write_bytes(result.stdout)
        (batch / "campaign.stderr.log").write_bytes(result.stderr)
        if result.returncode:
            raise SystemExit(result.returncode)
        print(result.stdout.decode().strip())
    else:
        parser.error("choose prepare, measure, or campaign")
