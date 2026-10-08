"""Resource checks need actual descendants, not just selected launch roots."""
from pathlib import Path

from v1 import common


def test_snapshot_returns_full_owned_pid_scope_including_detached_roots(tmp_path, monkeypatch):
    proc = tmp_path / 'proc'
    proc.mkdir()
    parents = {100: (1, 10), 101: (100, 20), 102: (101, 30),
               200: (1, 11), 201: (200, 12), 300: (1, 1000)}
    for pid, (parent, rss) in parents.items():
        directory = proc / str(pid)
        directory.mkdir()
        (directory / 'stat').write_text(f'{pid} (name ) with parentheses) S {parent} 0\n')
        (directory / 'status').write_text(f'VmRSS:\t{rss} kB\n')
    real_path = Path
    monkeypatch.setattr(common, 'Path', lambda value: proc if value == '/proc' else real_path(value))
    roots = {100, 200}  # owned launcher plus independently found detached shim
    rss, count, pids = common.snapshot(roots, include_pids=True)
    assert pids == {100, 101, 102, 200, 201}
    assert rss == 83 and count == 5
    assert common.snapshot(roots) == (83, 5)


def test_missing_launcher_is_not_returned_as_observed_live_pid(tmp_path, monkeypatch):
    proc = tmp_path / 'proc'
    proc.mkdir()
    monkeypatch.setattr(common, 'Path', lambda _: proc)
    assert common.snapshot({123}, include_pids=True) == (0, 0, set())


def test_child_scope_is_not_hidden_by_unavailable_rss(tmp_path, monkeypatch):
    proc = tmp_path / 'proc'
    proc.mkdir()
    for pid, parent in [(100, 1), (101, 100)]:
        directory = proc / str(pid)
        directory.mkdir()
        (directory / 'stat').write_text(f'{pid} (worker) S {parent} 0\n')
        (directory / 'status').write_text('VmRSS: 10 kB\n' if pid == 100 else 'Name: worker\n')
    monkeypatch.setattr(common, 'Path', lambda _: proc)
    rss, _, pids = common.snapshot({100}, include_pids=True)
    assert pids == {100, 101}
    assert rss == 10  # partial RSS proxy; actual cgroup memory is separate
