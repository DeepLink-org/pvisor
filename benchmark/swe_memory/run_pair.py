"""Benchmark: B-SWE-MEMORY-ENG (benchmark/REGISTRY.md#b-swe-memory-eng).
Role: engineering A/B.
Motivation: Does offload reduce queueing under the same scoped RAM budget?
Conclusion sought: Correct batch throughput, queue latency and memory-time.
Design: Fixed60 arrivals,32GiB, two vCPUs; disabled versus SSD offload.

Runs an explicitly prepared frozen bridge bundle, never a real model.
Preparation is separate from measurement. No automatic retry or pressure ramp.
"""

import argparse
import fcntl
import os
import re
import signal
import subprocess
import sys
from pathlib import Path

from arrival_common import cpus, load, save
from verify_inputs import verify_metadata

HERE = Path(__file__).resolve().parent
PROTOCOL = load(HERE / "protocol.json")


def validate_contexts(contexts):
    if set(contexts) != {"baseline", "offload"}:
        raise ValueError("Exactly baseline and offload contexts are required")
    seen = set()
    image_ids = set()
    for policy, c in contexts.items():
        if c["policy"] != policy:
            raise ValueError("Context policy mismatch")
        for key in ("root", "ssd"):
            p = Path(c[key])
            if not p.is_absolute() or p.resolve() != p or not p.is_dir():
                raise ValueError("Prepared absolute paths without symlinks required")
            if any(p == q or p in q.parents or q in p.parents for q in seen):
                raise ValueError("Condition/output directories must be disjoint")
            seen.add(p)
        for key, suffix in [("slice", ".slice"), ("unit", ".service"), ("supervisor", ".service")]:
            if not re.fullmatch(r"pvisor-[a-zA-Z0-9_-]+" + re.escape(suffix), c[key]):
                raise ValueError("Invalid owned systemd name")
        if cpus(c["vm_cpus"]) & cpus(c["helper_cpus"]):
            raise ValueError("VM and helper affinity must be disjoint")
        if len(cpus(c["vm_cpus"])) != 56 or len(cpus(c["helper_cpus"])) != 4:
            raise ValueError("Keep the measured56/4 logical-CPU placement envelope")
        root = Path(c["root"])
        if load(root / "spec.json") != PROTOCOL["arrival_spec"]:
            raise ValueError("The frozen arrival specification changed")
        if load(root / "admission-plan.json") != PROTOCOL["admission_plan"]:
            raise ValueError("The frozen admission rules changed")
        for sub in ("jobs", "batches", "logs"):
            if not (root / sub).is_dir() or any((root / sub).iterdir()):
                raise ValueError("Fresh empty jobs/batches/logs required; never restart")
        if any(Path(c["ssd"]).iterdir()):
            raise ValueError("Fresh empty SSD run directory required")
        verify_metadata(c)
        arm_ids = set()
        for path in c["image_files"]:
            st = Path(path).stat()
            arm_ids.add((st.st_dev, st.st_ino))
        if image_ids & arm_ids:
            raise ValueError("Image file inodes shared between conditions")
        image_ids |= arm_ids
    a, b = contexts["baseline"], contexts["offload"]
    for k in ("vm_cpus", "helper_cpus", "supervisor", "image_identity_sha256"):
        if a[k] != b[k]:
            raise ValueError("Both arms must use identical placement/coordinator")
    if a["slice"] == b["slice"] or a["unit"] == b["unit"]:
        raise ValueError("Private slices and units required")
    if Path(a["ssd"]).stat().st_dev != Path(b["ssd"]).stat().st_dev:
        raise ValueError("Both arms must use the same physical SSD filesystem")


def execute(context_path, context, script, *args, suffix=""):
    unit = context["unit"].removesuffix(".service") + suffix + ".service"
    command = [
        "systemd-run",
        "--user",
        "--wait",
        "--pipe",
        "--unit=" + unit,
        "--slice=" + context["slice"],
        "-p",
        "MemorySwapMax=0",
        "-p",
        "CPUAffinity=" + context["helper_cpus"],
        "-p",
        "RuntimeMaxSec=4200",
        "-p",
        "TimeoutStopSec=180",
        "-p",
        "KillMode=control-group",
        "--setenv=PVISOR_SWE_CONTEXT=" + str(context_path),
        sys.executable,
        str(HERE / script),
        *map(str, args),
    ]
    try:
        subprocess.run(command, check=True, timeout=4400)
    except BaseException:
        # A disconnected systemd-run client does not stop its service.
        subprocess.run(["systemctl", "--user", "stop", unit], check=True, timeout=200)
        raise


def run(pair_path):
    if sys.flags.optimize or sys.platform != "linux":
        raise RuntimeError("Linux cgroup v2 and enabled assertions are required")
    pair = load(pair_path)
    paths = {p: Path(v).resolve() for p, v in pair["contexts"].items()}
    contexts = {p: load(v) for p, v in paths.items()}
    for policy, c in contexts.items():
        if Path(c["context_file"]).resolve() != paths[policy]:
            raise ValueError("Context path mismatch")
    validate_contexts(contexts)
    own = Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1]
    if not own.endswith("/" + contexts["baseline"]["supervisor"]):
        raise ValueError("The pair must run inside its named capped coordinator")
    out = Path(pair["output"]).resolve()
    # No reuse or overwrite, including failed attempts.
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    with (out / "pair.lock").open("x") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        results = {}
        save(out / "plan.json", {"protocol": PROTOCOL, "contexts": contexts})
        try:
            for policy in ("baseline", "offload"):
                c = contexts[policy]
                save(
                    out / "progress.json",
                    {"status": "running", "policy": policy, "completed": list(results)},
                )
                # Hash and warm private images IN the condition slice, never outside
                # its RAM budget. Verify source/runtime/trace hashes before and after.
                execute(paths[policy], c, "verify_inputs.py", paths[policy], suffix="-preflight")
                execute(paths[policy], c, "arrival_batch.py", Path(c["root"]) / "spec.json", policy)
                execute(paths[policy], c, "review.py", suffix="-review")
                execute(paths[policy], c, "verify_inputs.py", paths[policy], suffix="-postflight")
                review = load(Path(c["root"]) / "condition-review.json")
                if not review["offline_batch_passed"]:
                    raise RuntimeError("Strict offline acceptance failed: " + policy)
                results[policy] = review
                save(out / "pair-results.json", results)
            save(out / "progress.json", {"status": "completed", "completed": list(results)})
        except BaseException as exc:
            save(
                out / "progress.json",
                {"status": "failed", "error": str(exc), "completed": list(results)},
            )
            # A failed worker never authorizes broad cleanup. Use its own receipts.
            for policy, c in contexts.items():
                env = dict(os.environ, PVISOR_SWE_CONTEXT=str(paths[policy]))
                subprocess.run(
                    [sys.executable, str(HERE / "cleanup.py")], env=env, check=True, timeout=180
                )
            raise
    return out


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pair", type=Path)
    args = parser.parse_args()

    def interrupted(signum, frame):
        raise RuntimeError("Interrupted: " + str(signum))

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    os.umask(0o077)
    print(run(args.pair.resolve()))
