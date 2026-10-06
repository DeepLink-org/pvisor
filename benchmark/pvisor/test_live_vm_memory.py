import copy
import json
import subprocess
import threading
from pathlib import Path
import pytest
import live_vm_memory as runner
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


@pytest.fixture
def quiet_host(monkeypatch):
    monkeypatch.setattr(runner,'host_jobs',lambda unit=None:dict(time_ns=0,jobs=[],inspection_errors=[]))
    monkeypatch.setattr(runner,'wait_for_quiet',lambda logs:True)


def service_config(tmp_path):
    config=dict(root=str(tmp_path),budget_bytes=2048*1024**2,cpu_affinity='0,1')
    (tmp_path/'config.json').write_text(json.dumps(config))
    return config


def test_timeout_retains_launch_output_and_proves_teardown(monkeypatch,tmp_path,quiet_host):
    calls=[]
    def run(cmd,**kwargs):
        calls.append((cmd,kwargs))
        if cmd[0]=='systemd-run':
            assert json.loads((tmp_path/'service-launch.json').read_text())['started_at']
            raise subprocess.TimeoutExpired(cmd,240,output=b'partial stdout',stderr=b'partial stderr')
        if cmd[2]=='stop':return subprocess.CompletedProcess(cmd,0,b'stopped',b'')
        return subprocess.CompletedProcess(cmd,0,b'ActiveState=inactive\nLoadState=loaded\n',b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    record=runner.run_service('owned-unit',service_config(tmp_path),tmp_path/'harness.py')
    assert record['deadline'] and record['correctness']=='failed' and record['unit_quiescent']
    assert (tmp_path/'service.stdout').read_bytes()==b'partial stdout'
    assert (tmp_path/'service.stderr').read_bytes()==b'partial stderr'
    assert json.loads((tmp_path/'service-result.json').read_text())==record
    assert json.loads((tmp_path/'service-quiescence.json').read_text())['unit_quiescent']
    assert [call[1]['timeout'] for call in calls]==[240,15,10]
    assert calls[1][0]==['systemctl','--user','stop','owned-unit']
    assert calls[2][0][3]=='owned-unit'
    cmd=calls[0][0]
    assert '--property=MemoryMax=2147483648' in cmd
    assert '--property=MemorySwapMax=0' in cmd
    assert '--property=CPUQuota=200%' in cmd
    assert '--property=CPUAffinity=0 1' in cmd
    assert '--property=KillMode=control-group' in cmd
    runtime=int(next(arg.split('=',2)[2] for arg in cmd if arg.startswith('--property=RuntimeMaxSec=')))
    assert 180<runtime<=240
    assert '--property=TimeoutStopSec=10' in cmd


@pytest.mark.parametrize('stdout,returncode,proven',[
    (b'ActiveState=inactive\nLoadState=loaded\n',0,True),
    (b'ActiveState=failed\nLoadState=loaded\n',0,True),
    (b'LoadState=not-found\n',0,True),
    (b'ActiveState=active\nLoadState=loaded\n',0,False),
    (b'ActiveState=inactive\n',1,False),
    (b'',0,False),
    (b'OtherActiveState=inactive\n',0,False),
])
def test_settle_requires_explicit_final_state(monkeypatch,tmp_path,stdout,returncode,proven):
    def run(cmd,**kwargs):
        if cmd[2]=='stop':return subprocess.CompletedProcess(cmd,0,b'',b'')
        return subprocess.CompletedProcess(cmd,returncode,stdout,b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    assert runner.settle_unit('owned-unit',tmp_path) is proven
    assert json.loads((tmp_path/'service-quiescence.json').read_text())['unit_quiescent'] is proven


@pytest.mark.parametrize('operation',['stop','show'])
def test_cleanup_timeout_is_unproven(monkeypatch,tmp_path,operation):
    def run(cmd,**kwargs):
        if cmd[2]==operation:raise subprocess.TimeoutExpired(cmd,kwargs['timeout'])
        return subprocess.CompletedProcess(cmd,0,b'',b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    assert not runner.settle_unit('owned-unit',tmp_path)
    proof=json.loads((tmp_path/'service-quiescence.json').read_text())
    assert not proof['unit_quiescent'] and proof['error']


@pytest.mark.parametrize('scenario',['timeout-cleanup','passed-cleanup','interference','admission'])
def test_campaign_never_launches_next_vm_after_failed_gate(monkeypatch,tmp_path,quiet_host,scenario):
    worker_passed=scenario!='timeout-cleanup'
    example=tmp_path/'example';example.write_bytes(b'example')
    receipt=tmp_path/'receipt.json'
    receipt.write_text(json.dumps(dict(example_sha256='hash',source_manifest_sha256='hash')))
    (tmp_path/'source-manifest.json').write_text('{}')
    rootfs=tmp_path/'rootfs';rootfs.mkdir()
    firmware=tmp_path/'firmware';firmware.mkdir()
    output=tmp_path/'output'
    argv=['live_vm_memory.py','--example',str(example),'--build-receipt',str(receipt),
        '--rootfs',str(rootfs),'--firmware',str(firmware),'--output',str(output)]
    # A measured failure normally continues; unproven cleanup must override that.
    if scenario!='passed-cleanup':argv+=['--warmups','0']
    monkeypatch.setattr(runner.sys,'argv',argv)
    monkeypatch.setattr(runner,'digest',lambda path:'hash')
    monkeypatch.setattr(runner.shutil,'copytree',lambda source,destination,**kwargs:destination.mkdir())
    launches=[]
    if scenario=='admission':monkeypatch.setattr(runner,'wait_for_quiet',lambda logs:False)
    elif scenario=='interference':
        def jobs(unit=None):
            return dict(time_ns=0,jobs=[dict(pid=99,kind='other VM')] if launches else [],inspection_errors=[])
        monkeypatch.setattr(runner,'host_jobs',jobs)
    def run(cmd,**kwargs):
        if cmd[0]=='systemd-run':
            launches.append(cmd)
            if not worker_passed:raise subprocess.TimeoutExpired(cmd,240,output=b'timeout')
            config=json.loads(runner.Path(cmd[-1]).read_text())
            runner.Path(config['result']).write_text(json.dumps(dict(correctness='passed')))
            return subprocess.CompletedProcess(cmd,0,b'complete',b'')
        if cmd[2]=='stop':return subprocess.CompletedProcess(cmd,0,b'',b'')
        state='inactive' if scenario=='interference' else 'active'
        return subprocess.CompletedProcess(cmd,0,f'ActiveState={state}\nLoadState=loaded\n'.encode(),b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    with pytest.raises(SystemExit) as error:runner.main()
    assert error.value.code==1
    assert len(launches)==(0 if scenario=='admission' else 1)
    report=json.loads((output/'report.json').read_text())
    assert report['arguments']['samples']==30
    assert report['arguments']['warmups']==(3 if scenario=='passed-cleanup' else 0)
    assert report['arguments']['budget_mib']==2048 and report['arguments']['cpu_affinity']=='0,1'
    assert not report['rows'] and len(report['failures'])==1 and len(report['attempts'])==1
    attempt=report['attempts'][0]
    assert attempt['unit_quiescent'] is (scenario in ('interference','admission'))
    assert attempt['correctness']=='failed'
    if scenario!='admission':assert attempt['deadline'] is (not worker_passed)
    if scenario in ('interference','admission'):assert 'cohort invalid' in report['stopped']
    assert report['protocol']['host_guard']==runner.HOST_GUARD_PROTOCOL
    assert Path(attempt['logs']).name=='0-0'


def test_host_jobs_excludes_only_owned_service_group_and_logs_inaccessible_fds(monkeypatch,tmp_path):
    proc=tmp_path/'proc';proc.mkdir()
    def process(pid,group,command,target):
        root=proc/str(pid);root.mkdir();(root/'fd').mkdir()
        (root/'cgroup').write_text('0::'+group+'\n')
        (root/'cmdline').write_bytes(command+b'\0')
        (root/'fd/3').symlink_to(target)
    process(1,'/parent/owned.service','cargo'.encode(),'/dev/kvm')
    process(2,'/parent/owned.service/child',b'worker','anon_inode:kvm-vm')
    process(3,'/parent',b'foreign','/dev/kvm')
    process(4,'/parent/owned.service-other',b'foreign','anon_inode:[kvm-vcpu]')
    process(5,'/parent/other.service',b'/usr/bin/cargo','/no-kvm')
    process(6,'/parent/other.service',b'unknown','/dev/kvm')
    path_type=runner.Path
    monkeypatch.setattr(runner,'Path',lambda value:path_type(proc) if str(value)=='/proc' else path_type(value))
    readlink=runner.os.readlink
    def link(path):
        if str(path)==str(proc/'6/fd/3'):raise PermissionError('FD inaccessible')
        return readlink(path)
    monkeypatch.setattr(runner.os,'readlink',link)
    row=runner.host_jobs('owned')
    assert {(job['pid'],job['kind']) for job in row['jobs']}=={(3,'other VM'),(4,'other VM'),(5,'build/test')}
    assert row['inspection_errors'][0]['pid']==6
    # Admission has no owned service to exclude, even in a shared parent group.
    assert {job['pid'] for job in runner.host_jobs()['jobs']}=={1,2,3,4,5}
    uid=runner.os.getuid()
    monkeypatch.setattr(runner.os,'getuid',lambda:uid+1)
    assert not runner.host_jobs('owned')['jobs']


def test_quiet_admission_resets_window_and_has_180_second_bound(monkeypatch,tmp_path):
    clock=[0.]
    monkeypatch.setattr(runner.time,'monotonic',lambda:clock[0])
    monkeypatch.setattr(runner.time,'sleep',lambda seconds:clock.__setitem__(0,clock[0]+seconds))
    def jobs(unit=None):
        assert unit is None
        return dict(time_ns=0,jobs=[dict(pid=1)] if clock[0]==10 else [],inspection_errors=[])
    monkeypatch.setattr(runner,'host_jobs',jobs)
    assert runner.wait_for_quiet(tmp_path)
    assert clock[0]==41 # quiet restarted at second 11
    rows=[json.loads(line) for line in (tmp_path/'prelaunch-wait.jsonl').read_text().splitlines()]
    assert rows[10]['jobs'] and not rows[-1]['jobs']
    clock[0]=0
    monkeypatch.setattr(runner,'host_jobs',lambda unit=None:dict(time_ns=0,jobs=[dict(pid=1)],inspection_errors=[]))
    assert not runner.wait_for_quiet(tmp_path)
    assert clock[0]==180


@pytest.mark.parametrize('kind',['other VM','build/test'])
def test_external_guard_detects_transient_interference_during_service(monkeypatch,tmp_path,kind):
    entered=threading.Event();detected=threading.Event();checks=[]
    coordinator=threading.get_ident()
    def jobs(unit=None):
        assert unit=='owned-unit'
        checks.append(threading.get_ident())
        row=dict(time_ns=0,jobs=[],inspection_errors=[dict(pid=7,error='FD inaccessible')])
        if threading.get_ident()!=coordinator and entered.is_set():
            row['jobs']=[dict(pid=99,kind=kind)];detected.set()
        return row
    monkeypatch.setattr(runner,'host_jobs',jobs)
    def run(cmd,**kwargs):
        if cmd[0]=='systemd-run':
            entered.set()
            assert detected.wait(3),'external guard did not poll during service'
            (tmp_path/'result.json').write_text(json.dumps(dict(correctness='passed')))
            return subprocess.CompletedProcess(cmd,0,b'complete',b'')
        if cmd[2]=='stop':return subprocess.CompletedProcess(cmd,0,b'',b'')
        return subprocess.CompletedProcess(cmd,0,b'ActiveState=inactive\n',b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    record=runner.run_service('owned-unit',service_config(tmp_path),tmp_path/'harness.py')
    assert record['correctness']=='failed' and record['unit_quiescent']
    assert record['host_interference'][0]['jobs'][0]['kind']==kind
    assert checks[0]==coordinator and checks[-1]==coordinator and any(pid!=coordinator for pid in checks)
    rows=[json.loads(line) for line in (tmp_path/'host-guard.jsonl').read_text().splitlines()]
    assert any(row['jobs'] for row in rows) and rows[0]['inspection_errors']


def test_guard_failure_rejects_even_successful_service(monkeypatch,tmp_path):
    calls=[]
    def jobs(unit=None):
        if calls:raise OSError('proc inspection failed')
        return dict(time_ns=0,jobs=[],inspection_errors=[])
    monkeypatch.setattr(runner,'host_jobs',jobs)
    def run(cmd,**kwargs):
        calls.append(cmd)
        if cmd[0]=='systemd-run':(tmp_path/'result.json').write_text(json.dumps(dict(correctness='passed')))
        return subprocess.CompletedProcess(cmd,0,b'ActiveState=inactive\n',b'')
    monkeypatch.setattr(runner.subprocess,'run',run)
    record=runner.run_service('owned-unit',service_config(tmp_path),tmp_path/'harness.py')
    assert record['correctness']=='failed' and record['host_guard_errors']
