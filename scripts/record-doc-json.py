#!/usr/bin/env python3
"""Collect actual JSON outputs in an isolated offline workspace.

Requires a built CLI and the Linux host prerequisites for safe staging. It leaves
its temporary task files available for inspection and never applies changes.
"""
import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/pvisor')
    parser.add_argument('--output', type=Path, default=ROOT / 'docs/overrides/assets/examples/json')
    args = parser.parse_args()
    binary = args.binary.resolve()
    work = Path(tempfile.mkdtemp(prefix='pvisor-doc-json-'))
    workspace = work / 'workspace'
    workspace.mkdir()
    (workspace / 'obsolete.txt').write_text('old\n')
    env = {**os.environ, 'PVISOR_RUN_HOME': str(work / 'runs'), 'PVISOR_STARTUP_TIMING': '0'}

    def invoke(command):
        return subprocess.run([str(binary), *command], cwd=workspace, env=env,
                              capture_output=True, text=True, timeout=30)

    def query(command):
        result = invoke(command)
        if result.returncode:
            raise RuntimeError(f'{command}: exit {result.returncode}: {result.stderr}')
        return json.loads(result.stdout)

    outputs, commands, codes = {}, {}, {}
    for name, shell, expected, options in [
        ('run-bundle', 'mkdir -p src; printf "ready\\n" > src/result.txt; rm obsolete.txt', 0, []),
        ('failed-run', 'printf "candidate\\n" > failed.txt; exit 7', 7, []),
        ('timeout-run', 'printf "candidate\\n" > timed.txt; sleep 2', 1, ['--timeout', '100ms']),
    ]:
        stage = work / name
        command = ['run', '--safe', '--overlaynet-deny-all', '--stdio', 'capture',
                   '--stage', str(stage), *options, '--', '/bin/sh', '-c', shell]
        result = invoke(command)
        if result.returncode != expected:
            raise RuntimeError(f'{name}: expected exit {expected}, got {result.returncode}: {result.stderr}')
        commands[name], codes[name] = command, result.returncode
        outputs[name] = query(['status', '--review', '--json', str(stage)])
        if name == 'run-bundle':
            for output, command_args in {
                'status': ['status', '--json', str(stage)],
                'checkpoint-list': ['checkpoint', 'list', str(stage), '--json'],
                'kill-stopped': ['kill', str(stage), '--json'],
            }.items():
                outputs[output] = query(command_args)

    replacements = {str(work): '/tmp/pvisor-example'}
    for name in commands:
        run = outputs[name]['run']
        for key in ('run_id', 'attempt_id', 'session_id'):
            replacements.setdefault(run[key], f'{name}-{key}')

    def normalize(value):
        if isinstance(value, dict):
            return {key: (0 if key.endswith('_at_unix_ms') or key == 'generated_at_unix_ms'
                          else normalize(item)) for key, item in value.items()}
        if isinstance(value, list):
            return [normalize(item) for item in value]
        if isinstance(value, str):
            for old, new in replacements.items():
                value = value.replace(old, new)
        return value

    args.output.mkdir(parents=True, exist_ok=True)
    for name, value in outputs.items():
        (args.output / f'{name}.json').write_text(
            json.dumps(normalize(value), ensure_ascii=False, indent=2) + '\n')
    sources = [
        'crates/pvisor/src/runtime/bundle.rs', 'crates/pvisor/src/cli/runtime.rs',
        'crates/pvisor/src/cli/checkpoint.rs', 'crates/pvisor/src/cli/product.rs',
    ]
    provenance = {
        'kind': 'actual CLI output; paths, identities and numeric wall-clock fields normalized',
        'commands': commands, 'return_codes': codes,
        'cli_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'source_sha256': {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                          for name in sources},
        'platform': 'Linux x86_64; host executor; safe staging and deny-all networking',
    }
    (args.output / 'provenance.json').write_text(json.dumps(normalize(provenance), indent=2) + '\n')
    print(f'Recorded {len(outputs)} JSON examples in {args.output}; temporary task files: {work}')


if __name__ == '__main__':
    main()
