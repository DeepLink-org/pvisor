#!/usr/bin/env python3
"""Materialize one explicit trajectory condition; semspec bench executes it."""
import argparse
import hashlib
import json
import shlex
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--batch', type=Path, required=True)
    parser.add_argument('condition', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    condition = args.condition
    if condition[:1] == ['--']:
        condition = condition[1:]
    if '--output' in condition or '--service' in condition:
        parser.error('batch owns output and service selection')
    batch = args.batch.resolve()
    batch.mkdir(parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parents[2]
    template = Path(__file__).with_name('smoke.json')
    command = json.loads(template.read_text())['single-condition']
    command = command.replace('@REPO@', shlex.quote(str(repo))).replace(
        '@CONDITION@', shlex.join(condition + ['--output', str(batch / 'sample')]))
    (batch / 'in.json').write_text(json.dumps({'single-condition': command}, indent=2) + '\n')
    (batch / 'manifest.json').write_text(json.dumps(dict(benchmark_id='B-TRAJECTORY-ENG',
        role='engineering A/B', condition=condition, template_sha256=hashlib.sha256(template.read_bytes()).hexdigest(),
        scope='one engineering smoke condition; no publication or cross-batch pooling'), indent=2) + '\n')
    print(batch)


if __name__ == '__main__':
    main()
