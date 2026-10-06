import copy
import json

import pytest

from publish_vm_memory import validate_retained_evidence, validate_row_evidence
from reference_baselines import digest


def fixture(tmp_path, live):
    directory=tmp_path/'trials'/'one';directory.mkdir(parents=True)
    config=dict(root=str(directory),result=str(directory/'result.json'),trial=0,
        budget_bytes=2048*1024**2,cpu_affinity='0,1')
    row=dict(correctness='passed',trial=0,logs=str(directory))
    if live:
        config.update(pattern='random',compressed=True,seed=20261009)
        row.update(pattern='random',compressed=True,report=dict(proof='complete'))
        (directory/'vm').mkdir();(directory/'vm/raw.json').write_text(json.dumps(row['report']))
    else:
        config.update(kind='random',storage='raw',payload_bytes=64*1024**2)
        row.update(kind='random',storage='raw',ready=dict(token='same',bytes=64*1024**2),restored=dict(token='same',bytes=64*1024**2))
        (directory/'run.stdout').write_text('PVISOR_MEMORY_READY '+json.dumps(row['ready'])+'\n')
        (directory/'resume.stdout').write_text('PVISOR_MEMORY_RESULT '+json.dumps(row['restored'])+'\n')
        for name in ('suspend.stdout','suspend.json'):(directory/name).write_text('{"state":"suspended"}')
    (directory/'config.json').write_text(json.dumps(config))
    retained=row if live else {key:value for key,value in row.items() if key!='logs'}
    (directory/'result.json').write_text(json.dumps(retained))
    report=dict(arguments=dict(budget_mib=2048,cpu_affinity='0,1',warmups=3),rows=[row])
    (tmp_path/'rootfs-manifest.json').write_text('{"files":"frozen inputs"}')
    report['rootfs_manifest_sha256']=digest(tmp_path/'rootfs-manifest.json')
    report['binary_build']={'source_manifest_sha256':'frozen source'}
    (tmp_path/'build-receipt.json').write_text(json.dumps(report['binary_build']))
    return report,row,directory


@pytest.mark.parametrize('live',[False,True])
def test_independent_result_must_match_memory_aggregate(tmp_path,live):
    report,row,directory=fixture(tmp_path,live)
    assert validate_row_evidence(tmp_path,report,row,live)['trial']==0
    broken=copy.deepcopy(row);broken['correctness']='invented'
    with pytest.raises(ValueError,match='retained memory result'):
        validate_row_evidence(tmp_path,report,broken,live)


@pytest.mark.parametrize('field,value',[('budget_bytes',1),('cpu_affinity','2,3'),('trial',5),('seed',4)])
def test_independent_config_cannot_change_measurement(tmp_path,field,value):
    report,row,directory=fixture(tmp_path,True)
    config=json.loads((directory/'config.json').read_text());config[field]=value
    (directory/'config.json').write_text(json.dumps(config))
    with pytest.raises(ValueError):validate_row_evidence(tmp_path,report,row,True)


def test_independent_sdk_proof_cannot_be_replaced(tmp_path):
    report,row,directory=fixture(tmp_path,True)
    (directory/'vm/raw.json').write_text('{"proof":"different"}')
    with pytest.raises(ValueError,match='retained SDK proof'):
        validate_row_evidence(tmp_path,report,row,True)


@pytest.mark.parametrize('filename,content',[
    ('run.stdout',''),('resume.stdout','PVISOR_MEMORY_RESULT {"token":"different"}\n'),
    ('suspend.stdout','{"state":"running"}'),('suspend.json','{"state":"running"}')])
def test_snapshot_outputs_must_prove_original_execution(tmp_path,filename,content):
    report,row,directory=fixture(tmp_path,False)
    (directory/filename).write_text(content)
    with pytest.raises(ValueError):validate_row_evidence(tmp_path,report,row,False)


def test_outside_cohort_evidence_is_rejected(tmp_path):
    root=tmp_path/'cohort';root.mkdir();outside=tmp_path/'outside';outside.mkdir()
    report,row,directory=fixture(outside,True)
    with pytest.raises(ValueError,match='outside-cohort'):
        validate_row_evidence(root,report,row,True)


def test_symlink_cannot_redirect_sdk_output_outside_cohort(tmp_path):
    root=tmp_path/'cohort';root.mkdir();report,row,directory=fixture(root,True)
    outside=tmp_path/'proof.json';outside.write_text(json.dumps(row['report']))
    proof=directory/'vm/raw.json';proof.unlink();proof.symlink_to(outside)
    with pytest.raises(ValueError,match='outside-cohort'):
        validate_row_evidence(root,report,row,True)


def test_frozen_memory_harness_requires_exact_inventory_and_hashes(tmp_path):
    report,row,directory=fixture(tmp_path,True)
    harness=tmp_path/'harness';harness.mkdir();script=harness/'runner.py';script.write_text('# frozen\n')
    report['harness_sha256']={'runner.py':digest(script)}
    assert len(validate_retained_evidence(tmp_path,report,True))==1
    script.write_text('# changed\n')
    with pytest.raises(ValueError,match='hash mismatch'):validate_retained_evidence(tmp_path,report,True)
    script.write_text('# frozen\n');(harness/'unexpected.py').write_text('# unexpected\n')
    with pytest.raises(ValueError,match='inventory'):validate_retained_evidence(tmp_path,report,True)


def test_matching_empty_snapshot_markers_do_not_prove_recovery(tmp_path):
    report,row,directory=fixture(tmp_path,False)
    row['ready']['bytes']=row['restored']['bytes']=0
    (directory/'result.json').write_text(json.dumps({k:v for k,v in row.items() if k!='logs'}))
    for filename,prefix,key in (('run.stdout','PVISOR_MEMORY_READY ','ready'),('resume.stdout','PVISOR_MEMORY_RESULT ','restored')):
        (directory/filename).write_text(prefix+json.dumps(row[key])+'\n')
    with pytest.raises(ValueError,match='condition differs'):validate_row_evidence(tmp_path,report,row,False)


@pytest.mark.parametrize('filename',['rootfs-manifest.json','build-receipt.json'])
def test_retained_input_and_build_records_cannot_be_replaced(tmp_path,filename):
    report,row,directory=fixture(tmp_path,True)
    harness=tmp_path/'harness';harness.mkdir();report['harness_sha256']={}
    (tmp_path/filename).write_text('{}')
    with pytest.raises(ValueError):validate_retained_evidence(tmp_path,report,True)
