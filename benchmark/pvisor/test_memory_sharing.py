"""Public sharing protocol checks, without VM or systemd access."""
import copy
import json
import pytest

from memory_scale import validate_report
from memory_sharing import matrix
from publish_memory_sharing import load_cohort
from test_memory_scale import valid


def test_public_matrix_separates_baseline_and_dynamic_dedup_controls():
    cells=matrix()
    assert len(cells)==36 and len(set(cells))==36
    for n in (1,2,4):
        for pattern in ('repeated','random-shared','random-unique'):
            assert (n,pattern,'independent') in cells
            assert (n,pattern,'shared') in cells
            assert (n,pattern,'ksm-off') in cells
            assert (n,pattern,'ksm-on') in cells


def test_independent_control_requires_distinct_physical_ram_inodes():
    raw,config=valid(scanner='1')
    config['independent_inodes']=True;config['cpus']=2
    raw['conditions']['independent_inodes']=True;raw['conditions']['cpus']=2;raw['profile']['cpus']=2
    with pytest.raises(ValueError,match='inode proof'):validate_report(raw,config)
    raw['checks'] += [dict(name='independent_ram_inode',passed=True,
                          evidence=dict(instance=i,device=1,inode=i,bytes=256*1024**2)) for i in (1,2)]
    validate_report(raw,config)
    bad=copy.deepcopy(raw);bad['checks'][-1]['evidence']['inode']=1
    with pytest.raises(ValueError,match='inode proof'):validate_report(bad,config)


def test_engineering_and_preflight_cohorts_cannot_publish(tmp_path):
    value=dict(schema='pvisor-memory-sharing-cohort/v1',role='engineering A/B',complete=True,
               arguments=dict(preflight=False,samples=30,warmups=3,scan_seconds=30),
               selected=matrix(),input_verification={'rootfs':True,'firmware':True})
    path=tmp_path/'report.json'
    for changes in ({},{'role':'user-facing','arguments':dict(preflight=True,samples=1,warmups=0,scan_seconds=2)}):
        path.write_text(json.dumps(value|changes))
        with pytest.raises(ValueError,match='public sharing cohort'):load_cohort(path)
