import copy
import json
from pathlib import Path

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


def test_legacy_retained_provenance_does_not_require_new_guard_files(tmp_path):
    report,row,directory=fixture(tmp_path,True)
    harness=tmp_path/'harness';harness.mkdir();report['harness_sha256']={}
    report['attempts']=[dict(result=row,host_admitted=False,unit_quiescent=True)]
    assert len(validate_retained_evidence(tmp_path,report,True))==1


def guarded_fixture(tmp_path):
    report,row,directory=fixture(tmp_path,True)
    report['arguments'].update(samples=30,warmups=1)
    report['protocol']={'host_guard':{'enabled':True}}
    harness=tmp_path/'harness';harness.mkdir();report['harness_sha256']={}
    report['rows']=[];report['attempts']=[]
    phase=dict(cgroup='/owned',current_bytes=100,stat=dict(anon=50,file=40,kernel=10),
        cpu=dict(usage_usec=100),events=dict(oom=0,oom_kill=0))
    metrics=dict(active=phase,offloaded=phase,restored=phase,backed_bytes=256*1024**2,
        offload_ms=1,offload_resume_ms=1,baseline_read_ms=1,restored_read_ms=1)
    for trial in range(-1,30):
        for pattern in ('repeated','random'):
            for compressed in (False,True):
                logs=tmp_path/'trials'/f'{trial+1}-{pattern}-{compressed}';logs.mkdir()
                result=copy.deepcopy(row)
                result.update(metrics)
                result.update(trial=trial,pattern=pattern,compressed=compressed,logs=str(logs))
                result['report']=dict(schema='pvisor-live-offload/v1',correctness='passed',pattern=pattern,
                    compressed=compressed,seed=20261007+trial,samples=1,warmups=0,guest_data_bytes=64*1024**2,
                    memory_mib=256,cpus=2,settle_ms=2000,long_pause_seconds=0,stress=False,cancel_while=None,
                    rows=[copy.deepcopy(metrics)])
                config=dict(root=str(logs),result=str(logs/'result.json'),trial=trial,pattern=pattern,
                    compressed=compressed,seed=20261007+trial,budget_bytes=2048*1024**2,cpu_affinity='0,1')
                (logs/'config.json').write_text(json.dumps(config))
                (logs/'result.json').write_text(json.dumps(result))
                (logs/'vm').mkdir();(logs/'vm/raw.json').write_text(json.dumps(result['report']))
                service=dict(unit=logs.name,command=['service',logs.name],started_at='retained start',
                    timeout_seconds=240,deadline=False,correctness='passed',returncode=0,
                    logs=str(logs),result=result,host_interference=[],host_guard_errors=[],unit_quiescent=True)
                (logs/'service-result.json').write_text(json.dumps(service))
                launch={key:service[key] for key in ('unit','command','started_at','timeout_seconds','deadline','logs')}
                launch['correctness']='failed'
                (logs/'service-launch.json').write_text(json.dumps(launch))
                quiet=[dict(time_ns=second*10**9,jobs=[],inspection_errors=[]) for second in range(31)]
                (logs/'prelaunch-wait.jsonl').write_text(''.join(json.dumps(sample)+'\n' for sample in quiet))
                (logs/'host-guard.jsonl').write_text(''.join(json.dumps(sample)+'\n' for sample in quiet[:2]))
                proof=dict(unit=logs.name,unit_quiescent=True,show_returncode=0,
                    state=dict(ActiveState='inactive',LoadState='not-found'))
                (logs/'service-quiescence.json').write_text(json.dumps(proof))
                (logs/'unit-final.txt').write_text('ActiveState=inactive\nLoadState=not-found\n')
                report['attempts'].append(service|dict(host_admitted=True,trial=trial,pattern=pattern,compressed=compressed))
                if trial>=0:report['rows'].append(result)
    return report


def test_guarded_retained_evidence_includes_warmups_and_service_proofs(tmp_path):
    report=guarded_fixture(tmp_path)
    evidence=validate_retained_evidence(tmp_path,report,True)
    assert len(evidence)==124 and sum(row['trial']==-1 for row in evidence)==4
    assert all(len(row['files'])==9 for row in evidence)


def test_logged_inspection_limitations_are_not_reinterpreted_as_guard_failures(tmp_path):
    report=guarded_fixture(tmp_path)
    directory=Path(report['attempts'][0]['logs'])
    for name in ('prelaunch-wait.jsonl','host-guard.jsonl'):
        samples=[json.loads(line) for line in (directory/name).read_text().splitlines()]
        for sample in samples:sample['inspection_errors']=[dict(pid=123,error='inaccessible FD')]
        (directory/name).write_text(''.join(json.dumps(sample)+'\n' for sample in samples))
    assert len(validate_retained_evidence(tmp_path,report,True))==124


@pytest.mark.parametrize('filename',['prelaunch-wait.jsonl','host-guard.jsonl','service-launch.json',
    'service-result.json','service-quiescence.json','unit-final.txt','result.json','vm/raw.json'])
def test_guarded_warmup_missing_proof_rejects_publication(tmp_path,filename):
    report=guarded_fixture(tmp_path)
    directory=Path(report['attempts'][0]['logs'])
    (directory/filename).unlink()
    with pytest.raises(ValueError,match='missing or outside-cohort'):
        validate_retained_evidence(tmp_path,report,True)


@pytest.mark.parametrize('mutation',['interference','guard_error','empty_guard','short_admission',
    'late_admission_job','service_result','launch','quiescence','final_state'])
def test_guarded_retained_failures_cannot_hide_behind_passed_rows(tmp_path,mutation):
    report=guarded_fixture(tmp_path)
    directory=Path(report['attempts'][0]['logs'])
    if mutation in ('interference','guard_error','empty_guard'):
        sample=dict(time_ns=1,jobs=['foreign VM'] if mutation=='interference' else [])
        if mutation=='guard_error':sample['guard_error']='inspection failed'
        (directory/'host-guard.jsonl').write_text('' if mutation=='empty_guard' else (json.dumps(sample)+'\n')*2)
    elif mutation in ('short_admission','late_admission_job'):
        samples=[dict(time_ns=0,jobs=[]),dict(time_ns=30*10**9,jobs=[])]
        if mutation=='short_admission':samples[-1]['time_ns']=29*10**9
        else:samples[-1]['jobs']=['build']
        (directory/'prelaunch-wait.jsonl').write_text(''.join(json.dumps(sample)+'\n' for sample in samples))
    elif mutation=='final_state':(directory/'unit-final.txt').write_text('ActiveState=active\n')
    else:
        filename={'service_result':'service-result.json','launch':'service-launch.json',
            'quiescence':'service-quiescence.json'}[mutation]
        proof=json.loads((directory/filename).read_text())
        if mutation=='service_result':proof['host_interference']=[{'jobs':['foreign VM']}]
        elif mutation=='launch':proof['unit']='other service'
        else:proof['unit_quiescent']=False
        (directory/filename).write_text(json.dumps(proof))
    with pytest.raises(ValueError):validate_retained_evidence(tmp_path,report,True)
