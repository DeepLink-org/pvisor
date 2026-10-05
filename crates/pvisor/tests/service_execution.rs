//! Explicit service gates: real processes, FUSE backing and two bounded KVM guests.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use pvisor::{
    environment_snapshot::{Compatibility, SnapshotStore},
    node::Pin,
};
use pvisor_cluster::{client::Client, *};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{
    collections::BTreeMap,
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

const ADMIN: &str = "service-test-admin-0123456789012345";
const WORKER: &str = "service-test-worker-0123456789012345";
struct Deployment {
    root: tempfile::TempDir,
    config: PathBuf,
    process: Child,
    client: Client,
    unit: Option<String>,
}
impl Deployment {
    async fn start(with_worker: bool) -> Self {
        Self::start_bound(with_worker, false).await
    }
    async fn start_bound(with_worker: bool, delegated: bool) -> Self {
        Self::start_profile(with_worker, delegated, false).await
    }
    async fn start_profile(with_worker: bool, delegated: bool, native: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let executable = Path::new(env!("CARGO_BIN_EXE_pvisor"));
        // Pin the exact three executables for native internal reentry and restarts.
        for name in ["pvisor", "pvisor-worker", "pvisor-cluster"] {
            let source = executable.parent().unwrap().join(name);
            assert!(
                source.is_file(),
                "build companions with just service-build first"
            );
            fs::copy(source, root.path().join(name)).unwrap();
        }
        fs::create_dir(root.path().join("snapshots")).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let config = root.path().join("service.toml");
        let worker = if native {
            let firmware = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR")
                .map(PathBuf::from)
                .expect("native gate requires PVISOR_TEST_LIBKRUNFW_DIR containing libkrunfw.so.5");
            assert!(firmware.join("libkrunfw.so.5").is_file());
            let profile = format!(
                "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\n[vm]\nrootfs_immutable = true\nlibrary_dir = {}\n",
                serde_json::to_string(&firmware.canonicalize().unwrap()).unwrap()
            );
            fs::write(root.path().join("vm.toml"), profile).unwrap();
            ["a", "b"].into_iter().map(|id| format!("\n[[workers]]\nid = '{id}'\nbackend = 'vm'\nprofile = 'vm.toml'\nslots = 1\nmemory_bytes = 134217728\ncpu_millis = 1000\npoll_ms = 100\n")).collect::<String>()
        } else if with_worker {
            "\n[[workers]]\nid = 'one'\nbackend = 'host'\nslots = 1\nmemory_bytes = 67108864\ncpu_millis = 100\npoll_ms = 100\n"
                .to_owned()
        } else {
            String::new()
        };
        let cache = if native {
            "cache_backend = 'filesystem'\ncache_location = 'cache'\n"
        } else {
            ""
        };
        let cgroup = if delegated {
            "cgroup_root = ':self:'\n"
        } else {
            ""
        };
        fs::write(&config, format!("state = 'state'\n{cgroup}[controller]\nlisten = '{address}'\nlease_ms = 3000\n[node]\n{cache}snapshot_roots = ['snapshots']\nmax_owners = 1\nwarm_owners = 0\nmax_cache_bytes = 32768\n{worker}")).unwrap();
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
                "--setenv=PVISOR_CLUSTER_TOKEN",
                "--setenv=PVISOR_CLUSTER_WORKER_TOKEN",
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
            .env("PVISOR_CLUSTER_TOKEN", ADMIN)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
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
            client: Client::new(&format!("http://{address}"), ADMIN.into()).unwrap(),
            unit,
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(status) = deployment.request(serde_json::json!({"op":"status"})).await
                    && status["roles"]["node"]["state"] == "running"
                    && (!with_worker
                        || deployment
                            .client
                            .workers()
                            .await
                            .is_ok_and(|workers| workers.len() >= if native { 2 } else { 1 }))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "two capped real KVM guests: just test-service-vm"]
async fn native_vm_workers_share_environment_mount_and_private_uppers() {
    let mut deployment = Deployment::start_profile(true, true, true).await;
    let publisher = deployment.root.path().join("publisher");
    let cache = deployment.root.path().join("cache");
    let layer = tokio::task::spawn_blocking(move || {
        native_cache::publish_layer(&publisher, &cache, "service-native", &[], true)
    })
    .await
    .unwrap();
    assert!(Pin::image(&deployment.socket(), &layer.handle, "sha256:wrong-manifest").is_err());
    let environment = deployment
        .client
        .publish_environment(&EnvironmentTemplate {
            version: CLUSTER_VERSION,
            architecture: "amd64".into(),
            base: layer,
            workspace: None,
            toolkits: vec![],
        })
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for id in ["a", "b"] {
        let mut spec = task();
        spec.id = format!("native-{id}");
        spec.run.run_id = pvisor_core::RunId(spec.id.clone());
        spec.execution = ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        };
        spec.resources.memory_bytes = 128 * 1024 * 1024;
        spec.resources.cpu_millis = 1000;
        spec.environment = Some(environment.digest.clone());
        spec.run.runtime.resource_limits.memory_bytes = Some(128 * 1024 * 1024);
        spec.run.runtime.timeout_ms = Some(45_000);
        let RunInvocation::Process(process) = &mut spec.run.invocation;
        process.args = vec![
            "-c".into(),
            format!(
                "set -eu; printf '{id}\\n' > /tmp/private; printf ready > /tmp/ready; while [ ! -e /tmp/go ]; do /bin/sleep 0.1; done; read -r value < /tmp/private; test \"$value\" = '{id}'; printf '%s\\n' \"$value\""
            ),
        ];
        deployment.client.submit(&spec).await.unwrap();
        tasks.push(spec.id);
    }
    let uppers = tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let mut uppers = Vec::new();
            for task in &tasks {
                let record = deployment.client.task(task).await.unwrap();
                assert!(
                    !record.phase.terminal(),
                    "native task stopped before ready: {record:?}"
                );
                if let Some(lease) = record.lease {
                    let upper = deployment
                        .root
                        .path()
                        .join("state/workers")
                        .join(lease.key.worker_id)
                        .join("tasks")
                        .join(format!("{task}-1/upper/tmp"));
                    if upper.join("ready").is_file() {
                        uppers.push(upper);
                    }
                }
            }
            if uppers.len() == 2 {
                break uppers;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("two bounded guests must reach useful readiness");
    let before = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_eq!(before["node_resources"]["active_pins"], 2);
    assert_eq!(before["node_resources"]["live_owners"], 1);
    let after = deployment
        .request(serde_json::json!({"op":"restart", "role":"controller"}))
        .await
        .unwrap();
    assert!(after.get("error").is_none(), "{after}");
    for role in ["node", "worker:a", "worker:b"] {
        assert_eq!(before["roles"][role]["pid"], after["roles"][role]["pid"]);
    }
    for upper in uppers {
        fs::write(upper.join("go"), b"go").unwrap();
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut finished = 0;
            for (id, expected) in tasks.iter().zip([b"a\n", b"b\n"]) {
                let record = deployment.client.task(id).await.unwrap();
                if record.phase.terminal() {
                    assert_eq!(record.phase, TaskPhase::Succeeded, "{record:?}");
                    assert_eq!(
                        record.result.unwrap().output.stdout.unwrap().as_bytes(),
                        expected
                    );
                    finished += 1;
                }
            }
            if finished == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let final_status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_kernel_caps(
        &final_status,
        &["controller", "node", "worker:a", "worker:b"],
    );
    deployment.stop().await;
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
#[ignore = "explicit delegated failure-cleanup gate, no VMs: just test-service"]
async fn proxy_exit_does_not_skip_transient_service_cleanup() {
    let mut deployment = Deployment::start_bound(false, true).await;
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
#[ignore = "explicit delegated systemd cgroup gate, no VMs: just test-service"]
async fn service_installs_kernel_caps_before_roles_execute() {
    let mut deployment = Deployment::start_bound(true, true).await;
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert_kernel_caps(&status, &["controller", "node", "worker:one"]);
    deployment.client.submit(&task()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if deployment.client.task("service-live").await.unwrap().phase == TaskPhase::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_kernel_caps(&status, &["controller", "node", "worker:one"]);
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

fn task() -> TaskSpec {
    let mut run = RunSpec::process("service-live", "service-test", "/bin/sh");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.args = vec!["-c".into(), "sleep 2; printf survived".into()];
    process.inherit_env = false;
    run.runtime.max_output_bytes = 4096;
    run.runtime.termination_grace_ms = 100;
    run.runtime.timeout_ms = Some(10_000);
    run.runtime.resource_limits.memory_bytes = Some(64 * 1024 * 1024);
    run.runtime.resource_limits.cpu_time_ms = Some(2000);
    TaskSpec {
        version: CLUSTER_VERSION,
        id: "service-live".into(),
        tenant: "test".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
        },
        resources: Resources {
            slots: 1,
            memory_bytes: 64 * 1024 * 1024,
            cpu_millis: 100,
        },
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: false,
        environment: None,
        retain_artifacts: None,
        gateway: None,
        cpu_qos: None,
        restore: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit local service gate: just test-service"]
async fn controller_restart_preserves_worker_node_and_live_execution_identity() {
    let mut deployment = Deployment::start(true).await;
    deployment.client.submit(&task()).await.unwrap();
    let running = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = deployment.client.task("service-live").await.unwrap();
            if task.phase == TaskPhase::Running {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    let after = deployment
        .request(serde_json::json!({"op":"restart","role":"controller"}))
        .await
        .unwrap();
    assert!(after.get("error").is_none(), "{after}");
    assert_ne!(
        before["roles"]["controller"]["pid"],
        after["roles"]["controller"]["pid"]
    );
    assert_eq!(
        before["roles"]["node"]["pid"],
        after["roles"]["node"]["pid"]
    );
    assert_eq!(
        before["roles"]["worker:one"]["pid"],
        after["roles"]["worker:one"]["pid"]
    );
    let finished = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = deployment.client.task("service-live").await.unwrap();
            if task.phase.terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished.phase, TaskPhase::Succeeded, "{finished:?}");
    assert_eq!(finished.lease.unwrap().key, running.lease.unwrap().key);
    let status = deployment
        .request(serde_json::json!({"op":"status"}))
        .await
        .unwrap();
    assert!(status.to_string().find(ADMIN).is_none());
    assert!(status.to_string().find(WORKER).is_none());
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
#[ignore = "explicit FUSE backing gate, no VMs: just test-service"]
async fn node_pins_share_ram_across_stores_preserve_cow_and_fence_restart() {
    let mut deployment = Deployment::start(false).await;
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
    let controller = deployment
        .request(serde_json::json!({"op":"restart","role":"controller"}))
        .await
        .unwrap();
    assert_eq!(controller["roles"]["node"]["pid"], old_pid);
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
