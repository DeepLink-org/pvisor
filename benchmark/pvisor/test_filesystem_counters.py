"""A diagnostic counter cohort cannot silently use changed prepared inputs."""
import hashlib
import json
import sys

import pytest

import filesystem_counters as counters


@pytest.fixture
def invocation(tmp_path, monkeypatch):
    assets = tmp_path / 'assets'
    assets.mkdir()
    (assets / 'assets.json').write_text('{}')
    (assets / 'input-manifest.json').write_text('[]')
    binary = tmp_path / 'pvisor'
    binary.write_bytes(b'frozen-binary-fixture')
    manifest = tmp_path / 'source-manifest.json'
    manifest.write_text('[]')
    receipt = tmp_path / 'build-receipt.json'
    receipt.write_text(json.dumps(dict(
        pvisor_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        source_manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest())))
    firmware = tmp_path / 'firmware'
    firmware.mkdir()
    (firmware / 'libkrunfw.so.5').write_bytes(b'frozen-firmware-fixture')
    output = tmp_path / '.data/counters'
    argv = ['filesystem_counters.py', '--assets', str(assets), '--binary', str(binary),
            '--build-receipt', str(receipt), '--firmware', str(firmware),
            '--output', str(output), '--modes', 'filesystem', '--backends', 'pvisor-staged',
            '--samples', '1']
    monkeypatch.setattr(sys, 'argv', argv)
    monkeypatch.setenv('PVISOR_FS_PROFILE', 'before-test')
    return output


def test_invalid_inputs_reject_diagnostics_before_any_task(invocation, monkeypatch):
    def invalid(_):
        raise ValueError('fixture bytes changed')

    def forbidden(*_):
        pytest.fail('invalid inputs must never reach the workload')

    monkeypatch.setattr(counters, 'verify_reference_inputs', invalid)
    monkeypatch.setattr(counters, 'run_trial', forbidden)
    with pytest.raises(ValueError, match='fixture bytes changed'):
        counters.main()
    report = json.loads((invocation / 'report.json').read_text())
    assert report['input_verification']['state'] == 'failed'
    assert report['rows'] == []
    assert (invocation / 'source-manifest.json').read_text() == '[]'


def test_changed_inputs_fail_after_preserving_collected_counters(invocation, monkeypatch):
    observations = iter([{'input_manifest_sha256': 'before'}, {'input_manifest_sha256': 'after'}])
    monkeypatch.setattr(counters, 'verify_reference_inputs', lambda _: next(observations))

    def run(args, _, backend, mode, trial):
        assert args.resource_observation == 'off'
        directory = args.output / 'trials/example'
        directory.mkdir(parents=True)
        profile = dict(schema=2, pid=1, component='core', instance=1, final_record=True,
                       measurements={'read': dict(calls=3, total_ns=1000, units=100)})
        (directory / 'stderr.log').write_text('pvisor-fs-profile ' + json.dumps(profile) + '\n')
        return dict(logs=str(directory), backend=backend, mode=mode, trial=trial)

    monkeypatch.setattr(counters, 'run_trial', run)
    with pytest.raises(SystemExit) as caught:
        counters.main()
    assert caught.value.code == 1
    report = json.loads((invocation / 'report.json').read_text())
    assert report['input_final_verification']['state'] == 'failed'
    assert report['failures'][0]['phase'] == 'final-input-verification'
    assert report['rows'][0]['filesystem'][0]['measurements']['read']['calls'] == 3
    assert 'reference_inputs.py' in report['harness_file_sha256']
    assert (invocation / 'counter-summary.csv').is_file()
