"""A common missing prefix prevents unequal reads of existing Python bytecode."""

import copy
import os
import py_compile
import subprocess
import sys
from pathlib import Path

import pytest

from reference_baselines import validate_python_cache, validate_tool_cache
import reference_workload

POLICY = dict(dont_write_bytecode=True, prefix='/__pvisor_reference_no_pyc__', prefix_exists=False)


def test_no_write_alone_can_read_old_cache_but_empty_prefix_cannot(tmp_path):
    source = tmp_path / 'cached_fixture.py'
    source.write_text("value='cached'\n")
    before = source.stat()
    cached = py_compile.compile(str(source), doraise=True)
    source.write_text("value='source'\n")
    assert source.stat().st_size == before.st_size
    os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))
    env = os.environ | {'PYTHONDONTWRITEBYTECODE': '1'}
    env.pop('PYTHONPYCACHEPREFIX', None)
    argv = [sys.executable, '-c', 'import cached_fixture; print(cached_fixture.value)']
    first = subprocess.run(argv, cwd=tmp_path, env=env, check=True, capture_output=True, text=True)
    assert first.stdout.strip() == 'cached'
    prefix = tmp_path / 'absent-cache'
    second = subprocess.run(argv, cwd=tmp_path,
                            env=env | {'PYTHONPYCACHEPREFIX': str(prefix)},
                            check=True, capture_output=True, text=True)
    assert second.stdout.strip() == 'source'
    assert not prefix.exists()
    assert source.read_text() == "value='source'\n"
    assert os.path.isfile(cached)


def test_parent_and_all_filesystem_children_must_confirm_policy():
    value = {'python_cache': POLICY, 'filesystem': {name: {'python_cache': POLICY}
             for name in ('metadata', 'read', 'write', 'git', 'rg', 'cargo', 'npm')}}
    validate_python_cache(value)
    missing = copy.deepcopy(value)
    del missing['filesystem']['git']['python_cache']
    with pytest.raises(ValueError, match='filesystem child'):
        validate_python_cache(missing)


@pytest.mark.parametrize('fault', ['missing', 'write-enabled', 'wrong-prefix', 'prefix-present', 'coerced-boolean'])
def test_unverified_or_unequal_cache_policy_is_rejected(fault):
    value = {'python_cache': dict(POLICY)}
    if fault == 'missing':
        del value['python_cache']
    elif fault == 'write-enabled':
        value['python_cache']['dont_write_bytecode'] = False
    elif fault == 'wrong-prefix':
        value['python_cache']['prefix'] = None
    elif fault == 'prefix-present':
        value['python_cache']['prefix_exists'] = True
    else:
        value['python_cache']['dont_write_bytecode'] = 1
    with pytest.raises(ValueError, match='Python payload'):
        validate_python_cache(value)


def tool_result(mode='filesystem'):
    cache = dict(TMPDIR='/work/_reference_tmp', HOME='/work/_reference_tmp/reference-home',
                 CARGO_HOME='/work/_reference_tmp/reference-cargo',
                 NODE_COMPILE_CACHE='/work/_reference_tmp/node-compile-cache',
                 NODE_DISABLE_COMPILE_CACHE=None, NODE_OPTIONS=None)
    value = dict(mode=mode, workspace='/work', tool_cache=cache)
    if mode == 'filesystem':
        value['filesystem'] = {name: {'tool_cache': dict(cache), 'tool_scratch': 'workspace'}
                               for name in ('metadata', 'read', 'write', 'git', 'rg', 'cargo', 'npm')}
    if mode == 'env':
        value['versions'] = {'node_compile_cache': dict(status='ALREADY_ENABLED', directory=cache['NODE_COMPILE_CACHE'] + '/v24.18.0-x64-cf738c9d-1000')}
    return value


