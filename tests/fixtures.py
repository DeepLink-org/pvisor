"""Shared temporary workspace and scoped cleanup for host regression tests."""

import tempfile
from contextlib import ExitStack, contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


@contextmanager
def workspace():
    with tempfile.TemporaryDirectory(prefix="pvisor-test-") as directory, ExitStack() as resources:
        yield Path(directory).resolve(), resources
