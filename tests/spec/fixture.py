"""Own pVisor scenario resources; Scrut owns Markdown discovery and matching."""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parents[1]


def stop_group(process):
    """Always terminate owned descendants, including after normal Bash exit."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    # On Linux the caller observes exit with WNOWAIT, retaining the group
    # leader's PID until both signals are sent. Do not reap that anchor early.
    time.sleep(0.05)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def adopt_detached_children():
    """Keep orphaned Linux descendants owned by this short-lived fixture."""
    if sys.platform != "linux":
        return
    # PR_SET_CHILD_SUBREAPER: a setsid/double-fork cannot hand its descendants
    # to init and escape cleanup. This changes only this fixture process.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def stop_adopted_children():
    """Reap only our children; never identify ownership by command name."""
    if sys.platform != "linux":
        return
    children = Path(f"/proc/self/task/{os.getpid()}/children")
    deadline = time.monotonic() + 3
    while True:
        owned = [int(pid) for pid in children.read_text().split()]
        if not owned:
            return
        for pid in owned:
            # Unreaped children retain their PID, so it cannot be reused here.
            # Killing a parent may adopt another generation on the next pass.
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, os.WNOHANG)
        if time.monotonic() >= deadline:
            raise RuntimeError(f"owned children did not terminate: {owned}")
        time.sleep(0.01)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("vocabulary", choices=("use", "stage", "doc", "vm", "none"))
    parser.add_argument("identifier")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--xfail-platform", choices=("macos", "linux", "all"))
    parser.add_argument("--xfail-reason")
    args = parser.parse_args(argv)
    if args.timeout <= 0:
        parser.error("timeout must be positive")
    if bool(args.xfail_platform) != bool(args.xfail_reason):
        parser.error("xfail platform and reason must be specified together")
    # Keep scenario roots in the host temporary area, as before the migration.
    # In particular --safe gives /tmp a private writable view; moving an
    # outside-workspace probe under the checkout changes the tested policy.
    # Do not use Scrut's TMPDIR: Scrut removes it even after test failure.
    artifact_root = Path(
        os.environ.get("PVISOR_CASE_ARTIFACTS", f"/tmp/pvisor-cases-{os.getuid()}")
    )
    artifact_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix=args.identifier + "-", dir=artifact_root)).resolve()
    workspace = root / "ws"
    workspace.mkdir()
    # Retained evidence must not silently source a later revision's fixtures.
    fixtures = root / "fixtures"
    shutil.copytree(
        HERE / "fixtures", fixtures, ignore=shutil.ignore_patterns("regression", "__pycache__")
    )
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("BASH_FUNC_")
        and key not in ("BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS")
    }
    environment.update(
        CASE_ROOT=str(root),
        WS=str(workspace),
        CASE_FIXTURES=str(fixtures),
        PVISOR_PROJECT_ROOT=str(PROJECT),
        HOME=str(root / "home"),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
        XDG_CACHE_HOME=str(root / "cache"),
        PVISOR_RUN_HOME=str(root / "sessions"),
        # Explicit caller overrides must not escape this scenario's ownership.
        # Leave stores absent so read-only/invalid-command checks can detect
        # accidental initialization.
        PVISOR_DOCKER_HOME=str(root / "docker"),
        PVISOR_IMAGE_STORE=str(root / "images"),
    )
    for name in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "PVISOR_RUN_HOME"):
        Path(environment[name]).mkdir()
    setup = {
        "none": [],
        "use": ["use"],
        "stage": ["use", "stage"],
        "doc": ["doc"],
        "vm": ["doc", "vm"],
    }[args.vocabulary]
    prelude = "set -euo pipefail\n"
    prelude += "".join(f'source "$CASE_FIXTURES/{name}.bash"\n' for name in setup)
    script = root / "check.bash"
    script.write_text(prelude + sys.stdin.read(), encoding="utf-8")
    process = None
    interrupted = None

    def interrupt(signum, _frame):
        raise InterruptedError(signum)

    previous = {sig: signal.signal(sig, interrupt) for sig in (signal.SIGINT, signal.SIGTERM)}
    # Scrut may disappear without signalling grandchildren. Watch the invoking
    # shell and its owner as well as our own parent relationship.
    parent = os.getppid()
    owner = int(
        subprocess.check_output(["ps", "-o", "ppid=", "-p", str(parent)], text=True).strip()
    )
    timed_out = False
    adopt_detached_children()
    try:
        with (root / "stdout").open("wb") as stdout, (root / "stderr").open("wb") as stderr:
            process = subprocess.Popen(
                ["bash", "--noprofile", "--norc", str(script)],
                cwd=workspace,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
            )
            deadline = time.monotonic() + args.timeout
            while True:
                if sys.platform == "linux":
                    # Keep the exited anchor waitable until group cleanup so
                    # its PID/PGID cannot be reused for an unrelated process.
                    exited = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    if exited is not None:
                        break
                    time.sleep(min(0.05, max(0.001, deadline - time.monotonic())))
                else:
                    try:
                        process.wait(timeout=min(0.1, max(0.001, deadline - time.monotonic())))
                        break
                    except subprocess.TimeoutExpired:
                        pass
                if time.monotonic() >= deadline:
                    timed_out = True
                    break
                if os.getppid() != parent:
                    raise InterruptedError(signal.SIGTERM)
                try:
                    os.kill(owner, 0)
                except ProcessLookupError:
                    raise InterruptedError(signal.SIGTERM) from None
    except InterruptedError as error:
        interrupted = int(error.args[0])
    finally:
        if process is not None:
            stop_group(process)
        stop_adopted_children()
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    code = process.returncode if process is not None else None
    platform = "macos" if sys.platform == "darwin" else sys.platform
    xfail = args.xfail_platform in ("all", platform)
    if interrupted:
        verdict = "INTERRUPTED"
    elif timed_out:
        verdict = "TIMEOUT"
    elif code == 77:
        verdict = "SKIP"
    elif code == 0:
        verdict = "XPASS" if xfail else "PASS"
    elif xfail and code is not None and 1 <= code <= 125:
        verdict = "XFAIL"
    else:
        verdict = "FAIL"
    success = verdict == "PASS" or (
        verdict == "XFAIL"
        and os.environ.get("PVISOR_CASE_REQUIRE_PASS") != "1"
        and not args.identifier.startswith("S-USE-")
    )
    print(f"{verdict} {args.identifier}", flush=True)
    if not success or verdict == "XFAIL" or os.environ.get("PVISOR_CASE_KEEP") == "1":
        # Native Scrut JSON intentionally omits successful command output.
        # Keep accepted XFAIL observations visible without inventing a second
        # runner report format or calling an accepted exception a product PASS.
        (root / "result.json").write_text(
            json.dumps(
                {
                    "id": args.identifier,
                    "verdict": verdict,
                    "exit_code": code,
                    "xfail_reason": args.xfail_reason,
                    "subject": environment.get("SUBJECT_BIN"),
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        if not success:
            for stream in ("stdout", "stderr"):
                sys.stderr.write(f"\n--- {stream}: {root / stream} ---\n")
                sys.stderr.flush()
                sys.stderr.buffer.write((root / stream).read_bytes())
            sys.stderr.write(f"\nRetained scenario resources: {root}\n")
            if args.xfail_reason:
                sys.stderr.write(f"Known violation: {args.xfail_reason}\n")
    else:
        shutil.rmtree(root)
    return 128 + interrupted if interrupted else 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
