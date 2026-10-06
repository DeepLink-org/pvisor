import copy
import pytest

from apply_plan_ab import APPLY, EXAMPLE, validate_sources


def fixture():
    left=dict(rustc='same compiler',cargo='same cargo',command=['cargo','--release','--locked','--offline'],pvisor_sha256='old')
    right=copy.deepcopy(left)|dict(pvisor_sha256='new')
    before=[dict(path=APPLY,sha256='old'),dict(path=EXAMPLE,sha256='old'),dict(path='Cargo.lock',sha256='unchanged')]
    after=copy.deepcopy(before);after[0]['sha256']='new';after[1]['sha256']='new'
    return left,right,before,after


def test_apply_comparison_accepts_only_targeted_compiled_source_delta():
    assert validate_sources(*fixture())==sorted([APPLY,EXAMPLE])


@pytest.mark.parametrize('mutation',['compiler','dependency','inventory','identical','no-apply','debug'])
def test_unmatched_apply_binaries_cannot_establish_optimization_gain(mutation):
    left,right,before,after=fixture()
    if mutation=='compiler':right['rustc']='different compiler'
    elif mutation=='dependency':after[2]['sha256']='changed dependency'
    elif mutation=='inventory':after.pop()
    elif mutation=='identical':right['pvisor_sha256']='old'
    elif mutation=='no-apply':after[0]['sha256']='old'
    else:right['command'].remove('--release')
    with pytest.raises(ValueError):validate_sources(left,right,before,after)
