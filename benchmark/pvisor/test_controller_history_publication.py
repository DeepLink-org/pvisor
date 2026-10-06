import copy
import pytest

from publish_controller_history import summarize
from test_controller_history import report as example_report


def cohort():
    example=example_report();value=example['rows'][0]
    value.update(reopen_us=1000,wal_bytes_after_completion=4096,controller_process_memory_before_query=dict(VmRSS_kib=1024))
    value['indexed_counts']['p50']=100
    before=dict(peak_bytes=1024,events={},cpu=dict(usage_usec=1000))
    after=dict(peak_bytes=4096,events={},cpu=dict(usage_usec=2000))
    return dict(benchmark_id='B-CLUSTER',arguments=dict(repetitions=3,sizes='1000',query_samples=30,cpu_affinity='0,1',memory_mib=16384),
        rows=[dict(size=1000,trial=i,correctness='passed',memory_bytes=16384*1024**2,example_report=copy.deepcopy(example),
            before=copy.deepcopy(before),after=copy.deepcopy(after),samples=[]) for i in range(3)])


def test_history_keeps_independent_process_and_query_sample_counts_separate():
    rows=summarize(cohort())
    assert len(rows)==6 and all(r['n']==3 and r['query_batches_per_process']==30 for r in rows)
    assert all('p95' not in r and 'p99' not in r for r in rows)


@pytest.mark.parametrize('mutation',['missing','duplicate','failed','budget','oom','cpu','implementation','samples'])
def test_invalid_history_observation_cannot_be_published(mutation):
    value=cohort();row=value['rows'][0]
    if mutation=='missing':value['rows'].pop()
    elif mutation=='duplicate':value['rows'][-1]=copy.deepcopy(row)
    elif mutation=='failed':row['correctness']='failed'
    elif mutation=='budget':row['memory_bytes']=2048*1024**2
    elif mutation=='oom':row['samples']=[dict(peak_bytes=8192,events=dict(oom_kill=1))]
    elif mutation=='cpu':row['after']['cpu']['usage_usec']=0
    elif mutation=='implementation':row['example_report']['schema']='pvisor-controller-history-load/v3'
    else:row['example_report']['rows'][0]['indexed_counts']['n']=90
    with pytest.raises(ValueError):summarize(value)
