import copy
import csv
import json

import pytest

from publish_apply import publish, summarize
from reference_baselines import digest


def cohort():
    counts={'10':30,'1000':10,'100000':3}
    actions=['apply','drop','conflict','copy','git-apply']
    rows=[dict(files=int(size),workload=action,trial=trial,wall_ms=1+index+trial*.01,correctness='passed')
        for size,count in counts.items() for index,action in enumerate(actions) for trial in range(count)]
    states=['prepared','target_applied','committed']
    rows += [dict(files=10000,workload='crash-recovery',requested_state=state,state_at_death=state,
        kill_window_hit=True,trial=trial,recovery_ms=10+trial,correctness='passed')
        for state in states for trial in range(3)]
    return dict(benchmark_id='B-APPLY',apply_protocol=dict(sizes=list(map(int,counts)),actions=actions,
        samples_per_size=counts,warmups_per_size={'10':1,'1000':1,'100000':0},timing='command only'),
        crash_protocol=dict(files=10000,states=states),cli_arguments=dict(samples=30,cpu_affinity='0,1'),
        recorded_at='2026-10-06',platform='fixture',cpu='fixture',rows=rows,capabilities={})


def test_complete_apply_sweep_preserves_sample_counts_and_no_small_sample_tails():
    summary,recovery=summarize(cohort())
    assert sum(row['n'] for row in summary)==215
    assert sum(row['actual_window_hits'] for row in recovery)==9
    assert all(row['p95_reference']=='' for row in summary if row['n']<30)
    assert all(row['p95_reference']=='' for row in recovery)


def test_failed_attempt_is_counted_without_inventing_a_latency():
    report=cohort();report['rows'].pop(0)
    report['capabilities']['apply/10/apply']=dict(state='failed',failures=[dict(trial=0,error='timeout')])
    summary,_=summarize(report);row=summary[0]
    assert row['n']==29 and row['failed_samples']==1 and row['minimum']>0
    assert row['p95_reference']==''


def test_missed_window_is_not_a_successful_requested_state_injection():
    report=cohort();probe=next(row for row in report['rows'] if row['workload']=='crash-recovery')
    probe.update(kill_window_hit=False,state_at_death='target_applied',recovery_ms=99999)
    _,recovery=summarize(report);row=recovery[0]
    assert row['actual_window_hits']==2 and row['missed_windows']==1 and row['recovered_results']==3
    assert row['maximum']==12


@pytest.mark.parametrize('mutation',['missing','duplicate','incorrect','nan','crash_missing','false_window'])
def test_incomplete_or_inconsistent_evidence_cannot_be_published(mutation):
    report=cohort()
    if mutation=='missing':report['rows'].pop(0)
    elif mutation=='duplicate':report['rows'][1]=copy.deepcopy(report['rows'][0])
    elif mutation=='incorrect':report['rows'][0]['correctness']='failed'
    elif mutation=='nan':report['rows'][0]['wall_ms']=float('nan')
    elif mutation=='crash_missing':report['rows'].pop()
    else:report['rows'][-1]['kill_window_hit']=False
    with pytest.raises(ValueError):summarize(report)


def retained_report(tmp_path):
    root=tmp_path/'cohort';root.mkdir();(root/'bin').mkdir();(root/'harness').mkdir()
    binary=root/'bin/pvisor';binary.write_bytes(b'frozen binary')
    manifest=root/'source-manifest.json';manifest.write_text('{"entries":[]}\n')
    runner=root/'harness/runner.py';runner.write_text('# frozen runner\n')
    receipt=dict(pvisor_sha256=digest(binary),source_manifest_sha256=digest(manifest))
    (root/'build-receipt.json').write_text(json.dumps(receipt))
    report=cohort();report.update(binary_build=receipt,binary_sha256=receipt['pvisor_sha256'],
        harness_sha256={'runner.py':digest(runner)})
    path=root/'report.json';path.write_text(json.dumps(report));return path


def test_publication_exports_derived_cohort_data_and_limits_comparisons(tmp_path):
    path=retained_report(tmp_path);output=tmp_path/'published'
    summary,recovery,comparisons=publish(path,output)
    assert len(summary)==15 and len(recovery)==3 and len(comparisons)==3
    assert comparisons[0]['ci95_low_ms']!='' and comparisons[1]['ci95_low_ms']==''
    assert all(row['cohort']=='cohort' and row['report_sha256']==digest(path) for row in summary+recovery)
    with (output/'apply.csv').open() as stream:assert len(list(csv.DictReader(stream)))==15
    assert sorted(p.name for p in output.iterdir())==['apply-comparisons.csv','apply-provenance.csv','apply-recovery.csv','apply.csv']


def test_mismatched_report_identity_writes_no_public_artifacts(tmp_path):
    path=retained_report(tmp_path);report=json.loads(path.read_text());report['binary_sha256']='wrong'
    path.write_text(json.dumps(report));output=tmp_path/'published'
    with pytest.raises(ValueError):publish(path,output)
    assert not output.exists()
