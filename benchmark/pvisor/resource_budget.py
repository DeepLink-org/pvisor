"""Read-only resource witnesses for registered reference-runtime cohorts.

This helper supports B-STARTUP/B-FS-TOOLS/B-AGENT-TASK; it creates no groups,
moves no processes and supplies no measurements by itself. A caller must
establish its private budget before timing, identify every required process
scope, retain witnesses and classify missed process lifetimes as unknown.
"""

from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath


def parse_cpus(value):
    """Parse kernel CPU lists, rejecting malformed or overlapping intervals."""
    cpus = set()
    for component in value.strip().split(','):
        bounds = component.split('-')
        if len(bounds) not in (1, 2) or any(not x.isdigit() for x in bounds):
            raise ValueError('invalid CPU list')
        first, last = int(bounds[0]), int(bounds[-1])
        if first > last:
            raise ValueError('reversed CPU interval')
        # Kernel lists are small; reject unbounded caller-supplied ranges.
        if last > 1048575:
            raise ValueError('CPU index exceeds supported list range')
        interval = set(range(first, last + 1))
        if cpus & interval:
            raise ValueError('overlapping CPU intervals')
        cpus.update(interval)
    return cpus


def unified_path(value):
    lines = [line.removeprefix('0::') for line in value.splitlines() if line.startswith('0::')]
    if len(lines) != 1:
        raise ValueError('missing unique cgroup v2 membership')
    raw = lines[0]
    path = PurePosixPath(raw)
    if not path.is_absolute() or '..' in path.parts or str(path) != raw:
        raise ValueError('noncanonical cgroup membership')
    return path


def counters(value):
    result = {}
    for line in value.splitlines():
        key, count = line.split()
        if key in result or not count.isdigit():
            raise ValueError('invalid cgroup counter')
        result[key] = int(count)
    return result


