"""Round17 owned launcher; execute only after the coordinator grants the slot.

One command per case, from the repository root, with explicit campaign inputs:
  PVISOR_PLUGIN_EXTENDED_LAUNCH=1 \
  PVISOR_PLUGIN_BINARY=/absolute/read-only/pvisor \
  PVISOR_PLUGIN_EXTENDED_BINARY_SHA256='<64-hex-digit-approved-sha256>' \
  PVISOR_PLUGIN_EXTENDED_DEADLINE='<timezone-aware-ISO-UTC-deadline>' \
  python3 plugins/test/launch-extended-recovery.py cancel

Replace cancel with failure, server-death or stale-approval only after the prior
case's evidence and cleanup have been reviewed. This is not a public mise task.
The launcher creates durable exclusive evidence, never installs or rebuilds,
and checks that TAP executed the requested title exactly once (not just wrappers).
"""

import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

MODES = ("cancel", "failure", "server-death", "stale-approval")


def utc():
    return datetime.datetime.now(datetime.timezone.utc)


def digest(file):
    hasher = hashlib.sha256()
    with file.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def main():
    if os.environ.get("PVISOR_PLUGIN_EXTENDED_LAUNCH") != "1":
        raise RuntimeError("No launch authorization: require exact explicit launch opt-in 1")
    if len(sys.argv) != 2 or sys.argv[1] not in MODES:
        raise RuntimeError(f"Choose exactly one case: {MODES}")
    binary_input = os.environ.get("PVISOR_PLUGIN_BINARY", "")
    if not binary_input or not Path(binary_input).is_absolute():
        raise RuntimeError("Require explicit absolute PVISOR_PLUGIN_BINARY")
    binary = Path(binary_input).resolve(strict=True)
    expected_hash = os.environ.get("PVISOR_PLUGIN_EXTENDED_BINARY_SHA256", "")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", expected_hash):
        raise RuntimeError("Require explicit 64-hex PVISOR_PLUGIN_EXTENDED_BINARY_SHA256")
    deadline_input = os.environ.get("PVISOR_PLUGIN_EXTENDED_DEADLINE", "")
    deadline = datetime.datetime.fromisoformat(deadline_input.replace("Z", "+00:00"))
    if deadline.utcoffset() != datetime.timedelta(0):
        raise RuntimeError("Require timezone-aware ISO UTC PVISOR_PLUGIN_EXTENDED_DEADLINE")
    if utc() >= deadline:
        raise RuntimeError("Campaign deadline reached: no new workload")
    root = Path.cwd().resolve()
    if not (root / "plugins/test/extended-recovery.native.mjs").is_file():
        raise RuntimeError("Run from the attached pvisor repository root")
    mode = sys.argv[1]
    title = f"extended recovery {mode}: exact-stage long-lived recovery"
    # A failed/unknown attempt must never overwrite a previous evidence frontier.
    owned = root / ".asv8/round17" / f"{mode}-{utc().strftime('%Y%m%dT%H%M%S%fZ')}"
    owned.mkdir(parents=True, exist_ok=False)
    journal = owned / "launcher.jsonl"

    def log(event, **detail):
        with journal.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps({"utc": utc().isoformat(), "event": event, **detail}) + "\n")

    def resources(phase):
        memory = Path("/proc/meminfo").read_text()
        available = int(re.search(r"^MemAvailable:\s+(\d+)", memory, re.M)[1]) * 1024
        disk = shutil.disk_usage(owned).free
        load = os.getloadavg()[0]
        log("resources", phase=phase, available=available, disk=disk, load1=load)
        if available < 6 * 1024**3 or disk < 20 * 1024**3 or load > 8:
            raise RuntimeError(f"Resource gate crossed at {phase}")

    process = None
    primary_failure = None
    started = time.monotonic()
    try:
        resources("preflight")
        actual_hash = digest(binary)
        log(
            "artifact",
            path=str(binary),
            sha256=actual_hash,
            read_only=True,
            approved_sha256=expected_hash.lower(),
            deadline=deadline.isoformat(),
        )
        if actual_hash != expected_hash.lower():
            raise RuntimeError(
                "Read-only native artifact hash differs; do not rebuild or substitute"
            )
        sources = sorted((root / "plugins").glob("*.mjs")) + sorted(
            (root / "plugins/test").glob("*.mjs")
        )
        sources.append(Path(__file__).resolve())
        sources.append(root / "mise.toml")
        before_sources = {str(file.relative_to(root)): digest(file) for file in sources}
        log("source frontier", sha256=before_sources)
        node = Path("/usr/bin/node")
        mise = shutil.which("mise")
        if not node.is_file() or not mise:
            raise RuntimeError("Existing /usr/bin/node and mise required; no installs")
        shim = owned / "bin"
        shim.mkdir()
        # mise task already supplies --test; strip exactly that one flag.
        # Filters are injected before filenames, never appended to the mise call.
        node_shim = shim / "node"
        node_shim.write_text(
            "#!/bin/sh\nset -eu\n"
            '[ "$#" -gt 0 ] && [ "$1" = "--test" ] || exit 64\n'
            "shift\n"
            "exec /usr/bin/node --test --test-concurrency=1 --test-timeout=800000 "
            '--test-reporter=tap --test-name-pattern="$ROUND17_FILTER" "$@"\n'
        )
        node_shim.chmod(0o700)
        temporary = owned / "tmp"
        output = owned / "evidence"
        temporary.mkdir()
        output.mkdir()
        env = {
            **os.environ,
            "PATH": str(shim) + os.pathsep + os.environ["PATH"],
            "MISE_TRUSTED_CONFIG_PATHS": str(root),
            "MISE_YES": "1",
            "MISE_AUTO_INSTALL": "0",
            "PVISOR_PLUGIN_EXTENDED_RECOVERY": "1",
            "PVISOR_PLUGIN_EXTENDED_CASE": mode,
            "PVISOR_PLUGIN_EXTENDED_DEADLINE": deadline.isoformat(),
            "PVISOR_PLUGIN_BINARY": str(binary),
            "PVISOR_PLUGIN_TEST_OUTPUT_DIR": str(output),
            "TMPDIR": str(temporary),
            "ROUND17_FILTER": "^" + re.escape(title) + "$",
            "CARGO_BUILD_JOBS": "2",
            "RUST_TEST_THREADS": "1",
            "NEXTEST_TEST_THREADS": "1",
            "OMP_NUM_THREADS": "1",
            "OPENBLAS_NUM_THREADS": "1",
        }
        command = [
            "nice",
            "-n",
            "10",
            "timeout",
            "-k",
            "30s",
            "850s",
            mise,
            "run",
            "test-agent-plugins-native",
        ]
        if utc() >= deadline:
            raise RuntimeError("Campaign deadline reached during setup: no new workload")
        # Record before launch; no retry is performed on any exception or failure.
        log(
            "launch attempt",
            command=command,
            exact_title=title,
            pattern=env["ROUND17_FILTER"],
            owned=str(owned),
        )
        print(f"Round17 {mode}: durable evidence {owned}", flush=True)
        tap = owned / "native.tap"
        with tap.open("xb") as stream:
            process = subprocess.Popen(
                command, cwd=root, env=env, stdout=stream, stderr=subprocess.STDOUT
            )
            log("owned launcher child", pid=process.pid)
            code = process.wait(timeout=885)
        log("command exit", code=code, elapsed_seconds=time.monotonic() - started)
        text = tap.read_text(errors="replace")
        # A skipped name or an empty passing file wrapper is not an executed case.
        lines = text.splitlines()
        headers = [line for line in lines if line == "# Subtest: " + title]
        passes = [line for line in lines if re.fullmatch(r"ok \d+ - " + re.escape(title), line)]
        other_passes = [
            line
            for line in lines
            if re.match(r"^ok \d+ - ", line) and "# SKIP" not in line and line not in passes
        ]
        log(
            "TAP selection",
            actual_headers=len(headers),
            actual_passes=len(passes),
            other_reported_passes=other_passes,
            exact_title=title,
        )
        after_artifact = digest(binary)
        after_sources = {str(file.relative_to(root)): digest(file) for file in sources}
        changed_sources = {
            file: {"before": before_sources[file], "after": after_sources[file]}
            for file in before_sources
            if before_sources[file] != after_sources[file]
        }
        log(
            "post-run frontier",
            artifact_sha256=after_artifact,
            sha256=after_sources,
            changed_sources=changed_sources,
            artifact_changed=after_artifact != actual_hash,
        )
        if code != 0 or len(headers) != 1 or len(passes) != 1:
            raise RuntimeError(
                f"Requested case did not execute and pass exactly once; inspect {tap}"
            )
        if after_artifact != actual_hash or changed_sources:
            raise RuntimeError(
                "Executed frontier changed during the case; retain evidence without a stable-frontier PASS"
            )
    except BaseException as failure:
        primary_failure = failure
        log(
            "launcher failure",
            type=type(failure).__name__,
            message=str(failure),
            preserve=str(owned),
        )
        raise
    finally:
        # Signal only our owned timeout child. GNU timeout forwards TERM to its
        # command; its own 850+30 bound is the primary supervisor, not this wait.
        cleanup_errors = []
        try:
            if process is not None and process.poll() is None:
                log("owned launcher cancellation", pid=process.pid)
                process.terminate()
                try:
                    process.wait(timeout=32)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                    log("uncertain descendant ownership", preserve=str(owned), no_next_case=True)
        except BaseException as cleanup_error:
            cleanup_errors.append(cleanup_error)
        try:
            resources("postflight")
        except BaseException as cleanup_error:
            cleanup_errors.append(cleanup_error)
        log(
            "launcher end",
            elapsed_seconds=time.monotonic() - started,
            retained=str(owned),
            cleanup_errors=[str(error) for error in cleanup_errors],
        )
        if cleanup_errors and primary_failure is None:
            raise cleanup_errors[0]
    print(
        f"PASS: exactly one requested case and stable frontier; inspect {journal} and {output}",
        flush=True,
    )


if __name__ == "__main__":
    main()
