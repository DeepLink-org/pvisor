"""Reject altered reference tools before any performance sample is started."""
import json
from pathlib import Path

import pytest

from reference_inputs import ASSETS, create_reference_manifest, verify_reference_inputs


@pytest.fixture
def inputs(tmp_path):
    root = tmp_path / 'assets'
    (root / 'rootfs/bench').mkdir(parents=True)
    for name in [*ASSETS, 'rootfs/bench/reference_workload.py', 'rootfs/bench/affinity']:
        (root / name).write_bytes(('fixed input ' + name).encode())
    (root / 'rootfs/bench/affinity').chmod(0o755)
    (root / 'rootfs/bench/alias').symlink_to('affinity')
    create_reference_manifest(root)
    return root


def test_complete_input_identity_includes_directory_modes(inputs):
    result = verify_reference_inputs(inputs)
    assert result['files'] == 9 and result['symlinks'] == 1
    assert result['directories'] == 2 and result['directory_metadata_coverage'] == 'complete'


@pytest.mark.parametrize('change', ['contents', 'size', 'missing', 'extra', 'permissions',
                                  'symlink', 'directory-mode', 'special-node'])
def test_changed_inputs_are_rejected(inputs, change):
    target = inputs / 'rootfs/bench/affinity'
    if change == 'contents':
        original = target.read_bytes()
        target.write_bytes(b'x' * len(original))
    elif change == 'size':
        target.write_bytes(b'truncated')
    elif change == 'missing':
        target.unlink()
    elif change == 'extra':
        (inputs / 'rootfs/new-input').write_text('not in frozen inventory')
    elif change == 'permissions':
        target.chmod(0o644)
    elif change == 'symlink':
        link = inputs / 'rootfs/bench/alias'
        link.unlink()
        link.symlink_to('reference_workload.py')
    elif change == 'directory-mode':
        (inputs / 'rootfs/bench').chmod(0o700)
    else:
        import os
        target.unlink()
        os.mkfifo(target)
    with pytest.raises(ValueError):
        verify_reference_inputs(inputs)


@pytest.mark.parametrize('change', ['duplicate', 'absolute', 'traversal', 'missing-required',
                                  'nonarray', 'extra-field', 'missing-file-mode', 'boolean-size'])
def test_invalid_manifest_does_not_certify_a_cohort(inputs, change):
    path = inputs / 'input-manifest.json'
    entries = json.loads(path.read_text())
    file = next(entry for entry in entries if entry['path'] == 'rootfs/bench/affinity')
    if change == 'duplicate':
        entries.append(dict(file))
    elif change in ('absolute', 'traversal'):
        file['path'] = '/etc/passwd' if change == 'absolute' else 'rootfs/../elsewhere'
    elif change == 'missing-required':
        entries.remove(file)
    elif change == 'nonarray':
        entries = {'entries': entries}
    elif change == 'extra-field':
        file['verified'] = True
    elif change == 'missing-file-mode':
        file.pop('mode')
    else:
        file['bytes'] = True
    path.write_text(json.dumps(entries))
    with pytest.raises(ValueError):
        verify_reference_inputs(inputs)


def test_legacy_array_remains_explicit_about_missing_directory_metadata(inputs):
    path = inputs / 'input-manifest.json'
    entries = [entry for entry in json.loads(path.read_text()) if not entry.get('directory')]
    for entry in entries:
        if entry['path'] in ASSETS:
            entry.pop('mode', None)
    path.write_text(json.dumps(entries))
    result = verify_reference_inputs(inputs)
    assert result['directory_metadata_coverage'] == 'not recorded by legacy manifest'
    (inputs / 'rootfs/unknown-file').write_text('still reject extra files')
    with pytest.raises(ValueError):
        verify_reference_inputs(inputs)


def test_existing_frozen_manifest_is_never_regenerated(inputs):
    original = (inputs / 'input-manifest.json').read_bytes()
    with pytest.raises(ValueError):
        create_reference_manifest(inputs)
    assert (inputs / 'input-manifest.json').read_bytes() == original


def test_input_modified_while_hashed_is_rejected(inputs, monkeypatch):
    import reference_inputs as module
    original = module.fingerprint
    target = inputs / 'rootfs/bench/affinity'

    def fingerprint(path, before):
        result = original(path, before)
        if path == target:
            path.write_bytes(b'x' * before.st_size)
        return result

    monkeypatch.setattr(module, 'fingerprint', fingerprint)
    # A file changed just after its own hash is a later mutation, so the gate
    # must also recheck every captured identity after the whole inventory pass.
    with pytest.raises(ValueError):
        verify_reference_inputs(inputs)
