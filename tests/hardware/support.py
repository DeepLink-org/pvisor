"""Artifact and response helpers for the native HVF probes; no test execution policy."""

import json
import os
import platform
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def require_hvf():
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("requires Apple Silicon macOS with Hypervisor entitlement")


def target_dir():
    return Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()


def binary():
    return Path(os.environ["PVISOR_VM_PROBE_BINARY"]).resolve(strict=True)


def guest_and_firmware():
    guest = (target_dir() / "debug/probes/guest_linux").resolve(strict=True)
    firmware = Path(os.environ.get("PVISOR_CASE_VM_LIBRARY_DIR", target_dir() / "release")).resolve(
        strict=True
    )
    if not (firmware / "libkrunfw.5.dylib").is_file():
        raise RuntimeError("Set PVISOR_CASE_VM_LIBRARY_DIR to a local built firmware directory")
    return guest, firmware


def assert_response(test, result, error=None):
    detail = f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    if error is None:
        test.assertEqual(result.returncode, 0, detail)
        test.assertNotIn("panicked", result.stderr, detail)
    else:
        test.assertNotEqual(result.returncode, 0, detail)
        test.assertIn(error, result.stderr, detail)


def write_evidence(record):
    """Write optional probe data; unittest alone owns the verdict."""
    destination = os.environ.get("PVISOR_VM_PROBE_REPORT")
    if destination:
        path = Path(destination)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(record, ensure_ascii=False, indent=2) + "\n")
