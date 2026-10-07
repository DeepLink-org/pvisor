"""Publication gates exercised with synthetic fixtures, never public results."""
import copy
import json
from pathlib import Path

import pytest
from memory_savings import digest,matrix
from publish_memory_savings import load_cohort,publish
from test_memory_savings import report as raw_fixture


def cohort(root):
    root.mkdir()
    artifacts={'probe':'synthetic binary','harness.py':'synthetic harness',
               'linux_cold_runtime.py':'synthetic helper','source-manifest.json':'[]\n'}
    for name,value in artifacts.items():(root/name).write_text(value)
    receipt=dict(example_sha256=digest(root/'probe'),source_manifest_sha256=digest(root/'source-manifest.json'))
    (root/'build-receipt.json').write_text(json.dumps(receipt));(root/'input-manifest.json').write_text('{}')
    value=dict(schema='pvisor-memory-savings-cohort/v1',benchmark_id='B-VM-MEMORY',role='user-facing',
        complete=True,arguments=dict(samples=30,warmups=3,wait=60),selected=matrix(),
        input_verification={'rootfs':True,'firmware':True},
        host_verification={k:True for k in ('kernel','cpu_model','ksm','host_cp_sha256')},
        binary_sha256=digest(root/'probe'),harness_sha256=digest(root/'harness.py'),
        helper_sha256=digest(root/'linux_cold_runtime.py'),host={},
        budget=dict(cpu_cores=4,memory_max=2147483648,swap_max=0),attempts=[])
    for r in range(-3,30):
        for mode,pattern in matrix():
            trial=root/str(len(value['attempts']));(trial/'w').mkdir(parents=True)
            raw=raw_fixture();raw.update(mode=mode,pattern=pattern,park_ms=1,resume_ack_ms=2,resume_to_task_ms=3)
            for task in raw['tasks']:task['task_ms']=1
            if mode=='release':raw['tasks'].append(dict(token='free',bytes=0,digest='0'*64))
            if mode in ('raw','compressed'):raw['storage_allocated_bytes']=4096
            path=trial/'w/raw.json';path.write_text(json.dumps(raw))
            monitor=[dict(group='/owned',memory_peak=110,budget={'cpu.max':'400000 100000',
                'memory.max':'2147483648','memory.swap.max':'0','memory.swap.current':'0','pids.max':'128'})]
            (trial/'monitor.json').write_text(json.dumps(monitor));(trial/'guard.json').write_text('[{"jobs":[]}]')
            value['attempts'].append(dict(condition=dict(mode=mode,pattern=pattern,wait=60),round=r,warmup=r<0,
                status='successful',returncode=0,unit_quiescent=True,raw_sha256=digest(path)))
    path=root/'report.json';path.write_text(json.dumps(value));return path,value


def test_complete_cohort_exports_only_aggregates(tmp_path):
    path,value=cohort(tmp_path/'fixture')
    rows=publish(path,tmp_path/'derived')
    assert rows and all(r['n']==30 for r in rows)
    assert {'park_cpu_ms', 'idle-60_anon_mib', 'idle-60_file_mib',
            'idle-60_kernel_mib'} <= {row['metric'] for row in rows}
    assert (tmp_path/'derived/memory-choices-comparisons.csv').is_file()
    assert len(load_cohort(path)[1])==33*16


@pytest.mark.parametrize('change',[
    lambda r:r['attempts'].pop(0),
    lambda r:r['attempts'].append(copy.deepcopy(r['attempts'][0])),
    lambda r:r['attempts'][0].update(status='failed'),
    lambda r:r['attempts'][0].update(warmup=False),
    lambda r:r['host_verification'].update(ksm=False),
    lambda r:r['budget'].update(memory_max=1073741824),
    lambda r:r['attempts'][0]['condition'].update(wait=5),
])
def test_incomplete_or_contaminated_evidence_cannot_publish(tmp_path,change):
    path,value=cohort(tmp_path/'fixture');change(value);path.write_text(json.dumps(value))
    with pytest.raises(ValueError):load_cohort(path)


def test_retained_raw_and_monitor_bytes_are_required(tmp_path):
    path,value=cohort(tmp_path/'fixture');raw_path=path.parent/'0/w/raw.json'
    raw=json.loads(raw_path.read_text());raw['tasks'][1]['digest']='b'*64
    raw_path.write_text(json.dumps(raw))
    with pytest.raises(ValueError,match='raw evidence'):load_cohort(path)
    value['attempts'][0]['raw_sha256']=digest(raw_path);path.write_text(json.dumps(value))
    with pytest.raises(ValueError,match='integrity'):load_cohort(path)


def test_static_export_has_values_without_quantiles_or_confidence_intervals(tmp_path):
    path,value=cohort(tmp_path/'fixture')
    value['arguments'].update(static=True,samples=1,warmups=0,wait=5)
    value['attempts']=value['attempts'][:16]
    for index,row in enumerate(value['attempts']):
        row.update(round=0,warmup=False);row['condition']['wait']=5
        raw_path=path.parent/str(index)/'w/raw.json'
        raw=json.loads(raw_path.read_text());raw['wait']=5
        for phase in raw['phases']:
            if phase['name']=='idle-60':phase['name']='idle-5'
        raw_path.write_text(json.dumps(raw));row['raw_sha256']=digest(raw_path)
        (raw_path.parent.parent/'guard.json').write_text('[{"jobs":[{"pid":123,"kind":"build/test"}]}]')
    path.write_text(json.dumps(value))
    with pytest.raises(ValueError):load_cohort(path)
    rows=publish(path,tmp_path/'derived',static=True)
    assert rows and all(row['n']==1 and 'value' in row and 'p50' not in row for row in rows)
    comparisons=(tmp_path/'derived/memory-choices-comparisons.csv').read_text()
    assert 'observed_difference' in comparisons and 'ci95' not in comparisons
