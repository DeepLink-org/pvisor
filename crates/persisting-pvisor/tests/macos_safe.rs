#![cfg(target_os = "macos")]

use persisting_control::IsolationKind;
use persisting_pvisor::RunBundle;
use std::fs;
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

fn macfuse_is_installed() -> bool {
    Path::new("/Library/Filesystems/macfuse.fs").is_dir()
}

fn only_run(root: &Path) -> PathBuf {
    let runs = fs::read_dir(root)
        .expect("read Run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("run-bundle.json").is_file())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one finalized Run in {root:?}");
    runs.into_iter().next().unwrap()
}

#[test]
fn safe_profile_stages_reviews_and_applies_on_macos() {
    if !macfuse_is_installed() {
        eprintln!(
            "skipping macOS safe-profile smoke test: install macFUSE to exercise staged writes"
        );
        return;
    }

    let temporary = tempfile::Builder::new()
        .prefix("pvmac")
        .tempdir_in("/tmp")
        .expect("create short macOS fixture path");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let outside = temporary.path().join("outside.txt");
    let outside_secret = temporary.path().join("outside-secret.txt");
    fs::create_dir(&workspace).unwrap();
    fs::write(&outside_secret, "read-compatible").unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["run", "--stdio", "capture", "--overlayfs-compose"])
        .arg(&workspace)
        .args([
            "--",
            "/bin/sh",
            "-c",
            r#"
                test "$PERSISTING_SANDBOX_FILESYSTEM" = seatbelt-write || exit 38
                test "$PERSISTING_SANDBOX_NETWORK" = ambient || exit 39
                test "$(cat "$2")" = read-compatible || exit 40
                if printf escaped > "$1" 2>/dev/null; then exit 41; fi
                ln -s "$1" outside-link
                if printf escaped > outside-link 2>/dev/null; then exit 42; fi
                if ln "$2" outside-hardlink 2>/dev/null; then
                    printf mutated > outside-hardlink 2>/dev/null || true
                    rm -f outside-hardlink
                fi
                test "$(cat "$2")" = read-compatible || exit 43
                printf scratch > "$TMPDIR/probe"
                printf staged > macos-staged.txt
                printf macos-ok
            "#,
            "pvisor-macos-test",
        ])
        .arg(&outside)
        .arg(&outside_secret);
    let output = command.output().expect("run macOS safe profile");
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("file system is not available")
    {
        eprintln!("skipping macOS safe-profile smoke test: macFUSE is installed but unavailable");
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("macos-staged.txt").exists());
    assert!(
        !outside.exists(),
        "Seatbelt allowed a write outside the stage"
    );
    assert_eq!(
        fs::read_to_string(&outside_secret).unwrap(),
        "read-compatible"
    );

    let run = only_run(&run_home);
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::SandboxedProcess)
    );
    assert!(bundle.safety.safe_profile_requested);
    assert!(bundle.safety.filesystem_changes_staged);
    assert!(!bundle.safety.filesystem_non_bypassable);
    assert!(!bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(!bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout == "macos-ok")
    );
    let filesystem = bundle.filesystem.as_ref().expect("filesystem summary");
    assert_eq!(filesystem.changed_files, 2);
    assert!(filesystem.upper.join("macos-staged.txt").is_file());
    assert!(filesystem.upper.join("outside-link").is_symlink());

    let review = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .args(["review", "--json"])
        .arg(&run)
        .output()
        .expect("review macOS Run");
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&review.stdout).unwrap();
    assert_eq!(reviewed["safety"]["filesystem_non_bypassable"], false);
    assert_eq!(reviewed["safety"]["filesystem_write_non_bypassable"], true);

    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .arg("apply")
        .arg(&run)
        .output()
        .expect("apply macOS Run");
    assert!(
        apply.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("macos-staged.txt")).unwrap(),
        "staged"
    );
}

#[test]
fn deny_all_blocks_ip_and_host_unix_sockets_on_macos() {
    if !macfuse_is_installed() {
        eprintln!("skipping macOS Seatbelt network test: macFUSE is not installed");
        return;
    }

    let temporary = tempfile::Builder::new()
        .prefix("pvmacnet")
        .tempdir_in("/tmp")
        .expect("create short macOS network fixture path");
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let outside_socket = temporary.path().join("host.sock");
    let loopback_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let loopback_port = loopback_listener.local_addr().unwrap().port();
    fs::create_dir(&workspace).unwrap();
    let _listener = UnixListener::bind(&outside_socket).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PERSISTING_RUN_HOME", &run_home)
        .env("LOOPBACK_PORT", loopback_port.to_string())
        .args([
            "run",
            "--overlaynet-deny-all",
            "--stdio",
            "capture",
            "--overlayfs-compose",
        ])
        .arg(&workspace)
        .args(["--pass-env", "LOOPBACK_PORT"])
        .args([
            "--",
            "/usr/bin/python3",
            "-c",
            r#"import errno, os, socket, sys
denied = (errno.EPERM, errno.EACCES)
assert os.environ["PERSISTING_SANDBOX_FILESYSTEM"] == "seatbelt-write"
assert os.environ["PERSISTING_SANDBOX_NETWORK"] == "deny"
agentctl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
agentctl.connect(os.environ["PERSISTING_AGENTCTL_ENDPOINT"])
agentctl.close()

try:
    inet = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    inet_code = inet.connect_ex(("192.0.2.1", 9))
except PermissionError as error:
    inet_code = error.errno

loopback = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
loopback_code = loopback.connect_ex(("127.0.0.1", int(os.environ["LOOPBACK_PORT"])))

try:
    host = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    host_code = host.connect_ex(sys.argv[1])
except PermissionError as error:
    host_code = error.errno

local = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
local.bind(os.path.join(os.environ["TMPDIR"], "local.sock"))
local.close()
print(inet_code, loopback_code, host_code)
raise SystemExit(0 if inet_code in denied and loopback_code == 0 and host_code in denied else 1)"#,
        ])
        .arg(&outside_socket)
        .output()
        .expect("run macOS deny-all profile");
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("file system is not available")
    {
        eprintln!("skipping macOS deny-all test: macFUSE is installed but unavailable");
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let run = only_run(&run_home);
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::SandboxedProcess)
    );
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("direct sockets may bypass"))
    );
}

