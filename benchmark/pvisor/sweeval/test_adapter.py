"""Protocol and model compatibility checks without an external model or KVM."""
import importlib.util
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest
from agent_runner import api_base
from transport import Client

os.environ['LITELLM_LOCAL_MODEL_COST_MAP'] = 'True'


def optional_package(name):
    if importlib.util.find_spec(name) is None:
        pytest.skip(f'optional integration dependency absent: {name}')
    # Installed-but-broken dependencies are failures, not skipped tests.
    return importlib.import_module(name)


@pytest.fixture
def worker(tmp_path):
    root = tmp_path / 'ipc'
    process = subprocess.Popen([sys.executable, str(Path(__file__).with_name('guest.py')),
                                'test-token', str(root)])
    for _ in range(100):
        if (root / 'boot.json').exists():
            break
        time.sleep(.02)
    assert (root / 'boot.json').exists()
    client = Client(root, 'test-token', tmp_path / 'events.jsonl')
    try:
        yield client
    finally:
        if process.poll() is None:
            client.call(op='stop', timeout=2)
        process.wait(timeout=5)
        client.close()


def test_persistent_execution_and_exit(worker, tmp_path):
    first = worker.call('printf %s "$VALUE" > marker; echo out; echo err >&2',
                        cwd=str(tmp_path), env={'VALUE': 'persisted'})
    assert first['stdout'] == 'out\n' and first['stderr'] == 'err\n'
    second = worker.call('cat marker; exit 7', cwd=str(tmp_path))
    assert second['stdout'] == 'persisted' and second['return_code'] == 7


def test_timeout_kills_process_group(worker, tmp_path):
    marker = tmp_path / 'late'
    result = worker.call(f'(sleep .5; touch {marker}) & wait', timeout=.1)
    assert result['timed_out'] and result['return_code'] == 124
    time.sleep(.6)
    assert not marker.exists()
    assert worker.call('echo alive')['stdout'] == 'alive\n'


def test_reject_wrong_token(worker):
    bad = Client(worker.root, 'wrong', worker.events)
    # Publish directly to avoid waiting the deliberate transport grace period.
    from transport import publish
    publish(worker.root / 'bad.request.json', dict(id='bad', token=bad.token, op='stop'))
    time.sleep(.1)
    assert not (worker.root / 'bad.result.json').exists()
    assert worker.call('true')['return_code'] == 0


def test_dead_vm_fails_without_waiting_for_command_timeout(tmp_path):
    worker = Client(tmp_path, 'test-token', tmp_path / 'events.jsonl', is_alive=lambda: False)
    started = time.monotonic()
    try:
        with pytest.raises(RuntimeError, match='VM process exited'):
            worker.call('echo unreachable', timeout=3600)
    finally:
        worker.close()
    assert time.monotonic() - started < 1
    rows = [json.loads(line) for line in worker.events.read_text().splitlines()]
    assert rows[-1]['event'] == 'tool_transport_error'


def test_host_reads_refuse_guest_symlinks_and_fifos(worker, tmp_path):
    target = tmp_path / 'host-private'
    target.write_text('must not read')
    (worker.root / 'link').symlink_to(target)
    with pytest.raises(OSError):
        worker.read('link')
    os.mkfifo(worker.root / 'fifo')
    with pytest.raises(ValueError):
        worker.read('fifo')


@pytest.mark.parametrize('url', ['https://example.invalid/api/paas/v4',
                                 'https://example.invalid/api/coding/paas/v4',
                                 'http://127.0.0.1:8000/v1'])
def test_api_prefix_preserved(url):
    assert api_base(url + '/') == url


@pytest.mark.parametrize('url', ['https://user:password@example.invalid/v1',
                                 'https://example.invalid/v1/chat/completions',
                                 'https://example.invalid/v1?key=secret'])
def test_unsafe_or_wrong_endpoint_rejected(url):
    with pytest.raises(ValueError):
        api_base(url)


@pytest.mark.parametrize('options', [{}, {'temperature': 0.5, 'reasoning_effort': 'high',
    'extra_body': {'thinking': {'type': 'enabled', 'clear_thinking': False}}}])
