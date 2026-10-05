import pytest
from publication import distribution, publish
import json


def test_small_cohort_has_no_tail_estimate():
    result=distribution([3,1,2])
    assert result['p50']==2 and result['p95_reference']==''
    assert 'p99' not in result


def test_separated_clusters_report_counts_instead_of_one_median():
    result=distribution([10]*20+[30]*10)
    assert result['p50']==''
    assert (result['low_n'],result['low_p50'],result['high_n'],result['high_p50'])==(20,10,10,30)


def test_invalid_or_incomplete_evidence_is_rejected(tmp_path):
    report={'rows':[],'arguments':{'modes':'ready','backends':'native','samples':'30'},
            'capabilities':{'ready/native':{'state':'available'}},'benchmark_ids':{'ready':'B-STARTUP'}}
    raw=tmp_path/'report.json';raw.write_text(json.dumps(report))
    with pytest.raises(ValueError,match='incomplete'):publish(raw,tmp_path/'public')
    with pytest.raises(ValueError):distribution([float('nan')])
