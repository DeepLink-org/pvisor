#!/usr/bin/env python3
"""Real macOS FSKit copy-up scheduling comparison.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role engineering A/B.
Motivation: decide whether lock-free copy preparation prevents metadata stalls
without regressing writable-open latency or correctness.
Conclusion sought: paired median changes and bootstrap 95% confidence intervals;
separated distributions retain cluster counts/medians, never a single ranking.
Design: frozen release A/B, only host/prepared-copy changes differ; 1 GiB warm
non-sparse lower, 64 cold metadata paths, nojournal/compact-strict separately,
3 warmups/30 seeded paired rounds, real fresh mounts. Actual partial temporary
copy must be observed before metadata starts. Complete bytes/source identities,
journal and detach are checked. Any error/missed window/build-test interference
fails the cohort; no slow-sample exclusions, profiles, P99 or Linux/VM claims.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import selectors
import shutil
import signal
import subprocess
import threading
import time
import traceback
import uuid
from pathlib import Path

from immutable_lower_cache import paired_ci, run, sha, source_files, write_json
from publication import distribution

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
CONDITIONS = ("nojournal", "compact")
ARMS = ("baseline", "candidate")
BASELINE_FILES = [
    f"crates/pvisor-overlay-core/src/{name}.rs" for name in ("core", "lib", "service", "apply")
]
BASELINE_FILES += [
    f"crates/pvisor-overlayfs/src/{name}.rs"
    for name in ("api", "cache", "fs", "lib", "mount", "dispatch")
]
HARNESS = (
    "host_copy_up_ab.py",
    "host_copy_up_driver.rs",
    "test_host_copy_up_ab.py",
    "immutable_lower_cache.py",
    "publication.py",
)


def file_digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def build(
    out,
    target,
    baseline_ref,
    *,
    baseline_files=BASELINE_FILES,
    driver_name="host_copy_up_driver.rs",
    harness=HARNESS,
):
    baseline_commit = (
        subprocess.check_output(
            ["git", "rev-parse", "--verify", "--end-of-options", f"{baseline_ref}^{{commit}}"],
            cwd=ROOT,
        )
        .decode()
        .strip()
    )
    out.mkdir(parents=True, exist_ok=False)
    receipt = {
        "baseline_commit": baseline_commit,
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT).decode().strip(),
        "status": subprocess.check_output(["git", "status", "--short"], cwd=ROOT).decode(),
        "rustc": subprocess.check_output(["rustc", "-vV"]).decode(),
        "cargo": subprocess.check_output(["cargo", "-V"]).decode(),
        "arms": {},
    }
    candidate = out / "candidate/source"
    for name in source_files():
        dst = candidate / name
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, dst)
    baseline = out / "baseline/source"
    shutil.copytree(candidate, baseline)
    for name in baseline_files:
        old = subprocess.run(
            ["git", "show", f"{baseline_commit}:{name}"], cwd=ROOT, capture_output=True
        )
        if old.returncode == 0:
            (baseline / name).write_bytes(old.stdout)
        elif name.endswith("/dispatch.rs"):
            (baseline / name).unlink()
        else:
            raise RuntimeError(f"baseline source unavailable: {name}")
    inventories = {}
    for arm in ARMS:
        directory = out / arm
        source = directory / "source"
        inventories[arm] = {
            str(p.relative_to(source)): file_digest(p)
            for p in sorted(source.rglob("*"))
            if p.is_file()
        }
        write_json(directory / "source-manifest.json", inventories[arm])
        driver = directory / "driver"
        driver.mkdir()
        shutil.copy2(HERE / driver_name, driver / "main.rs")
        (driver / "Cargo.toml").write_text(f"""[package]
