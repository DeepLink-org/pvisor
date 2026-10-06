import copy
import pytest
from live_vm_memory import validate_report


def valid():
    phase=dict(cgroup='/owned',current_bytes=100,stat=dict(anon=50,file=40,kernel=10),cpu=dict(usage_usec=100),events=dict(oom=0,oom_kill=0))
    row=dict(active=copy.deepcopy(phase),offloaded=copy.deepcopy(phase),restored=copy.deepcopy(phase),backed_bytes=256*1024**2,
             offload_ms=1,offload_resume_ms=1,baseline_read_ms=1,restored_read_ms=1)
    report=dict(schema='pvisor-live-offload/v1',correctness='passed',pattern='random',seed=5,compressed=True,samples=1,warmups=0,guest_data_bytes=64*1024**2,memory_mib=256,cpus=2,settle_ms=2000,long_pause_seconds=0,stress=False,cancel_while=None,rows=[row])
    return report,dict(pattern='random',seed=5,compressed=True,cgroup='/owned')


def test_live_offload_rejects_wrong_guest_or_missing_physical_accounting():
    report,config=valid();validate_report(report,config)
    for mutation in ('seed','cgroup','cache','oom','ram','correctness'):
        changed=copy.deepcopy(report)
        if mutation=='seed':changed['seed']=6
        elif mutation=='cgroup':changed['rows'][0]['offloaded']['cgroup']='/escaped'
        elif mutation=='cache':del changed['rows'][0]['offloaded']['stat']['file']
        elif mutation=='oom':changed['rows'][0]['restored']['events']['oom_kill']=1
        elif mutation=='ram':changed['rows'][0]['backed_bytes']=0
        else:changed['correctness']='failed'
        with pytest.raises(ValueError):validate_report(changed,config)
