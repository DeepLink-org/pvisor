//! Local service gates: real processes, delegated limits and shared FUSE owners.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use pvisor::{
    environment_snapshot::{Compatibility, SnapshotStore},
    node::Pin,
};
use std::{
    fs,
    io::Write,
    os::unix::{fs::MetadataExt, io::AsRawFd},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[path = "common/native_cache.rs"]
mod native_cache;

struct Deployment {
    root: tempfile::TempDir,
    config: PathBuf,
    process: Child,
    unit: Option<String>,
}
impl Deployment {
    async fn start() -> Self {
        Self::start_bound(false).await
    }
    async fn start_bound(delegated: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let executable = Path::new(env!("CARGO_BIN_EXE_pvisor"));
        fs::copy(executable, root.path().join("pvisor")).unwrap();
        fs::create_dir(root.path().join("snapshots")).unwrap();
        let config = root.path().join("service.toml");
        let cgroup = if delegated {
            "cgroup_root = ':self:'\n"
        } else {
            ""
        };
        fs::write(&config, format!("state = 'state'\n{cgroup}[node]\ncache_location = 'cache'\nsnapshot_roots = ['snapshots']\nmax_owners = 1\nwarm_owners = 0\nmax_cache_bytes = 32768\n")).unwrap();
        let unit = delegated.then(|| format!("pvisor-service-test-{}", uuid::Uuid::new_v4()));
        let mut command = if let Some(unit) = &unit {
            let mut command = Command::new("systemd-run");
            command.args([
                "--user",
                "--wait",
                "--collect",
                "--quiet",
                "--unit",
                unit,
                "--property=Delegate=yes",
                "--property=MemoryMax=2G",
                "--property=MemorySwapMax=0",
                "--property=CPUQuota=100%",
                "--setenv=TOKIO_WORKER_THREADS",
            ]);
            command.arg(root.path().join("pvisor"));
            command
        } else {
            Command::new(root.path().join("pvisor"))
        };
        let process = command
            .args(["service", "run", "--config"])
            .arg(&config)
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deployment = Self {
            root,
            config,
            process,
            unit,
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(status) = deployment.request(serde_json::json!({"op":"status"})).await
                    && status["roles"]["node"]["state"] == "running"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("deployment readiness");
        deployment
    }
    fn socket(&self) -> PathBuf {
        self.root.path().join("state/node/node.sock")
    }
    async fn request(&self, value: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let mut socket =
            tokio::net::UnixStream::connect(self.root.path().join("state/service.sock")).await?;
        socket.write_all(&serde_json::to_vec(&value)?).await?;
        socket.write_all(b"\n").await?;
        let mut text = String::new();
        tokio::time::timeout(
            Duration::from_secs(15),
            BufReader::new(socket).read_line(&mut text),
        )
        .await??;
        Ok(serde_json::from_str(&text)?)
    }
    async fn stop(&mut self) {
        let result = self
            .request(serde_json::json!({"op":"stop"}))
            .await
            .unwrap();
        assert!(result.get("error").is_none(), "{result}");
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.process.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!self.root.path().join("state/service.sock").exists());
        assert!(!self.socket().exists());
    }
}

impl Drop for Deployment {
    fn drop(&mut self) {
        // systemd-run is a proxy: its exit says nothing about the transient
        // service. Always stop our exact unit, including failed test paths.
        if let Some(unit) = &self.unit {
            let _ = Command::new("systemctl")
                .args(["--user", "stop", unit])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.process.wait();
            return;
        }
        if self.process.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.process.id() as i32, libc::SIGINT);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if self.process.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("service test cleanup still draining; retained data-owner processes");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated systemd; local failure-cleanup gate, no VMs"]
async fn proxy_exit_does_not_skip_transient_service_cleanup() {
    let mut deployment = Deployment::start_bound(true).await;
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    let node_pid = status["roles"]["node"]["pid"].as_u64().unwrap();
    let unit = deployment.unit.clone().unwrap();
    // Simulate a failed test killing only its systemd-run proxy. The managed
    // unit remains alive and must be cleaned by the fixture destructor.
    deployment.process.kill().unwrap();
    deployment.process.wait().unwrap();
    assert!(Path::new(&format!("/proc/{node_pid}")).exists());
    drop(deployment);
    assert!(
        !Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", &unit])
            .status()
            .unwrap()
            .success()
    );
    assert!(!Path::new(&format!("/proc/{node_pid}")).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated systemd; local cgroup gate, no VMs"]
async fn service_installs_kernel_caps_before_roles_execute() {
    let mut deployment = Deployment::start_bound(true).await;
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_kernel_caps(&status, &["node"]);
    let cli = tokio::process::Command::new(deployment.root.path().join("pvisor"))
        .args(["service", "status", "--config"])
        .arg(&deployment.config)
        .env("TOKIO_WORKER_THREADS", "2")
        .output()
        .await
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_status: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(
        cli_status["roles"]["node"]["pid"],
        status["roles"]["node"]["pid"]
    );
    deployment.stop().await;
}
fn assert_kernel_caps(status: &serde_json::Value, roles: &[&str]) {
    assert_eq!(status["kernel_limits"], true);
    for name in roles {
        let pid = status["roles"][*name]["pid"].as_u64().unwrap();
        let group = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let relative = group
            .lines()
            .find_map(|line| line.strip_prefix("0::/"))
            .unwrap();
        let directory = Path::new("/sys/fs/cgroup").join(relative);
        assert!(directory.ends_with(name.replace(':', "-")));
        assert_eq!(
            fs::read_to_string(directory.join("memory.max"))
                .unwrap()
                .trim(),
            "536870912"
        );
        assert_eq!(
            fs::read_to_string(directory.join("memory.swap.max"))
                .unwrap()
                .trim(),
            "0"
        );
        assert_eq!(
            fs::read_to_string(directory.join("cpu.max"))
                .unwrap()
                .trim(),
            "50000 100000"
        );
        let events = fs::read_to_string(directory.join("memory.events")).unwrap();
        assert!(events.lines().any(|line| line == "oom 0"));
        assert!(events.lines().any(|line| line == "oom_kill 0"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FUSE; local image ownership gate, no VMs"]
async fn node_image_pins_share_mount_and_fence_restart() {
    let mut deployment = Deployment::start().await;
    let publisher = deployment.root.path().join("publisher");
    let cache = deployment.root.path().join("cache");
    let layer = tokio::task::spawn_blocking(move || {
        native_cache::publish_layer(
            &publisher,
            &cache,
            "service-native",
            &[("env/shared", "immutable\n")],
            true,
        )
    })
    .await
    .unwrap();
    assert!(Pin::image(&deployment.socket(), &layer.handle, "sha256:wrong-manifest").is_err());
    let first = Pin::image(&deployment.socket(), &layer.handle, &layer.manifest_digest).unwrap();
    let second = Pin::image(&deployment.socket(), &layer.handle, &layer.manifest_digest).unwrap();
    assert_eq!(first.path(), second.path());
    assert_eq!(
        fs::read(first.path().join("env/shared")).unwrap(),
        b"immutable\n"
    );
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_eq!(status["node_resources"]["active_pins"], 2);
    assert_eq!(status["node_resources"]["live_owners"], 1);
    let refused = deployment
        .request(serde_json::json!({"op":"restart","role":"node"}))
        .await
        .unwrap();
    assert!(refused["error"].as_str().unwrap().contains("active pins"));
    drop(first);
    assert_eq!(
        fs::read(second.path().join("env/shared")).unwrap(),
        b"immutable\n"
    );
    drop(second);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = deployment
                .request(serde_json::json!({"op":"status"}))
                .await
                .unwrap();
            if status["node_resources"]["active_pins"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let restarted = deployment
        .request(serde_json::json!({"op":"restart","role":"node"}))
        .await
        .unwrap();
    assert!(restarted.get("error").is_none(), "{restarted}");
    assert_ne!(
        restarted["roles"]["node"]["pid"],
        status["roles"]["node"]["pid"]
    );
    deployment.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated systemd, two real KVM guests, FUSE and libkrunfw"]
async fn native_jobs_share_node_image_and_preserve_private_writes() {
    struct Guest {
        child: Child,
        stage: PathBuf,
        executable: PathBuf,
    }
    impl Drop for Guest {
        fn drop(&mut self) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            let _ = Command::new(&self.executable)
                .arg("kill")
                .arg(&self.stage)
                .status();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while self.child.try_wait().ok().flatten().is_none()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let mut deployment = Deployment::start_bound(true).await;
    let publisher = deployment.root.path().join("publisher");
    let cache = deployment.root.path().join("cache");
    let layer = tokio::task::spawn_blocking(move || {
        native_cache::publish_layer(&publisher, &cache, "service-native-guests", &[], true)
    })
    .await
    .unwrap();
    let mut pins = Vec::new();
    let mut guests = Vec::new();
    for id in ["a", "b"] {
        let pin = Pin::image(&deployment.socket(), &layer.handle, &layer.manifest_digest).unwrap();
        let workspace = deployment.root.path().join(format!("workspace-{id}"));
        fs::create_dir(&workspace).unwrap();
        let stage = deployment.root.path().join(format!("job-{id}"));
        let executable = deployment.root.path().join("pvisor");
        let mut command = Command::new(&executable);
        command
            .current_dir(&workspace)
            .args([
                "run",
                "--executor",
                "vm",
                "--overlaynet",
                "off",
                "--memory",
                "128MiB",
                "--stdio",
                "capture",
                "--rootfs",
            ])
            .arg(pin.path())
            .arg("--stage")
            .arg(&stage);
        if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
            command.arg("--vm-library-dir").arg(directory);
        }
        command.args(["--", "/bin/sh", "-c", &format!("set -eu; printf '{id}\\n' > private; printf ready > ready; while [ ! -e go ]; do /bin/sleep 0.1; done; read -r value < private; test \"$value\" = '{id}'; printf '%s\\n' \"$value\"")])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
        guests.push(Guest {
            child: command.spawn().unwrap(),
            stage,
            executable,
        });
        pins.push(pin);
    }
    assert_eq!(pins[0].path(), pins[1].path());
    let uppers = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let mut uppers = Vec::new();
            for guest in &mut guests {
                assert!(
                    guest.child.try_wait().unwrap().is_none(),
                    "guest exited before readiness"
                );
                if let Ok(record) = pvisor::RunRecord::read(&guest.stage)
                    && let Some(overlay) = record.overlay
                    && overlay.upper.upper_dir.join("ready").is_file()
                {
                    uppers.push(overlay.upper.upper_dir);
                }
            }
            if uppers.len() == 2 {
                break uppers;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("two native guests must reach useful readiness");
    assert_ne!(uppers[0], uppers[1]);
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_eq!(status["node_resources"]["active_pins"], 2);
    assert_eq!(status["node_resources"]["live_owners"], 1);
    assert_kernel_caps(&status, &["node"]);
    let refused = deployment
        .request(serde_json::json!({"op":"restart", "role":"node"}))
        .await
        .unwrap();
    assert!(refused["error"].as_str().unwrap().contains("active pins"));
    for upper in &uppers {
        fs::write(upper.join("go"), b"go").unwrap();
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let mut completed = 0;
            for guest in &mut guests {
                if let Some(status) = guest.child.try_wait().unwrap() {
                    assert!(status.success(), "native guest failed: {status}");
                    completed += 1;
                }
            }
            if completed == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for (upper, expected) in uppers.iter().zip([b"a\n", b"b\n"]) {
        assert_eq!(fs::read(upper.join("private")).unwrap(), expected);
    }
    drop(guests);
    drop(pins);
    tokio::time::timeout(Duration::from_secs(5), async {
        while deployment
            .request(serde_json::json!({"op":"status"}))
            .await
            .unwrap()["node_resources"]["active_pins"]
            != 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    deployment.stop().await;
}

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "node-test".into(),
        build: "node-test".into(),
        firmware: "node-test".into(),
        profile: "node-test".into(),
    }
}
fn publish(root: &Path, source: &Path, machine: &[u8]) -> String {
    let store = SnapshotStore::new(root).unwrap();
    let pending = store.begin().unwrap();
    let bytes: Vec<_> = (0..65536).map(|index| (index % 251) as u8).collect();
    pending.create_ram().unwrap().write_all(&bytes).unwrap();
    pending.publish(source, machine, compatibility()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FUSE; local RAM backing gate, no VMs"]
async fn node_pins_share_ram_across_stores_preserve_cow_and_fence_restart() {
    let mut deployment = Deployment::start().await;
    let source = deployment.root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"source").unwrap();
    let first_store = deployment.root.path().join("snapshots/a");
    let second_store = deployment.root.path().join("snapshots/b");
    let id = publish(&first_store, &source, b"machine");
    assert_eq!(publish(&second_store, &source, b"machine"), id);
    let first = Pin::ram(
        &deployment.socket(),
        &first_store,
        &id,
        compatibility(),
        None,
    )
    .unwrap();
    let second = Pin::ram(
        &deployment.socket(),
        &second_store,
        &id,
        compatibility(),
        None,
    )
    .unwrap();
    assert_eq!(first.path(), second.path());
    // Authorization and compatibility must still be checked on a hot owner hit.
    let unauthorized = deployment.root.path().join("unauthorized");
    assert_eq!(publish(&unauthorized, &source, b"machine"), id);
    assert!(
        Pin::ram(
            &deployment.socket(),
            &unauthorized,
            &id,
            compatibility(),
            None
        )
        .is_err()
    );
    let alias = deployment.root.path().join("snapshots/unauthorized-alias");
    std::os::unix::fs::symlink(&unauthorized, &alias).unwrap();
    assert!(Pin::ram(&deployment.socket(), &alias, &id, compatibility(), None).is_err());
    let mut incompatible = compatibility();
    incompatible.firmware = "different-firmware".into();
    assert!(Pin::ram(&deployment.socket(), &first_store, &id, incompatible, None).is_err());
    assert_eq!(
        fs::metadata(first.path()).unwrap().ino(),
        fs::metadata(second.path()).unwrap().ino()
    );
    let a = fs::File::open(first.path()).unwrap();
    let b = fs::File::open(second.path()).unwrap();
    unsafe {
        let x = libc::mmap(
            std::ptr::null_mut(),
            65536,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE,
            a.as_raw_fd(),
            0,
        );
        let y = libc::mmap(
            std::ptr::null_mut(),
            65536,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE,
            b.as_raw_fd(),
            0,
        );
        assert_ne!(x, libc::MAP_FAILED);
        assert_ne!(y, libc::MAP_FAILED);
        assert_eq!(*x.cast::<u8>(), 0);
        assert_eq!(*y.cast::<u8>(), 0);
        *x.cast::<u8>() = 99;
        assert_eq!(*y.cast::<u8>(), 0);
        assert_eq!(*y.cast::<u8>().add(65535), (65535 % 251) as u8);
        assert_eq!(libc::munmap(x, 65536), 0);
        assert_eq!(libc::munmap(y, 65536), 0);
    }
    drop((a, b));
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_eq!(status["node_resources"]["active_pins"], 2);
    assert_eq!(status["node_resources"]["live_owners"], 1);
    assert!(status["node_resources"]["cache_bytes"].as_u64().unwrap() <= 32768);
    let refused = deployment
        .request(serde_json::json!({"op":"restart","role":"node"}))
        .await
        .unwrap();
    assert!(refused["error"].as_str().unwrap().contains("active pins"));
    let old_pid = status["roles"]["node"]["pid"].clone();
    let refused_stop = deployment
        .request(serde_json::json!({"op":"stop","role":"node"}))
        .await
        .unwrap();
    assert!(
        refused_stop["error"]
            .as_str()
            .unwrap()
            .contains("active pins")
    );
    assert_eq!(refused_stop.get("roles"), None);
    assert_eq!(fs::read(first.path()).unwrap()[0], 0);
    let other = publish(&second_store, &source, b"different-machine");
    assert!(
        Pin::ram(
            &deployment.socket(),
            &second_store,
            &other,
            compatibility(),
            None
        )
        .is_err()
    );
    drop(first);
    assert_eq!(fs::read(second.path()).unwrap()[0], 0);
    drop(second);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if deployment
                .request(serde_json::json!({"op":"status"}))
                .await
                .unwrap()["node_resources"]["active_pins"]
                == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let restarted = deployment
        .request(serde_json::json!({"op":"restart","role":"node"}))
        .await
        .unwrap();
    assert!(restarted.get("error").is_none(), "{restarted}");
    assert_ne!(restarted["roles"]["node"]["pid"], old_pid);
    deployment.stop().await;
}
