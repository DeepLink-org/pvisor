"""Test bootstrap for local source imports."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
root_str = str(ROOT)
if root_str not in sys.path:
    sys.path.insert(0, root_str)


def pytest_addoption(parser):
    parser.addoption("--vm-bin", help="Signed pvisor CLI for real VM integration tests")


@pytest.fixture
def vm_bin(request):
    binary = request.config.getoption("--vm-bin")
    if not binary:
        pytest.skip("requires --vm-bin and VM hardware")
    path = Path(binary).resolve()
    if not path.is_file():
        pytest.fail(f"VM CLI does not exist: {path}")
    return str(path)