def test_native_agent_records_real_request_and_tool_events(worker, tmp_path, monkeypatch, options):
    litellm = optional_package('litellm')
    monkeypatch.setenv('MSWEA_GLOBAL_CONFIG_DIR', str(tmp_path / 'mini-config'))
    optional_package('minisweagent')
    from agent_runner import run

    requests = []

    def completion(**kwargs):
        requests.append(kwargs)
        return litellm.ModelResponse(choices=[dict(index=0, finish_reason='tool_calls',
            message=dict(role='assistant', content='test', reasoning_content='test reasoning',
                         tool_calls=[dict(id='call-' + str(len(requests)), type='function',
                                          function=dict(name='bash', arguments=json.dumps(dict(
                                              command='echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT'
                                              if len(requests) == 2 else 'echo native-tool'))))]))],
            usage=dict(prompt_tokens=10, completion_tokens=20, total_tokens=30))

    monkeypatch.setattr(litellm, 'completion', completion)
    monkeypatch.setenv('OPENAI_API_KEY', 'not-a-real-key')
    run(dict(control=str(worker.root), token=worker.token, events=str(worker.events),
             model_kwargs=options, model='fixture-model',
             base_url='https://example.invalid/api/paas/v4' if options else 'https://example.invalid/v1',
             cwd=str(tmp_path), guest_release='test', instruction='test task',
             trajectory=str(tmp_path / 'native.json'), outcome=str(tmp_path / 'outcome.json')))
    assert len(requests) == 2
    assert requests[0]['model'] == 'openai/fixture-model'
    if options:
        assert requests[0]['api_base'].endswith('/api/paas/v4')
        assert all(requests[0][key] == value for key, value in options.items())
    else:
        assert requests[0]['api_base'].endswith('/v1')
        assert 'extra_body' not in requests[0]
        assert 'reasoning_effort' not in requests[0]
    assert 'tools' in requests[0] and 'responses' not in requests[0]
    native = json.loads((tmp_path / 'native.json').read_text())
    assert native['trajectory_format'] == 'mini-swe-agent-1.1'
    assert native['info']['mini_version'] == '2.4.6'
    assert native['info']['exit_status'] == 'Submitted'
    assert 'not-a-real-key' not in (tmp_path / 'native.json').read_text()
    names = [json.loads(line)['event'] for line in Path(worker.events).read_text().splitlines()]
    assert names.count('model_request_start') == names.count('model_request_end') == 2
    assert names.count('tool_start') == names.count('tool_end') == 2


def test_async_environment_cancellation_stops_guest_command(worker, tmp_path):
    import asyncio

    optional_package('sweeval')
    from environment import PVisorEnvironment

    async def check():
        environment = PVisorEnvironment(rootfs=tmp_path, firmware=tmp_path,
                                       binary='/unused', output=tmp_path / 'vm')
        environment.client = worker
        marker = tmp_path / 'after-cancel'
        pending = asyncio.create_task(environment.exec(
            f'(sleep .7; touch {marker}) & wait', timeout_sec=20))
        await asyncio.sleep(.1)
        pending.cancel()
        with pytest.raises(asyncio.CancelledError):
            await pending
        await asyncio.sleep(.8)
        assert not marker.exists()
        assert (await environment.exec('echo healthy')).stdout == 'healthy\n'

    asyncio.run(check())


def test_resource_phase_assignment_and_cpu_accounting(tmp_path):
    from analyze import analyze

    samples = [dict(host_monotonic_ns=i * 1_000_000_000,
                    cpu={'usage_usec': i * 250_000}, memory_current=1024**3)
               for i in range(4)]
    events = [dict(host_monotonic_ns=0, event='trial.agent_start'),
              dict(host_monotonic_ns=0, event='model_request_start'),
              dict(host_monotonic_ns=1_000_000_000, event='model_request_end'),
              dict(host_monotonic_ns=1_000_000_000, event='tool_start', execution_id='x'),
              dict(host_monotonic_ns=2_000_000_000, event='tool_end', execution_id='x')]
    for name, rows in [('resources', samples), ('events', events)]:
        (tmp_path / (name + '.jsonl')).write_text('\n'.join(map(json.dumps, rows)))
    result = analyze(tmp_path)
    assert set(result['phases']) == {'model_wait', 'tool', 'agent_start'}
    for phase in result['phases'].values():
        assert phase['duration_s'] == 1
        assert phase['mean_cpu_cores'] == .25
        assert phase['mean_memory_mib'] == 1024
    assert 'trial_stage' in (tmp_path / 'timeline.csv').read_text()


