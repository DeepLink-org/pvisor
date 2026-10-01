#!/usr/bin/env python3
"""Compare C and Rust init under an identical signed libkrun/HVF host runner.

Requires a prepared Alpine aarch64 rootfs, the previous static C init binary,
and macOS libkrunfw. Outputs raw interleaved timings plus summary statistics.
Ready time ends when the same guest payload's marker reaches host stdout.
Completion additionally includes guest sync/reboot and VMM teardown, but neither
metric includes pVisor CLI orchestration or Run Bundle persistence.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import statistics
import subprocess
import threading
import time
from pathlib import Path

from startup import percentile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-init", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("target/guest-init-benchmark"))
    parser.add_argument("--iterations", type=int, default=50)
    parser.add_argument("--warmup", type=int, default=5)
    args = parser.parse_args()
    assert platform.system() == "Darwin" and platform.machine() == "arm64", (
        "Apple Silicon HVF runner"
    )
    assert args.iterations > 0 and args.warmup >= 0
    root = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    rootfs = args.rootfs.resolve()
    workspace = output / "workspace"
    workspace.mkdir(exist_ok=True)
    (rootfs / "workspace").mkdir(exist_ok=True)
    payload = output / "payload.rs"
    payload.write_text(
        'use std::io::Write; fn main() { std::io::stdout().write_all(b"PVISOR_GUEST_READY\\n").unwrap(); std::io::stdout().flush().unwrap(); std::thread::sleep(std::time::Duration::from_millis(20)); }\n'
    )
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    subprocess.run(
        [
            "rustc",
            "--edition",
            "2024",
            "--target",
            "aarch64-unknown-linux-musl",
            "-O",
            "-C",
            f"linker={sysroot}/lib/rustlib/aarch64-apple-darwin/bin/rust-lld",
            str(payload),
            "-o",
            str(rootfs / "payload"),
        ],
        check=True,
    )
    build = subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "--lib",
            "--locked",
            "-p",
            "pvisor",
            "--message-format=json",
        ],
        cwd=root,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    libraries = {}
    for line in build.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact":
            for filename in item["filenames"]:
                if filename.endswith(".rlib"):
                    libraries[item["target"]["name"]] = filename
    rust_init = root / "target/pvisor-guest/aarch64-unknown-linux-musl/release/pvisor-guest"
    runner = output / "runner"
    compile_env = dict(
        os.environ,
        PVISOR_BENCH_C_INIT=str(args.c_init.resolve()),
        PVISOR_BENCH_RUST_INIT=str(rust_init),
    )
    command = [
        "rustc",
        "--edition",
        "2024",
        "-C",
        "opt-level=z",
        "-C",
        "codegen-units=8",
        "-C",
        "panic=abort",
        "-C",
        "lto=thin",
        str(Path(__file__).with_suffix(".rs")),
        "-L",
        f"dependency={Path(libraries['krun']).parent}",
        "-o",
        str(runner),
    ]
    for name in ["krun", "pvisor_overlaynet", "pvisor_core"]:
        command += ["--extern", f"{name}={libraries[name]}"]
    subprocess.run(command, env=compile_env, check=True)
    subprocess.run(
        [
            "codesign",
            "--force",
            "--sign",
            "-",
            "--entitlements",
            str(root / "crates/pvisor/macos-hypervisor.entitlements"),
            str(runner),
        ],
        check=True,
    )
    environment = dict(os.environ, DYLD_LIBRARY_PATH=str(args.firmware.resolve().parent))
    cases = [
        (init, mode, net)
        for mode, net in [("direct", "off"), ("workspace", "off"), ("workspace", "network")]
        for init in ["c", "rust"]
    ]
    configs = {}
    for init, mode, net in cases:
        path = output / f"{init}-{mode}-{net}.config"
        if init == "rust":
            path.write_text(
                json.dumps(
                    dict(
                        argv=["/payload"],
                        env={},
                        cwd="/workspace" if mode == "workspace" else "/",
                        workspace="/workspace" if mode == "workspace" else None,
                        network={"address": [192, 0, 2, 2], "gateway": [192, 0, 2, 1]}
                        if net == "network"
                        else None,
                    )
                )
            )
        else:
            # Same shell/tty/cleanup/cd/env chain as the former pVisor helper.
            # Alpine mount is a BusyBox applet; execute it at /bin/mount.
            script = "#!/bin/sh\nset -eu\n"
            if mode == "workspace":
                script += "/bin/mount -t virtiofs pvisor-workspace /workspace\n"
            script += 'if [ -c /dev/hvc0 ]; then\n  if [ -t 0 ]; then exec 0</dev/hvc0; fi\n  if [ -t 1 ]; then exec 1>/dev/hvc0; fi\n  if [ -t 2 ]; then exec 2>/dev/hvc0; fi\nfi\nrm -f /init.krun "$0"\n'
            script += f"cd {'/workspace' if mode == 'workspace' else '/'}\nexec /usr/bin/env -i /payload\n"
            path.write_text(script)
        configs[(init, mode, net)] = path
    initial_load = os.getloadavg()
    rows = []
    rng = random.Random(20261001)
    for round_number in range(-args.warmup, args.iterations):
        ordered = list(cases)
        rng.shuffle(ordered)
        for init, mode, net in ordered:
            if init == "c" and mode != "direct":
                helper = rootfs / ".pvisor-exec-bench.sh"
                helper.write_bytes(configs[(init, mode, net)].read_bytes())
                helper.chmod(0o755)
            before = time.perf_counter_ns()
            process = subprocess.Popen(
                [
                    str(runner),
                    init,
                    mode,
                    net,
                    str(rootfs),
                    "embedded",
                    str(configs[(init, mode, net)]),
                    str(workspace),
                ],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            ready = None
            lines = []

            def read_output():
                nonlocal ready
                for line in process.stdout:
                    lines.append(line)
                    if line.strip() == b"PVISOR_GUEST_READY":
                        ready = time.perf_counter_ns()

            reader = threading.Thread(target=read_output)
            reader.start()
            result = []

            def wait_process():
                code = process.wait()
                result.append((code, time.perf_counter_ns()))

            waiter = threading.Thread(target=wait_process)
            waiter.start()
            waiter.join(timeout=20)
            if waiter.is_alive():
                process.kill()
                waiter.join()
                raise TimeoutError((init, mode, net))
            code, finished = result[0]
            reader.join()
            error = process.stderr.read().decode(errors="replace")
            assert code == 0 and ready is not None, (init, mode, net, code, lines, error)
            if round_number >= 0:
                rows.append(
                    dict(
                        round=round_number,
                        case=f"{init}-{mode}-{net}",
                        ready_ms=(ready - before) / 1e6,
                        completion_ms=(finished - before) / 1e6,
                    )
                )
        print(f"round {round_number + 1}/{args.iterations}", flush=True)
    summary = {}
    for init, mode, net in cases:
        case = f"{init}-{mode}-{net}"
        samples = [r for r in rows if r["case"] == case]
        summary[case] = {
            metric: dict(
                p50=percentile([r[metric] for r in samples], 50),
                p95=percentile([r[metric] for r in samples], 95),
                mean=statistics.fmean(r[metric] for r in samples),
            )
            for metric in ["ready_ms", "completion_ms"]
        }
    report = dict(
        load_before=initial_load,
        load_after=os.getloadavg(),
        host=platform.platform(),
        cpu=subprocess.check_output(
            ["sysctl", "-n", "machdep.cpu.brand_string"], text=True
        ).strip(),
        iterations=args.iterations,
        warmup=args.warmup,
        vcpus=1,
        ram_mib=128,
        host_build_profile="release, thin LTO, opt-level=z",
        payload_drain_wait_ms=20,
        artifacts={
            str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in [args.c_init, rust_init, args.firmware, runner, rootfs / "payload"]
        },
        summary=summary,
        rows=rows,
    )
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
