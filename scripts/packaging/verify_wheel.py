#!/usr/bin/env python3
"""Verify that a pVisor wheel installs its complete native CLI set."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
import venv
import zipfile
from email.parser import BytesParser
from pathlib import Path

EXPECTED_BINARIES = (
    "pvisor",
    "pvisor-tui",
    "pvisor-replay",
    "pvisor-daemon",
)
COMPANION_BINARIES = EXPECTED_BINARIES[1:-1]
FIRMWARE_NAMES = ("libkrunfw.so.5", "libkrunfw.5.dylib")


def expected_binaries(wheel: Path) -> tuple[str, ...]:
    name = wheel.name.lower()
    if ("macosx" in name and "arm64" in name) or ("linux" in name and "x86_64" in name):
        return EXPECTED_BINARIES
    raise RuntimeError(f"unsupported wheel platform: {wheel.name}")


def _assert_static_linux(name: str, executable: Path) -> None:
    headers = _run(["readelf", "-W", "-l", str(executable)])
    dynamic = _run(["readelf", "-W", "-d", str(executable)])
    if "INTERP" in headers or "(NEEDED)" in dynamic:
        raise RuntimeError(
            f"{name} must be fully static: found an ELF interpreter or shared library"
        )


def _wheel_contents(
    wheel: Path,
) -> tuple[str, dict[str, zipfile.ZipInfo], zipfile.ZipInfo | None]:
    with zipfile.ZipFile(wheel) as archive:
        metadata_names = [
            name for name in archive.namelist() if name.endswith(".dist-info/METADATA")
        ]
        if len(metadata_names) != 1:
            raise RuntimeError(f"expected one METADATA file, found {metadata_names}")
        metadata = BytesParser().parsebytes(archive.read(metadata_names[0]))
        if metadata.get("Name", "").lower() != "pvisor":
            raise RuntimeError(f"expected wheel Name 'pvisor', got {metadata.get('Name')!r}")
        version = metadata.get("Version")
        if not version:
            raise RuntimeError("wheel METADATA has no Version")

        scripts: dict[str, zipfile.ZipInfo] = {}
        expected = expected_binaries(wheel)
        if any(name.endswith("/scripts/pvisor-memory-pool") for name in archive.namelist()):
            raise RuntimeError("wheel contains retired executable pvisor-memory-pool")
        if any(name.endswith("/scripts/pvisor-cache") for name in archive.namelist()):
            raise RuntimeError("wheel contains retired executable pvisor-cache")
        for name in expected:
            matches = [
                info
                for info in archive.infolist()
                if ".data/scripts/" in info.filename and info.filename.endswith(f"/scripts/{name}")
            ]
            if len(matches) != 1:
                raise RuntimeError(f"expected one wheel script {name!r}, found {len(matches)}")
            mode = matches[0].external_attr >> 16
            if mode & 0o111 == 0:
                raise RuntimeError(f"wheel script {name!r} is not executable (mode {mode:o})")
            scripts[name] = matches[0]
        firmware = [
            info
            for info in archive.infolist()
            if ".data/scripts/" in info.filename
            and info.filename.rsplit("/", 1)[-1] in FIRMWARE_NAMES
        ]
        expected_firmware = 1 if "macosx" in wheel.name.lower() else 0
        if len(firmware) != expected_firmware:
            raise RuntimeError(
                f"expected {expected_firmware} libkrunfw payloads, found {len(firmware)}"
            )
        return version, scripts, firmware[0] if firmware else None


def _run(command: list[str], *, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        command,
        check=True,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=60,
    )
    return result.stdout


def _installed_script_dir(environment: Path) -> Path:
    return environment / ("Scripts" if os.name == "nt" else "bin")


def verify_native_payloads(
    wheel: Path,
    scripts: dict[str, zipfile.ZipInfo],
    firmware: zipfile.ZipInfo | None,
) -> None:
    wheel_name = wheel.name.lower()
    expected_arches: tuple[str, ...]
    if "arm64" in wheel_name or "aarch64" in wheel_name:
        expected_arches = ("arm64", "aarch64")
    elif "x86_64" in wheel_name:
        expected_arches = ("x86_64", "x86-64")
    else:
        raise RuntimeError(f"cannot determine native architecture from wheel name: {wheel.name}")

    with tempfile.TemporaryDirectory(prefix="pvisor-wheel-native-") as temporary:
        root = Path(temporary)
        with zipfile.ZipFile(wheel) as archive:
            if firmware is not None:
                firmware_path = root / Path(firmware.filename).name
                firmware_path.write_bytes(archive.read(firmware))
                firmware_description = _run(["file", str(firmware_path)]).lower()
                if not any(arch in firmware_description for arch in expected_arches):
                    raise RuntimeError(
                        f"libkrunfw architecture does not match {wheel.name}: "
                        f"{firmware_description.strip()}"
                    )
            for name, info in scripts.items():
                executable = root / name
                executable.write_bytes(archive.read(info))
                executable.chmod(0o755)
                description = _run(["file", str(executable)]).lower()
                if not any(arch in description for arch in expected_arches):
                    raise RuntimeError(
                        f"{name} architecture does not match {wheel.name}: {description.strip()}"
                    )

                if sys.platform == "darwin" and "macosx" in wheel_name:
                    dependencies = _run(["otool", "-L", str(executable)])
                    for line in dependencies.splitlines()[1:]:
                        dependency = line.strip().split(" ", 1)[0]
                        if not dependency.startswith(("/usr/lib/", "/System/Library/")):
                            raise RuntimeError(f"{name} has non-system dependency {dependency!r}")
                    if name == "pvisor":
                        entitlements = _run(
                            ["codesign", "-d", "--entitlements", ":-", str(executable)]
                        )
                        if "com.apple.security.hypervisor" not in entitlements:
                            raise RuntimeError(
                                f"{name} is missing the Hypervisor.framework entitlement"
                            )
                elif sys.platform == "linux" and "linux" in wheel_name:
                    _assert_static_linux(name, executable)


def acceptance(
    executable: Path, wheel: Path, directory: Path, env: dict[str, str], log: Path
) -> None:
    """Exercise the installed bytes, including real VM suspend/cold restore and apply."""
    directory.mkdir(parents=True, exist_ok=True)
    workspace = directory / "workspace"
    rootfs = directory / "rootfs"
    stage = directory / "stage"
    workspace.mkdir()
    for name in ("bin", "dev", "proc", "tmp"):
        (rootfs / name).mkdir(parents=True, exist_ok=True)
    guest = directory / "guest.rs"
    guest.write_text(
        r"""use std::{fs::OpenOptions, io::{Seek, SeekFrom, Write}, time::Duration};
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--once") {
        std::fs::write("applied", b"reviewed wheel bytes").unwrap();
        return;
    }
    OpenOptions::new().create_new(true).write(true).open("started-once").unwrap();
    let mut file = OpenOptions::new().create(true).truncate(true).write(true).open("counter").unwrap();
    let mut counter = 1000u64;
    loop {
        counter += 1;
        file.seek(SeekFrom::Start(0)).unwrap();
        let bytes = format!("{counter}\n");
        file.write_all(bytes.as_bytes()).unwrap();
        file.set_len(bytes.len() as u64).unwrap();
        file.sync_all().unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
}
""",
        encoding="utf-8",
    )
    machine = platform.machine().lower()
    architecture = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(machine)
    if architecture is None:
        raise RuntimeError(f"unsupported acceptance architecture: {machine}")
    log.parent.mkdir(parents=True, exist_ok=True)
    results = {
        "wheel": wheel.name,
        "wheel_sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "steps": [],
    }

    def record(name, code, output):
        results["steps"].append({"step": name, "returncode": code, "output": output})
        log.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")

    def run(args, expected=0, *, busy_ok=False):
        result = subprocess.run(
            [str(executable), *map(str, args)],
            cwd=workspace,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=180,
        )
        record(str(args[0]), result.returncode, result.stdout + result.stderr)
        if result.returncode != expected:
            if busy_ok and "Error: Job operation already in progress:" in result.stderr:
                return None
            raise RuntimeError(
                f"acceptance {args[0]} returned {result.returncode}, expected {expected}; see {log}"
            )
        return result.stdout if expected == 0 else result.stdout + result.stderr

    compiled = subprocess.run(
        [
            "rustc",
            "--edition=2024",
            "--target",
            f"{architecture}-unknown-linux-musl",
            "-C",
            "linker=rust-lld",
            "-C",
            "panic=abort",
            str(guest),
            "-o",
            str(rootfs / "bin/probe"),
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=180,
    )
    record("compile-guest-oracle", compiled.returncode, compiled.stdout)
    if compiled.returncode:
        raise RuntimeError(f"acceptance needs the pinned musl target; see {log}")
    run(
        [
            "spawn",
            "--executor",
            "host",
            "--filesystem",
            "host",
            "--overlaynet",
            "off",
            "--",
            "/bin/sh",
            "-c",
            "exit 7",
        ],
        expected=7,
    )
    rejected = subprocess.run(
        [
            str(executable),
            "spawn",
            "--strict",
            "--executor",
            "host",
            "--",
            "/bin/sh",
            "-c",
            "touch forbidden",
        ],
        cwd=workspace,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=60,
    )
    record("strict-refusal", rejected.returncode, rejected.stdout)
    if rejected.returncode == 0 or (workspace / "forbidden").exists():
        raise RuntimeError("strict admission failed to refuse before workload execution")
    vm = [
        "spawn",
        "--executor",
        "vm",
        "--overlaynet",
        "off",
        "--memory",
        "256MiB",
        "--stdio",
        "capture",
        "--rootfs",
        rootfs,
    ]
    apply_stage = directory / "apply-stage"
    run([*vm, "--stage", apply_stage, "--", "/bin/probe", "--once"])
    review = json.loads(run(["status", apply_stage, "--review", "--json"]))
    if review.get("schema_version") != 5 or (workspace / "applied").exists():
        raise RuntimeError("fresh VM did not stage its writes with a valid review Bundle")
    run(["apply", apply_stage, "--path", "applied"])
    if (workspace / "applied").read_bytes() != b"reviewed wheel bytes":
        raise RuntimeError("apply did not publish the reviewed bytes")
    record("vm-stage-review-apply", 0, "PASS")
    processes = []
    with (log.parent / "vm-output.log").open("w") as output:
        try:
            source = subprocess.Popen(
                [str(executable), *map(str, vm), "--stage", str(stage), "--", "/bin/probe"],
                cwd=workspace,
                env=env,
                stdout=output,
                stderr=output,
            )
            processes.append(source)

            def progress(minimum):
                deadline = time.monotonic() + 120
                while time.monotonic() < deadline:
                    if processes[-1].poll() is not None:
                        raise RuntimeError("VM exited before progress; see vm-output.log")
                    if (stage / "run.json").exists():
                        observed = run(["status", stage, "--json"], busy_ok=True)
                        if observed is None:
                            time.sleep(0.1)
                            continue
                        status = json.loads(observed)
                        upper = (
                            status.get("run", {})
                            .get("overlay", {})
                            .get("upper", {})
                            .get("upper_dir")
                        )
                        if upper:
                            try:
                                counter = int((Path(upper) / "counter").read_text())
                                if counter > minimum:
                                    return status["run"], Path(upper), counter
                            except (OSError, ValueError):
                                pass
                    time.sleep(0.1)
                raise RuntimeError("VM made no progress within acceptance deadline")

            first, _, _ = progress(1000)
            suspended = json.loads(
                run(
                    [
                        "suspend",
                        stage,
                        "--ram-storage",
                        "raw",
                        "--request-id",
                        "wheel-suspend",
                        "--json",
                    ]
                )
            )
            if not suspended.get("checkpoint_id"):
                raise RuntimeError("suspend omitted its durable checkpoint")
            source.wait(timeout=60)
            stopped = json.loads(run(["status", stage, "--json"]))["run"]
            frozen_upper = Path(stopped["overlay"]["upper"]["upper_dir"])
            frozen = int((frozen_upper / "counter").read_text())
            resumed = subprocess.Popen(
                [str(executable), "resume", str(stage), "--request-id", "wheel-resume"],
                cwd=workspace,
                env=env,
                stdout=output,
                stderr=output,
            )
            processes.append(resumed)
            current, _, _ = progress(frozen)
            if current["run_id"] != first["run_id"] or current["attempt_id"] == first["attempt_id"]:
                raise RuntimeError("cold restore did not preserve Job and replace Attempt identity")
            run(["kill", stage, "--json"])
            resumed.wait(timeout=60)
            review = json.loads(run(["status", stage, "--review", "--json"]))
            if review.get("schema_version") != 5:
                raise RuntimeError("final review did not validate the protocol-aware Bundle")
            if (workspace / "counter").exists():
                raise RuntimeError("VM bypassed workspace staging")
            # Restores compose immutable checkpoint layers; apply is deliberately unsupported.
            refused = run(["apply", stage, "--path", "counter"], expected=1)
            if "composes read-only layers" not in refused or (workspace / "counter").exists():
                raise RuntimeError("restored layered Job did not refuse unsupported apply")
            record("vm-restore-stage-review", 0, "PASS; layered apply refused")
        finally:
            if any(process.poll() is None for process in processes):
                try:
                    deadline = time.monotonic() + 15
                    while run(["kill", stage, "--json"], busy_ok=True) is None:
                        if time.monotonic() >= deadline:
                            raise RuntimeError("Job stayed busy during acceptance cleanup")
                        time.sleep(0.1)
                finally:
                    for process in processes:
                        if process.poll() is None:
                            process.terminate()
                            try:
                                process.wait(timeout=15)
                            except subprocess.TimeoutExpired:
                                process.kill()
                                process.wait(timeout=15)


def install_smoke(wheel: Path, version: str, acceptance_log: Path | None = None) -> None:
    with tempfile.TemporaryDirectory(prefix="pvisor-wheel-") as temporary:
        environment = Path(temporary) / "venv"
        venv.EnvBuilder(with_pip=True, symlinks=True).create(environment)
        scripts = _installed_script_dir(environment)
        python = scripts / ("python.exe" if os.name == "nt" else "python")
        _run([str(python), "-m", "pip", "install", "--no-deps", str(wheel)])
        _run(
            [
                str(python),
                "-I",
                "-c",
                "import pvisor; from importlib.metadata import version; "
                "import sys; assert pvisor.__version__ == version('pvisor') == sys.argv[1]",
                version,
            ]
        )

        env = os.environ.copy()
        env.pop("PVISOR_BIN", None)
        env["PATH"] = os.pathsep.join((str(scripts), env.get("PATH", "")))
        for name in expected_binaries(wheel):
            executable = scripts / name
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise RuntimeError(
                    f"installed executable is missing or not executable: {executable}"
                )
            output = _run([str(executable), "--version"], env=env)
            if version not in output:
                raise RuntimeError(
                    f"{name} version output {output.strip()!r} does not contain wheel version {version!r}"
                )
            _run([str(executable), "--help"], env=env)

        _run([str(scripts / "pvisor-daemon"), "serve", "--help"], env=env)
        _run([str(scripts / "pvisor-daemon"), "protocol"], env=env)

        _run([str(scripts / "pvisor"), "run", "--help"], env=env)
        _run([str(scripts / "pvisor"), "spawn", "--help"], env=env)
        for command in ("prepare", "publish", "read"):
            _run([str(scripts / "pvisor"), "cache", command, "--help"], env=env)
        for command in ("tui", "replay"):
            _run([str(scripts / "pvisor"), command, "--help"], env=env)

        if acceptance_log is not None:
            # Private runner state; do not consume the operator's Job defaults.
            env["HOME"] = str(Path(temporary) / "home")
            Path(env["HOME"]).mkdir()
            env["PVISOR_RUN_HOME"] = str(Path(temporary) / "runs")
            acceptance(
                scripts / "pvisor", wheel, Path(temporary) / "acceptance", env, acceptance_log
            )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheel", type=Path)
    parser.add_argument("--install-smoke", action="store_true")
    parser.add_argument(
        "--acceptance-log",
        type=Path,
        help="Require real VM cold restore, stage/apply and refusal acceptance",
    )
    args = parser.parse_args()
    wheel = args.wheel.resolve()
    if not wheel.is_file():
        raise SystemExit(f"wheel does not exist: {wheel}")

    version, scripts, firmware = _wheel_contents(wheel)
    verify_native_payloads(wheel, scripts, firmware)
    print(f"wheel={wheel.name} version={version} scripts={','.join(sorted(scripts))} native=PASS")
    if args.install_smoke or args.acceptance_log:
        install_smoke(
            wheel, version, args.acceptance_log.resolve() if args.acceptance_log else None
        )
        print("install=PASS versions=PASS dispatch=PASS")


if __name__ == "__main__":
    main()
