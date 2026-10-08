use pvisor::ram_backing::ipc::PoolClient;
use pvisor_daemon::memory_pool::{PoolConfig, ensure_service};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn daemon_pool_shares_objects_and_survives_owner_reopen_without_replacement() {
    let state = tempfile::tempdir().unwrap();
    let directory = state.path().join("memory-pool");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let config = PoolConfig {
        max_bytes: 4096,
        max_objects: 16,
        max_connections: 8,
        max_references: 16,
    };
    fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon"));
    let mut child = OwnedChild(
        Command::new(executable)
            .args(["memory-pool", "--directory"])
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = directory.join("pool.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "pool exited before readiness"
        );
        assert!(Instant::now() < deadline, "pool readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut a = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    let mut b = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    #[cfg(target_os = "linux")]
    {
        a.enable_shared_mapping().unwrap();
        b.enable_shared_mapping().unwrap();
    }
    let bytes = vec![
        7;
        if cfg!(target_os = "linux") {
            4096
        } else {
            65536
        }
    ];
    let first = a.put(&bytes).unwrap();
    let second = b.put(&bytes).unwrap();
    assert_eq!(a.stats().unwrap().objects, 1);
    assert_eq!(a.stats().unwrap().cross_session_objects, 1);
    assert_eq!(
        ensure_service(state.path(), executable, config.clone()).unwrap(),
        socket
    );
    let mut output = vec![0; bytes.len()];
    b.restore(&second, &mut output).unwrap();
    assert_eq!(output, bytes);
    let mut changed = config;
    changed.max_bytes += 1;
    assert!(ensure_service(state.path(), executable, changed).is_err());
    a.release(first).unwrap();
    b.restore(&second, &mut output).unwrap();
    assert_eq!(output, bytes);
    b.release(second).unwrap();
    assert_eq!(b.stats().unwrap().objects, 0);
    assert!(child.0.try_wait().unwrap().is_none());
}
