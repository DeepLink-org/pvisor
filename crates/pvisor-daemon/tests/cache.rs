//! Exercise the installed service entry point without a registry or native VM.
use pvisor::cache::{CacheClient, Request};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"));
    command.args(["serve", "--cache"]);
    for key in [
        "PVISOR_CACHE_LOCATION",
        "PVISOR_CACHE_SERVER",
        "PVISOR_CACHE_TOKEN",
    ] {
        command.env_remove(key);
    }
    command
}

fn wait_ready(service: &mut Service, client: &CacheClient) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            service.0.try_wait().unwrap().is_none(),
            "cache service exited"
        );
        if client.request(Request::Ping).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cache service did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cache_service_answers_clients_and_fences_duplicate_owners() {
    let root = tempfile::Builder::new()
        .prefix("pvc-")
        .tempdir_in("/tmp")
        .unwrap();
    let address = format!("unix://{}", root.path().join("cache.sock").display());
    let store = root.path().join("images");
    let mut service = Service(
        daemon()
            .args(["--cache-listen", &address, "--cache-image-store"])
            .arg(&store)
            // Client preferences must not change the daemon's service role.
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_READ_ONLY", "true")
            .env("OPEN_SANDBOX_API_KEY", "unused-by-cache-only")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = CacheClient::new(address.clone(), None).unwrap();
    wait_ready(&mut service, &client);
    let duplicate = daemon()
        .args(["--cache-listen", &address, "--cache-image-store"])
        .arg(&store)
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("socket lock held"));
    client.request(Request::Ping).unwrap();
    unsafe {
        libc::kill(service.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let fresh = CacheClient::new(address.clone(), None).unwrap();
    assert!(
        fresh.request(Request::Ping).is_err(),
        "cache worker survived daemon shutdown"
    );
    // Restart on the stale socket, then simulate an owner crash rather than graceful stop.
    let mut restarted = Service(
        daemon()
            .args(["--cache-listen", &address, "--cache-image-store"])
            .arg(&store)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_ready(&mut restarted, &fresh);
    restarted.0.kill().unwrap();
    restarted.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if CacheClient::new(address.clone(), None)
            .unwrap()
            .request(Request::Ping)
            .is_err()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cache worker survived daemon crash"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn tcp_cache_service_requires_authentication() {
    let root = tempfile::tempdir().unwrap();
    let output = daemon()
        .args(["--cache-listen", "tcp://127.0.0.1:0", "--cache-image-store"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("TCP requires PVISOR_CACHE_TOKEN"));
}

#[test]
fn failed_sandbox_start_does_not_leave_a_cache_service() {
    let root = tempfile::Builder::new()
        .prefix("pvc-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("cache.sock");
    let output = daemon()
        .args([
            "--sandbox",
            "--api-key",
            "fixture-key",
            "--images-dir",
            "/nonexistent-pvisor-images",
            "--cgroup-root",
            "/nonexistent-pvisor-cgroup",
            "--cache-listen",
            &format!("unix://{}", socket.display()),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        !socket.exists(),
        "cache started despite sandbox startup failure"
    );
}

#[test]
fn no_component_selection_fails_before_creating_state() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
        .arg("serve")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("select at least one service"));
    assert!(!root.path().join(".pvisor").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn cache_worker_failure_stops_its_daemon() {
    use std::os::{fd::AsRawFd, unix::net::UnixStream};
    let root = tempfile::Builder::new()
        .prefix("pvc-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("cache.sock");
    let address = format!("unix://{}", socket.display());
    let mut service = Service(
        daemon()
            .args(["--cache-listen", &address, "--cache-image-store"])
            .arg(root.path().join("images"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_ready(&mut service, &CacheClient::new(address, None).unwrap());
    let peer = UnixStream::connect(socket).unwrap();
    #[cfg(target_os = "macos")]
    let pid = {
        let mut pid: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    peer.as_raw_fd(),
                    libc::SOL_LOCAL,
                    libc::LOCAL_PEERPID,
                    (&mut pid as *mut libc::pid_t).cast(),
                    &mut length,
                )
            },
            0
        );
        pid
    };
    #[cfg(target_os = "linux")]
    let pid = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    peer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut credentials as *mut libc::ucred).cast(),
                    &mut length,
                )
            },
            0
        );
        credentials.pid
    };
    assert!(pid > 0 && pid as u32 != service.0.id() && pid as u32 != std::process::id());
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            assert!(
                !status.success(),
                "worker failure was reported as clean shutdown"
            );
            break;
        }
        assert!(Instant::now() < deadline, "daemon survived worker failure");
        std::thread::sleep(Duration::from_millis(20));
    }
}
