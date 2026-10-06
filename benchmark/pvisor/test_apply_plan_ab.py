import copy
import sys
import pytest

from apply_plan_ab import APPLY, main, validate_sources


def fixture():
    cli=dict(rustc=1,features='["default", "gateway"]',declared_features='["default", "gateway"]',
        profile=2,rustflags=[],config=3,compile_kind=0)
    units=dict(cli=cli,libraries=[cli|dict(name='tokio',features='["default", "time"]')])
    left=dict(rustc='same compiler',cargo='same cargo',command=['cargo','build','--release','--locked','--offline',
        '-p','pvisor','--bin','pvisor','--features','gateway','--target-dir','/old/target'],
        compiled_units=units,pvisor_sha256='old')
    right=copy.deepcopy(left)|dict(pvisor_sha256='new')
    before=[dict(path=APPLY,sha256='old'),dict(path='crates/pvisor/src/lib.rs',sha256='unchanged'),dict(path='Cargo.lock',sha256='unchanged')]
    after=copy.deepcopy(before);after[0]['sha256']='new'
    return left,right,before,after


def test_apply_comparison_accepts_only_targeted_compiled_source_delta():
    assert validate_sources(*fixture())==[APPLY]


@pytest.mark.parametrize('mutation',['compiler','dependency','unrelated-source','inventory','identical','no-apply','debug'])
def test_unmatched_apply_binaries_cannot_establish_optimization_gain(mutation):
    left,right,before,after=fixture()
    if mutation=='compiler':right['rustc']='different compiler'
    elif mutation=='dependency':after[2]['sha256']='changed dependency'
    elif mutation=='unrelated-source':after[1]['sha256']='changed unrelated source'
    elif mutation=='inventory':after.pop()
    elif mutation=='identical':right['pvisor_sha256']='old'
    elif mutation=='no-apply':after[0]['sha256']='old'
    else:right['command'].remove('--release')
    with pytest.raises(ValueError):validate_sources(left,right,before,after)


def test_output_directory_and_equivalent_feature_namespace_do_not_change_build():
    left,right,before,after=fixture()
    right['command'][-1]='/new/target'
    right['command'][right['command'].index('gateway')]='pvisor/gateway'
    assert validate_sources(left,right,before,after)


@pytest.mark.parametrize('extra',[
    ['--example','unrelated_example'],['-p','unrelated-package'],['--all-targets'],['--tests'],['--lib']])
def test_joint_targets_cannot_establish_apply_gain(extra):
    left,right,before,after=fixture();right['command'].extend(extra)
    with pytest.raises(ValueError):validate_sources(left,right,before,after)


@pytest.mark.parametrize('mutation',['missing','library-features','profile','rustflags','command','duplicate-source'])
def test_actual_build_configuration_must_match(mutation):
    left,right,before,after=fixture()
    if mutation=='missing':right.pop('compiled_units')
    elif mutation=='library-features':right['compiled_units']['libraries'][0]['features']='["test-util"]'
    elif mutation=='profile':right['compiled_units']['cli']['profile']=99
    elif mutation=='rustflags':right['compiled_units']['cli']['rustflags']=['-Ctarget-cpu=native']
    elif mutation=='command':right['command'].extend(['--target','other-target'])
    else:after.append(copy.deepcopy(after[0]))
    with pytest.raises(ValueError):validate_sources(left,right,before,after)


@pytest.mark.parametrize('options',[
    ['--trace-syscalls','tracer'],['--tracer-receipt','receipt'],
    ['--trace-syscalls','tracer','--tracer-receipt','receipt'],
    ['--profile','--trace-syscalls','tracer'],['--profile','--tracer-receipt','receipt']])
def test_tracing_requires_diagnostic_mode_and_complete_provenance(options,monkeypatch,capsys):
    argv=['apply_plan_ab.py','--baseline','old','--candidate','new','--baseline-receipt','old.json',
        '--candidate-receipt','new.json','--firmware','firmware','--output','new-output',*options]
    monkeypatch.setattr(sys,'argv',argv)
    with pytest.raises(SystemExit) as error:main()
    assert error.value.code==2
    assert 'syscall tracing requires --profile and both tracer binary/receipt' in capsys.readouterr().err