#[test]
fn required_sandbox_blocks_original_files_and_direct_sockets_but_allows_its_proxy() {
    if !macfuse_is_installed() {
        eprintln!("skipping required sandbox integration: macFUSE is not installed");
        return;
    }
    let temporary = tempfile::Builder::new()
        .prefix("pvstrict")
        .tempdir_in("/tmp")
        .unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let outside = temporary.path().join("outside.key");
    let outside_socket = temporary.path().join("host.sock");
    fs::create_dir(&workspace).unwrap();
    fs::write(&outside, "dummy-private-key").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("alias")).unwrap();
    let _unix = UnixListener::bind(&outside_socket).unwrap();
    // The listener needs no application server: a successful CONNECT proves the
    // proxy reached this local fixture. No request leaves the machine.
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = destination.local_addr().unwrap().port();
    let rule = format!(
        r#"host="127.0.0.1",ports=[{port}],transports=["tcp_tunnel"],allow_private_ips=true"#
    );
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let outside_file = fs::File::open(&outside).unwrap();
    let outside_fd = outside_file.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .current_dir(&workspace)
        .env("PERSISTING_RUN_HOME", &run_home)
        .args([
            "run",
            "--safe",
            "--stdio",
            "capture",
            "--overlaynet-policy",
            "allowlist",
            "--overlaynet-rule",
            &rule,
            "--",
            "/usr/bin/python3",
            "-c",
            r#"
import errno, os, socket, subprocess, sys, urllib.parse
assert os.environ['PERSISTING_SANDBOX_FILESYSTEM'] == 'seatbelt-read-write'
assert os.environ['PERSISTING_SANDBOX_NETWORK'] == 'proxy-only'
denied = (errno.EACCES, errno.EPERM)
try: os.read(177, 1)
except OSError as e: assert e.errno == errno.EBADF
else: raise AssertionError('inherited descriptor leaked')
for path in [sys.argv[1], 'alias']:
    try:
        with open(path) as f: f.read()
    except OSError as e:
        assert e.errno in denied, (path, e)
    else:
        raise AssertionError('outside file readable: ' + path)
assert subprocess.run(['/bin/cat', sys.argv[1]], capture_output=True).returncode != 0
for family, target in [(socket.AF_INET, ('127.0.0.1', int(sys.argv[3]))),
                       (socket.AF_INET, ('192.0.2.1', 443)),
                       (socket.AF_INET6, ('::1', int(sys.argv[3]))),
                       (socket.AF_UNIX, sys.argv[2])]:
    try:
        with socket.socket(family, socket.SOCK_STREAM) as s:
            s.settimeout(1)
            code = s.connect_ex(target)
    except PermissionError as e: code = e.errno
    assert code in denied, (target, code)
try:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.sendto(b'probe', ('127.0.0.1', int(sys.argv[3])))
except PermissionError: pass
else: raise AssertionError('UDP escaped')
try:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(('127.0.0.1', 0))
        s.listen(1)
except PermissionError: pass
else: raise AssertionError('inbound listener escaped')
proxy = urllib.parse.urlparse(os.environ['HTTP_PROXY'])
try: proxy_socket = socket.create_connection((proxy.hostname, proxy.port), timeout=2)
except OSError as e: raise AssertionError((proxy.hostname, proxy.port, e)) from e
with proxy_socket as s:
    request = 'CONNECT 127.0.0.1:{0} HTTP/1.1\r\nHost: 127.0.0.1:{0}\r\n\r\n'.format(sys.argv[3])
    s.sendall(request.encode())
    response = s.recv(4096)
    assert b'200' in response.split(b'\r\n')[0], response
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
    s.connect(os.environ['PERSISTING_AGENTCTL_ENDPOINT'])
with open(os.path.join(os.environ['HOME'], 'state'), 'w') as f: f.write('local-state')
with open('result.txt', 'w') as f: f.write('staged')
print('required-sandbox-ok')
"#,
        ])
        .arg(&outside)
        .arg(&outside_socket)
        .arg(port.to_string());
    // Simulate a caller accidentally passing an already-open sensitive file.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(outside_fd, 177) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    if !output.status.success() {
        let bundle = RunBundle::read(&only_run(&run_home)).unwrap();
        eprintln!("Agent output: {:?}", bundle.run.output);
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&run_home)).unwrap();
    assert!(bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .unwrap_or("")
            .contains("required-sandbox-ok")
    );
    assert!(!workspace.join("result.txt").exists());
}
