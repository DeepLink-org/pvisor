"""Shared pytest options for host, benchmark and opt-in integration checks."""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

import pytest


def pytest_addoption(parser):
    parser.addoption("--vm-bin", help="Signed pvisor CLI for real VM integration tests")
    parser.addoption(
        "--guest-fs-dir",
        action="append",
        default=[],
        help="Filesystem directory to test inside a Linux VM; repeat for multiple mounts",
    )
    parser.addoption(
        "--zcode-integration",
        action="store_true",
        help="Run Linux zcode integration using PVISOR_BIN",
    )


def pytest_generate_tests(metafunc):
    if "guest_fs_dir" in metafunc.fixturenames:
        metafunc.parametrize(
            "guest_fs_dir", metafunc.config.getoption("--guest-fs-dir") or [None], indirect=True
        )


@pytest.fixture
def guest_fs_dir(request):
    if request.param is None:
        pytest.skip("requires --guest-fs-dir inside a Linux VM")
    if sys.platform != "linux" or os.geteuid() != 0:
        pytest.fail("guest filesystem checks require Linux and root inside the VM")
    with tempfile.TemporaryDirectory(prefix="pvisor-fs-", dir=request.param) as directory:
        yield Path(directory).absolute()


@pytest.fixture
def vm_bin(request):
    binary = request.config.getoption("--vm-bin")
    if not binary:
        pytest.skip("requires --vm-bin and VM hardware")
    path = Path(binary).resolve()
    if not path.is_file():
        pytest.fail(f"VM CLI does not exist: {path}")
    return str(path)
