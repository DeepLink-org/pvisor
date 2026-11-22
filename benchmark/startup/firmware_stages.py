"""B-STARTUP-DIAG: compare console discovery, independently of shell success."""

import argparse
import hashlib
import json
import os
import random
import shutil
import subprocess
import time
from pathlib import Path
from statistics import median


def console_interval(log, pid):
    points = {}
    prefix = b"pvisor-startup-profile "
    for line in log.splitlines():
        if not line.startswith(prefix):
            continue
        span = json.loads(line[len(prefix) :])
        stage = span["stage"]
        if stage not in ("vm.runtime_configure", "vmm.console_ack"):
            continue
        if (
            not (
                span["pid"] == pid
                and span["schema"] == 1
                and span["clock"] == "host_monotonic_process_relative"
                and span["epoch"] == "vm-runtime"
                and span["outcome"] == "ok"
            )
            or stage in points
        ):
            raise ValueError("invalid or duplicate firmware stage")
        points[stage] = span["start_ns"]
    if len(points) != 2:
        return None
    interval = points["vmm.console_ack"] - points["vm.runtime_configure"]
    if interval < 0:
        raise ValueError("reversed firmware stages")
    return interval


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", required=True, type=Path)
    parser.add_argument("--linux-firmware", type=Path, help="required for a dynamic Linux probe")
    parser.add_argument("--rootfs", required=True, type=Path, help="trusted, prepared /bin/sh tree")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--pairs", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=10)
    args = parser.parse_args()
    root, output = args.rootfs.resolve(), args.output.resolve()
    if min(args.pairs, args.timeout) < 1 or output.is_relative_to(root):
        parser.error("positive pairs/timeout and output outside the rootfs required")
    marker = ".fw-stage-result"
    if not (root / "bin/sh").is_file() or os.path.lexists(root / marker):
        parser.error("prepared rootfs must have /bin/sh and no preexisting result")
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    rng = random.Random(402)
    plan = [rng.sample(["linux", "rust"], 2) for _ in range(args.pairs)]
    probe = args.probe.resolve(strict=True)
    files = [probe] + ([args.linux_firmware.resolve()] if args.linux_firmware else [])
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    (output / "manifest.json").write_text(
        json.dumps(dict(plan=plan, hashes=hashes, root=str(root)))
    )
    rows = []
    for i, pair in enumerate(plan):
        for kernel in pair:
            trial = output / f"{i:03}-{kernel}"
            trial.mkdir()
            tree = shutil.copytree(root, trial / "root", symlinks=True)
            command = [str(probe), "--rootfs", str(tree), "--memory", "256", "--cpus", "1"]
            if kernel == "linux" and args.linux_firmware:
                command += ["--firmware", str(args.linux_firmware.resolve())]
            command += ["--", "/bin/sh", "-c", f"printf 'REFERENCE_READY\\n' > /{marker}"]
            result = tree / marker
            expected = b"REFERENCE_READY\n"
            observed = None
            with (trial / "stderr").open("wb") as err, (trial / "stdout").open("wb") as out:
                start = time.monotonic_ns()
                with subprocess.Popen(
                    command,
                    stdin=subprocess.DEVNULL,
                    stdout=out,
                    stderr=err,
                    env={
                        **os.environ,
                        "PVISOR_GUEST_KERNEL": "rust-native" if kernel == "rust" else "linux",
                        "PVISOR_STARTUP_PROFILE": "1",
                    },
                ) as child:
                    deadline = start + args.timeout * 1_000_000_000
                    try:
                        while observed is None:
                            status = child.poll()
                            if result.is_file() and result.read_bytes() == expected:
                                observed = time.monotonic_ns() - start
                            if status is not None or observed is not None:
                                break
                            if time.monotonic_ns() >= deadline:
                                raise subprocess.TimeoutExpired(command, args.timeout)
                            time.sleep(0.001)
                        status = child.wait(timeout=max(0, (deadline - time.monotonic_ns()) / 1e9))
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
                        status = "timeout"
            shell_ok = status == 0 and observed is not None and result.read_bytes() == expected
            row = dict(
                kernel=kernel,
                pair=i,
                command=command,
                status=status,
                pid=child.pid,
                console_ns=console_interval((trial / "stderr").read_bytes(), child.pid),
                shell_ok=shell_ok,
                shell_file_observed_ns=observed if shell_ok else None,
            )
            (trial / "result.json").write_text(json.dumps(row) + "\n")
            rows.append(row)
    for kernel in ("linux", "rust"):
        arm = [r for r in rows if r["kernel"] == kernel]
        values = [r["console_ns"] for r in arm if r["console_ns"] is not None]
        ms = median(values) / 1e6 if len(values) == len(arm) else None
        shell = [r["shell_file_observed_ns"] for r in arm if r["shell_ok"]]
        shell_ms = median(shell) / 1e6 if len(shell) == len(arm) else None
        print(
            f"{kernel}: console_ack_median_ms={ms} stages={len(values)}/{len(arm)} "
            f"shell_file_median_ms={shell_ms} verified_shells={len(shell)}/{len(arm)}"
        )
    if any(not r["shell_ok"] or r["console_ns"] is None for r in rows):
        raise SystemExit("incomplete run: retained stage diagnostics only")


if __name__ == "__main__":
    main()
