import pytest
from publish_reference_campaign import paired_comparison


def values(value):
    return [dict(trial=i,value=value) for i in range(30)]


def test_matching_rounds_support_a_direction_with_confidence():
    result=paired_comparison(values(10),values(20),iterations=100)
    assert result['difference_ms']==-10
    assert result['ci95_high_ms'] < 0 and result['conclusion']=='candidate faster'


def test_duplicate_or_missing_pairs_are_rejected():
    with pytest.raises(ValueError,match='matching trial IDs'):
        paired_comparison(values(10),values(20)[:-1])
    duplicate=values(10);duplicate[-1]['trial']=0
    with pytest.raises(ValueError,match='unique'):
        paired_comparison(duplicate,values(20))


def test_separated_distribution_does_not_receive_one_median_ranking():
    separated=[dict(trial=i,value=10 if i<20 else 40) for i in range(30)]
    result=paired_comparison(separated,values(20),iterations=100)
    assert result['difference_ms']=='' and 'separated' in result['conclusion']
