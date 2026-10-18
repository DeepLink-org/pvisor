#!/usr/bin/env python3
"""Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), engineering A/B.

Question: which V2 mechanisms, resource boundaries and complete local workflows
benefit from guest writeback, at equal checked file/directory sync boundaries?
Design: same frozen host/firmware/payload, guest WB 0/1, host WB/DATA 0;
N3 independent paired fresh VMs per case, three dependent rounds, no quiet,
interference, full-inventory or slow-value gate. Separate N1 diagnostics.
Preparation is untimed; invoke in.json through semspec bench for measurement.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import threading
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
import curves_diagnostics as DIAG  # noqa: E402 - import after module-loading setup
import workloads_v2 as DESIGN  # noqa: E402 - import after module-loading setup

MIB = 1 << 20
HARNESS = (
    "workloads_v2.py",
    "workloads_v2_run.py",
    "workloads_v2_driver.rs",
    "workloads_v2_guest.rs",
    "workloads_v2_scenarios.rs",
    "workloads_v2_analysis.py",
    "curves_diagnostics.py",
)


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def mix(value):
    mask = (1 << 64) - 1
    value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & mask
    value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & mask
    return value ^ (value >> 31)


def fixture_pattern(size):
    return b"".join(
        mix((0x4653563246495854 + i * 0x9E3779B97F4A7C15) & ((1 << 64) - 1)).to_bytes(8, "little")
        for i in range((size + 7) // 8)
    )[:size]


def make_fixture(layer):
    """Only generated private inputs; never edits the prepared product rootfs."""
    root = layer / "fs-v2-fixture"
    (root / "meta").mkdir(parents=True)
    pattern = fixture_pattern(4 * MIB)
    (root / "read.bin").write_bytes(pattern)
    (root / "sentinel.bin").write_bytes(pattern[:64])
    for i in range(2048):
        (root / "meta" / f"f{i:08}.bin").write_bytes(pattern[:64])
    repo = root / "repo"
    package = layer.parent / "package-source"
    for i in range(1024):
        relative = Path(f"d{i // 32:02}") / f"f{i % 32:03}"
        for parent, suffix, prefix, fill in (
            (repo, ".txt", f"file {i:04} V2_NEEDLE\n", b"x"),
            (package, ".dat", f"package {i:04}\n", b"p"),
        ):
            path = parent / (str(relative) + suffix)
            path.parent.mkdir(parents=True, exist_ok=True)
            header = prefix.encode()
            path.write_bytes(header + fill * (4096 - len(header)))
    git_env = {
        **os.environ,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_AUTHOR_NAME": "filesystem-v2",
        "GIT_COMMITTER_NAME": "filesystem-v2",
        "GIT_AUTHOR_EMAIL": "fixture@localhost",
        "GIT_COMMITTER_EMAIL": "fixture@localhost",
        "GIT_AUTHOR_DATE": "2020-01-01T00:00:00Z",
        "GIT_COMMITTER_DATE": "2020-01-01T00:00:00Z",
    }
    for command in (
        ["git", "init", "-q", "-b", "fixture"],
        ["git", "add", "."],
        ["git", "-c", "commit.gpgsign=false", "commit", "-qm", "fixed offline fixture"],
    ):
        subprocess.run(command, cwd=repo, env=git_env, check=True, capture_output=True)
    with tarfile.open(root / "package.tar", "w", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(package.rglob("*.dat")):
            info = archive.gettarinfo(str(path), arcname=str(path.relative_to(package)))
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ""
            with path.open("rb") as source:
                archive.addfile(info, source)
    shutil.rmtree(package)
    build = root / "build"
    build.mkdir()
    for i in range(100):
        (build / f"unit{i:03}.c").write_text(f"int unit{i:03}(void) {{ return {i}; }}\n")
    main = "extern int printf(const char *, ...);\n" + "".join(
        f"int unit{i:03}(void);\n" for i in range(100)
    )
    main += "int main(void) { int sum = 0;\n"
    main += "".join(f"sum += unit{i:03}();\n" for i in range(100))
    main += 'printf("%d\\n", sum); return 0; }\n'
    (build / "main.c").write_text(main)
    return root


def product_hashes():
    selected = [REPO / "Cargo.toml", REPO / "Cargo.lock", REPO / ".cargo/config.toml"]
    selected += [
        p
        for base in ("crates", "vendor/fuser")
        for p in (REPO / base).rglob("*")
        if p.is_file()
        and ".data" not in p.parts
        and "target" not in p.parts
        and "__pycache__" not in p.parts
    ]
    return {str(p.relative_to(REPO)): sha(p) for p in sorted(selected)}


def complete_compiler_runtime(layer, rootfs):
    """Freeze missing local compiler shared libraries in our private layer."""
    receipt = []
    tools = [
        "usr/bin/cc",
        "usr/bin/as",
        "usr/bin/ld",
        "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
        "usr/libexec/gcc/x86_64-redhat-linux/16/collect2",
    ]
    for relative in tools:
        executable = rootfs / relative
        require(executable.is_file(), "prepared compiler unavailable: " + relative)
        output = subprocess.check_output(["ldd", str(executable)], text=True)
        for line in output.splitlines():
            if "=>" not in line:
                continue
            resolved = line.split("=>", 1)[1].strip().split()[0]
            require(resolved.startswith("/"), "host dependency unavailable: " + line)
            source = Path(resolved)
            soname = source.name
            if any((rootfs / location / soname).is_file() for location in ("lib64", "usr/lib64")):
                continue
            destination = layer / "usr/lib64" / soname
            destination.parent.mkdir(parents=True, exist_ok=True)
            if not destination.exists():
                shutil.copy2(source.resolve(), destination)
                receipt.append(
                    dict(
                        source=str(source),
                        destination=str(destination.relative_to(layer)),
                        sha256=sha(destination),
                    )
                )
    # GCC 16's libatomic linker script names this leaf even for our small C
    # fixture; ldd(cc1) cannot discover linker-script-only inputs.
    for relative in ("usr/lib64/libatomic.so.1.2.0", "usr/lib64/libatomic.so.1"):
        if not (rootfs / relative).is_file():
            destination = layer / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(Path("/") / relative, destination)
            receipt.append(
                dict(source="/" + relative, destination=relative, sha256=sha(destination))
            )
    return receipt


def materialize_upper_view(upper, destination, case):
    """Read-only hardlink view: hide only validated physical overlay metadata.

    Guest oracles always use the real merged view. This separate post-exit host
    oracle reads retained bytes without treating private .wh markers as outputs.
    Unknown markers, special files and markers hiding retained names fail.
    """
    markers = []
    destination.mkdir(parents=True, exist_ok=False)
    if not upper.exists():
        return markers  # Read-only cases may never materialize the work directory.
    for root, directories, files in os.walk(upper, followlinks=False):
        source = Path(root)
        relative = source.relative_to(upper)
        target = destination / relative
        for name in directories:
            path = source / name
            require(not path.is_symlink(), "unexpected output directory symlink")
            (target / name).mkdir()
        for name in files:
            path = source / name
            metadata = path.lstat()
            require(stat.S_ISREG(metadata.st_mode), "unexpected nonregular output")
            if name.startswith(".wh."):
                allowed = name == ".wh..wh..opq"
                if case == "M12" and relative == Path("."):
                    allowed |= name in {
                        f".wh.{prefix}-{i:04}.bin"
                        for prefix in ("temp", "renamed")
                        for i in range(128)
                    }
                require(
                    allowed and metadata.st_size == 0, "unexpected overlay marker: " + str(path)
                )
                if name != ".wh..wh..opq":
                    require(
                        not (source / name[4:]).exists(), "whiteout conflicts with retained output"
                    )
                markers.append(str(path.relative_to(upper)))
                continue
            os.link(path, target / name)
    return markers


def prepare(args):
    batch = args.prepare.resolve()
    require(".data" in batch.parts, "raw evidence belongs under .data")
    batch.mkdir(parents=True, exist_ok=False)
    for directory in ("harness", "bin", "driver/src", "firmware-source"):
        (batch / directory).mkdir(parents=True)
    for name in HARNESS:
        shutil.copy2(HERE / name, batch / "harness" / name)
    shutil.copy2(args.firmware, batch / "bin/libkrunfw.so")
    firmware_source = json.loads(args.firmware_source.read_text())
    require(
        firmware_source["firmware_sha256"] == sha(batch / "bin/libkrunfw.so"),
        "firmware SOURCE must identify these exact bytes",
    )
    shutil.copy2(args.firmware_source, batch / "firmware-source/SOURCE.json")
    source_hashes = product_hashes()
    save(batch / "source-before.json", source_hashes)
    fixture = make_fixture(batch / "fixture-layer")
    compiler_runtime = complete_compiler_runtime(batch / "fixture-layer", args.rootfs.resolve())
    fixture_hashes = {
        str(p.relative_to(batch / "fixture-layer")): sha(p)
        for p in sorted((batch / "fixture-layer").rglob("*"))
        if p.is_file()
    }
    save(batch / "fixture-manifest.json", fixture_hashes)
    driver = batch / "driver"
    shutil.copy2(batch / "harness/workloads_v2_driver.rs", driver / "src/main.rs")
    (driver / "Cargo.toml").write_text(
        '[workspace]\n[package]\nname="pvisor-fs-workloads-v2-subject"\nversion="0.0.0"\nedition="2024"\n'
        "[dependencies]\npvisor-vm={path="
        + json.dumps(str(REPO / "crates/pvisor-vm"))
        + '}\nserde_json="1"\n'
        '[profile.release]\nopt-level=3\nlto="thin"\nstrip="symbols"\ncodegen-units=8\npanic="abort"\n'
        '[profile.release.build-override]\nstrip="none"\n[profile.performance]\ninherits="release"\n'
        '[profile.performance.build-override]\nstrip="none"\n[patch.crates-io]\nfuser={path='
        + json.dumps(str(REPO / "vendor/fuser"))
        + "}\n"
    )
    shutil.copy2(REPO / "Cargo.lock", driver / "Cargo.lock")
    commands = [
        [
            "rustc",
            "--edition=2024",
            "-O",
            "--target",
            "x86_64-unknown-linux-musl",
            str(batch / "harness/workloads_v2_guest.rs"),
            "-o",
            str(batch / "bin/payload"),
        ],
        [
            "cargo",
            "build",
            "--offline",
            "--profile",
            "performance",
            "--manifest-path",
            str(driver / "Cargo.toml"),
            "--target-dir",
            str(REPO / "target"),
        ],
    ]
    with (batch / "build.log").open("w") as log:
        for command in commands:
            log.write(json.dumps(command) + "\n")
            log.flush()
            subprocess.run(command, cwd=REPO, stdout=log, stderr=subprocess.STDOUT, check=True)
    shutil.copy2(REPO / "target/performance/pvisor-fs-workloads-v2-subject", batch / "bin/subject")
    require(source_hashes == product_hashes(), "product source changed during build")
    catalog = DESIGN.case_catalog(512)
    for case in catalog:
        case.update(executor="workloads_v2_guest.rs", fixture="fixture-manifest.json")
        if case["layer"] == "scenario":
            case.update(
                task_spec_status="implemented and frozen in workloads_v2_scenarios.rs",
                metric="completed_ns",
            )
            case["parameters"].update(
                dataset="fixture-manifest.json",
                argv="frozen workloads_v2_scenarios.rs tool subprocesses",
            )
    scenario_specs = {
        "S01": dict(files=1024, bytes_per_file=4096, tools=["git status", "git grep"]),
        "S02": dict(
            units=100,
            edited_units_per_round=1,
            compiler="/usr/bin/cc",
            flags=["-O0"],
            main_header_dependency=False,
            setup="full initial build, run and sync outside task clock",
        ),
        "S03": dict(
            archive_files=1024,
            archive_bytes_per_file=4096,
            generated_small_files=128,
            small_file_bytes=4096,
            stream_bytes=32 * MIB,
            stream_block_bytes=1024,
            extraction="prepared /usr/bin/python3 stdlib tarfile",
            publish="all files and directories synced before rename, then parent directory synced",
        ),
    }
    for case in catalog:
        if case["id"] in scenario_specs:
            case["parameters"].update(scenario_specs[case["id"]])
    selected = args.cases.split(",") if args.cases else [c["id"] for c in catalog]
    require(
        len(selected) == len(set(selected)) and set(selected) <= {c["id"] for c in catalog},
        "unknown or duplicate selected cases",
    )
    rng = random.Random(args.seed)
    orders = []
    for pair in range(args.samples):
        case_order = selected.copy()
        rng.shuffle(case_order)
        for case in case_order:
            arms = ["baseline", "candidate"]
            rng.shuffle(arms)
            orders.append(dict(case_id=case, pair=pair, conditions=arms))
    arms = {
        arm["id"]: dict(
            env={
                **arm["requested_environment"],
                "PVISOR_FS_PROFILE": "1" if args.diagnostic else "0",
            }
        )
        for arm in DESIGN.comparison_arms("guest-writeback")
    }
    artifacts = {
        str(p.relative_to(batch)): sha(p)
        for folder in ("harness", "bin", "firmware-source")
        for p in (batch / folder).rglob("*")
        if p.is_file()
    }
    protocol = dict(
        schema=2,
        benchmark_id="B-FS-DIAG" if args.diagnostic else "B-FS-ENG",
        role="diagnostic" if args.diagnostic else "engineering A/B",
        comparison="guest-writeback",
        catalog=catalog,
        inventory=dict(selected_cell_ids=selected),
        samples=args.samples,
        resident_rounds=3,
        seed=args.seed,
        orders=orders,
        conditions=arms,
        rootfs=str(args.rootfs.resolve()),
        fixture=str(fixture),
        source_hashes=source_hashes,
        compiler_runtime=compiler_runtime,
        rootfs_scope="external prepared rootfs; selected tool identities recorded, not a fully copied rootfs",
        rootfs_tools={
            relative: sha(args.rootfs / relative)
            for relative in (
                "usr/bin/git",
                "usr/bin/cc",
                "usr/bin/python3",
                "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
            )
        },
        artifacts=artifacts,
        fixture_manifest_sha256=sha(batch / "fixture-manifest.json"),
        build_commands=commands,
        source_head=subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        diagnostic=args.diagnostic,
        trial_timeout_seconds=args.timeout,
        budget=dict(
            cpus=[0, 1],
            quota="200000 100000",
            memory_bytes=2 << 30,
            swap_bytes=0,
            guest_mib=512,
            guest_vcpus=2,
        ),
        cache="fresh guest per case/arm/pair; shared prepared host inputs, uncontrolled host page cache; not cold disk",
        independent_unit="fresh case environment; three dependent rounds are not independent samples",
        timing="Ready excludes exit; work and final sync separately; completed=work+finalsync; setup/validation excluded and retained",
        quiet_gate=False,
        interference_gate=False,
        inventory_gate=False,
        failures="retain all failures and valid slow observations, no replacement; complete independent cases may report",
        limitations=[
            "SDK path, not ordinary CLI or IOCP offload",
            "N3 exploratory only; no tail percentile or population-wide ranking",
            "resource monitor covers whole environment, not task-only or individual operation",
            "no guest dirty-page/queue occupancy instrumentation; pressure knee remains unproven",
        ],
        host=dict(
            platform=platform.platform(),
            uname=list(platform.uname()),
            cpuinfo=Path("/proc/cpuinfo").read_text(),
            filesystem=subprocess.check_output(
                ["findmnt", "-T", str(batch), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS", "--json"],
                text=True,
            ),
        ),
    )
    save(batch / "protocol.json", protocol)
    save(
        batch / "in.json",
        {
            "filesystem-v2-shallow": "$ "
            + sys.executable
            + " "
            + str(batch / "harness/workloads_v2_run.py")
            + " --measure "
            + str(batch)
        },
    )
    print(batch, flush=True)


def cgroup_state():
    relative = Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1]
    root = Path("/sys/fs/cgroup") / relative.lstrip("/")
    result = dict(cgroup=str(root), affinity=sorted(os.sched_getaffinity(0)))
    for name in (
        "cpu.max",
        "cpu.stat",
        "memory.max",
        "memory.swap.max",
        "memory.current",
        "memory.peak",
        "memory.events",
        "memory.stat",
        "io.stat",
    ):
        if (root / name).exists():
            result[name] = (root / name).read_text().strip()
    return result


def usage(state):
    return int(dict(line.split() for line in state["cpu.stat"].splitlines())["usage_usec"])


def verify_artifacts(batch, protocol):
    for name, expected in protocol["artifacts"].items():
        require(sha(batch / name) == expected, "frozen artifact changed: " + name)


def verify_fixture(batch, protocol):
    """Check frozen generated inputs at campaign boundaries, never per round."""
    path = batch / "fixture-manifest.json"
    require(sha(path) == protocol["fixture_manifest_sha256"], "fixture manifest changed")
    for relative, expected in json.loads(path.read_text()).items():
        require(sha(batch / "fixture-layer" / relative) == expected, "fixture changed: " + relative)


def validate_row(row, case):
    require(
        row.get("correctness") is True
        and row["completed_ns"] == row["work_ns"] + row["final_sync_ns"],
        "correctness/clock partition failed",
    )
    expected = {
        "M01": dict(metadata_ops=2048),
        "M02": dict(metadata_ops=2048),
        "M03": dict(metadata_ops=2048),
        "M04": dict(read_ops=192, bytes_read=12 * MIB),
        "M05": dict(read_ops=1024, bytes_read=4 * MIB),
        "M06": dict(write_ops=4096, bytes_written=4 * MIB),
        "M07": dict(write_ops=4, bytes_written=4 * MIB),
        "M08": dict(write_ops=1024, bytes_written=4 * MIB),
        "M09": dict(write_ops=256, bytes_written=16 * MIB),
        "M10": dict(write_ops=256, bytes_written=MIB, periodic_fsyncs=256),
        "M11": dict(write_ops=1024, bytes_written=4 * MIB, read_ops=1024, bytes_read=4 * MIB),
        "M12": dict(
            write_ops=128,
            bytes_written=512 * 1024,
            read_ops=128,
            bytes_read=512 * 1024,
            metadata_ops=384,
            periodic_fsyncs=128,
        ),
    }
    if case.startswith("R"):
        expected[case] = dict(write_ops=262144, bytes_written=1024 * MIB, final_fsyncs=5)
    require(
        all(row.get(key) == value for key, value in expected.get(case, {}).items()),
        "logical work or synchronization differs from frozen case contract",
    )


def one_trial(batch, protocol, case, pair, condition):
    folder = batch / "trials" / f"{case}-{pair:03}-{condition}"
    folder.mkdir(parents=True, exist_ok=False)
    stage = folder / "stage"
    for name in ("upper", "work"):
        (stage / name).mkdir(parents=True)
    command = [
        str(batch / "bin/subject"),
        protocol["rootfs"],
        str(batch / "fixture-layer"),
        str(batch / "bin/libkrunfw.so"),
        str(batch / "bin/payload"),
        str(stage),
        case,
        "3",
        "512",
    ]
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("PVISOR_", "CURVES_", "WORKLOADS_V2_"))
    }
    env.update(protocol["conditions"][condition]["env"])
    env.update(PVISOR_STARTUP_TIMING="0", PVISOR_STARTUP_PROFILE="0", RUST_LOG="off")
    before = cgroup_state()
    peak = int(before["memory.current"])
    snapshots = []
    stop = threading.Event()

    def monitor():
        nonlocal peak
        root = Path(before["cgroup"])
        while not stop.wait(0.05):
            current = int((root / "memory.current").read_text())
            peak = max(peak, current)
            # Progress and memory have environment-relative observer timestamps.
            snapshots.append(dict(elapsed_ns=time.monotonic_ns() - started, memory_current=current))

    rows = []
    record = dict(
        case_id=case,
        pair=pair,
        condition=condition,
        command=command,
        environment={k: v for k, v in env.items() if k.startswith("PVISOR_")},
        rows=rows,
        resource_before=before,
        valid=False,
        host_verified=False,
    )
    process = None
    started = time.monotonic_ns()
    ready = done = None
    thread = threading.Thread(target=monitor, daemon=True)
    thread.start()
    try:
        with (folder / "stdout.log").open("w") as out, (folder / "stderr.log").open("w") as err:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                env=env,
                start_new_session=True,
            )
            timed_out = threading.Event()

            def timeout():
                timed_out.set()
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

            timer = threading.Timer(protocol["trial_timeout_seconds"], timeout)
            timer.start()
            try:
                for line in process.stdout:
                    now = time.monotonic_ns()
                    out.write(line)
                    out.flush()
                    if line.startswith("WORKLOADS_V2_READY"):
                        require(ready is None, "duplicate Ready")
                        ready = now
                        record["ready_record"] = (
                            json.loads(line.split(" ", 1)[1]) if " " in line.strip() else {}
                        )
                    elif line.startswith("WORKLOADS_V2_RESULT "):
                        rows.append(json.loads(line.split(" ", 1)[1]))
                    elif line.startswith("WORKLOADS_V2_DONE"):
                        require(done is None, "duplicate DONE")
                        done = now
                    elif line.startswith("WORKLOADS_V2_PROGRESS "):
                        snapshots.append(
                            dict(
                                elapsed_ns=now - started, progress=json.loads(line.split(" ", 1)[1])
                            )
                        )
                returncode = process.wait()
            finally:
                timer.cancel()
        ended = time.monotonic_ns()
        after = cgroup_state()
        record.update(
            returncode=returncode,
            timed_out=timed_out.is_set(),
            ready_ms=(ready - started) / 1e6 if ready else None,
            job_ms=(ended - started) / 1e6,
            exit_after_done_ms=(ended - done) / 1e6 if done else None,
            resource=dict(
                cpu_usage_usec_delta=usage(after) - usage(before),
                sampled_memory_current_peak=peak,
                before=before,
                after=after,
                scope="whole fresh environment including setup/guest validation/exit; parent file cache may predate trial",
            ),
        )
        require(
            returncode == 0 and not timed_out.is_set() and ready is not None and done is not None,
            "exit/Ready/DONE failed",
        )
        require(
            len(rows) == 3
            and {(r["id"], r["round"]) for r in rows} == {(case, r) for r in (1, 2, 3)},
            "case round output differs",
        )
        for row in rows:
            validate_row(row, case)
        stderr = (folder / "stderr.log").read_text()
        if protocol["diagnostic"]:
            profiles = DIAG.collect_profiles(stderr.splitlines())
            record["profiles"] = profiles
            overlay = next(
                p["measurements"] for p in profiles if p["component"] == "virtio-fs-overlay"
            )
            actual = overlay.get("guest_writeback_enabled", {}).get("units")
            require(actual == (condition == "candidate"), "actual guest WB negotiation differs")
            # The frozen host emits this positive-only label when enabling its
            # buffer; it does not emit an explicit negotiated zero flag.
            host_flag = overlay.get("host_writeback_enabled", {}).get("units")
            require(host_flag in (None, 0), "unexpected positive host writeback flag")
            record["host_writeback_state"] = dict(
                requested="0",
                reported_enabled_units=host_flag,
                explicit_zero_flag_available=False,
                label_contract="source-defined positive-only enablement event",
            )
            record["counter_scope"] = (
                "whole case lifetime, three rounds plus setup/verification/exit; not isolated work phase"
            )
        else:
            require("pvisor-fs-profile " not in stderr, "profile present in formal timing")
        host_view = folder / "host-view"
        record["physical_overlay_markers"] = materialize_upper_view(
            stage / "upper/fs-v2-work", host_view, case
        )
        verify_cmd = [
            str(batch / "bin/payload"),
            "--verify",
            case,
            str(host_view),
            protocol["fixture"],
            "3",
            "512",
        ]
        verified = subprocess.run(verify_cmd, capture_output=True, text=True, timeout=180)
        (folder / "host-verification.log").write_text(verified.stdout + verified.stderr)
        receipt = [
            json.loads(line.split(" ", 1)[1])
            for line in verified.stdout.splitlines()
            if line.startswith("WORKLOADS_V2_VERIFIED ")
        ]
        require(
            verified.returncode == 0 and len(receipt) == 1 and receipt[0].get("verified") is True,
            "independent host output verification failed",
        )
        record.update(host_verified=True, host_verification=receipt[0], valid=True)
    except Exception as error:
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        record["error"] = repr(error)
    finally:
        stop.set()
        thread.join()
        save(folder / "observations.json", snapshots)
        save(folder / "result.json", record)
    if record["valid"]:
        shutil.rmtree(folder / "host-view")
        shutil.rmtree(stage)
    return record


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_artifacts(batch, protocol)
    verify_fixture(batch, protocol)
    state = cgroup_state()
    require(
        state["cpu.max"] == "200000 100000"
        and state["memory.max"] == str(2 << 30)
        and state["memory.swap.max"] == "0"
        and state["affinity"] == [0, 1],
        "declared resource budget not installed",
    )
    save(batch / "budget-before.json", state)
    records = []
    for order in protocol["orders"]:
        for condition in order["conditions"]:
            record = one_trial(batch, protocol, order["case_id"], order["pair"], condition)
            records.append(record)
            save(batch / "records.partial.json", records)
            print(
                json.dumps(
                    {
                        k: record.get(k)
                        for k in ("case_id", "pair", "condition", "valid", "job_ms", "error")
                    }
                ),
                file=sys.stderr,
                flush=True,
            )
    verify_artifacts(batch, protocol)
    verify_fixture(batch, protocol)
    save(batch / "records.json", records)
    save(batch / "budget-after.json", cgroup_state())
    failures = sum(not r["valid"] for r in records)
    save(
        batch / "status.json",
        dict(
            trials=len(records),
            failed=failures,
            excluded=0,
            complete=failures == 0,
            measurements=sum(len(r["rows"]) for r in records if r["valid"]),
        ),
    )
    print(json.dumps(dict(trials=len(records), failed=failures)), flush=True)
    # Retain complete independent cases even when another case fails.


def measure(batch):
    require(not (batch / "records.partial.json").exists(), "refuse measured batch reuse")
    unit = "pvisor-fs-v2-" + uuid.uuid4().hex[:12]
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit",
        unit,
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
        str(batch / "harness/workloads_v2_run.py"),
        "--campaign",
        str(batch),
    ]
    save(batch / "campaign-command.json", command)
    with (
        (batch / "campaign.stdout.log").open("w") as out,
        (batch / "campaign.stderr.log").open("w") as err,
    ):
        process = subprocess.run(command, stdout=out, stderr=err, timeout=14400)
    require(process.returncode == 0, "campaign failed; inspect retained stderr")
    print((batch / "campaign.stdout.log").read_text(), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--prepare", type=Path)
    action.add_argument("--measure", type=Path)
    action.add_argument("--campaign", type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument("--firmware-source", type=Path)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--cases")
    parser.add_argument("--diagnostic", action="store_true")
    parser.add_argument("--seed", type=int, default=20261018)
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    if args.prepare:
        require(
            all((args.rootfs, args.firmware, args.firmware_source)),
            "prepare requires rootfs/firmware/SOURCE",
        )
        require(1 <= args.samples <= 30 and args.timeout > 0, "invalid samples/timeout")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    else:
        campaign(args.campaign.resolve())


if __name__ == "__main__":
    main()