def test_model_requires_explicit_endpoint_and_id_and_uses_only_host_key(monkeypatch):
    from agent_runner import model_key, model_settings

    for env in ({}, {'OPENAI_MODEL': 'fixture'}, {'OPENAI_BASE_URL': 'https://example.invalid/v1'}):
        with pytest.raises(ValueError, match='OPENAI_BASE_URL and OPENAI_MODEL'):
            model_settings(env)
    monkeypatch.delenv('OPENAI_API_KEY', raising=False)
    monkeypatch.setenv('GLM_API_KEY', 'ignored-legacy-key')
    with pytest.raises(ValueError, match='OPENAI_API_KEY'):
        model_key()
    monkeypatch.setenv('OPENAI_API_KEY', 'host-only-test-key')
    assert model_key() == 'host-only-test-key'
    settings = model_settings({'OPENAI_BASE_URL': 'https://example.invalid/v1/',
                               'OPENAI_MODEL': 'custom-model',
                               'SWE_MODEL_KWARGS': '{"max_tokens": 4096, "timeout": 30}'})
    assert settings == dict(model='custom-model', base_url='https://example.invalid/v1',
                            model_kwargs={'max_tokens': 4096, 'timeout': 30})
    assert 'host-only-test-key' not in json.dumps(settings)


@pytest.mark.parametrize('options', ['not-json', '[]', '{"api_key":"forbidden"}',
    '{"model":"other"}', '{"extra_body":{"messages":[]}}',
    '{"extra_body":{"api_key":"forbidden"}}', '{"extra_body":[]}'])
def test_model_options_cannot_replace_credentials_routing_or_tool_protocol(options):
    from agent_runner import model_settings

    with pytest.raises(ValueError):
        model_settings({'OPENAI_BASE_URL': 'https://example.invalid/v1',
                        'OPENAI_MODEL': 'fixture', 'SWE_MODEL_KWARGS': options})


@pytest.mark.parametrize('valid', [True, False])
def test_api_preflight_retains_success_and_failure(tmp_path, monkeypatch, valid):
    litellm = optional_package('litellm')
    from preflight import check

    monkeypatch.setenv('SWE_MODEL_KWARGS', '{}')
    monkeypatch.setenv('OPENAI_API_KEY', 'fake-preflight-key')
    monkeypatch.setenv('OPENAI_BASE_URL', 'https://example.invalid/v1')
    monkeypatch.setenv('OPENAI_MODEL', 'fixture-model')
    requests = []

    def completion(**kwargs):
        requests.append(kwargs)
        assert 'extra_body' not in kwargs
        if len(requests) == 1:
            message = dict(role='assistant', content=None, tool_calls=[dict(
                id='fixture-call', type='function', function=dict(name='bash',
                arguments=json.dumps(dict(command='printf pvisor-api-ok' if valid else 'unexpected'))))])
        else:
            assert kwargs['messages'][-1]['tool_call_id'] == 'fixture-call'
            message = dict(role='assistant', content='pvisor-api-ok')
        return litellm.ModelResponse(choices=[dict(index=0, message=message, finish_reason='stop')])

    monkeypatch.setattr(litellm, 'completion', completion)
    output = tmp_path / 'preflight'
    if valid:
        assert check(output)['status'] == 'passed'
    else:
        with pytest.raises(ValueError, match='unexpected tool arguments'):
            check(output)
    result = json.loads((output / 'result.json').read_text())
    assert result['executed_commands'] == 0
    assert result['completed_requests'] == (2 if valid else 1)
    assert 'fake-preflight-key' not in (output / 'settings.json').read_text()


def test_oci_whiteouts_preserve_layer_order_and_subtree_boundaries():
    from pathlib import PurePosixPath

    from rootfs import apply_layer_identities

    directory = (1, 2, 0o755, True)
    regular = (1, 2, 0o644, False)
    identities = {name: directory if name in ('a', 'a/sub', 'ab', 'replace') else regular
                  for name in ('a', 'a/old', 'a/sub', 'a/sub/old', 'ab', 'ab/kept',
                               'replace', 'replace/old', 'gone', 'gone/child')}
    def member(name, isdir=False):
        return (PurePosixPath(name), 10, 20, 0o755 if isdir else 0o644, isdir)
    apply_layer_identities(identities, [member('a/new'), member('a/.wh..wh..opq'),
                                      member('.wh.gone'), member('gone/recreated'),
                                      member('replace')])
    assert set(identities) == {'a', 'a/new', 'ab', 'ab/kept', 'gone/recreated', 'replace'}
    assert identities['a'] == directory
    apply_layer_identities(identities, [member('.wh..wh..opq'), member('fresh')])
    assert set(identities) == {'fresh'}


