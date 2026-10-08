import pytest
import json
import sys
import kernel_comparison as kernel
from kernel_comparison import verify_firmware
from prepare_firmware_comparison import config_values
from reference_baselines import digest


def receipt(tmp_path):
    (tmp_path/'linux-6.12.109').mkdir()
    (tmp_path/'libkrunfw.so.5').write_bytes(b'new-built-library')
    (tmp_path/'linux-6.12.109/.config').write_text(
        ''.join(option+'=y\n' for option in kernel.REQUIRED_KERNEL_CONFIG)
        +'# CONFIG_PCI is not set\nCONFIG_LOCALVERSION="test"\n')
    (tmp_path/'linux-6.12.109/vmlinux').write_bytes(b'new-built-kernel')
    return dict(packaging='identical current compact bundle for both configurations',source_manifest_sha256='source',kernel_tarball_sha256='input',
        variants=dict(candidate=dict(firmware_sha256=digest(tmp_path/'libkrunfw.so.5'),config_sha256=digest(tmp_path/'linux-6.12.109/.config'),vmlinux_sha256=digest(tmp_path/'linux-6.12.109/vmlinux'))))


def test_declared_build_identity_requires_all_actual_artifacts(tmp_path):
    r=receipt(tmp_path)
    assert verify_firmware(r,'candidate',tmp_path)==r['variants']['candidate']
    (tmp_path/'libkrunfw.so.5').write_bytes(b'old-library')
    with pytest.raises(ValueError,match='firmware bytes'):verify_firmware(r,'candidate',tmp_path)


def test_changed_config_or_kernel_is_not_a_matching_build(tmp_path):
    r=receipt(tmp_path)
    (tmp_path/'linux-6.12.109/.config').write_text('CONFIG_SMP=n\n')
    with pytest.raises(ValueError,match='config'):verify_firmware(r,'candidate',tmp_path)


def test_bundling_and_source_provenance_cannot_be_omitted(tmp_path):
    r=receipt(tmp_path);r.pop('source_manifest_sha256')
    with pytest.raises(ValueError,match='provenance'):verify_firmware(r,'candidate',tmp_path)


def test_config_changes_preserve_enabled_disabled_and_literal_values(tmp_path):
    receipt(tmp_path)
    (tmp_path/'linux-6.12.109/.config').write_text('CONFIG_SMP=y\n# CONFIG_PCI is not set\nCONFIG_LOCALVERSION="test"\n')
    assert config_values(tmp_path/'linux-6.12.109/.config')==dict(CONFIG_SMP='y',CONFIG_PCI='n',CONFIG_LOCALVERSION='"test"')


@pytest.mark.parametrize('option', ['CONFIG_CPU_MITIGATIONS','CONFIG_SECCOMP_FILTER','CONFIG_VIRTIO_FS','CONFIG_NET'])
def test_required_kernel_capabilities_are_checked_in_actual_config(tmp_path,option):
    r=receipt(tmp_path);path=tmp_path/'linux-6.12.109/.config'
    path.write_text(path.read_text().replace(option+'=y\n',option+'=n\n'))
    r['variants']['candidate']['config_sha256']=digest(path)
    with pytest.raises(ValueError,match='omits required '+option):verify_firmware(r,'candidate',tmp_path)


@pytest.fixture
def source_build(tmp_path):
    source=tmp_path/'source';source.mkdir()
    (source/'Makefile').write_text('same build rules\n')
    (source/'config-libkrunfw_x86_64').write_text('candidate config\n')
    manifest=[dict(path=p.name,sha256=digest(p)) for p in sorted(source.iterdir())]
    (tmp_path/'source-manifest.json').write_text(json.dumps(manifest))
    variants={}
    for name in ('baseline','candidate'):
        build=tmp_path/name;build.mkdir()
        (build/'Makefile').write_text('same build rules\n')
        (build/'config-libkrunfw_x86_64').write_text(name+' config\n')
        variants[name]=dict(input_config_sha256=digest(build/'config-libkrunfw_x86_64'))
    path=tmp_path/'build-receipt.json'
    path.write_text(json.dumps(dict(source_manifest_sha256=digest(tmp_path/'source-manifest.json'),variants=variants)))
    return path


@pytest.mark.parametrize('directory', ['source','baseline','candidate'])
def test_source_or_build_input_changes_reject_the_comparison(source_build, directory):
    assert kernel.verify_firmware_sources(source_build)['files']==2
    (source_build.parent/directory/'Makefile').write_text('changed build rules\n')
    with pytest.raises(ValueError,match='changed'):
        kernel.verify_firmware_sources(source_build)


