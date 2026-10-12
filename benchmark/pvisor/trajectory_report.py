#!/usr/bin/env python3
"""Non-mutating B-TRAJECTORY-ENG audit and descriptive phase extraction."""
import argparse
import csv
import json
import statistics
from pathlib import Path

from prepare_replay_rootfs import digest, inventory
from trajectory_benchmark import comparable_output, historical_output


def read(path):
    return json.loads(path.read_text())


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--data', required=True, type=Path)
    parser.add_argument('--runs', nargs='+', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    results, phases, prepared_paths = [], [], set()
    reference = None
    for name in args.runs:
        root = args.data / '.data' / name / 'sample'
        result, config = read(root / 'result.json'), read(root / 'config.json')
        assert result['correctness'] == 'passed', name
        guest = root / 'vm/stage/upper/replay-output'
        control = root / 'vm/stage/upper/replay-control'
        boot = read(control / 'boot.json')
        prepared_path = Path(config['rootfs']).parents[1] / 'prepared.json'
        prepared_paths.add(prepared_path)
        prepared = read(prepared_path)
        assert digest(prepared_path) == config['prepared_sha256']
        assert digest(Path(config['binary'])) == config['binary_sha256']
        assert digest(Path(config['firmware']) / 'libkrunfw.so.5') == config['firmware_sha256']
        history = rows(Path(prepared['sample']) / 'steps.jsonl')
        assert len(history) == 44
        outputs, source_hashes, observations = [], [], []
        for expected in history:
            i = expected['step']
            step = read(guest / ('step-%04d.json' % i))
            output = guest / ('tool-%04d.txt' % i)
            assert step['step'] == i and step['command'] == expected['arguments']['command']
            assert step['output_sha256'] == digest(output) and not step['timed_out']
            assert step['returncode'] == (expected['expected_observation']['returncode'] if i < 44 else 0)
            assert step['execution_token'] == boot['execution_token'] and step['pid'] == boot['pid']
            if i < 44:
                observations.append(dict(step=i, review=historical_output(output.read_bytes(),
                    expected['expected_observation']['raw_output'].encode(), i, boot['preflight']['import_warnings'])))
            outputs.append(comparable_output(output.read_bytes(), i))
            source_hashes.append(step['source_sha256'])
        signatures = dict(outputs=outputs, sources=source_hashes, files=read(guest / 'files.json'),
                          patch_sha256=digest(guest / 'final.patch'))
        assert signatures['patch_sha256'] == prepared['input_hashes']['recorded.patch']
        if reference is None:
            reference = signatures
        assert signatures == reference, 'cross-mode output/source/patch mismatch: ' + name
        events = rows(root / 'vm/events.jsonl')
        memory = rows(root / 'memory.jsonl')
        for step in config['barriers']:
            es = {e['event']: e for e in events if e['step'] == step}
            def timestamp(key):
                return es[key]['host_monotonic_ns']
            hold_start = timestamp('control_done') if config['mode'] != 'resident' else timestamp('no_early_advance_verified')
            hold_end = timestamp('wait_end')
            hold = [m for m in memory if hold_start <= m['host_monotonic_ns'] <= hold_end]
            assert hold, 'missing hold samples'
            phase = dict(run=name, mode=config['mode'], step=step,
                control_ms=(timestamp('control_done') - timestamp('control_begin')) / 1e6 if config['mode'] != 'resident' else 0,
                resume_ms=(timestamp('resume_done') - timestamp('resume_begin')) / 1e6 if config['mode'] != 'resident' else 0,
                go_to_ack_ms=(timestamp('resumed_received') - timestamp('go_sent')) / 1e6,
                hold_median_mib=statistics.median(m['memory_current'] for m in hold) / 1024**2,
                hold_anon_mib=statistics.median(m['anon'] for m in hold) / 1024**2,
                hold_file_mib=statistics.median(m['file'] for m in hold) / 1024**2,
                hold_samples=len(hold))
            phases.append(phase)
        results.append(dict(run=name, result=result, historical_stdout_review=observations,
            patch_sha256=signatures['patch_sha256'], binary_sha256=config['binary_sha256'],
            prepared_sha256=config['prepared_sha256'], enomem_workaround=config.get('enomem_workaround', 'auto'),
            io_accounting=memory[0]['io_accounting']))
    lower_checks = []
    for path in sorted(prepared_paths):
        prepared = read(path)
        saved = path.parent / 'rootfs-inventory.json'
        assert digest(saved) == prepared['inventory_sha256']
        assert inventory(Path(prepared['rootfs'])) == read(saved), 'lower changed: ' + str(path)
        lower_checks.append(dict(prepared=str(path), unchanged=True, inventory_sha256=digest(saved)))
    attempts = []
    for path in sorted((args.data / '.data').glob('*/sample/result.json')):
        r = read(path)
        attempts.append(dict(run=path.parents[1].name, correctness=r['correctness'], mode=r['mode'],
                             kind=r['kind'], scenario=r['scenario'], failures=r['failures']))
    summary = dict(schema='pvisor-trajectory-audit/v1', passed=True, runs=results,
                   lower_checks=lower_checks, all_attempts=attempts,
                   limitations=['engineering smoke, not a statistical performance study',
                                'shared lower cache charge can be outside service cgroup',
                                'io.stat unavailable without delegated IO controller',
                                'no official SWE-bench evaluation'])
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    with (args.output / 'phases.csv').open('w') as stream:
        writer = csv.DictWriter(stream, fieldnames=list(phases[0]))
        writer.writeheader()
        writer.writerows(phases)
    print(json.dumps(dict(passed=True, runs=args.runs, phases=len(phases))))


if __name__ == '__main__':
    main()