def test_portable_config_expands_only_paths(tmp_path, monkeypatch):
    from config import load_config

    monkeypatch.setenv('SWE_ROOT', str(tmp_path))
    path = tmp_path / 'config.json'
    config = dict(inputs='${SWE_ROOT}/inputs', tasks=[dict(
        name='example-1', prepared='${SWE_ROOT}/prepared.json', environment={'LITERAL': '$HOME'})])
    path.write_text(json.dumps(config))
    loaded = load_config(path)
    assert loaded['inputs'] == str(tmp_path / 'inputs')
    assert loaded['tasks'][0]['environment']['LITERAL'] == '$HOME'
    monkeypatch.delenv('SWE_ROOT')
    with pytest.raises(ValueError, match='unresolved variable'):
        load_config(path)


@pytest.mark.parametrize('names', [[], ['../outside'], ['same', 'same']])
def test_config_rejects_unsafe_or_duplicate_task_names(tmp_path, names):
    from config import load_config

    path = tmp_path / 'config.json'
    path.write_text(json.dumps(dict(tasks=[dict(name=n, prepared='/unused') for n in names])))
    with pytest.raises(ValueError):
        load_config(path)


@pytest.mark.parametrize('image,expected', [
    ('registry.invalid:5000/group/image', 'registry.invalid:5000/group/image'),
    ('registry.invalid:5000/group/image:tag', 'registry.invalid:5000/group/image'),
    ('registry.invalid/group/image@sha256:old', 'registry.invalid/group/image'),
    ('ubuntu:latest', 'ubuntu'),
])
def test_pinned_images_keep_registry_ports(image, expected):
    from prepare import pin_image

    assert pin_image(image, 'sha256:new') == expected + '@sha256:new'


def test_reference_gate_rejects_missing_failed_or_mismatched_validation(tmp_path):
    from config import validate_oracle

    plan = [(dict(name='task'), None, dict(image_digest='sha256:fixture'))]
    with pytest.raises(ValueError, match='require_oracle'):
        validate_oracle({}, plan)
    path = tmp_path / 'gate.json'
    config = dict(require_oracle=str(path), krun_enomem_workaround=False)
    valid = dict(status='passed', images={'task': 'sha256:fixture'}, krun_enomem_workaround=False)
    path.write_text(json.dumps(valid))
    validate_oracle(config, plan)
    for mismatch in (dict(status='failed'), dict(images={'other': 'sha256:fixture'}),
                     dict(krun_enomem_workaround=None)):
        path.write_text(json.dumps({**valid, **mismatch}))
        with pytest.raises(ValueError, match='matching task'):
            validate_oracle(config, plan)


@pytest.mark.parametrize('timestamp,usage', [(0, 100), (1_000_000_000, -1)])
def test_resource_analysis_rejects_invalid_counter_intervals(tmp_path, timestamp, usage):
    from analyze import analyze

    samples = [dict(host_monotonic_ns=0, cpu={'usage_usec': 0}, memory_current=1024),
               dict(host_monotonic_ns=timestamp, cpu={'usage_usec': usage}, memory_current=1024)]
    (tmp_path / 'resources.jsonl').write_text('\n'.join(map(json.dumps, samples)))
    (tmp_path / 'events.jsonl').write_text('')
    with pytest.raises(ValueError, match='monotonic'):
        analyze(tmp_path)


def test_plan_checks_public_image_mapping_without_mutating_tasks(tmp_path):
    optional_package('sweeval')
    from run import load_plan

    task = tmp_path / 'task'
    (task / 'tests').mkdir(parents=True)
    toml = task / 'task.toml'
    toml.write_text('schema_version = "1.0"\n[task]\nname = "local/fixture"\n'
                    '[environment]\ndocker_image = "private.invalid/task:1"\n')
    (task / 'instruction.md').write_text('Fixture only.')
    (task / 'tests/test.sh').write_text('#!/bin/sh\nexit 0\n')
    before = toml.read_bytes()
    prepared = tmp_path / 'prepared.json'
    entry = dict(name='fixture', directory='task', prepared=str(prepared),
                 image='public.invalid/task@sha256:fixture')
    config = dict(task_dir=str(tmp_path), tasks=[entry])
    assert load_plan(config)[0][2] is None
    descriptor = dict(source_image='private.invalid/task:1', resolved_image=entry['image'])
    prepared.write_text(json.dumps(descriptor))
    assert load_plan(config)[0][2] == descriptor
    entry['image'] = 'public.invalid/wrong:1'
    with pytest.raises(ValueError, match='task/override'):
        load_plan(config)
    assert toml.read_bytes() == before