def test_task_local_cache_is_shared_only_within_one_task(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'workspace')
    for name, value in dict(NODE_DISABLE_COMPILE_CACHE='1', NODE_COMPILE_CACHE='/foreign',
                            NODE_COMPILE_CACHE_PORTABLE='1', NODE_OPTIONS='--jitless',
                            PVISOR_REFERENCE_TMPDIR='/dev/shm/foreign', TMPDIR='/tmp',
                            HOME='/foreign-home', CARGO_HOME='/foreign-cargo').items():
        monkeypatch.setenv(name, value)
    first = reference_workload.tools_env()
    cache = tmp_path / '_reference_tmp'
    assert first['TMPDIR'] == str(cache)
    assert first['HOME'] == str(cache / 'reference-home')
    assert first['CARGO_HOME'] == str(cache / 'reference-cargo')
    assert first['NODE_COMPILE_CACHE'] == str(cache / 'node-compile-cache')
    assert not set(('NODE_DISABLE_COMPILE_CACHE', 'NODE_OPTIONS', 'NODE_COMPILE_CACHE_PORTABLE')) & first.keys()
    (cache / 'private-state').write_text('same task')
    assert reference_workload.tools_env() == first
    second_task = tmp_path / 'next-task'
    second_task.mkdir()
    monkeypatch.chdir(second_task)
    second = reference_workload.tools_env()
    assert second['TMPDIR'] != first['TMPDIR']
    assert list((second_task / '_reference_tmp').iterdir()) == []


@pytest.mark.parametrize('kind', ['directory', 'file', 'symlink', 'dangling-symlink'])
def test_preexisting_task_cache_is_rejected_before_workload(tmp_path, monkeypatch, kind):
    monkeypatch.chdir(tmp_path)
    cache = tmp_path / '_reference_tmp'
    if kind == 'directory':
        cache.mkdir()
    elif kind == 'file':
        cache.write_text('contamination')
    else:
        cache.symlink_to(tmp_path if kind == 'symlink' else tmp_path / 'absent')
    monkeypatch.setattr(sys, 'argv', ['workload', '--mode', 'filesystem'])
    with pytest.raises(ValueError, match='absent before'):
        reference_workload.main()


def test_cache_cannot_follow_a_foreign_symlink(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'workspace')
    (tmp_path / '_reference_tmp').symlink_to(tmp_path / 'outside')
    with pytest.raises(ValueError, match='symlink'):
        reference_workload.tools_env()


@pytest.mark.parametrize('mode', ['filesystem', 'tools', 'env'])
def test_correct_task_cache_policy_is_accepted(mode):
    validate_tool_cache(tool_result(mode))


@pytest.mark.parametrize('fault', ['no-workspace', 'relative-workspace', 'no-cache', 'global-tmp',
                                   'global-home', 'global-cargo', 'node-disabled', 'global-node',
                                   'node-options', 'missing-child', 'unequal-child'])
def test_unequal_or_unverified_task_cache_is_rejected(fault):
    value = tool_result()
    if fault == 'no-workspace':
        del value['workspace']
    elif fault == 'relative-workspace':
        value['workspace'] = 'work'
    elif fault == 'no-cache':
        del value['tool_cache']
    elif fault == 'missing-child':
        del value['filesystem']['npm']['tool_cache']
    elif fault == 'unequal-child':
        value['filesystem']['npm']['tool_cache']['NODE_COMPILE_CACHE'] = '/tmp/cache'
    else:
        field, replacement = {
            'global-tmp': ('TMPDIR', '/tmp'), 'global-home': ('HOME', '/root'),
            'global-cargo': ('CARGO_HOME', '/tmp/cargo'), 'node-disabled': ('NODE_DISABLE_COMPILE_CACHE', '1'),
            'global-node': ('NODE_COMPILE_CACHE', '/tmp/cache'), 'node-options': ('NODE_OPTIONS', '--jitless'),
        }[fault]
        value['tool_cache'][field] = replacement
    with pytest.raises(ValueError):
        validate_tool_cache(value)


@pytest.mark.parametrize('fault', ['missing', 'disabled', 'different-directory', 'parent-escape', 'arbitrary-child', 'nested-child'])
def test_actual_node_capability_is_required(fault):
    value = tool_result('env')
    if fault == 'missing':
        del value['versions']['node_compile_cache']
    elif fault == 'disabled':
        value['versions']['node_compile_cache']['status'] = 'DISABLED'
    elif fault == 'different-directory':
        value['versions']['node_compile_cache']['directory'] = '/tmp/cache'
    else:
        suffix = {'parent-escape': '/../v24.18.0-x64-hash-1000', 'arbitrary-child': '/foreign',
                  'nested-child': '/nested/v24.18.0-x64-hash-1000'}[fault]
        value['versions']['node_compile_cache']['directory'] = value['tool_cache']['NODE_COMPILE_CACHE'] + suffix
    with pytest.raises(ValueError, match='actual Node'):
        validate_tool_cache(value)


