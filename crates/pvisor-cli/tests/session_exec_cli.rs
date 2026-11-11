//! Real same-Execution sandbox and PTY contracts. No host PID/namespace joining.
#![cfg(target_os = "linux")]
use std::process::Command;

const CASE: &str = r#"
import os, pathlib, subprocess, sys, time, json, pty, select, signal, termios, fcntl, struct
binary, root, mode = sys.argv[1:]
root=pathlib.Path(root); project=root/'project'; project.mkdir()
home=root/'home'; home.mkdir(); stage=root/'stage'
env={'PATH':os.environ['PATH'],'HOME':str(home),'TERM':'xterm-256color','PVISOR_RUN_HOME':str(root/'runs')}
def cli(*args, **kw):
    return subprocess.run([binary,*map(str,args)],cwd=project,env=env,capture_output=True,text=True,timeout=10,**kw)
def state():
    r=cli('status',stage,'--lifecycle','--json')
    return json.loads(r.stdout) if r.returncode==0 else {}
script="for ns in mnt net pid user; do readlink /proc/self/ns/$ns > $ns.txt; done; printf main > main.txt; exec sleep 60"
run=subprocess.Popen([binary,'spawn','--safe','--no-agent-defaults','--executor','host',
    '--overlaynet-deny-all','--stdio','capture','--stage',str(stage),'--','sh','-c',script],
    cwd=project,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
try:
    deadline=time.monotonic()+10
    while state().get('state')!='running':
        assert run.poll() is None, run.communicate()
        assert time.monotonic()<deadline, 'Session never became running'
        time.sleep(.02)
    if mode=='ordinary':
        r=cli('exec','--session',stage,'sh','-c',
            'i=0; while ! test "$(cat main.txt 2>/dev/null)" = main; do i=$((i+1)); test "$i" -lt 500 || exit 30; sleep .01; done; '
            'for ns in mnt net pid user; do test "$(readlink /proc/self/ns/$ns)" = "$(cat $ns.txt)" || exit 31; done; '
            'cat main.txt; printf child > child.txt; exit 7')
        assert r.returncode==7 and r.stdout=='main', (r.returncode,r.stdout,r.stderr)
        assert not list(project.iterdir()), list(project.iterdir())
        assert state()['state']=='running', state()
        r=cli('exec','--session','-i',stage,'sh','-c','read value; printf "GOT:%s" "$value"',input='hello\n')
        assert r.returncode==0 and r.stdout=='GOT:hello', (r.stdout,r.stderr)
        r=cli('exec','--session',stage,'sh','-c','sleep 6; printf still-connected')
        assert r.returncode==0 and r.stdout=='still-connected', (r.stdout,r.stderr)
        outside=root/'outside'; outside.mkdir(); (outside/'secret').write_text('unchanged')
        r=cli('exec','--session',stage,'sh','-c',
            'test ! -e "$1"; ! (printf bad > "$1")','sh',str(outside/'secret'))
        assert r.returncode==0 and (outside/'secret').read_text()=='unchanged', (r.stdout,r.stderr)
        r=cli('exec','--session',stage,'python3','-c',
            'import os; links=[]\nfor fd in os.listdir("/proc/self/fd"):\n try: links.append(os.readlink("/proc/self/fd/"+fd))\n except FileNotFoundError: pass\nassert not any(x.startswith("socket:") for x in links), links')
        assert r.returncode==0, (r.stdout,r.stderr)
        daemon='import os,time\nif os.fork()==0:\n os.setsid()\n if os.fork()==0:\n  print(os.getpid(),flush=True);time.sleep(60)\n os._exit(0)\ntime.sleep(60)'
        execution=subprocess.Popen([binary,'exec','--session',str(stage),'python3','-c',daemon],
            cwd=project,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            ready,_,_=select.select([execution.stdout],[],[],5)
            assert ready, 'exec daemon never started'
            child=int(execution.stdout.readline().strip())
            execution.terminate(); execution.wait(timeout=5)
            deadline=time.monotonic()+5
            while True:
                r=cli('exec','--session',stage,'sh','-c','kill -0 "$1" 2>/dev/null','sh',str(child))
                if r.returncode!=0: break
                assert time.monotonic()<deadline, 'disconnected exec leaked an owned daemon'
                time.sleep(.05)
            assert state()['state']=='running'
        finally:
            if execution.poll() is None: execution.kill(); execution.wait()
            execution.stdout.close(); execution.stderr.close()
        # Omitted file/ledger counts are unknown, not fabricated zero.
        light=state()
        assert light['detail']=='lifecycle' and 'filesystem' not in light, light
        assert 'workspace_checkpoint_count' not in light
        assert light['exec_supported'] is True
        # A growing upper tree must not turn lifecycle polling into inventory
        # output or manufacture counts. These are disposable fixture files.
        r=cli('exec','--session',stage,'python3','-c',
            'import pathlib; p=pathlib.Path("many"); p.mkdir(); '
            '[(p/str(i)).write_text("x") for i in range(2048)]')
        assert r.returncode==0, (r.stdout,r.stderr)
        for _ in range(3):
            r=cli('status',stage,'--lifecycle','--json')
            observed=json.loads(r.stdout)
            assert observed['state']=='running' and observed['exec_supported']
            assert 'filesystem' not in observed and len(r.stdout)<16384, observed
        assert not list(project.iterdir())
    elif mode=='overlap':
        peer_project=root/'peer-project'; peer_project.mkdir(); peer_stage=root/'peer-stage'
        peer=subprocess.Popen([binary,'spawn','--safe','--no-agent-defaults','--executor','host',
            '--overlaynet-deny-all','--stdio','capture','--stage',str(peer_stage),'--','sleep','60'],
            cwd=peer_project,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            deadline=time.monotonic()+10
            while True:
                r=cli('status',peer_stage,'--lifecycle','--json')
                if r.returncode==0 and json.loads(r.stdout)['state']=='running': break
                assert peer.poll() is None, peer.communicate()
                assert time.monotonic()<deadline, (r.returncode,r.stdout,r.stderr)
                time.sleep(.02)
            assert cli('exec','--session',peer_stage,'true').returncode==0
            assert cli('kill',stage,'--json').returncode==0
            run.communicate(timeout=60)
            dropped=cli('drop',stage)
            assert dropped.returncode==0, (dropped.stdout,dropped.stderr)
            peer_status=json.loads(cli('status',peer_stage,'--lifecycle','--json').stdout)
            assert peer_status['state']=='running', peer_status
        finally:
            cli('kill',peer_stage,'--json')
            peer.communicate(timeout=60)
            dropped=cli('drop',peer_stage)
            assert dropped.returncode==0, (dropped.stdout,dropped.stderr)
    elif mode=='pty':
        pid,master=pty.fork()
        if pid==0:
            os.chdir(project); before=termios.tcgetattr(0)
            args=[binary,'exec','--session','-it',str(stage),'sh','-c',
                'test -t 0 && test -t 1 || exit 32; printf "TTY_READY\\n"; read value; stty size; printf "GOT:%s\\n" "$value"; exit 6']
            child=subprocess.Popen(args,env=env)
            try:
                code=child.wait(timeout=15)
                restored=termios.tcgetattr(0)==before
                print('RESTORED:%s CODE:%s'%(int(restored),code),flush=True)
                os._exit(0 if restored and code==6 else 33)
            except BaseException:
                child.kill(); child.wait(); os._exit(34)
        output=bytearray(); resized=None; sent=False; deadline=time.monotonic()+20
        try:
            while time.monotonic()<deadline:
                ready,_,_=select.select([master],[],[],.05)
                if ready:
                    try: chunk=os.read(master,4096)
                    except OSError: break
                    if not chunk: break
                    output.extend(chunk); assert len(output)<65536
                if b'TTY_READY' in output and resized is None:
                    fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',42,100,0,0))
                    resized=time.monotonic()
                if resized and not sent and time.monotonic()-resized>.15:
                    os.write(master,b'hello tty\n'); sent=True
                if b'RESTORED:' in output: break
            _,status=os.waitpid(pid,0)
            assert status==0 and b'GOT:hello tty' in output and b'42 100' in output, bytes(output)
            assert b'RESTORED:1 CODE:6' in output, bytes(output)
        except BaseException:
            try: os.kill(pid,signal.SIGKILL)
            except ProcessLookupError: pass
            raise
        finally: os.close(master)
        assert state()['state']=='running'
finally:
    # Owner allows 2s cancellation plus 45s receipt finalization; wait for it.
    killed=cli('kill',stage,'--json')
    out,err=run.communicate(timeout=60)
assert cli('exec','--session',stage,'true').returncode!=0, 'stopped Session was implicitly restarted'
review=cli('review',stage,'--json')
assert review.returncode==0, review.stderr
if mode=='ordinary':
    reviewed=json.loads(review.stdout)
    assert 'filesystem' in reviewed, {'review':reviewed, 'kill':(killed.returncode,killed.stdout,killed.stderr), 'owner':(run.returncode,out,err)}
    assert any(x['path']=='child.txt' for x in reviewed['filesystem']['changes']), reviewed
if mode!='overlap':
    dropped=cli('drop',stage)
    assert dropped.returncode==0, (dropped.stdout,dropped.stderr,killed.stdout,killed.stderr,out,err,state())
"#;

fn run(mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .args(["-c", CASE, env!("CARGO_BIN_EXE_pvisor")])
        .arg(root.path())
        .arg(mode)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn same_session_exec_shares_policy_and_cleans_only_its_own_processes() {
    run("ordinary");
}

#[test]
fn session_exec_pty_handles_input_resize_exit_and_terminal_restore() {
    run("pty");
}

#[test]
fn another_live_sandbox_cannot_pin_this_sessions_fuse_teardown() {
    run("overlap");
}
