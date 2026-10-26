"""Experiment-only arrival planning and evidence helpers (no product changes)."""

import hashlib
import json
import os
import random
import sys
from pathlib import Path

# Import is side-effect free; execution requires an explicit prepared context.
CONTEXT_PATH = os.environ.get("PVISOR_SWE_CONTEXT")
CONTEXT = json.loads(Path(CONTEXT_PATH).read_text()) if CONTEXT_PATH else {}
ROOT = Path(CONTEXT.get("root", "/nonexistent-swe-benchmark"))
SSD = Path(CONTEXT.get("ssd", "/nonexistent-swe-benchmark-ssd"))
PY = sys.executable
SLICE = CONTEXT.get("slice", "pvisor-benchmark-test.slice")
SUPERVISOR = CONTEXT.get("supervisor", "pvisor-benchmark-test.service")
VM_CPUS = CONTEXT.get("vm_cpus", "2-15,18-31,34-47,50-63")
HELPER_CPUS = CONTEXT.get("helper_cpus", "0,16,32,48")
GIB = 2**30
TASKS = ["qutebrowser-667", "vuls-666", "nodebb-291"]


def save(path, value):
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2) + "\n")
    tmp.replace(path)


def load(path):
    return json.loads(path.read_text())


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def cpus(spec):
    result = set()
    for item in spec.split(","):
        ends = list(map(int, item.split("-")))
        assert len(ends) <= 2 and ends[-1] >= ends[0]
        result.update(range(ends[0], ends[-1] + 1))
    return result


def schedule(rate_per_min, duration_s, seed):
    assert rate_per_min > 0 and duration_s > 0
    rng = random.Random(seed)
    rows = []
    at = 0.0
    block = []
    while at < duration_s:
        if not block:
            block = list(TASKS)
            rng.shuffle(block)
        rows.append(dict(index=len(rows), at_s=at, task=block.pop()))
        at += 60 / rate_per_min * rng.uniform(0.5, 1.5)
    return rows


def counters(path):
    return {k: int(v) for k, v in (line.split() for line in path.read_text().splitlines())}


def cgroup_snapshot(group):
    return dict(
        cpu_max=(group / "cpu.max").read_text().strip() if (group / "cpu.max").exists() else None,
        memory_current=int((group / "memory.current").read_text()),
        cpu=counters(group / "cpu.stat"),
        memory_events=counters(group / "memory.events"),
        swap_current=int((group / "memory.swap.current").read_text()),
        memory_stat=counters(group / "memory.stat"),
        io=(group / "io.stat").read_text() if (group / "io.stat").exists() else None,
        io_accounting_available=(group / "io.stat").exists(),
        pressure={k: (group / (k + ".pressure")).read_text() for k in ["cpu", "memory", "io"]},
    )


def quantiles(xs):
    if not xs:
        return dict(n=0)
    values = sorted(xs)

    def p(q):
        at = (len(values) - 1) * q
        i = int(at)
        return values[i] + (values[min(i + 1, len(values) - 1)] - values[i]) * (at - i)

    return dict(n=len(xs), p50=p(0.5), p95=p(0.95), p99=p(0.99), maximum=max(xs))


class JsonlTail:
    def __init__(self, path):
        self.path = path
        self.offset = 0
        self.partial = b""
        self.lag_bytes = 0

    def read(self):
        if not self.path.exists():
            return []
        with self.path.open("rb") as f:
            f.seek(self.offset)
            chunk = f.read(256 * 1024)
            self.offset = f.tell()
            self.lag_bytes = max(0, self.path.stat().st_size - self.offset)
        parts = (self.partial + chunk).split(b"\n")
        self.partial = parts.pop()
        return [json.loads(s) for s in parts if s]


class PhaseTracker:
    def __init__(self, vm):
        self.events = JsonlTail(vm / "events.jsonl")
        self.vm = False
        self.agent = False
        self.wait = False
        self.tools = set()
        self.state = "running"

    def update(self):
        for e in self.events.read():
            name = e["event"]
            if name == "vm_start":
                self.vm = True
            elif name == "vm_stop":
                self.vm = False
                self.state = "stopped"
            elif name == "trial.agent_start":
                self.agent = True
            elif name == "trial.agent_end":
                self.agent = False
                self.wait = False
            elif name == "model_request_start":
                self.wait = True
            elif name in ["model_request_end", "model_request_error"]:
                self.wait = False
            elif name == "tool_start":
                self.tools.add(e["execution_id"])
            elif name == "tool_end":
                self.tools.discard(e["execution_id"])
        return dict(
            vm=int(self.vm),
            agent=int(self.vm and self.agent),
            model_wait=int(self.vm and self.agent and self.wait),
            tool_exec=int(self.vm and self.agent and bool(self.tools) and not self.wait),
            state_not_sampled=int(self.vm),
            log_lag_bytes=self.events.lag_bytes,
        )