def test_executor_cache_preserves_tmpdir_and_starts_fresh_per_task(tmp_path, monkeypatch):
    parent = tmp_path / 'executor-tmp'
    parent.mkdir()
    work = tmp_path / 'work'
    work.mkdir()
    monkeypatch.chdir(work)
    monkeypatch.setenv('TMPDIR', str(parent))
    monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'executor')
    first = reference_workload.tools_env()
    private = Path(first['TMPDIR'])
    assert private.parent == parent / '.data'
    assert private.name.startswith('pvisor-reference-')
    assert list(private.iterdir()) == []
    assert not (work / '_reference_tmp').exists()
    (private / 'retained-state').write_text('only this task')
    assert reference_workload.tools_env()['TMPDIR'] == str(private)
    next_work = tmp_path / 'next'
    next_work.mkdir()
    monkeypatch.chdir(next_work)
    second = Path(reference_workload.tools_env()['TMPDIR'])
    assert second != private and second.parent == private.parent
    assert list(second.iterdir()) == []


def test_nested_tool_action_reuses_only_explicit_private_cache(tmp_path, monkeypatch):
    private = tmp_path / '.data/pvisor-reference-abcdefgh'
    private.mkdir(parents=True)
    (private / 'same-task').write_text('private')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'executor')
    monkeypatch.setenv('PVISOR_REFERENCE_CACHE_DIRECTORY', str(private))
    assert reference_workload.tools_env()['TMPDIR'] == str(private)
    assert (private / 'same-task').read_text() == 'private'


@pytest.mark.parametrize('fault', ['relative', 'foreign-name', 'symlink-parent', 'unknown-policy', 'inherited-foreign'])
def test_invalid_executor_scratch_is_rejected(tmp_path, monkeypatch, fault):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'executor')
    monkeypatch.setenv('TMPDIR', str(tmp_path))
    if fault == 'relative':
        monkeypatch.setenv('TMPDIR', 'relative')
    elif fault == 'foreign-name':
        monkeypatch.setenv('PVISOR_REFERENCE_CACHE_DIRECTORY', str(tmp_path))
    elif fault == 'symlink-parent':
        (tmp_path / '.data').symlink_to(tmp_path / 'foreign')
    elif fault == 'unknown-policy':
        monkeypatch.setenv('PVISOR_REFERENCE_TOOL_SCRATCH', 'unknown')
    else:
        monkeypatch.setenv('PVISOR_REFERENCE_CACHE_DIRECTORY', '/tmp/foreign-cache')
    with pytest.raises(ValueError):
        reference_workload.tools_env()


def test_new_jobs_cannot_accept_missing_or_wrong_scratch_policy():
    value = tool_result()
    with pytest.raises(ValueError, match='declared experiment'):
        validate_tool_cache(value, 'executor')
    value['tool_scratch'] = 'workspace'
    validate_tool_cache(value, 'workspace')
    del value['filesystem']['npm']['tool_scratch']
    with pytest.raises(ValueError, match='child scratch policy'):
        validate_tool_cache(value, 'workspace')


def test_executor_policy_requires_private_directory_and_actual_storage_type():
    value = tool_result('env')
    value['tool_scratch'] = 'executor'
    old = value['tool_cache']['TMPDIR']
    new = '/.pvisor-tmp-run-test/.data/pvisor-reference-abcdefgh'
    value['tool_cache'] = {k: v.replace(old, new) if isinstance(v, str) else v for k, v in value['tool_cache'].items()}
    probe = value['versions']['node_compile_cache']
    probe['directory'] = probe['directory'].replace(old, new)
    probe['filesystem_type'] = 0x01021994
    validate_tool_cache(value, 'executor')
    probe['filesystem_type'] = None
    with pytest.raises(ValueError, match='storage type'):
        validate_tool_cache(value, 'executor')
    probe['filesystem_type'] = 0x01021994
    value['tool_cache']['TMPDIR'] = '/tmp'
    with pytest.raises(ValueError, match='fresh private'):
        validate_tool_cache(value, 'executor')
