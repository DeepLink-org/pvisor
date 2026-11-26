//! A cancelled cold `run` must release its registry connection and image lock.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::process::Command;

const CANCELLATION: &str = r#"
import ctypes, os, signal, socket, subprocess, sys, time
from pathlib import Path

binary, root, sig, mode = sys.argv[1:]
root = Path(root)
sig = getattr(signal, sig)
PR_SET_CHILD_SUBREAPER = 36
assert ctypes.CDLL(None).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0

def request(conn):
    conn.settimeout(3)
    with conn.makefile("rb") as data:
        assert data.readline(512) == b"CONNECT registry.invalid:443 HTTP/1.1\r\n"
        for _ in range(20):
            if data.readline(512) == b"\r\n":
                return
    raise AssertionError("incomplete proxy request")

def reap_workers():
    children = Path(f"/proc/self/task/{os.getpid()}/children")
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        for pid in children.read_text().split():
            # Only this loop reaps adopted children, so these PIDs cannot be reused.
            try:
                os.kill(int(pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        time.sleep(.01)
    raise AssertionError("test descendants did not exit")

with socket.socket() as proxy:
    proxy.bind(("127.0.0.1", 0))
    proxy.listen(2)
    proxy.settimeout(4)
    env = {
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(root / "config"),
        "PVISOR_DOCKER_HOME": str(root / "docker"),
        "PVISOR_IMAGE_STORE": str(root / "images"),
        "HTTPS_PROXY": f"http://127.0.0.1:{proxy.getsockname()[1]}",
    }
    if mode == "ephemeral":
        env["PVISOR_OCI_EPHEMERAL"] = "1"
    command = [binary, "run", "--rm", "--name", "retry", "--network", "none",
        "--cpus", "1", "--memory", "128m", "registry.invalid/example:latest", "true"]
    def run():
        return subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)

    cancelled = run()
    retry = None
    try:
        connection, _ = proxy.accept()
        with connection:
            request(connection)
            cancelled.send_signal(sig)
            output, _ = cancelled.communicate(timeout=3)
            assert (cancelled.returncode, output) == (-sig, b"")
            retry = run()
            assert connection.recv(1) == b"", "cancelled run still owns its proxy connection"
            second, _ = proxy.accept()
            with second:
                request(second)
                second.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            output, error = retry.communicate(timeout=3)
            assert (retry.returncode, output) == (125, b""), (retry.returncode, output, error)
            assert error.endswith(b": tunnel error: unsuccessful\n"), error
    finally:
        for frontend in [cancelled, retry]:
            if frontend is not None:
                frontend.kill()
                frontend.wait(timeout=3)
        reap_workers()
"#;

fn assert_cold_run_retry(signal: &str, mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let result = Command::new("python3")
        .args(["-c", CANCELLATION, env!("CARGO_BIN_EXE_pvisor")])
        .arg(root.path())
        .arg(signal)
        .arg(mode)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn cold_run_frontend_sigterm_releases_the_image_for_retry() {
    assert_cold_run_retry("SIGTERM", "default");
}

#[test]
fn cold_run_frontend_sigkill_releases_the_image_for_retry() {
    assert_cold_run_retry("SIGKILL", "default");
}

#[test]
fn ephemeral_cold_run_frontend_sigterm_releases_the_image_for_retry() {
    assert_cold_run_retry("SIGTERM", "ephemeral");
}

#[test]
fn ephemeral_cold_run_frontend_sigkill_releases_the_image_for_retry() {
    assert_cold_run_retry("SIGKILL", "ephemeral");
}
