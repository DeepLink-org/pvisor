import copy
import pytest

from controller_history import validate_result


def report():
    return dict(schema='pvisor-controller-history-load/v4',benchmark_id='B-CLUSTER',build_has_debug_assertions=False,
        source_identity_unchanged_during_measurement=True,affinity='0-1',rows=[dict(tasks=1000,ready_tasks_before_poll=1,
        cancelled_history=999,indexed_counts=dict(n=30),replay_fencing_verified=True,current_polls_until_assignment=1,
        final_counts=dict(cancelled=999,failed=1))])


def test_history_requires_current_verified_record_and_replay_counts():
    assert validate_result(report(),1000,30,'0,1')['tasks']==1000


@pytest.mark.parametrize('mutation',['schema','debug','source','records','query','fencing','affinity'])
def test_unverified_history_cannot_be_published(mutation):
    value=copy.deepcopy(report())
    if mutation=='schema':value['schema']='pvisor-controller-history-load/v3'
    elif mutation=='debug':value['build_has_debug_assertions']=True
    elif mutation=='source':value['source_identity_unchanged_during_measurement']=False
    elif mutation=='records':value['rows'][0]['final_counts']['cancelled']=998
    elif mutation=='query':value['rows'][0]['indexed_counts']['n']=20
    elif mutation=='fencing':value['rows'][0]['replay_fencing_verified']=False
    else:value['affinity']='0-2'
    with pytest.raises(ValueError):validate_result(value,1000,30,'0,1')


def test_nonadjacent_cpu_budget_does_not_accept_intermediate_cpu():
    value=report();value['affinity']='1-3'
    with pytest.raises(ValueError,match='affinity'):validate_result(value,1000,30,'1,3')
