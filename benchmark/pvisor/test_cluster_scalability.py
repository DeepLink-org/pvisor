import json
import pytest
from cluster_scalability import EXPECTED, MIB, validate_result


def record(**changes):
    result=dict(token='task-1',bytes=32*MIB,checksum=EXPECTED,changes=4,integrity='passed')|changes
    return dict(phase='succeeded',lease=dict(key=dict(worker_id='qs-a')),
        result=dict(output=dict(stdout='CLUSTER_RESULT '+json.dumps(result)+'\n')))


def test_complete_useful_task_requires_original_token_and_checked_work():
    assert validate_result(record(),'task-1','a')['integrity']=='passed'


@pytest.mark.parametrize('changes',[dict(token='other-task'),dict(bytes=0),dict(checksum='wrong'),dict(changes=0),dict(integrity='failed')])
def test_fast_but_incorrect_outputs_do_not_count_as_throughput(changes):
    with pytest.raises(ValueError,match='incorrect work'):
        validate_result(record(**changes),'task-1','a')


def test_wrong_worker_and_duplicate_or_failed_results_are_rejected():
    value=record()
    with pytest.raises(ValueError,match='wrong Worker'):validate_result(value,'task-1','b')
    value['result']['output']['stdout']*=2
    with pytest.raises(ValueError,match='unique'):validate_result(value,'task-1','a')
    value['phase']='failed'
    with pytest.raises(ValueError,match='did not succeed'):validate_result(value,'task-1','a')


def test_useful_throughput_plot_rejects_ready_only_and_changed_total_budget():
    import copy
    from plot_cluster_scalability import summarize_execution
    with pytest.raises(ValueError,match='useful-task'):summarize_execution(dict(passed=True,batches=[]),'sha')
    sample=dict(correctness='passed',workers=1,submitted=12,completed=12,failed=0,controls=dict(cpu_max=[200000,100000],memory_max=2048*1024**2,swap_max=0),
                after=dict(events={},peak_bytes=100),before=dict(peak_bytes=90),samples=[],completion_ms=100,validated_per_second=120)
    report=dict(benchmark_id='B-CLUSTER',arguments=dict(sizes='1',repetitions=30,warmups=0,tasks=12,budget_mib=2048,cpu_affinity='0,1'),rows=[copy.deepcopy(sample)|dict(trial=i) for i in range(30)])
    assert len(summarize_execution(report,'sha'))==3
    changed=copy.deepcopy(report);changed['rows'][0]['controls']['cpu_max']=[400000,100000]
    with pytest.raises(ValueError,match='controls differ'):summarize_execution(changed,'sha')
    changed=copy.deepcopy(report);changed['rows'][0]['completed']=0
    with pytest.raises(ValueError,match='useful-task batch'):summarize_execution(changed,'sha')


def retained_cohort(tmp_path):
    from reference_baselines import digest
    report=dict(arguments=dict(tasks=1),rows=[])
    for name,field in [('cluster_scalability.py','harness_sha256'),('cluster_worker.py','worker_sha256'),('cluster-quickstart.py','quickstart_sha256'),('input-manifest.json','input_manifest_sha256')]:
        path=tmp_path/name;path.write_text('retained input');report[field]=digest(path)
    root=tmp_path/'trial';root.mkdir()
    value=record();(root/'task-1.json').write_text(json.dumps(value))
    result=json.loads(value['result']['output']['stdout'].removeprefix('CLUSTER_RESULT '))
    report['rows']=[dict(logs=str(root),tasks=[dict(id='task-1',node='a',result=result)])]
    return report


def test_cluster_publication_binds_each_result_to_retained_execution(tmp_path):
    from plot_cluster_scalability import verify_execution_evidence
    report=retained_cohort(tmp_path)
    assert len(verify_execution_evidence(report,tmp_path))==1


@pytest.mark.parametrize('mutation',['input','missing','duplicate','result','worker','outside'])
def test_cluster_publication_rejects_unverified_execution(tmp_path,mutation):
    import copy
    from plot_cluster_scalability import verify_execution_evidence
    report=retained_cohort(tmp_path);row=report['rows'][0];path=tmp_path/'trial/task-1.json'
    if mutation=='input':(tmp_path/'cluster_worker.py').write_text('changed input')
    elif mutation=='missing':path.unlink()
    elif mutation=='duplicate':report['rows'].append(copy.deepcopy(row))
    elif mutation=='result':row['tasks'][0]['result']['changes']=0
    elif mutation=='worker':row['tasks'][0]['node']='b'
    else:
        outside=tmp_path.parent/(tmp_path.name+'-outside.json');outside.write_bytes(path.read_bytes());path.unlink();path.symlink_to(outside)
    with pytest.raises((ValueError,FileNotFoundError)):verify_execution_evidence(report,tmp_path)


def test_cluster_completion_comparison_preserves_paired_rounds():
    from plot_cluster_scalability import compare_completion
    report=dict(arguments=dict(sizes='1,2'),rows=[dict(workers=size,trial=trial,completion_ms=100/size)
        for size in (1,2) for trial in range(30)])
    value=compare_completion(report,'sha')[0]
    assert value['difference_ms']==value['ci95_low_ms']==value['ci95_high_ms']==-50
    report['rows'].pop()
    with pytest.raises(ValueError,match='paired comparison'):compare_completion(report,'sha')
