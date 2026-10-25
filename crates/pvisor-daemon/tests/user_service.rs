//! Real manager/IPC contracts without KVM, guest firmware or a sandbox API key.
use pvisor::ram_backing::ipc::PoolClient;
use pvisor_daemon::memory_pool::PoolConfig;
use serde_json::Value;
use std::{
    fs,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    pool: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, pool: None }
    }
    fn state(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap()
    }
    fn command(&self, operation: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"));
        command.arg(operation).arg("--state").arg(self.state());
        command
    }
    fn call(&self, operation: &str, args: &[&str]) -> Output {
        self.command(operation).args(args).output().unwrap()
    }
    fn success(&self, operation: &str, args: &[&str]) -> Value {
        decode(self.call(operation, args))
    }
    // A foreground, test-owned pool lets teardown kill/reap only its own child.
    // Manager bootstrap must adopt this pool without changing its identity.
    fn pool(&mut self) -> PathBuf {
        let directory = self.state().join("memory-pool");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        fs::write(
            directory.join("config.json"),
            serde_json::to_vec(&PoolConfig::default()).unwrap(),
        )
        .unwrap();
        self.pool = Some(
            Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
                .args(["memory-pool", "--directory"])
                .arg(&directory)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = directory.join("pool.sock");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(&socket)
                && PoolClient::new(stream, Duration::from_secs(1)).is_ok()
            {
                return socket;
            }
            assert!(Instant::now() < deadline, "test pool did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.call("stop", &[]);
        if let Some(child) = &mut self.pool {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn decode(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn concurrent_start_converges_and_read_only_status_never_starts() {
    let fixture = Fixture::new();
    assert!(!fixture.call("status", &[]).status.success());
    assert!(!fixture.state().join("control.sock").exists());
    let children: Vec<_> = (0..6)
        .map(|_| {
            fixture
                .command("start")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let results: Vec<_> = children
        .into_iter()
        .map(|child| decode(child.wait_with_output().unwrap()))
        .collect();
    assert!(results.iter().all(|result| result == &results[0]));
    assert_eq!(results[0]["protocol"], 1);
    assert!(results[0]["memory_pool"].is_null());
    let missing = fixture.call("resolve-memory-pool", &[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("lacks memory-pool capability"));
    assert_eq!(fixture.success("status", &[]), results[0]);
    fixture.success("stop", &[]);
    assert!(!fixture.state().join("control.sock").exists());
    assert!(!fixture.call("status", &[]).status.success());
    let next = fixture.success("start", &[]);
    assert_ne!(next["instance_id"], results[0]["instance_id"]);
}

#[test]
fn implicit_start_and_manager_restart_preserve_pool_identity_and_fixed_mode() {
    let mut fixture = Fixture::new();
    let socket = fixture.pool();
    let inode = fs::metadata(&socket).unwrap().ino();
    // The CLI's auto path calls precisely this resolver. No manager exists yet.
    assert_eq!(
        fixture.success("resolve-memory-pool", &[]),
        serde_json::to_value(&socket).unwrap()
    );
    let first = fixture.success("status", &[]);
    assert_eq!(
        first["memory_pool"]["socket"],
        serde_json::to_value(&socket).unwrap()
    );
    assert_eq!(fixture.success("start", &["--memory-pool"]), first);
    let mode = if cfg!(target_os = "macos") {
        "physical-pages"
    } else {
        "compressed"
    };
    let conflict = fixture.call("start", &["--memory-pool", "--memory-pool-mode", mode]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("mode conflicts"));
    fixture.success("stop", &[]);
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
    let mut client = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    client.stats().unwrap();
    assert_eq!(
        fixture.success("resolve-memory-pool", &[]),
        serde_json::to_value(&socket).unwrap()
    );
    assert_ne!(
        fixture.success("status", &[])["instance_id"],
        first["instance_id"]
    );
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
    fixture.success("stop", &[]);
    fixture.pool.as_mut().unwrap().kill().unwrap();
    fixture.pool.as_mut().unwrap().wait().unwrap();
    // Pool-process loss is not manager restart: stale RAM endpoints are retained.
    assert!(!fixture.call("resolve-memory-pool", &[]).status.success());
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
}

#[test]
fn unsafe_paths_and_incompatible_endpoints_do_not_bootstrap() {
    use std::os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixListener,
    };
    let fixture = Fixture::new();
    let socket = fixture.state().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let worker = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        stream.write_all(b"{\"Ok\":{\"protocol\":99,\"instance_id\":\"00000000-0000-0000-0000-000000000000\",\"memory_pool\":null}}\n").unwrap();
    });
    let output = fixture.call("start", &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible daemon protocol"));
    worker.join().unwrap();
    assert!(!fixture.state().join("manager.log").exists());
    fs::remove_file(&socket).unwrap();
    let target = fixture.state().join("target");
    fs::write(&target, b"preserve").unwrap();
    symlink(&target, &socket).unwrap();
    assert!(!fixture.call("start", &[]).status.success());
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
    fs::remove_file(&socket).unwrap();
    fs::set_permissions(fixture.state(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!fixture.call("start", &[]).status.success());
    fs::set_permissions(fixture.state(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!fixture.state().join("manager.log").exists());
}

#[test]
fn unresponsive_endpoint_times_out_without_starting_another_manager() {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    let fixture = Fixture::new();
    let socket = fixture.state().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let (done, stopped) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        let _ = stopped.recv_timeout(Duration::from_secs(5));
    });
    let started = Instant::now();
    let output = fixture.call("start", &[]);
    let _ = done.send(());
    worker.join().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("response timed out"));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!fixture.state().join("manager.log").exists());
}

#[test]
fn environment_selects_an_instance_across_working_directories_and_state_overrides_it() {
    let fixture = Fixture::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
        .arg("start")
        .env("PVISOR_DAEMON_HOME", fixture.state())
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    let first = decode(output);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
        .arg("status")
        .env("PVISOR_DAEMON_HOME", fixture.state())
        .current_dir("/")
        .output()
        .unwrap();
    assert_eq!(decode(output), first);
    // --state selects fixture even when the environment names another directory.
    let output = fixture
        .command("status")
        .env("PVISOR_DAEMON_HOME", elsewhere.path())
        .output()
        .unwrap();
    assert_eq!(decode(output), first);
    assert!(!elsewhere.path().join("control.sock").exists());
}

#[test]
fn stop_requires_the_observed_instance_and_protocol() {
    use pvisor::host_transport::{read_host_frame_sync, write_host_frame_sync};
    let fixture = Fixture::new();
    let first = fixture.success("start", &[]);
    for request in [
        serde_json::json!({"protocol":1,"operation":"stop","instance_id":uuid::Uuid::new_v4()}),
        serde_json::json!({"protocol":99,"operation":"stop","instance_id":first["instance_id"]}),
    ] {
        let mut stream = UnixStream::connect(fixture.state().join("control.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_host_frame_sync(&mut stream, &request).unwrap();
        let response: Value = read_host_frame_sync(&mut stream).unwrap();
        assert!(response["Err"].is_string());
        assert_eq!(fixture.success("status", &[]), first);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn implicit_bootstrap_creates_a_real_pool_and_reuses_it_after_manager_crash() {
    use std::os::fd::{AsRawFd, FromRawFd};
    struct PoolOwner(std::fs::File);
    impl Drop for PoolOwner {
        fn drop(&mut self) {
            // The pidfd pins the identity obtained from our private pool socket;
            // cleanup never kills by a persisted/recycled PID.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
    fn peer_pidfd(path: &std::path::Path) -> std::fs::File {
        let stream = UnixStream::connect(path).unwrap();
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut credentials as *mut libc::ucred).cast(),
                    &mut length,
                )
            },
            0
        );
        assert_eq!(credentials.uid, unsafe { libc::geteuid() });
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, credentials.pid, 0) };
        assert!(descriptor >= 0);
        unsafe { std::fs::File::from_raw_fd(descriptor as i32) }
    }
    let fixture = Fixture::new();
    let resolved = fixture.success("resolve-memory-pool", &[]);
    let socket = PathBuf::from(resolved.as_str().unwrap());
    let _pool_owner = PoolOwner(peer_pidfd(&socket));
    let inode = fs::metadata(&socket).unwrap().ino();
    let first = fixture.success("status", &[]);
    let manager = peer_pidfd(&fixture.state().join("control.sock"));
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                manager.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        },
        0
    );
    let mut exit = libc::pollfd {
        fd: manager.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut exit, 1, 3000) }, 1);
    // Crash leaves a control socket; the manager lock permits replacing only it.
    assert!(fixture.state().join("control.sock").exists());
    assert_eq!(fixture.success("resolve-memory-pool", &[]), resolved);
    assert_ne!(
        fixture.success("status", &[])["instance_id"],
        first["instance_id"]
    );
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
    let mut client = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    client.stats().unwrap();
    fixture.success("stop", &[]);
    client.stats().unwrap();
}
