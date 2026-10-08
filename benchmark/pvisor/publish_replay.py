#!/usr/bin/env python3
"""Audit complete B-REPLAY cohorts before deriving public adapter tables."""

import argparse
import json
import random
from pathlib import Path

from publication import distribution, write_csv
from publish_density import verify_harness
from reference_baselines import digest, verified_build_receipt
from v1.replay import validate_prefix


ADAPTERS = ('claude-code', 'codex', 'opencode', 'mini-swe-agent',
            'openhands', 'pi-agent', 'swe-agent')


def audit(path):
    path = Path(path).resolve()
    root = path.parent
    report = json.loads(path.read_text())
    protocol = report['replay_protocol']
    if (report.get('benchmark_id') != 'B-REPLAY'
            or protocol['adapters'] != list(ADAPTERS)
            or protocol['tasks'] != 20 or protocol['repetitions'] != 3
            or protocol['mode'] != 'prepare-only'):
        raise ValueError('publication requires all seven adapters, twenty tasks and three repetitions')
    verify_harness(report, root)
    if digest(root / 'harness/v1/replay.py') != digest(Path(__file__).parent / 'v1/replay.py'):
        raise ValueError('retained fidelity predicate differs from publication predicate')
    receipt = verified_build_receipt(root / 'build-receipt.json', root / 'bin/pvisor')
    replay_sha = digest(root / 'bin/pvisor-replay')
    if (replay_sha != receipt['binaries']['pvisor-replay']['sha256']
            or replay_sha != report['replay_binary_sha256']):
        raise ValueError('replay artifact does not match the source build receipt')
    expected = {(agent, task, trial) for agent in ADAPTERS
                for task in range(20) for trial in range(3)}
    rows = report['rows']
    good = {(row['agent'], row['task'], row['trial']): row for row in rows}
    if len(good) != len(rows):
        raise ValueError('duplicate replay rows')
    bad = {}
    for key, capability in report['capabilities'].items():
        if not key.startswith('replay/'):
            raise ValueError('unexpected or aborted replay capability')
        for failure in capability.get('failures', []):
            condition = (key.removeprefix('replay/'), failure['task'], failure['trial'])
            if condition in bad:
                raise ValueError('duplicate replay failures')
            bad[condition] = failure
    if good.keys() & bad.keys() or good.keys() | bad.keys() != expected:
        raise ValueError('incomplete or conflicting replay conditions')
    rng = random.Random(int(report['cli_arguments']['seed']))
    ordered = []
    for trial in range(3):
        cases = [(agent, task) for agent in ADAPTERS for task in range(20)]
        rng.shuffle(cases)
        ordered.extend((agent, task, trial) for agent, task in cases)
    directories = sorted((root / 'trials').iterdir())
    if len(directories) != len(ordered):
        raise ValueError('missing or extra retained trial directories')
    checked = []
    profiles = {}
    for ordinal, (directory, condition) in enumerate(zip(directories, ordered), 1):
        agent, task, trial = condition
        if directory.name != f'{ordinal:05d}-replay-{agent}-{task}':
            raise ValueError('retained trial order differs from seeded protocol')
        trajectory = directory / ('trajectory.jsonl' if agent in
                                  ('claude-code', 'codex', 'opencode', 'pi-agent') else 'trajectory.json')
        command = json.loads((directory / 'command.json').read_text())
        argv = [str(root / 'bin/pvisor-replay'), '--agent', agent,
                '--trajectory', str(trajectory), '--after-step', '1', '--prepare-only',
                '--workspace', str(directory / 'workspace'),
                '--state-dir', str(directory / 'state'), '--output-dir', str(directory / 'output')]
        affinity = report['cli_arguments']['cpu_affinity']
        if affinity:
            argv = ['taskset', '--cpu-list', affinity, *argv]
        if command['argv'] != argv or command['cwd'] != str(directory / 'workspace'):
            raise ValueError('retained command does not match replay condition')
        workspace = directory / 'workspace'
        if (list(workspace.iterdir()) != [workspace / 'existing']
                or (workspace / 'existing').read_text() != 'existing workspace must survive preparation\n'):
            raise ValueError('prepare-only modified retained workspace')
        fidelity_error = None
        try:
            if command['exit_code'] != 0 or command.get('timed_out'):
                raise ValueError('replay command did not exit successfully')
            results = list((directory / 'output').glob('*/result.json'))
            if len(results) != 1:
                raise ValueError('missing unique replay result')
            result = json.loads(results[0].read_text())
            if (result['failure'] is not None or result['replayed_tool_calls'] != 0
                    or result['phase'] != 'prepared' or result['agent_status'] != 'not_started'):
                raise ValueError('prepare-only failed or executed an agent/tool')
            manifest = json.loads(results[0].with_name('manifest.json').read_text())
            profiles[agent] = manifest['agent']['profile']
            validate_prefix(manifest, results[0].parent, agent,
                            f'printf fixture-{task} > marker-{task}.txt',
                            f'cat marker-{task}.txt', digest(trajectory))
        except (ValueError, OSError, KeyError, StopIteration) as error:
            fidelity_error = str(error)
        if condition in good:
            row = good[condition]
            if (fidelity_error is not None or row['correctness'] != 'passed'
                    or row['executed_tools'] != 0 or row['wall_ms'] != command['wall_ms']
                    or any(row.get(field) is not True for field in
                           ('prefix_arguments_exact', 'native_observation_preserved', 'source_digest_exact'))
                    or Path(row['logs']).resolve() != directory):
                raise ValueError('successful row is not supported by retained fidelity evidence')
        elif fidelity_error is None:
            raise ValueError('reported failed fidelity no longer reproduces from retained artifacts')
        checked.append({'agent': agent, 'task': task, 'trial': trial,
                        'state': 'passed' if condition in good else 'failed',
                        'command_sha256': digest(directory / 'command.json'),
                        'source_sha256': digest(trajectory), 'failure': fidelity_error})
    audit_record = dict(state='passed', report_sha256=digest(path),
                        script_sha256=digest(Path(__file__)), conditions=len(checked),
                        passed=len(good), failed=len(bad), rows=checked,
                        scope='complete retained commands/source/native-prefix/workspace audit; synthetic format fixtures, not real SDK execution')
    (root / 'replay-publication-audit.json').write_text(json.dumps(audit_record, indent=2) + '\n')
    provenance = dict(cohort=root.name, recorded_at=report['recorded_at'],
                      report_sha256=digest(path), replay_binary_sha256=replay_sha,
                      source_manifest_sha256=receipt['source_manifest_sha256'],
                      audit_sha256=digest(root / 'replay-publication-audit.json'),
                      cpu_affinity=report['cli_arguments']['cpu_affinity'],
                      memory_limit='no benchmark-specific host memory cap')
    summaries = []
    for agent in ADAPTERS:
        values = [row['wall_ms'] for key, row in good.items() if key[0] == agent]
        stats = distribution(values) if values else {key: '' for key in distribution([0])}
        summaries.append(dict(agent=agent, profile=profiles.get(agent, ''),
                              planned_conditions=60, failed_conditions=sum(k[0] == agent for k in bad),
                              **stats, unit='ms', timing='CLI launch through prepare-only exit; input generation and post-run audit excluded',
                              samples='twenty synthetic native prefixes, three seeded repetitions; no replay-suite warmups',
                              scope='exact arguments, historical observations, source digest, one complete batch, zero tools and unchanged workspace',
                              **provenance))
    return summaries, provenance


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    summaries, provenance = audit(args.report)
    args.output.mkdir(parents=True, exist_ok=True)
    write_csv(args.output / 'replay.csv', summaries)
    write_csv(args.output / 'replay-provenance.csv',
              [dict(field=key, value=value) for key, value in provenance.items()])
    print('Audited complete replay cohort and wrote derived tables.')


if __name__ == '__main__':
    main()