def test_extra_frozen_source_cannot_hide_outside_the_manifest(source_build):
    (source_build.parent/'source/unrecorded.patch').write_text('additional patch\n')
    with pytest.raises(ValueError,match='inventory changed'):
        kernel.verify_firmware_sources(source_build)


def test_frozen_source_rejects_parent_escape(source_build):
    manifest=source_build.parent/'source-manifest.json'
    manifest.write_text(json.dumps([dict(path='../foreign',sha256='unused')]))
    receipt=json.loads(source_build.read_text());receipt['source_manifest_sha256']=digest(manifest)
    source_build.write_text(json.dumps(receipt))
    with pytest.raises(ValueError,match='invalid frozen source path'):
        kernel.verify_firmware_sources(source_build)


@pytest.fixture
def invocation(tmp_path,monkeypatch):
    assets=tmp_path/'assets';assets.mkdir()
    (assets/'assets.json').write_text('{}');(assets/'input-manifest.json').write_text('[]')
    binary=tmp_path/'pvisor';binary.write_bytes(b'current product fixture')
    manifest=tmp_path/'source-manifest.json';manifest.write_text('[]')
    build=tmp_path/'build-receipt.json'
    build.write_text(json.dumps(dict(pvisor_sha256=digest(binary),source_manifest_sha256=digest(manifest))))
    firmware=tmp_path/'firmware-build-receipt.json'
    firmware.write_text(json.dumps(dict(source_manifest_sha256=digest(manifest),variants={
        'baseline':dict(firmware_sha256='a'),'candidate':dict(firmware_sha256='b')})))
    output=tmp_path/'.data/kernel'
    monkeypatch.setattr(kernel,'verify_firmware',lambda *_:None)
    monkeypatch.setattr(kernel,'verify_firmware_sources',lambda *_:dict(state='passed',files=2))
    monkeypatch.setenv('PVISOR_FS_PROFILE','test-before')
    monkeypatch.setenv('PVISOR_STARTUP_TIMING','test-before')
    argv=['kernel_comparison.py','--assets',str(assets),'--binary',str(binary),
          '--build-receipt',str(build),'--firmware-receipt',str(firmware),
          '--baseline',str(tmp_path/'baseline'),'--candidate',str(tmp_path/'candidate'),
          '--output',str(output),'--modes','ready','--samples','1','--warmups','0']
    for name in ('baseline','candidate'):
        p=tmp_path/name;p.mkdir();(p/'libkrunfw.so.5').write_bytes(name.encode())
    monkeypatch.setattr(sys,'argv',argv)
    return output


def test_invalid_inputs_fail_before_any_kernel_task(invocation,monkeypatch):
    def invalid(_):raise ValueError('input bytes changed')
    def forbidden(*_):pytest.fail('invalid inputs reached a kernel task')
    monkeypatch.setattr(kernel,'verify_reference_inputs',invalid)
    monkeypatch.setattr(kernel,'run_trial',forbidden)
    with pytest.raises(ValueError,match='input bytes changed'):kernel.main()
    report=json.loads((invocation/'report.json').read_text())
    assert report['input_verification']['state']=='failed'
    assert report['rows']==[]


def test_changed_final_inputs_preserve_kernel_rows_and_fail(invocation,monkeypatch):
    inputs=iter([dict(identity='before'),dict(identity='after')])
    monkeypatch.setattr(kernel,'verify_reference_inputs',lambda _:next(inputs))
    def run(args,meta,backend,mode,trial):
        assert args.backends=='pvisor-vm'
        return dict(trial=trial,mode=mode)
    monkeypatch.setattr(kernel,'run_trial',run)
    with pytest.raises(SystemExit) as error:kernel.main()
    assert error.value.code==1
    report=json.loads((invocation/'report.json').read_text())
    assert len(report['rows'])==2
    assert report['input_final_verification']['state']=='failed'
    assert report['failures'][0]['phase']=='final-input-verification'


def test_warmup_failure_still_audits_final_input_identity(invocation,monkeypatch):
    sys.argv[sys.argv.index('--warmups')+1]='1'
    observations=[]
    def inputs(_):observations.append(True);return dict(identity='unchanged')
    def run(args,meta,backend,mode,trial):
        if trial==-1:raise RuntimeError('warmup task failed')
        return dict(trial=trial,mode=mode)
    monkeypatch.setattr(kernel,'verify_reference_inputs',inputs)
    monkeypatch.setattr(kernel,'run_trial',run)
    with pytest.raises(SystemExit) as error:kernel.main()
    assert error.value.code==1 and len(observations)==2
    report=json.loads((invocation/'report.json').read_text())
    assert report['input_final_verification']['state']=='passed'
    assert report['rows']==[] and report['failures']
