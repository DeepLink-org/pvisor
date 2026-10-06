#!/usr/bin/env python3
"""Freeze independently supplied Firecracker kernels; never build pVisor firmware.

Preparation helper for B-STARTUP. Provenance is an operator assertion backed by
retained metadata/artifacts, not automatic vendor signature authentication.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def validate_provenance(kind, provenance):
    required = ('distribution', 'package', 'package_version', 'source_url') if kind == 'fc-system' else (
        'source_url', 'source_revision', 'build_command', 'compiler')
    if kind not in ('fc-system', 'fc-reference'):
        raise ValueError('unknown Firecracker kernel variant')
    if not isinstance(provenance, dict) or any(
            not isinstance(provenance.get(key), str) or not provenance[key].strip() for key in required):
        raise ValueError('missing explicit kernel source/package/build provenance')
    expected = 'official-distro-stock' if kind == 'fc-system' else 'independent-custom'
    if provenance.get('origin') != expected or provenance.get('pvisor_firmware') is not False:
        raise ValueError('kernel must have explicit independent non-pvisor origin')


def verify_kernel_receipt(path, kind):
    path = Path(path).resolve()
    record = json.loads(path.read_text())
    if not isinstance(record, dict) or record.get('schema') != 'pvisor-firecracker-kernel/v1' or record.get('variant') != kind:
        raise ValueError('kernel receipt schema/variant mismatch')
    validate_provenance(kind, record.get('provenance'))
    artifacts = record.get('artifacts')
    if not isinstance(artifacts, dict):
        raise ValueError('missing kernel artifact inventory')
    required = {'kernel', 'config', 'provenance'} | ({'vmlinuz', 'extractor'} if kind == 'fc-system' else set())
    if not required <= artifacts.keys() or set(artifacts) - required - {'initrd'}:
        raise ValueError('incomplete or unknown kernel artifacts')
    resolved = {}
    for name, item in artifacts.items():
        if not isinstance(item, dict) or not isinstance(item.get('file'), str) or not isinstance(item.get('sha256'), str):
            raise ValueError('malformed kernel artifact inventory')
        artifact = path.parent / item['file']
        if artifact.parent.resolve() != path.parent or artifact.is_symlink() or not artifact.is_file():
            raise ValueError('kernel artifact must be a retained regular file')
        if sha256(artifact) != item['sha256']:
            raise ValueError(f'kernel artifact hash mismatch: {name}')
        resolved[name] = str(artifact)
    if json.loads(Path(resolved['provenance']).read_text()) != record['provenance']:
        raise ValueError('kernel provenance metadata mismatch')
    with Path(resolved['kernel']).open('rb') as stream:
        if stream.read(4) != b'\x7fELF':
            raise ValueError('Firecracker kernel must be extracted ELF bytes')
    return {'receipt_sha256': sha256(path), 'record': record, 'paths': resolved}


def prepare_kernel(*, kind, config, provenance, output, kernel=None, vmlinuz=None, extractor=None, initrd=None):
    provenance = Path(provenance)
    metadata = json.loads(provenance.read_text())
    validate_provenance(kind, metadata)
    if kind == 'fc-system':
        if kernel or not vmlinuz or not extractor:
            raise ValueError('fc-system requires official vmlinuz and extractor, not a rebuilt kernel')
    elif not kernel or vmlinuz or extractor:
        raise ValueError('fc-reference requires an independent prebuilt ELF kernel')
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    inputs = {'config': config, 'provenance': provenance}
    inputs.update({'vmlinuz': vmlinuz, 'extractor': extractor} if kind == 'fc-system' else {'kernel': kernel})
    if initrd:
        inputs['initrd'] = initrd
    source_hashes = {name: sha256(path) for name, path in inputs.items()}
    for name, path in inputs.items():
        shutil.copyfile(path, output / name)
        if sha256(path) != source_hashes[name] or sha256(output / name) != source_hashes[name]:
            raise ValueError(f'preparation input changed: {name}')
    if kind == 'fc-system':
        # Standard Linux scripts/extract-vmlinux emits the exact decompressed ELF.
        # Execute only the operator-supplied trusted script, from retained bytes.
        command = ['bash', str(output / 'extractor'), str(output / 'vmlinuz')]
        with (output / 'kernel').open('wb') as stream, (output / 'extraction.stderr').open('wb') as errors:
            subprocess.run(command, stdout=stream, stderr=errors, check=True, timeout=120)
    else:
        command = None
    for name, path in inputs.items():
        if sha256(path) != source_hashes[name] or sha256(output / name) != source_hashes[name]:
            raise ValueError(f'preparation input changed: {name}')
    names = set(inputs) | {'kernel'}
    record = {'schema': 'pvisor-firecracker-kernel/v1', 'variant': kind,
              'provenance': metadata, 'extraction_command': command,
              'artifacts': {name: {'file': name, 'sha256': sha256(output / name)} for name in sorted(names)}}
    receipt = output / 'kernel-receipt.json'
    receipt.write_text(json.dumps(record, indent=2) + '\n')
    verify_kernel_receipt(receipt, kind)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kind', choices=('fc-system', 'fc-reference'), required=True)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--provenance', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--kernel', type=Path, help='Independent custom prebuilt ELF (fc-reference only)')
    parser.add_argument('--vmlinuz', type=Path, help='Official installed /boot/vmlinuz bytes (fc-system only)')
    parser.add_argument('--extractor', type=Path, help='Trusted scripts/extract-vmlinux; frozen and executed with bash')
    parser.add_argument('--initrd', type=Path, help='Exact preprepared initrd, if required for stock drivers')
    print(prepare_kernel(**vars(parser.parse_args())))


if __name__ == '__main__':
    main()
