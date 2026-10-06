import pytest
from kernel_comparison import verify_firmware
from prepare_firmware_comparison import config_values
from reference_baselines import digest


def receipt(tmp_path):
    (tmp_path/'linux-6.12.109').mkdir()
    (tmp_path/'libkrunfw.so.5').write_bytes(b'new-built-library')
    (tmp_path/'linux-6.12.109/.config').write_text('CONFIG_SMP=y\n# CONFIG_PCI is not set\nCONFIG_LOCALVERSION="test"\n')
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
    assert config_values(tmp_path/'linux-6.12.109/.config')==dict(CONFIG_SMP='y',CONFIG_PCI='n',CONFIG_LOCALVERSION='"test"')
