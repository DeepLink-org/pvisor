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
        mode: Default::default(),
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

#[cfg(target_os = "linux")]
fn compressed_pool() -> (
    tempfile::TempDir,
    OwnedChild,
    std::path::PathBuf,
    PoolConfig,
) {
    let state = tempfile::tempdir().unwrap();
    let directory = state.path().join("memory-pool");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let config = PoolConfig {
        mode: pvisor_daemon::memory_pool::PoolMode::Compressed,
        max_bytes: 8192,
        max_objects: 2,
        max_connections: 8,
        max_references: 2,
    };
    fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
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
    (state, child, socket, config)
}

#[cfg(target_os = "linux")]
fn sealed_client(socket: &std::path::Path) -> pvisor::ram_backing::ipc::SealedClient {
    let mut client =
        PoolClient::new(UnixStream::connect(socket).unwrap(), Duration::from_secs(2)).unwrap();
    assert!(client.enable_shared_mapping().is_err());
    pvisor::ram_backing::ipc::SealedClient::new(client).unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_compressed_pool_shares_and_retains_local_recovery_after_service_exit() {
    use std::os::unix::fs::MetadataExt;
    let (state, mut child, socket, config) = compressed_pool();
    let mut a = sealed_client(&socket);
    let mut b = sealed_client(&socket);
    let bytes: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
    let ar = a.put(&bytes).unwrap();
    let br = b.put(&bytes).unwrap();
    let unique = a.put(&vec![3; 65536]).unwrap();
    let stats = a.stats().unwrap();
    assert_eq!(
        (
            stats.encoded_bytes,
            stats.objects,
            stats.session_references,
            stats.cross_session_objects
        ),
        (8192, 2, 2, 1)
    );
    assert!(b.put(&vec![4; 65536]).is_err());
    let inode = fs::metadata(&socket).unwrap().ino();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon"));
    assert_eq!(
        ensure_service(state.path(), executable, config.clone()).unwrap(),
        socket
    );
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
    let mut changed = config.clone();
    changed.mode = pvisor_daemon::memory_pool::PoolMode::Physical;
    assert!(ensure_service(state.path(), executable, changed).is_err());

    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let mut output = vec![0; bytes.len()];
    // Restore before any liveness probe: neither GET nor service ownership is
    // involved after the validated descriptor handoff.
    a.restore(&ar, &mut output).unwrap();
    assert_eq!(output, bytes);
    output.fill(0x91);
    b.restore(&br, &mut output).unwrap();
    assert_eq!(output, bytes);
    a.restore(&unique, &mut output).unwrap();
    assert_eq!(output, vec![3; 65536]);
    assert!(ensure_service(state.path(), executable, config).is_err());
    assert_eq!(
        fs::metadata(&socket).unwrap().ino(),
        inode,
        "stale socket must not be replaced"
    );
    assert_eq!(a.stats().unwrap().encoded_bytes, 8192);
    assert!(!a.connected());
    assert_eq!(
        a.put(&bytes).err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
    a.release(ar).unwrap();
    a.release(unique).unwrap();
    b.release(br).unwrap();
    assert_eq!(a.stats().unwrap().session_references, 0);
    assert_eq!(b.stats().unwrap().objects, 0);
}

#[test]
fn legacy_pool_configuration_retains_physical_mode() {
    let config: PoolConfig = serde_json::from_value(serde_json::json!({
        "max_bytes": 4096, "max_objects": 16, "max_connections": 8, "max_references": 16
    }))
    .unwrap();
    assert_eq!(config.mode, pvisor_daemon::memory_pool::PoolMode::Physical);
    assert!(serde_json::from_value::<PoolConfig>(serde_json::json!({
        "mode": "unknown", "max_bytes": 4096, "max_objects": 16, "max_connections": 8, "max_references": 16
    })).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn daemon_compressed_disconnected_handles_stay_budgeted_until_peer_exit() {
    use pvisor::ram_backing::ipc::SealedClient;
    const CHILD: &str = "PVISOR_SEALED_POOL_TEST_SOCKET";
    if let Some(socket) = std::env::var_os(CHILD) {
        let stream = UnixStream::connect(socket).unwrap();
        let disconnect = stream.try_clone().unwrap();
        let mut client =
            SealedClient::new(PoolClient::new(stream, Duration::from_secs(2)).unwrap()).unwrap();
        let object = client.put(&vec![0x19; 65536]).unwrap();
        disconnect.shutdown(std::net::Shutdown::Both).unwrap();
        let mut output = vec![0; 65536];
        client.restore(&object, &mut output).unwrap();
        assert_eq!(output, vec![0x19; 65536]);
        assert_eq!(client.stats().unwrap().session_references, 1);
        assert!(!client.connected());
        fs::write(
            std::env::var_os("PVISOR_SEALED_POOL_TEST_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        // Parent owns and terminates this exact child after observing the pin.
        std::thread::sleep(Duration::from_secs(15));
        panic!("parent did not complete lifetime check before deadline");
    }
    let (state, _service, socket, _config) = compressed_pool();
    let ready = state.path().join("peer-ready");
    let mut peer = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_compressed_disconnected_handles_stay_budgeted_until_peer_exit",
                "--nocapture",
            ])
            .env(CHILD, &socket)
            .env("PVISOR_SEALED_POOL_TEST_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            peer.0.try_wait().unwrap().is_none(),
            "peer failed before publishing reference"
        );
        assert!(Instant::now() < deadline, "peer readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut observer = sealed_client(&socket);
    let shared = observer.put(&vec![0x19; 65536]).unwrap();
    assert_eq!(observer.stats().unwrap().cross_session_objects, 1);
    let unique = observer.put(&vec![0x20; 65536]).unwrap();
    observer.release(shared).unwrap();
    assert_eq!(observer.stats().unwrap().objects, 2);
    assert!(
        observer.put(&vec![0x21; 65536]).is_err(),
        "disconnected peer must still consume the file-page budget"
    );
    peer.0.kill().unwrap();
    peer.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while observer.stats().unwrap().objects != 1 {
        assert!(Instant::now() < deadline, "peer-exit collection deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let next = observer.put(&vec![0x21; 65536]).unwrap();
    observer.release(next).unwrap();
    observer.release(unique).unwrap();
    assert_eq!(observer.stats().unwrap().objects, 0);
}
