//! Real controlling-terminal regressions, not isatty-only socket/pipe tests.
#![cfg(target_os = "linux")]
use std::process::Command;

const PTY: &str = r#"
import os, pty, select, signal, subprocess, sys, time
binary, root, mode = sys.argv[1:]
pid, master = pty.fork()
if pid == 0:
    os.chdir(root)
    original = os.tcgetpgrp(0)
    env = dict(os.environ, PVISOR_RUN_HOME=os.path.join(root, 'runs'), PVISOR_STARTUP_TIMING='0')
    script = "printf 'INPUT_READY\\n'; read value; printf 'GOT:%s\\n' \"$value\"; /bin/sleep 60"
    if mode == 'inspect':
        args = [binary, 'inspect', os.path.join(root, 'stage'), '--', '/bin/sh', '-c', script]
    else:
        args = [binary, 'run', '--no-agent-defaults', '--overlaynet', 'off', '--', '/bin/sh', '-c', script]
    child = subprocess.Popen(args, env=env)
    try:
        code = child.wait(timeout=30)
        restored = os.tcgetpgrp(0) == original
        os.write(1, ('RESTORED:%s EXIT:%s\n' % (int(restored), code)).encode())
        os._exit(0 if restored and code == 130 else 31)
    except BaseException as e:
        os.write(2, ('PTY_CHILD_ERROR:%s\n' % e).encode())
        child.kill(); child.wait(); os._exit(32)
output = bytearray()
sent_input = sent_cancel = False
deadline = time.monotonic() + 35
try:
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], .1)
        if ready:
            try: chunk = os.read(master, 4096)
            except OSError: break
            if not chunk: break
            output.extend(chunk)
            if len(output) > 65536: raise AssertionError('unbounded PTY test output')
            if b'INPUT_READY' in output and not sent_input:
                os.write(master, b'hello tty\n'); sent_input = True
            if b'GOT:hello tty' in output and not sent_cancel:
                os.write(master, b'\x03'); sent_cancel = True
        if b'RESTORED:' in output: break
    else: raise AssertionError('PTY request timed out')
    _, status = os.waitpid(pid, 0)
    assert sent_input and sent_cancel, bytes(output)
    assert b'GOT:hello tty' in output, bytes(output)
    assert b'RESTORED:1 EXIT:130' in output, bytes(output)
    assert status == 0, (status, bytes(output))
except BaseException:
    try: os.kill(pid, signal.SIGKILL)
    except ProcessLookupError: pass
    raise
finally:
    os.close(master)
print(output.decode(errors='replace'))
"#;

const BACKGROUND_PTY: &str = r#"
import os, pty, select, signal, subprocess, sys, time
binary, root = sys.argv[1:]
pid, master = pty.fork()
if pid == 0:
    os.chdir(root)
    original = os.tcgetpgrp(0)
    env = dict(os.environ, PVISOR_RUN_HOME=os.path.join(root, 'runs'), PVISOR_STARTUP_TIMING='0')
    run = [binary, 'run', '--no-agent-defaults', '--overlaynet', 'off', '--', '/bin/true']
    try:
        seed = subprocess.run(run, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, timeout=20)
        assert seed.returncode == 0, seed.stderr
        for options in [dict(process_group=0), dict(start_new_session=True)]:
            child = subprocess.Popen([binary, 'status', 'last', '--json'], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, **options)
            while child.poll() is None:
                assert os.tcgetpgrp(0) == original, 'status stole foreground terminal'
                time.sleep(.005)
            out, err = child.communicate(timeout=10)
            assert child.returncode == 0, (out, err)
            assert os.tcgetpgrp(0) == original
        background = subprocess.run(run, process_group=0, env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        assert background.returncode != 0, background.stderr
        assert b'frontend foreground process group' in background.stderr, background.stderr
        assert os.tcgetpgrp(0) == original
        os.write(1, b'BACKGROUND_OK\n'); os._exit(0)
    except BaseException as e:
        os.write(2, ('BACKGROUND_ERROR:%r\n' % (e,)).encode()); os._exit(33)
output = bytearray()
deadline = time.monotonic() + 40
try:
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], .1)
        if ready:
            try: chunk = os.read(master, 4096)
            except OSError: break
            if not chunk: break
            output.extend(chunk)
            assert len(output) <= 65536
        if b'BACKGROUND_OK' in output or b'BACKGROUND_ERROR' in output: break
    else: raise AssertionError('background PTY test timed out')
    _, status = os.waitpid(pid, 0)
    assert status == 0 and b'BACKGROUND_OK' in output, (status, bytes(output))
except BaseException:
    try: os.kill(pid, signal.SIGKILL)
    except ProcessLookupError: pass
    raise
finally:
    os.close(master)
"#;

#[test]
fn background_status_and_noncontrolling_pty_do_not_handoff() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .args(["-c", BACKGROUND_PTY, env!("CARGO_BIN_EXE_pvisor")])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_pty(root: &std::path::Path, mode: &str) {
    let output = Command::new("python3")
        .args(["-c", PTY, env!("CARGO_BIN_EXE_pvisor")])
        .arg(root)
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
fn host_run_reads_controlling_terminal_ctrl_c_and_restores_foreground() {
    let root = tempfile::tempdir().unwrap();
    run_pty(root.path(), "run");
}
#[test]
fn inspect_reads_controlling_terminal_ctrl_c_and_restores_foreground() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let target = root.path().join("target");
    std::fs::create_dir_all(stage.join("upper")).unwrap();
    std::fs::create_dir(&target).unwrap();
    let record: pvisor::RunRecord = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run_id":"job-pty-inspect","session_id":"session-pty","agent":"sh",
        "pid":0,"command":["/bin/sh"],"state":"completed","started_at_unix_ms":1,
        "finished_at_unix_ms":2,"storage":stage,"network":{},"gateway_listen":null,
        "overlay":{"id":"job-pty-inspect","generation":7,"target":target,
            "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
            "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
    }))
    .unwrap();
    record.write().unwrap();
    run_pty(root.path(), "inspect");
}
