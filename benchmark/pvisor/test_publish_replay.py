"""Exercise publication accounting with synthetic retained-command fixtures."""
import json
import random
from pathlib import Path

import pytest

from publish_replay import ADAPTERS, audit
from reference_baselines import digest


def retained_cohort(root):
    root.mkdir()
    (root / 'bin').mkdir()
    (root / 'bin/pvisor').write_bytes(b'unit-test binary fixture')
    (root / 'bin/pvisor-replay').write_bytes(b'unit-test replay fixture')
    harness = root / 'harness/v1/replay.py'
    harness.parent.mkdir(parents=True)
    original = Path(__file__).parent / 'v1/replay.py'
    harness.write_bytes(original.read_bytes())
    (root / 'source-manifest.json').write_text('[]\n')
    receipt = dict(pvisor_sha256=digest(root / 'bin/pvisor'),
                   source_manifest_sha256=digest(root / 'source-manifest.json'),
                   binaries={'pvisor-replay': {'sha256': digest(root / 'bin/pvisor-replay')}})
    (root / 'build-receipt.json').write_text(json.dumps(receipt))
    rows = []
    rng = random.Random(20261006)
    ordinal = 0
    for trial in range(3):
        cases = [(agent, task) for agent in ADAPTERS for task in range(20)]
        rng.shuffle(cases)
        for agent, task in cases:
            ordinal += 1
            folder = root / 'trials' / f'{ordinal:05d}-replay-{agent}-{task}'
            work = folder / 'workspace'
            work.mkdir(parents=True)
            (work / 'existing').write_text('existing workspace must survive preparation\n')
            command = f'printf fixture-{task} > marker-{task}.txt'
            trajectory = folder / ('trajectory.jsonl' if agent in
                ('claude-code', 'codex', 'opencode', 'pi-agent') else 'trajectory.json')
            trajectory.write_text(json.dumps({'unit_fixture': command}) + '\n')
            output = folder / 'output/run/native'
            output.mkdir(parents=True)
            native_name = {'mini-swe-agent': 'prepared-prefix.json',
                           'openhands': 'prepared-replay-events.json',
                           'swe-agent': 'prepared-prefix.traj'}.get(agent, 'prepared-prefix.jsonl')
            # These are predicate fixtures, not simulated executions or claims
            # about the native SDK format parser, covered by Rust contracts.
            (output / native_name).write_text(json.dumps({'command': command,
                                                          'observation': 'historical observation'}) + '\n')
            manifest = dict(agent={'name': agent, 'profile': agent + '/unit-fixture'},
                            source={'sha256': digest(trajectory)},
                            boundary={'after_step': 1, 'tool_calls': 1, 'complete_tool_batch': True},
                            batches=[{'tool_calls': [{'arguments': {'command': command}}]}])
            (output.parent / 'manifest.json').write_text(json.dumps(manifest))
            (output.parent / 'result.json').write_text(json.dumps(dict(
                failure=None, replayed_tool_calls=0, phase='prepared', agent_status='not_started')))
            argv = ['taskset', '--cpu-list', '0,1', str(root / 'bin/pvisor-replay'),
                    '--agent', agent, '--trajectory', str(trajectory), '--after-step', '1',
                    '--prepare-only', '--workspace', str(work), '--state-dir', str(folder / 'state'),
                    '--output-dir', str(folder / 'output')]
            (folder / 'command.json').write_text(json.dumps(dict(argv=argv, cwd=str(work),
                exit_code=0, wall_ms=1.0, timed_out=False)))
            rows.append(dict(agent=agent, task=task, trial=trial, wall_ms=1.0, correctness='passed',
                executed_tools=0, prefix_arguments_exact=True, native_observation_preserved=True,
                source_digest_exact=True, logs=str(folder)))
    report = dict(benchmark_id='B-REPLAY', recorded_at='unit fixture',
        cli_arguments={'seed': 20261006, 'cpu_affinity': '0,1'},
        replay_protocol={'adapters': list(ADAPTERS), 'tasks': 20, 'repetitions': 3, 'mode': 'prepare-only'},
        replay_binary_sha256=digest(root / 'bin/pvisor-replay'),
        harness_sha256={'v1/replay.py': digest(harness)}, rows=rows, capabilities={})
    path = root / 'report.json'
    path.write_text(json.dumps(report))
    return path


def test_complete_replay_conditions_are_audited(tmp_path):
    path = retained_cohort(tmp_path / 'cohort')
    summaries, provenance = audit(path)
    assert len(summaries) == 7
    assert all(row['n'] == 60 and row['failed_conditions'] == 0 for row in summaries)
    record = json.loads((path.parent / 'replay-publication-audit.json').read_text())
    assert record['conditions'] == 420 and record['passed'] == 420
    assert provenance['report_sha256'] == digest(path)


@pytest.mark.parametrize('fault', ['missing', 'duplicate', 'source', 'observation',
    'next-action', 'tool-executed', 'workspace', 'command'])
def test_unsupported_replay_claims_cannot_be_published(tmp_path, fault):
    path = retained_cohort(tmp_path / 'cohort')
    report = json.loads(path.read_text())
    folder = Path(report['rows'][0]['logs'])
    if fault == 'missing':
        report['rows'].pop()
    elif fault == 'duplicate':
        report['rows'].append(report['rows'][0])
    elif fault == 'source':
        next(folder.glob('trajectory.*')).write_text('changed source bytes')
    elif fault in ('observation', 'next-action'):
        native = next((folder / 'output/run/native').iterdir())
        value = json.loads(native.read_text())
        if fault == 'observation':
            value['observation'] = 'different historical output'
        else:
            value['future'] = f"cat marker-{report['rows'][0]['task']}.txt"
        native.write_text(json.dumps(value))
    elif fault == 'tool-executed':
        result = folder / 'output/run/result.json'
        value = json.loads(result.read_text()); value['replayed_tool_calls'] = 1
        result.write_text(json.dumps(value))
    elif fault == 'workspace':
        (folder / 'workspace/marker').write_text('unexpected tool output')
    elif fault == 'command':
        command = folder / 'command.json'; value = json.loads(command.read_text())
        value['argv'].remove('--prepare-only'); command.write_text(json.dumps(value))
    path.write_text(json.dumps(report))
    with pytest.raises(ValueError):
        audit(path)
