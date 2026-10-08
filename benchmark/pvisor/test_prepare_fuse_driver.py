import json
import pytest
from prepare_fuse_driver import source_inventory, verify_driver_receipt
from reference_baselines import digest


@pytest.mark.parametrize('fault',[None,'binary','manifest','source','extra','escape','duplicate','incomplete'])
def test_driver_receipt_binds_complete_source_and_executable(tmp_path,fault):
    source=tmp_path/'source';source.mkdir();(source/'driver.rs').write_bytes(b'frozen control')
    binary=tmp_path/'driver';binary.write_bytes(b'built ELF')
    manifest=tmp_path/'source-manifest.json';manifest.write_text(json.dumps(source_inventory(source)))
    receipt=tmp_path/'build-receipt.json'
    value=dict(state='passed',driver_sha256=digest(binary),source_manifest_sha256=digest(manifest))
    receipt.write_text(json.dumps(value))
    assert verify_driver_receipt(receipt,binary)==value
    if fault is None:return
    if fault=='binary':binary.write_bytes(b'changed ELF')
    elif fault=='manifest':manifest.write_text('[]')
    elif fault=='source':(source/'driver.rs').write_bytes(b'changed source')
    elif fault=='extra':(source/'extra.rs').write_bytes(b'undeclared source')
    elif fault=='incomplete':value['state']='building';receipt.write_text(json.dumps(value))
    else:
        records=json.loads(manifest.read_text())
        if fault=='escape':records[0]['path']='../outside'
        else:records.append(records[0])
        manifest.write_text(json.dumps(records));value['source_manifest_sha256']=digest(manifest);receipt.write_text(json.dumps(value))
    with pytest.raises(ValueError):verify_driver_receipt(receipt,binary)