name = "host-copy-up-ab-driver"
version = "0.1.0"
edition = "2021"
[workspace]
[[bin]]
name = "host-copy-up-ab-driver"
path = "main.rs"
[dependencies]
anyhow = "1"
pvisor-overlayfs = {{ path = {json.dumps(str(source / "crates/pvisor-overlayfs"))}, default-features = false }}
pvisor-overlay-core = {{ path = {json.dumps(str(source / "crates/pvisor-overlay-core"))} }}
[patch.crates-io]
fuser = {{ path = {json.dumps(str(source / "vendor/fuser"))} }}
""")
        shutil.copy2(source / "Cargo.lock", driver / "Cargo.lock")
        command = [
            "cargo",
            "build",
            "--offline",
            "--release",
            "--manifest-path",
            str(driver / "Cargo.toml"),
            "--target-dir",
            str(target),
            "-j",
            "4",
        ]
        run(command, ROOT, directory / "build.log", timeout=600)
        binary = directory / "driver-bin"
        shutil.copy2(target / "release/host-copy-up-ab-driver", binary)
        receipt["arms"][arm] = dict(
            command=command,
            binary_sha256=sha(binary),
            source_manifest_sha256=sha(directory / "source-manifest.json"),
            manifest_sha256=sha(driver / "Cargo.toml"),
            lock_sha256=sha(driver / "Cargo.lock"),
            driver_sha256=sha(driver / "main.rs"),
        )
    changed = sorted(
        name
        for name in set(inventories["baseline"]) | set(inventories["candidate"])
        if inventories["baseline"].get(name) != inventories["candidate"].get(name)
    )
    assert set(changed) <= set(baseline_files) and changed
    receipt["changed_sources"] = changed
    (out / "harness").mkdir()
    for name in harness:
        shutil.copy2(HERE / name, out / "harness" / name)
    receipt["harness"] = {name: sha(out / "harness" / name) for name in harness}
    write_json(out / "build-receipt.json", receipt)
    return receipt


def verify_build(path):
    directory = path.parent
    receipt = json.loads(path.read_text())
    for arm, item in receipt["arms"].items():
        base = directory / arm
        assert sha(base / "driver-bin") == item["binary_sha256"]
        assert sha(base / "source-manifest.json") == item["source_manifest_sha256"]
        for name, digest in json.loads((base / "source-manifest.json").read_text()).items():
            assert file_digest(base / "source" / name) == digest, name
        for name, key in [
            ("Cargo.toml", "manifest_sha256"),
            ("Cargo.lock", "lock_sha256"),
            ("main.rs", "driver_sha256"),
        ]:
            assert sha(base / "driver" / name) == item[key]
    for name, digest in receipt["harness"].items():
        assert sha(directory / "harness" / name) == digest
        assert sha(HERE / name) == digest, "harness changed after build"
    return receipt


def order(seed, rounds):
    rng = random.Random(seed)
    result = []
    for _ in range(rounds):
        conditions = list(CONDITIONS)
        rng.shuffle(conditions)
        cells = []
        for condition in conditions:
            arms = list(ARMS)
            rng.shuffle(arms)
            cells.extend((condition, arm) for arm in arms)
        result.append(cells)
    return result


def identity(path):
    value = path.stat()
    return {
        name: getattr(value, name)
        for name in (
            "st_dev",
            "st_ino",
            "st_size",
            "st_mode",
            "st_uid",
            "st_gid",
            "st_nlink",
            "st_mtime_ns",
            "st_ctime_ns",
        )
    }


def fixture(lower, size):
    lower.mkdir()
    # Deterministic, allocated bytes rather than a sparse zero-file shortcut.
    block = random.Random(4207).randbytes(1024 * 1024)
    normal = hashlib.sha256()
    changed = hashlib.sha256()
    with (lower / "slow").open("wb") as stream:
        for offset in range(0, size, len(block)):
            chunk = block[: min(len(block), size - offset)]
            stream.write(chunk)
            normal.update(chunk)
            changed.update(b"changed" + chunk[7:] if offset == 0 else chunk)
        stream.flush()
        os.fsync(stream.fileno())
    for group in ("idle", "probe"):
        (lower / group).mkdir()
        for index in range(64):
            (lower / group / f"f{index:02}").write_bytes(b"metadata")
    (lower / "one").write_bytes(b"original")
    os.link(lower / "one", lower / "two")
    return dict(
        size=size,
        source_sha256=normal.hexdigest(),
        upper_sha256=changed.hexdigest(),
        source_identity=identity(lower / "slow"),
    )


def copying_paths(work, size):
    result = []
    for path in work.glob(".wh..pvisor-copyup-*"):
        candidates = path.iterdir() if path.is_dir() else [path]
        for candidate in candidates:
            try:
                value = candidate.stat().st_size
                if 0 < value < size:
                    result.append(dict(path=str(candidate), size=value))
            except FileNotFoundError:
                pass
    return result


def metadata_pass(mount, group):
    start = time.perf_counter()
    first = None
    for index in range(64):
        assert os.stat(mount / group / f"f{index:02}").st_size == 8
        if first is None:
            first = (time.perf_counter() - start) * 1000
    return first, (time.perf_counter() - start) * 1000


class Worker:
    def __init__(self, binary, lower, stage, condition):
        self.mount = Path("/Volumes/pvisor-copyup-ab-" + uuid.uuid4().hex[:12])
        stage.mkdir()
        self.log = (stage / "stderr.log").open("wb")
        env = {key: value for key, value in os.environ.items() if not key.startswith("PVISOR_")}
        env["PVISOR_FS_PROFILE"] = "0"
        command = [str(binary), str(lower), str(stage), str(self.mount), condition]
        write_json(stage / "launch.json", dict(command=command, profile=False))
        self.proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            env=env,
            start_new_session=True,
        )
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)
        try:
            self.receive("ready")
            deadline = time.monotonic() + 20
            while not os.path.ismount(self.mount):
                if self.proc.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("FSKit mount did not become ready")
                time.sleep(0.02)
        except BaseException:
            self.cleanup()
            raise

    def receive(self, expected):
        if not self.selector.select(60):
            raise TimeoutError("mount owner response timeout")
        line = self.proc.stdout.readline().decode().strip()
        assert line == expected, f"mount owner: expected {expected}, received {line}"

    def close(self):
        self.proc.stdin.write(b"stop\n")
        self.proc.stdin.flush()
        self.receive("stopped")
        self.proc.wait(timeout=15)
        assert self.proc.returncode == 0 and not os.path.ismount(self.mount)

    def cleanup(self):
        # Scope cleanup to this UUID mount/process; never change other mounts.
        if self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(timeout=10)
        if os.path.ismount(self.mount):
            subprocess.run(
                ["/sbin/umount", str(self.mount)], check=True, timeout=15, capture_output=True
            )
        if self.mount.exists():
            self.mount.rmdir()
        self.selector.close()
        self.log.close()


def trial(binary, lower, stage, condition, expected):
    worker = None
    try:
        worker = Worker(binary, lower, stage, condition)
        mount = worker.mount
        row = dict(idle_metadata_ms=metadata_pass(mount, "idle")[1])
        # Warm only source lookup, never the independent probe entries.
        assert os.stat(mount / "slow").st_size == expected["size"]
        done = threading.Event()
        results = {}

        def writer():
            start = time.perf_counter()
            try:
                fd = os.open(mount / "slow", os.O_RDWR)
                results["open_ms"] = (time.perf_counter() - start) * 1000
                try:
                    assert os.pwrite(fd, b"changed", 0) == 7
                finally:
                    os.close(fd)
                results["write_complete_ms"] = (time.perf_counter() - start) * 1000
            except BaseException:
                results["error"] = traceback.format_exc()
            finally:
                done.set()

        thread = threading.Thread(target=writer, daemon=True)
        thread.start()
        deadline = time.monotonic() + 60
        copying = []
        while time.monotonic() < deadline and not done.is_set():
            copying = copying_paths(stage / "work", expected["size"])
            if copying:
                break
            time.sleep(0.001)
        if not copying:
            raise RuntimeError("no active partial copy observed; sample rejected")
        row["copy_at_metadata_start"] = copying
        row["first_stat_ms"], row["copying_metadata_ms"] = metadata_pass(mount, "probe")
        row["copy_at_metadata_end"] = copying_paths(stage / "work", expected["size"])
        thread.join(60)
        assert not thread.is_alive(), "copy-up timeout"
        assert "error" not in results, results.get("error")
        row.update(results)
        assert file_digest(stage / "upper/slow") == expected["upper_sha256"], "upper bytes mismatch"
        assert file_digest(lower / "slow") == expected["source_sha256"], "lower bytes changed"
        assert identity(lower / "slow") == expected["source_identity"], "lower metadata changed"
        assert not list((stage / "work").iterdir()), "unused copies left behind"
        held = os.open(mount / "one", os.O_RDONLY)
        try:
            with (mount / "one").open("r+b") as output:
                output.write(b"updated!")
            assert os.pread(held, 8, 0) == b"updated!"
        finally:
            os.close(held)
        assert (mount / "two").read_bytes() == b"updated!"
        os.rename(mount / "one", mount / "renamed")
        assert (mount / "renamed").read_bytes() == b"updated!"
        assert (lower / "one").read_bytes() == b"original"
        worker.close()
        row.update(correctness="passed", detached=True)
        write_json(stage / "result.json", row)
        # Successful disposable upper bytes are verified before removal; logs,
        # hashes and the journal remain as evidence, failures keep their stage.
        shutil.rmtree(stage / "upper")
        shutil.rmtree(stage / "work")
        return row
    finally:
        if worker is not None:
            worker.cleanup()


def competing_work():
    lines = subprocess.check_output(["ps", "-axo", "pid=,comm=,args="]).decode().splitlines()
    result = []
    for line in lines:
        fields = line.split(None, 2)
        if len(fields) < 3:
            continue
        executable = Path(fields[1]).name
        if executable in ("cargo", "rustc", "rustdoc", "pytest", "cargo-nextest") or (
            executable.startswith("python")
            and any(token in fields[2].split() for token in ("pytest", "unittest"))
        ):
            result.append(line)
    return result


def summarize(rows, samples):
    wanted = {
        (condition, arm, index)
        for condition in CONDITIONS
        for arm in ARMS
        for index in range(samples)
    }
    actual = {(row["condition"], row["arm"], row["round"]) for row in rows if row["round"] >= 0}
    assert actual == wanted and len([r for r in rows if r["round"] >= 0]) == len(wanted), (
        "incomplete/duplicate cohort"
    )
    assert all(row["correctness"] == "passed" and row["detached"] for row in rows)
    result = []
    for condition in CONDITIONS:
        selected = {
            arm: sorted(
                [
                    row
                    for row in rows
                    if row["round"] >= 0 and row["condition"] == condition and row["arm"] == arm
                ],
                key=lambda row: row["round"],
            )
            for arm in ARMS
        }
        for metric in ("first_stat_ms", "copying_metadata_ms", "open_ms", "idle_metadata_ms"):
            values = {arm: [row[metric] for row in selected[arm]] for arm in ARMS}
            record = dict(
                condition=condition,
                metric=metric,
                unit="ms",
                samples=samples,
                baseline=distribution(values["baseline"]),
                candidate=distribution(values["candidate"]),
            )
            record.update(paired_ci(values["baseline"], values["candidate"]))
            result.append(record)
    return result


def experiment(args):
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    receipt_path = args.build_receipt.resolve()
    verify_build(receipt_path)
    assert platform.system() == "Darwin", "this cohort requires macOS FSKit"
    report = dict(
        benchmark="B-FS-ENG",
        role="engineering A/B",
        arguments=vars(args).copy(),
        host=dict(
            uname=platform.uname()._asdict(),
            hardware=subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"])
            .decode()
            .strip(),
            memory_bytes=int(subprocess.check_output(["sysctl", "-n", "hw.memsize"])),
            macfuse=subprocess.check_output(
                [
                    "plutil",
                    "-extract",
                    "CFBundleShortVersionString",
                    "raw",
                    "/Library/Filesystems/macfuse.fs/Contents/Info.plist",
                ]
            )
            .decode()
            .strip(),
            mount_table=subprocess.check_output(["mount"]).decode(),
            cpu_affinity="not enforced; macOS scheduler, identical conditions; no whole-host budget",
        ),
        build_receipt=str(receipt_path),
        build_receipt_sha256=sha(receipt_path),
        rows=[],
        failures=[],
        guards=[],
        exclusion="no slow-sample exclusions; any error or detected build/test fails batch",
    )
    report["arguments"] = {
        k: str(v) if isinstance(v, Path) else v for k, v in report["arguments"].items()
    }
    lower = out / "lower"
    try:
        expected = fixture(lower, args.file_mib * 1024 * 1024)
        report["fixture"] = expected
        plan = order(args.seed, args.samples + args.warmups)
        report["order"] = plan
        write_json(out / "report.json", report)
        for index, cells in enumerate(plan):
            round_id = index - args.warmups
            for condition, arm in cells:
                before = competing_work()
                report["guards"].append(
                    dict(round=round_id, condition=condition, arm=arm, before=before)
                )
                assert not before, "competing build/test detected"
                stage = out / "trials" / f"{index:02}-{condition}-{arm}"
                stage.parent.mkdir(exist_ok=True)
                stop = threading.Event()
                during = []
                checks = []

                def observe():
                    try:
                        while not stop.wait(0.25):
                            detected = competing_work()
                            checks.append(dict(at=time.monotonic(), detected=detected))
                            during.extend(detected)
                    except BaseException:
                        during.append(traceback.format_exc())

                observer = threading.Thread(target=observe, daemon=True)
                observer.start()
                try:
                    row = trial(
                        receipt_path.parent / arm / "driver-bin", lower, stage, condition, expected
                    )
                finally:
                    stop.set()
                    observer.join()
                    report["guards"][-1]["during"] = during
                    report["guards"][-1]["checks"] = checks
                after = competing_work()
                report["guards"][-1]["after"] = after
                assert not after and not during, "competing build/test detected"
                row.update(condition=condition, arm=arm, round=round_id)
                report["rows"].append(row)
                write_json(out / "report.json", report)
            print(json.dumps(dict(round=round_id, completed=len(report["rows"]))), flush=True)
        verify_build(receipt_path)
        report["summary"] = summarize(report["rows"], args.samples)
        report["status"] = "passed"
    except BaseException:
        report["failures"].append(traceback.format_exc())
        report["status"] = "failed"
        raise
    finally:
        write_json(out / "report.json", report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--baseline-ref", default="HEAD")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/host-copy-up-ab")
    parser.add_argument("--build-receipt", type=Path)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--file-mib", type=int, default=1024)
    parser.add_argument("--seed", type=int, default=4207)
    args = parser.parse_args()
    if args.build:
        build(args.output.resolve(), args.target_dir.resolve(), args.baseline_ref)
    else:
        assert args.build_receipt and args.samples > 0 and args.warmups >= 0 and args.file_mib > 0
        experiment(args)


if __name__ == "__main__":
    main()
