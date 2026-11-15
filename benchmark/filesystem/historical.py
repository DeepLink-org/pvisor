"""Read-only contracts for auditing historical Python filesystem observations.

No clocks, subprocess execution or sampling live here. Rust/Criterion owns new
measurements; these declarations keep the original evidence interpretable.
"""

import json
import sys

from archive import FIELDS as FIELDS
from archive import summarize as summarize
from common import FILESYSTEM_SCOPE as SCOPE
from common import ROOT
from workload import MODES

__all__ = ["FIELDS", "SCOPE", "summarize"]

RUNS = 30
WARMUP = 3
TIMEOUT = 30
FIXTURE_SCOPE = "native Python whole-command probes; 2048 files in 32 directories; 64 MiB read with SHA256; warm cache; distinct from seven-tool internal timings"
ROWS = {"native-" + mode: mode for mode in MODES}


def command(batch, mode):
    return [sys.executable, str(ROOT / "filesystem/workload.py"), mode, str(batch / "fixture")]


def validate_result(record, mode):
    if record["exit_code"] != 0 or json.loads(record["stdout"]) != dict(
        workload=mode, correctness="passed"
    ):
        raise ValueError("filesystem subject failed or returned incorrect output")
