//! Cancellation must own escaped groups and survive frontend disconnection.
#![cfg(target_os = "linux")]
use std::process::Command;
const CANCEL: &str = r#"
import glob, json, os, select, signal, socket, subprocess, sys, time
binary, root, kind = sys.argv[1:]
marker = os.path.join(root, 'escaped.pid')
workload = '''import os, signal, time, sys
signal.signal(signal.SIGTERM, signal.SIG_IGN)
signal.signal(signal.SIGINT, signal.SIG_IGN)
pid = os.fork()
if pid == 0:
    os.setsid()
    if os.fork() != 0: os._exit(0)
    with open(sys.argv[1] + '.tmp', 'w') as f: f.write(str(os.getpid()))
    os.rename(sys.argv[1] + '.tmp', sys.argv[1])
    while True: time.sleep(1)
while not os.path.exists(sys.argv[1]): time.sleep(.01)
print('READY', flush=True)
while True: time.sleep(1)
'''
child = subprocess.Popen([binary, 'run', '--no-agent-defaults', '--overlaynet', 'off', '--', sys.executable, '-c', workload, marker],
    cwd=root, env=dict(os.environ, PVISOR_RUN_HOME=os.path.join(root, 'runs'), PVISOR_STARTUP_TIMING='0'), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
try:
    ready, _, _ = select.select([child.stdout], [], [], 20)
    assert ready, 'workload readiness timed out'
    assert child.stdout.readline() == b'READY\n'
    escaped = int(open(marker).read())
    started = time.monotonic()
    if kind in ('kill', 'pidfd-fallback'):
        record = json.load(open(glob.glob(os.path.join(root, 'runs', 'run-*', 'run.json'))[0]))
        endpoint = '/tmp/pvisor-host-%s/cancel-%s.sock' % (os.geteuid(), record['pid'])
        assert os.path.exists(endpoint), 'CLI Run must have a cooperative endpoint'
        if kind == 'kill':
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
                peer.settimeout(3); peer.connect(endpoint)
                peer.sendall((json.dumps({'version': 1, 'request_id': 'x' * 256,
                    'target': {'job_id': record['run_id'], 'attempt_id': 'stale-attempt', 'generation': None},
                    'command': {}}) + '\n').encode())
                response = json.loads(peer.makefile('rb').readline(4097))
                assert response['request_id'] == 'x' * 256, response
                assert response['result']['Err']['code'] == 'conflict', response
            assert child.poll() is None, 'stale cancellation must not terminate the Job'
        if kind == 'pidfd-fallback': os.unlink(endpoint)  # only this test-owned worker's socket
        killed = subprocess.run([binary, 'kill', 'last', '--json'], cwd=root,
            env=dict(os.environ, PVISOR_RUN_HOME=os.path.join(root, 'runs'), PVISOR_STARTUP_TIMING='0'),
            capture_output=True, timeout=6)
        assert killed.returncode == 0, killed.stderr
        assert json.loads(killed.stdout)['cooperative'] == (kind == 'kill'), killed.stdout
    else:
        child.send_signal(signal.SIGINT if kind == 'cancel' else signal.SIGKILL)
    out, err = child.communicate(timeout=10)
    expected = 130 if kind == 'cancel' else -9 if kind == 'disconnect' else 143
    assert child.returncode == expected, (child.returncode, err)
    assert time.monotonic() - started < 9, err
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        try:
            stat = open('/proc/%s/stat' % escaped).read().rsplit(')', 1)[1].split()
            if stat[0] == 'Z': break  # init may reap non-child orphans asynchronously
        except (FileNotFoundError, ProcessLookupError): break
        time.sleep(.02)
    else: raise AssertionError('escaped session survived request cleanup: %s' % escaped)
finally:
    if child.poll() is None: child.kill(); child.wait()
"#;
#[test]
fn ignored_signals_and_double_fork_sessions_are_bounded_on_cancel_and_disconnect() {
    for kind in ["cancel", "disconnect", "kill", "pidfd-fallback"] {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new("python3")
            .args(["-c", CANCEL, env!("CARGO_BIN_EXE_pvisor")])
            .arg(root.path())
            .arg(kind)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{kind}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
