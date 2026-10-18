#!/usr/bin/env python3
"""Controlled B-FS-DIAG, never a throughput/latency benchmark.

Prepare outside the clock; execute the emitted in.json with semspec bench.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(args):
    batch = args.prepare.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    for name in (
        "guest_writeback_pipeline.py",
        "guest_writeback_pipeline_guest.rs",
        "guest_writeback_pipeline_delay.c",
    ):
        shutil.copy2(HERE / name, batch / "harness" / name)
    shutil.copy2(args.subject, batch / "subject")
    subprocess.run(
        [
            "rustc",
            "--edition=2021",
            "--target",
            "x86_64-unknown-linux-musl",
            "-O",
            str(batch / "harness/guest_writeback_pipeline_guest.rs"),
            "-o",
            str(batch / "payload"),
        ],
        check=True,
    )
    subprocess.run(
        [
            "cc",
            "-shared",
            "-fPIC",
            "-O2",
            str(batch / "harness/guest_writeback_pipeline_delay.c"),
            "-ldl",
            "-o",
            str(batch / "delay.so"),
        ],
        check=True,
    )
    (batch / "plan").write_text("diagnostic\n")
    firmware = {"old": str(args.old_firmware.resolve()), "new": str(args.firmware.resolve())}
    artifacts = {
        str(p): sha(p)
        for p in [
            *sorted((batch / "harness").iterdir()),
            batch / "subject",
            batch / "payload",
            batch / "delay.so",
            *map(Path, firmware.values()),
        ]
    }
    protocol = dict(
        role="B-FS-DIAG",
        delay_ns=1500000000,
        rootfs=str(args.rootfs.resolve()),
        firmware=firmware,
        artifacts=artifacts,
        subject_source_receipt=str(args.subject_receipt.resolve()),
        subject_source_receipt_sha256=sha(args.subject_receipt),
        scope="Controlled injected backing delay; no performance claim; caller-owned prepared rootfs",
    )
    (batch / "protocol.json").write_text(json.dumps(protocol, indent=2) + "\n")
    command = f"$ {sys.executable} {batch}/harness/guest_writeback_pipeline.py --measure {batch}"
    (batch / "in.json").write_text(json.dumps({"controlled-pipeline": command}, indent=2) + "\n")
    print(batch)


def measure(batch):
    batch = batch.resolve()
    protocol = json.loads((batch / "protocol.json").read_text())
    for path, expected in protocol["artifacts"].items():
        if sha(path) != expected:
            raise ValueError(f"changed frozen artifact: {path}")
    results = {}
    for arm, firmware in protocol["firmware"].items():
        trial = batch / arm
        (trial / "stage/upper/curve-work").mkdir(parents=True, exist_ok=False)
        (trial / "stage/work").mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_", "CURVES_"))}
        env.update(
            PVISOR_VM_FS_WRITEBACK="1",
            PVISOR_VM_GUEST_CACHE="1",
            PVISOR_VM_FS_HOST_WRITEBACK="0",
            PVISOR_FS_PROFILE="0",
            PVISOR_STARTUP_PROFILE="0",
            PVISOR_STARTUP_TIMING="0",
            RUST_LOG="off",
            CURVES_PROFILE="0",
            LD_PRELOAD=str(batch / "delay.so"),
            CURVE_PIPELINE_EVENTS=str(trial / "events.jsonl"),
        )
        command = [
            str(batch / "subject"),
            protocol["rootfs"],
            firmware,
            str(batch / "payload"),
            str(batch / "plan"),
            str(trial / "stage"),
            "1",
        ]
        result = subprocess.run(command, env=env, capture_output=True, timeout=90)
        (trial / "stdout.log").write_bytes(result.stdout)
        (trial / "stderr.log").write_bytes(result.stderr)
        receipt = dict(
            command=command,
            returncode=result.returncode,
            environment={
                k: v for k, v in env.items() if k.startswith(("PVISOR_", "CURVE", "LD_PRELOAD"))
            },
        )
        (trial / "invocation.json").write_text(json.dumps(receipt, indent=2) + "\n")
        if result.returncode or b"PIPELINE_BYTES_OK" not in result.stdout:
            raise ValueError(f"{arm}: guest failed, see logs")
        for name, byte in [("pipeline-a", 0x41), ("pipeline-b", 0x42)]:
            if (trial / "stage/upper/curve-work" / name).read_bytes() != bytes([byte]) * (
                1024 * 1024
            ):
                raise ValueError(f"{arm}: invalid backing bytes")
        events = [json.loads(line) for line in (trial / "events.jsonl").read_text().splitlines()]

        def first(file, phase):
            return min(e["ns"] for e in events if e["file"] == file and e["phase"] == phase)

        a_begin, a_end, b_begin, b_end = [
            first(*key) for key in [("a", "begin"), ("a", "end"), ("b", "begin"), ("b", "end")]
        ]
        if a_end - a_begin < protocol["delay_ns"]:
            raise ValueError(f"{arm}: delay did not occur")
        results[arm] = dict(
            a_begin_ns=a_begin,
            a_end_ns=a_end,
            b_begin_ns=b_begin,
            b_end_ns=b_end,
            b_completed_while_a_pending=a_begin < b_end < a_end,
            b_begin_after_a_begin_ms=(b_begin - a_begin) / 1e6,
            verified_bytes=2 * 1024 * 1024,
        )
    (batch / "result.json").write_text(json.dumps(results, indent=2) + "\n")
    print(
        json.dumps(
            {
                f"{arm}_{key}": value
                for arm, result in results.items()
                for key, value in result.items()
            }
        )
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path)
    parser.add_argument("--measure", type=Path)
    for name in ["subject", "subject-receipt", "firmware", "old-firmware", "rootfs"]:
        parser.add_argument("--" + name, type=Path)
    options = parser.parse_args()
    if options.prepare:
        prepare(options)
    elif options.measure:
        measure(options.measure)
    else:
        parser.error("choose --prepare or --measure")
