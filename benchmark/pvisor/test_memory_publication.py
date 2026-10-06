import copy
import pytest
from publish_vm_memory import validate_cohort


def cohort():
    phase=dict(cgroup='/owned',current_bytes=100,stat=dict(anon=50,file=40,kernel=10),cpu=dict(usage_usec=100),events=dict(oom=0,oom_kill=0))
    report=dict(benchmark_id='B-VM-MEMORY',mechanism='current SDK whole-VM offload',arguments=dict(samples=30,warmups=3),failures=[],rows=[])
    for trial in range(30):
        for pattern in ('repeated','random'):
            for compressed in (False,True):
                row=dict(active=copy.deepcopy(phase),offloaded=copy.deepcopy(phase),restored=copy.deepcopy(phase),backed_bytes=256*1024**2,
                         offload_ms=1,offload_resume_ms=1,baseline_read_ms=1,restored_read_ms=1)
                proof=dict(schema='pvisor-live-offload/v1',correctness='passed',pattern=pattern,seed=20261006+trial+3,compressed=compressed,samples=1,warmups=0,guest_data_bytes=64*1024**2,memory_mib=256,cpus=2,settle_ms=2000,long_pause_seconds=0,stress=False,cancel_while=None,rows=[copy.deepcopy(row)])
                report['rows'].append(row|dict(pattern=pattern,compressed=compressed,trial=trial,correctness='passed',report=proof))
    return report


def test_partial_or_duplicate_live_memory_cannot_be_published():
    report=cohort();assert validate_cohort(report)
    for mutation in ('missing','duplicate','failed','seed','timing','memory','transient_oom'):
        changed=copy.deepcopy(report)
        if mutation=='missing':changed['rows'].pop()
        elif mutation=='duplicate':changed['rows'][-1]=copy.deepcopy(changed['rows'][0])
        elif mutation=='failed':changed['failures'].append(dict(error='failed trial'))
        elif mutation=='seed':changed['rows'][0]['report']['seed']=0
        elif mutation=='timing':changed['rows'][0]['offload_ms']=0
        elif mutation=='memory':changed['rows'][0]['offloaded']['current_bytes']=1
        else:changed['rows'][0]['samples']=[dict(events=dict(oom=1,oom_kill=0))]
        with pytest.raises(ValueError):validate_cohort(changed)


@pytest.mark.parametrize('value',[float('nan'),float('inf'),-1,True])
def test_matching_sdk_proof_cannot_publish_invalid_numeric_metrics(value):
    report=cohort();row=report['rows'][0]
    row['offload_ms']=row['report']['rows'][0]['offload_ms']=value
    with pytest.raises(ValueError):validate_cohort(report)


@pytest.mark.parametrize('value',[float('nan'),float('inf'),-1])
def test_memory_component_cannot_be_nonfinite_or_negative(value):
    report=cohort();row=report['rows'][0]
    row['offloaded']['stat']['file']=row['report']['rows'][0]['offloaded']['stat']['file']=value
    with pytest.raises(ValueError):validate_cohort(report)