@dataclass(frozen=True)
class ResourceBudget:
    group: Path
    memory_bytes: int
    cpus: frozenset
    cpu_count: int = 2
    cpu_placement: str = 'cpuset'
    cgroup_root: Path = field(default_factory=lambda: Path('/sys/fs/cgroup'))
    proc_root: Path = field(default_factory=lambda: Path('/proc'))

    def __post_init__(self):
        root, group = self.cgroup_root.resolve(), self.group.resolve()
        if group == root or not group.is_relative_to(root):
            raise ValueError('budget must be a private descendant cgroup')
        if (type(self.memory_bytes) is not int or self.memory_bytes <= 0
                or type(self.cpu_count) is not int or self.cpu_count <= 0):
            raise ValueError('budget values must be positive integers')
        if (len(self.cpus) != self.cpu_count
                or any(type(cpu) is not int or cpu < 0 for cpu in self.cpus)):
            raise ValueError('CPU set must contain exactly the declared physical CPUs')
        if self.cpu_placement not in ('cpuset', 'affinity'):
            raise ValueError('CPU placement must be explicitly cpuset or affinity')
        object.__setattr__(self, 'group', group)
        object.__setattr__(self, 'cgroup_root', root)
        object.__setattr__(self, 'proc_root', self.proc_root.resolve())
        object.__setattr__(self, 'cpus', frozenset(self.cpus))

    def read(self):
        """Require the actual whole-parent limits; VM configured RAM is separate."""
        group = self.group
        if (group / 'cgroup.type').read_text().strip() != 'domain':
            raise ValueError('budget is not a domain cgroup')
        memory_max = (group / 'memory.max').read_text().strip()
        swap_max = (group / 'memory.swap.max').read_text().strip()
        quota, period = (group / 'cpu.max').read_text().split()
        if memory_max != str(self.memory_bytes) or swap_max != '0':
            raise ValueError('whole-parent memory/swap limits do not match')
        if not quota.isdigit() or not period.isdigit() or int(period) <= 0:
            raise ValueError('CPU quota is unavailable')
        if int(quota) != self.cpu_count * int(period):
            raise ValueError('whole-parent CPU quota does not match')
        cpuset_file = group / 'cpuset.cpus.effective'
        effective = parse_cpus(cpuset_file.read_text()) if cpuset_file.exists() else None
        if self.cpu_placement == 'cpuset' and effective != self.cpus:
            raise ValueError('whole-parent effective CPUs do not match')
        return dict(
            cgroup=str(group), memory_max_bytes=int(memory_max), swap_max_bytes=0,
            cpu_quota=int(quota), cpu_period=int(period),
            effective_cpus=sorted(effective) if effective is not None else None,
            declared_cpu_set=sorted(self.cpus), cpu_placement=self.cpu_placement,
            memory_current_bytes=int((group / 'memory.current').read_text()),
            memory_peak_bytes=int((group / 'memory.peak').read_text()),
            memory_peak_scope='cgroup lifetime high-water mark; not a per-task peak without a separate verified reset',
            memory_stat=counters((group / 'memory.stat').read_text()),
            memory_events=counters((group / 'memory.events').read_text()),
            cpu_stat=counters((group / 'cpu.stat').read_text()),
            scope='whole selected cgroup and descendants; precharged shared cache outside this parent is excluded',
        )

    def process(self, pid):
        """A missing/exited PID is not a successful membership witness."""
        if type(pid) is not int or pid <= 0:
            raise ValueError('invalid PID')
        proc = self.proc_root / str(pid)

        def identity(directory=proc, expected_pid=pid):
            text = (directory / 'stat').read_text()
            prefix, fields = text.rsplit(') ', 1)
            if prefix.split(' (', 1)[0] != str(expected_pid):
                raise ValueError('process stat PID does not match')
            fields = fields.split()
            if fields[0] in ('Z', 'X', 'x'):
                raise ValueError('process already exited')
            return int(fields[19])

        started = identity()
        path = unified_path((proc / 'cgroup').read_text())
        expected = PurePosixPath('/') / self.group.relative_to(self.cgroup_root).as_posix()
        if not path.is_relative_to(expected):
            raise ValueError('process escaped whole-parent budget')
        threads = []
        if self.cpu_placement == 'affinity':
            def thread_ids():
                ids = {int(p.name) for p in (proc / 'task').iterdir() if p.name.isdigit()}
                if not ids or pid not in ids:
                    raise ValueError('missing complete thread scope')
                return ids

            initial_threads = thread_ids()
            for tid in sorted(initial_threads):
                task = proc / 'task' / str(tid)
                task_started = identity(task, tid)
                task_path = unified_path((task / 'cgroup').read_text())
                if not task_path.is_relative_to(expected):
                    raise ValueError('thread escaped whole-parent budget')
                lists = [line.split(':', 1)[1].strip()
                         for line in (task / 'status').read_text().splitlines()
                         if line.startswith('Cpus_allowed_list:')]
                if len(lists) != 1 or parse_cpus(lists[0]) != self.cpus:
                    raise ValueError('thread CPU affinity does not match')
                if identity(task, tid) != task_started or unified_path((task / 'cgroup').read_text()) != task_path:
                    raise ValueError('thread identity or cgroup changed during witness')
                threads.append(dict(tid=tid, start_ticks=task_started,
                                    cgroup=str(task_path), cpus=sorted(self.cpus)))
            if thread_ids() != initial_threads:
                raise ValueError('thread scope changed during witness')
        # Read identity and membership again to reject PID reuse or migration
        # within this observation. Future migration requires further witnesses.
        if identity() != started or unified_path((proc / 'cgroup').read_text()) != path:
            raise ValueError('process identity or cgroup changed during witness')
        witness = dict(pid=pid, start_ticks=started, cgroup=str(path),
                       scope='this live PID at observation; not all descendants or future membership')
        if self.cpu_placement == 'affinity':
            witness.update(cpu_placement='affinity', threads=threads)
        return witness

    def processes(self, pids):
        """Validate explicit scopes; the caller owns complete process discovery."""
        pids = list(pids)
        if not pids or len(pids) != len(set(pids)):
            raise ValueError('process scopes must be nonempty and unique')
        before = self.read()
        witnesses = [self.process(pid) for pid in pids]
        after = self.read()
        return dict(before=before, after=after, witnesses=witnesses,
                    scope='only the enumerated live PIDs; absent scope coverage cannot be claimed')
