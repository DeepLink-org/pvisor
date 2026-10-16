"""B-FS-TOOLS: three resident rounds and two fixed cache/writeback workloads.

Ready is emitted by reference_workload before this coordinator starts. Operation
clocks exclude interpreter startup and all post-operation byte validation. No
operation time includes VM exit. Every runtime executes the same system calls.
"""

import hashlib
import json
import os
from pathlib import Path
import sys
import time


OPERATIONS = ("metadata", "read", "write", "git", "rg", "cargo", "npm",
              "hot_open", "stream_write")
PATTERN = b"pvisor-workload\n"
WRITE_CHUNK = (PATTERN * (65536 // len(PATTERN) + 1))[:65536]
STREAM_CHUNK = (PATTERN * (1024 // len(PATTERN) + 1))[:1024]
STREAM_CALLS = 8192
OPEN_CALLS = 10000


def run_rounds(rounds, root, harness, checked, tools_env):
    """Keep the VM, tool outputs and caches alive across exactly three rounds."""
    if type(rounds) is not int or rounds != 3:
        raise ValueError("the resident protocol requires exactly three rounds")
    records = {operation: [] for operation in OPERATIONS}
    extra = Path(__file__).resolve()
    for number in range(1, rounds + 1):
        for operation in OPERATIONS:
            script = extra if operation in ("write", "hot_open", "stream_write") else harness / "v1/workload.py"
            output = checked([str(root / "usr/bin/python3"), str(script), operation],
                             cwd="_fs", env=tools_env())
            record = json.loads(output.strip().splitlines()[-1])
            if record["workload"] != operation:
                raise ValueError("wrong workload result")
            record["round"] = number
            records[operation].append(record)
    # Preserve the original host-side validators, including the standard write
    # directory. Consumers must use rounds, never this compatibility first clock,
    # to publish the new protocol.
    return {operation: dict(values[0], rounds=values)
            for operation, values in records.items()}


def run_operation(operation):
    """Execute the fixed call pattern and validate all written bytes afterward."""
    configuration = json.loads(Path("fixture.json").read_text())
    phases = {}
    if operation == "write":
        started = time.perf_counter_ns()
        Path("written").mkdir(exist_ok=True)
        for index in range(256):
            Path(f"written/{index:04d}").write_bytes(WRITE_CHUNK)
        total = sum(path.stat().st_size for path in Path("written").iterdir())
        elapsed = (time.perf_counter_ns() - started) / 1e6
        assert total == 256 * len(WRITE_CHUNK)
        assert all(Path(f"written/{index:04d}").read_bytes() == WRITE_CHUNK for index in range(256))
        check = dict(files=256, bytes=total, file_sha256=hashlib.sha256(WRITE_CHUNK).hexdigest())
    elif operation == "hot_open":
        target = next(Path("tree").rglob("*.txt"))
        expected = target.stat().st_size
        # Prime this exact inode once, outside the loop's timer, in every round
        # and runtime. No file bytes are read by the measured loop.
        descriptor = os.open(target, os.O_RDONLY)
        assert os.fstat(descriptor).st_size == expected
        os.close(descriptor)
        total = 0
        started = time.perf_counter_ns()
        for _ in range(OPEN_CALLS):
            descriptor = os.open(target, os.O_RDONLY)
            total += os.fstat(descriptor).st_size
            os.close(descriptor)
        elapsed = (time.perf_counter_ns() - started) / 1e6
        assert total == OPEN_CALLS * expected
        check = dict(opens=OPEN_CALLS, stats=OPEN_CALLS, closes=OPEN_CALLS,
                     file_bytes=expected, summed_sizes=total, content_reads=0)
    elif operation == "stream_write":
        # The directory setup precedes the clock. A fresh file is used every
        # round so this case always creates/writes the same amount of new data.
        Path("stream-written").mkdir(exist_ok=True)
        target = Path("stream-written") / f"{len(list(Path('stream-written').iterdir())) + 1:02d}.bin"
        started = time.perf_counter_ns()
        descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        for _ in range(STREAM_CALLS):
            if os.write(descriptor, STREAM_CHUNK) != len(STREAM_CHUNK):
                raise ValueError("short stream write")
        writes_finished = time.perf_counter_ns()
        os.fsync(descriptor)
        synced = time.perf_counter_ns()
        os.close(descriptor)
        finished = time.perf_counter_ns()
        # The main operation includes its explicit flush barrier and close;
        # separately expose write acceptance so deferred work cannot disappear.
        elapsed = (finished - started) / 1e6
        phases = dict(write_ms=(writes_finished - started) / 1e6,
                      fsync_ms=(synced - writes_finished) / 1e6,
                      close_ms=(finished - synced) / 1e6)
        expected = STREAM_CHUNK * STREAM_CALLS
        assert target.read_bytes() == expected
        check = dict(file=str(target), writes=STREAM_CALLS, chunk_bytes=len(STREAM_CHUNK),
                     bytes=len(expected), fsyncs=1, sha256=hashlib.sha256(expected).hexdigest())
    else:
        raise ValueError(operation)
    return dict(workload=operation, worker_ms=elapsed, phases_ms=phases, check=check,
                python=sys.version.split()[0],
                python_cache=dict(dont_write_bytecode=sys.dont_write_bytecode,
                                  prefix=sys.pycache_prefix,
                                  prefix_exists=bool(sys.pycache_prefix and Path(sys.pycache_prefix).exists())),
                tool_cache={name: os.environ.get(name) for name in (
                    "TMPDIR", "HOME", "CARGO_HOME", "NODE_COMPILE_CACHE", "NODE_DISABLE_COMPILE_CACHE", "NODE_OPTIONS")},
                tool_scratch=os.environ.get("PVISOR_REFERENCE_TOOL_SCRATCH", "executor"))


if __name__ == "__main__":
    print(json.dumps(run_operation(sys.argv[1])), flush=True)
