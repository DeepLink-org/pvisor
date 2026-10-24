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

/// Uses an explicitly provided, signed CLI and a native Linux rootfs. Keep the
/// pool and both containers owned even when an assertion fails.
#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[ignore = "requires HVF, a signed PVISOR_MEMORY_POOL_TEST_PVISOR and PVISOR_DOCKER_TEST_ROOTFS"]
fn macos_container_vms_publish_and_restore_through_daemon_pool() {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };
    let binary = PathBuf::from(
        std::env::var_os("PVISOR_MEMORY_POOL_TEST_PVISOR").expect("set signed CLI path"),
    );
    let rootfs = PathBuf::from(
        std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS").expect("set native Linux rootfs"),
    );
    let (state, mut service, socket, _) = pool_service(PoolConfig::default());
    ensure_service(
        state.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon")),
        PoolConfig::default(),
    )
    .unwrap();
    let bases = socket.parent().unwrap().join("shared-kernel");
    let home = state.path().join("containers");
    pvisor::docker::Store::open(home.clone()).unwrap();
    let image = pvisor::docker::Image {
        reference: "fixture:latest".into(),
        id: format!("sha256:{}", "1".repeat(64)),
        digest: format!("sha256:{}", "2".repeat(64)),
        platform: "linux/arm64".into(),
        rootfs: rootfs.canonicalize().unwrap(),
        cache: None,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
        cwd: "/".into(),
        user: String::new(),
        stop_signal: 15,
        volumes: vec![],
        created_ms: 0,
    };
    fs::write(
        home.join("images.json"),
        serde_json::to_vec(&[image]).unwrap(),
    )
    .unwrap();
    struct Containers<'a>(&'a Path, &'a Path);
    impl Drop for Containers<'_> {
        fn drop(&mut self) {
            let _ = Command::new(self.0)
                .env("PVISOR_DOCKER_HOME", self.1)
                .args(["rm", "-f", "pool-a", "pool-b", "base-only"])
                .output();
        }
    }
    let _containers = Containers(&binary, &home);
    let call = |args: &[&str]| {
        let output = Command::new(&binary)
            .env("PVISOR_DOCKER_HOME", &home)
            .env("PVISOR_EXPERIMENTAL_MEMORY_METRICS", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    for name in ["pool-a", "pool-b"] {
        call(&[
            "run",
            "-d",
            "--name",
            name,
            "--cpus",
            "1",
            "-m",
            "128m",
            "--network",
            "none",
            "--memory-pool",
            socket.to_str().unwrap(),
            "--shared-kernel-base",
            bases.to_str().unwrap(),
            "fixture",
            "/bin/sh",
            "-c",
            "test -f /marker || echo retained >/marker; exec sleep 120",
        ]);
    }
    let store = pvisor::docker::Store::open(home.clone()).unwrap();
    let files: Vec<_> = fs::read_dir(&bases)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "same firmware must use one kernel inode");
    let original = fs::read(&files[0]).unwrap();
    let has_shared_cold = |name: &str| {
        let instance = store.resolve(name).unwrap();
        let log = fs::read_to_string(home.join("instances").join(&instance.id).join("stderr.log"))
            .unwrap_or_default();
        log.lines().any(|line| {
            line.starts_with("pvisor-cold-sample ")
                && ["cold_bytes=", "cross_session_objects="].iter().all(|key| {
                    line.split_whitespace()
                        .find_map(|part| part.strip_prefix(key))
                        .and_then(|v| v.parse::<u64>().ok())
                        .is_some_and(|v| v > 0)
                })
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !has_shared_cold("pool-a") || !has_shared_cold("pool-b") {
        assert!(
            Instant::now() < deadline,
            "both VMs must report cold bytes and cross-session shared objects"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = call(&["exec", "pool-a", "/bin/cat", "/marker"]);
    assert_eq!(output.stdout, b"retained\n");
    call(&["exec", "pool-a", "/bin/sh", "-c", "echo private >/marker"]);
    assert_eq!(
        call(&["exec", "pool-b", "/bin/cat", "/marker"]).stdout,
        b"retained\n"
    );
    call(&["stop", "pool-a"]);
    call(&["start", "pool-a"]);
    assert_eq!(
        store.resolve("pool-a").unwrap().config.memory_pool,
        Some(socket)
    );
    assert_eq!(
        store.resolve("pool-a").unwrap().config.shared_kernel_base,
        Some(bases.clone())
    );
    assert_eq!(
        call(&["exec", "pool-a", "/bin/cat", "/marker"]).stdout,
        b"private\n"
    );
    call(&[
        "run",
        "-d",
        "--name",
        "base-only",
        "--cpus",
        "1",
        "-m",
        "128m",
        "--network",
        "none",
        "--shared-kernel-base",
        bases.to_str().unwrap(),
        "fixture",
        "/bin/sh",
        "-c",
        "echo independent >/marker; exec sleep 120",
    ]);
    assert_eq!(
        fs::read(&files[0]).unwrap(),
        original,
        "guest boot/exec must not modify shared kernel"
    );
    service.0.kill().unwrap();
    service.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ["pool-a", "pool-b"].iter().all(|name| {
            let instance = store.resolve(name).unwrap();
            instance.state == "exited" && instance.exit_code.is_some_and(|code| code != 0)
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pool loss must terminate dependent VMs"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let unavailable = Command::new(&binary)
        .env("PVISOR_DOCKER_HOME", &home)
        .args(["start", "pool-a"])
        .output()
        .unwrap();
    assert!(
        !unavailable.status.success(),
        "start must not silently abandon the saved pool"
    );
    assert_eq!(
        call(&["exec", "base-only", "/bin/cat", "/marker"]).stdout,
        b"independent\n"
    );
    fs::remove_file(&files[0]).unwrap();
    assert_eq!(
        call(&["exec", "base-only", "/bin/cat", "/marker"]).stdout,
        b"independent\n"
    );
}

/// Static native acceptance: four real Linux guests, each configured for 512 MiB.
/// Inventories identify resident mapped backing, not PFNs or whole-host cost.
#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[ignore = "requires signed CLI, native rootfs and private PVISOR_SHARED_KERNEL_TEST_OUTPUT"]
fn macos_four_512m_containers_share_resident_kernel_pages() {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };
    let binary = PathBuf::from(
        std::env::var_os("PVISOR_MEMORY_POOL_TEST_PVISOR").expect("set signed CLI path"),
    );
    let rootfs = PathBuf::from(
        std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS").expect("set native Linux rootfs"),
    )
    .canonicalize()
    .unwrap();
    let output = PathBuf::from(
        std::env::var_os("PVISOR_SHARED_KERNEL_TEST_OUTPUT")
            .expect("set private evidence directory"),
    );
    let metadata = fs::symlink_metadata(&output).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
    );
    let mut results = Vec::new();
    // The same binary and workload; separate fresh groups, no cold compression.
    for shared in [false, true] {
        let group = output.join(if shared { "shared" } else { "private" });
        fs::create_dir(&group).unwrap();
        fs::set_permissions(&group, fs::Permissions::from_mode(0o700)).unwrap();
        let home = group.join("containers");
        let ram = group.join("ram");
        let process = group.join("process");
        let bases = group.join("bases");
        for directory in [&ram, &process, &bases] {
            fs::create_dir(directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = pvisor::docker::Store::open(home.clone()).unwrap();
        let image = pvisor::docker::Image {
            reference: "fixture:latest".into(),
            id: format!("sha256:{}", "1".repeat(64)),
            digest: format!("sha256:{}", "2".repeat(64)),
            platform: "linux/arm64".into(),
            rootfs: rootfs.clone(),
            cache: None,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
            cwd: "/".into(),
            user: String::new(),
            stop_signal: 15,
            volumes: vec![],
            created_ms: 0,
        };
        fs::write(
            home.join("images.json"),
            serde_json::to_vec(&[image]).unwrap(),
        )
        .unwrap();
        struct Containers<'a>(&'a std::path::Path, &'a std::path::Path);
        impl Drop for Containers<'_> {
            fn drop(&mut self) {
                let _ = Command::new(self.0)
                    .env("PVISOR_DOCKER_HOME", self.1)
                    .args(["rm", "-f", "vm-0", "vm-1", "vm-2", "vm-3"])
                    .output();
            }
        }
        let _containers = Containers(&binary, &home);
        let call = |args: &[&str]| {
            let result = Command::new(&binary)
                .env("PVISOR_DOCKER_HOME", &home)
                .env("PVISOR_EXPERIMENTAL_MEMORY_PAGE_INVENTORY", &ram)
                .env("PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY", &process)
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            result.stdout
        };
        let names = ["vm-0", "vm-1", "vm-2", "vm-3"];
        // BusyBox awk retains approximately 64 MiB of independently allocated
        // nonzero strings. Each guest also has private filesystem state.
        let workload = "echo $INSTANCE_TOKEN >/marker; exec awk 'BEGIN {s=sprintf(\"%4096s\",\"x\"); for(i=0;i<16384;i++) a[i]=i s; print \"PAYLOAD_READY\"; fflush(); system(\"sleep 180\"); if(length(a[1111])<4096)exit 1}'";
        for name in names {
            let token = format!("INSTANCE_TOKEN={name}");
            let mut args = vec![
                "run",
                "-d",
                "--name",
                name,
                "--cpus",
                "1",
                "-m",
                "512m",
                "--network",
                "none",
                "-e",
                &token,
            ];
            if shared {
                args.extend(["--shared-kernel-base", bases.to_str().unwrap()]);
            }
            args.extend(["fixture", "/bin/sh", "-c", workload]);
            call(&args);
            assert_eq!(store.resolve(name).unwrap().config.memory_mib, 512);
        }
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let ready = names.iter().all(|name| {
                let instance = store.resolve(name).unwrap();
                fs::read_to_string(home.join("instances").join(instance.id).join("stdout.log"))
                    .unwrap_or_default()
                    .contains("PAYLOAD_READY")
            });
            if ready
                && fs::read_dir(&ram)
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .is_ok_and(|e| e.path().extension().is_some_and(|s| s == "json"))
                    })
                    .count()
                    == 4
                && fs::read_dir(&process)
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .is_ok_and(|e| e.path().extension().is_some_and(|s| s == "json"))
                    })
                    .count()
                    == 4
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "four workload/inventory readiness deadline; evidence: {}",
                group.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut kernel_refs: BTreeMap<(u64, u64), BTreeSet<u64>> = BTreeMap::new();
        let mut process_refs = BTreeSet::new();
        let mut process_resident = 0;
        let mut kernel_resident = 0;
        let mut page_bytes = 0;
        let mut pids = Vec::new();
        for entry in fs::read_dir(&ram).unwrap() {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            let pid = value["pid"].as_u64().unwrap();
            page_bytes = value["page_bytes"].as_u64().unwrap();
            assert_eq!(value["pending_file_bytes_upper"], 0);
            pids.push(pid);
            for row in value["pages"].as_array().unwrap() {
                if row[3].as_u64().unwrap() & 1 == 0 {
                    continue;
                }
                let key = (row[1].as_u64().unwrap(), row[2].as_u64().unwrap());
                assert_ne!(key.0, 0);
                kernel_refs.entry(key).or_default().insert(pid);
                kernel_resident += page_bytes;
            }
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(process.join(format!("{pid}.json"))).unwrap())
                    .unwrap();
            assert_eq!(value["page_bytes"], page_bytes);
            for row in value["pages"].as_array().unwrap() {
                if row[3].as_u64().unwrap() & 1 == 0 {
                    continue;
                }
                let key = (row[1].as_u64().unwrap(), row[2].as_u64().unwrap());
                assert_ne!(key.0, 0, "cannot deduplicate unknown identities");
                process_refs.insert(key);
                process_resident += page_bytes;
            }
            let map = Command::new("/usr/bin/vmmap")
                .args(["-summary", &pid.to_string()])
                .output()
                .unwrap();
            fs::write(
                group.join(format!("vmmap-{pid}.txt")),
                [&map.stdout[..], &map.stderr[..]].concat(),
            )
            .unwrap();
        }
        pids.sort_unstable();
        pids.dedup();
        assert_eq!(pids.len(), 4, "four independent VMM processes");
        let common_pages = kernel_refs
            .values()
            .filter(|owners| owners.len() == 4)
            .count() as u64;
        let duplicated_kernel_bytes = kernel_refs
            .values()
            .map(|owners| owners.len().saturating_sub(1) as u64 * page_bytes)
            .sum::<u64>();
        if shared {
            assert!(
                common_pages > 0 && duplicated_kernel_bytes > 0,
                "real resident kernel sharing required"
            );
            let files: Vec<_> = fs::read_dir(&bases)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(files.len(), 1);
            let original = fs::read(&files[0]).unwrap();
            for name in names {
                assert_eq!(
                    call(&["exec", name, "/bin/cat", "/marker"]),
                    format!("{name}\n").as_bytes()
                );
                let mutated = format!("echo changed-{name} >/marker");
                call(&["exec", name, "/bin/sh", "-c", &mutated]);
                for peer in names.into_iter().filter(|peer| *peer > name) {
                    assert_eq!(
                        call(&["exec", peer, "/bin/cat", "/marker"]),
                        format!("{peer}\n").as_bytes()
                    );
                }
            }
            assert_eq!(fs::read(&files[0]).unwrap(), original);
            call(&["rm", "-f", "vm-0"]);
            fs::remove_file(&files[0]).unwrap();
            for name in &names[1..] {
                assert_eq!(
                    call(&["exec", name, "/bin/cat", "/marker"]),
                    format!("changed-{name}\n").as_bytes()
                );
            }
        }
        results.push(serde_json::json!({"shared":shared,"vms":4,"memory_mib_per_vm":512,"cpus_per_vm":1,
            "pids":pids,"host_page_bytes":page_bytes,"common_kernel_pages_all_four":common_pages,
            "kernel_resident_reference_bytes":kernel_resident,"kernel_unique_resident_bytes":kernel_refs.len() as u64*page_bytes,
            "kernel_duplicate_resident_reference_bytes":duplicated_kernel_bytes,
            "process_resident_reference_bytes":process_resident,"process_unique_resident_backing_bytes":process_refs.len() as u64*page_bytes,
            "scope":"one static group; mapped resident object+offset identities, not PFNs, PSS, whole-host memory, or density; includes diagnostic allocations"}));
    }
    let summary = serde_json::json!({"groups":results,"integrity":"passed"});
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("PVISOR_FOUR_VM_RESULT {summary}");
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[ignore = "requires signed CLI, rootfs, static guest probe and private evidence directory"]
fn macos_four_512m_vms_dynamically_share_and_cow_application_pages() {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };
    let binary =
        PathBuf::from(std::env::var_os("PVISOR_MEMORY_POOL_TEST_PVISOR").expect("signed CLI"));
    let rootfs =
        PathBuf::from(std::env::var_os("PVISOR_DOCKER_TEST_ROOTFS").expect("native rootfs"))
            .canonicalize()
            .unwrap();
    let guest =
        PathBuf::from(std::env::var_os("PVISOR_SHARED_TEST_GUEST").expect("static guest probe"));
    let output = PathBuf::from(
        std::env::var_os("PVISOR_SHARED_KERNEL_TEST_OUTPUT").expect("private evidence directory"),
    );
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(&output).unwrap();
    assert!(m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0);
    let (_state, _service, socket, _) = pool_service(PoolConfig {
        mode: pvisor_daemon::memory_pool::PoolMode::PhysicalPages,
        ..Default::default()
    });
    let home = output.join("containers");
    let ram = output.join("ram");
    for directory in [&ram, &output.join("controls")] {
        fs::create_dir(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let store = pvisor::docker::Store::open(home.clone()).unwrap();
    let image = pvisor::docker::Image {
        reference: "fixture:latest".into(),
        id: format!("sha256:{}", "1".repeat(64)),
        digest: format!("sha256:{}", "2".repeat(64)),
        platform: "linux/arm64".into(),
        rootfs,
        cache: None,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
        cwd: "/".into(),
        user: String::new(),
        stop_signal: 15,
        volumes: vec![],
        created_ms: 0,
    };
    fs::write(
        home.join("images.json"),
        serde_json::to_vec(&[image]).unwrap(),
    )
    .unwrap();
    let names = ["vm-0", "vm-1", "vm-2", "vm-3"];
    let physical = std::env::var_os("PVISOR_SHARED_TEST_CONTROL").is_none();
    struct Containers<'a>(
        &'a std::path::Path,
        &'a std::path::Path,
        &'a std::path::Path,
    );
    impl Drop for Containers<'_> {
        fn drop(&mut self) {
            if let Ok(entries) = fs::read_dir(self.1.join("instances")) {
                for entry in entries.flatten() {
                    let _ = fs::copy(
                        entry.path().join("stderr.log"),
                        self.2.join(format!(
                            "stderr-{}.log",
                            entry.file_name().to_string_lossy()
                        )),
                    );
                }
            }
            let _ = Command::new(self.0)
                .env("PVISOR_DOCKER_HOME", self.1)
                .args(["rm", "-f", "vm-0", "vm-1", "vm-2", "vm-3"])
                .output();
        }
    }
    let _containers = Containers(&binary, &home, &output);
    let call = |args: &[&str]| {
        let result = Command::new(&binary)
            .env("PVISOR_DOCKER_HOME", &home)
            .env("PVISOR_EXPERIMENTAL_MEMORY_METRICS", "1")
            .env("PVISOR_EXPERIMENTAL_MEMORY_PAGE_INVENTORY", &ram)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    };
    for (index, name) in names.iter().enumerate() {
        let control = output.join("controls").join(name);
        fs::create_dir(&control).unwrap();
        fs::copy(&guest, control.join("probe")).unwrap();
        fs::set_permissions(control.join("probe"), fs::Permissions::from_mode(0o755)).unwrap();
        let volume = format!("{}:/control", control.display());
        let token = (index + 1).to_string();
        let mut arguments = vec![
            "run",
            "-d",
            "--name",
            name,
            "--cpus",
            "1",
            "-m",
            "512m",
            "--network",
            "none",
            "-v",
            &volume,
            "fixture",
            "/control/probe",
            &token,
        ];
        if physical {
            arguments.splice(1..1, ["--memory-pool", socket.to_str().unwrap()]);
        }
        call(&arguments);
        assert_eq!(store.resolve(name).unwrap().config.memory_mib, 512);
    }
    let wait = |condition: &dyn Fn() -> bool, label: &str| {
        let deadline = Instant::now() + Duration::from_secs(90);
        while !condition() {
            for name in names {
                assert_eq!(
                    store.resolve(name).unwrap().state,
                    "running",
                    "{label}: {name} exited; see {}",
                    output.display()
                );
            }
            assert!(
                Instant::now() < deadline,
                "{label} timed out; see {}",
                output.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    wait(
        &|| {
            names.iter().all(|name| {
                fs::read_to_string(output.join("controls").join(name).join("result"))
                    .unwrap_or_default()
                    == "0 ready"
            })
        },
        "payload ready",
    );
    let shared_bytes = |name: &str| {
        let instance = store.resolve(name).unwrap();
        fs::read_to_string(home.join("instances").join(instance.id).join("stderr.log"))
            .unwrap_or_default()
            .lines()
            .rev()
            .find(|line| line.starts_with("pvisor-shared-sample "))
            .and_then(|line| {
                line.split_whitespace()
                    .find_map(|part| part.strip_prefix("shared_bytes="))
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .unwrap_or(0)
    };
    if physical {
        wait(
            &|| names.iter().all(|name| shared_bytes(name) > 1024 * 1024),
            "all four participate in sharing",
        );
    }
    wait(
        &|| {
            fs::read_dir(&ram)
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.path().extension().is_some_and(|s| s == "json"))
                })
                .count()
                == 4
        },
        "four native inventories",
    );
    let mut references: BTreeMap<(u64, u64), BTreeSet<u64>> = BTreeMap::new();
    let mut resident = 0u64;
    let mut paged_out = 0u64;
    let mut footprint_proxy = Vec::new();
    for entry in fs::read_dir(&ram).unwrap() {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
        assert_eq!(value["page_bytes"], 16384);
        let pid = value["pid"].as_u64().unwrap();
        for row in value["pages"].as_array().unwrap() {
            if row[3].as_u64().unwrap() & 16 != 0 {
                paged_out += 16384;
            }
            if row[3].as_u64().unwrap() & 1 == 0 {
                continue;
            }
            resident += 16384;
            references
                .entry((row[1].as_u64().unwrap(), row[2].as_u64().unwrap()))
                .or_default()
                .insert(pid);
        }
        let map = Command::new("/usr/bin/vmmap")
            .args(["-summary", &pid.to_string()])
            .output()
            .unwrap();
        footprint_proxy.push(
            String::from_utf8_lossy(&map.stdout)
                .lines()
                .find(|line| line.starts_with("Physical footprint:"))
                .unwrap_or("unavailable")
                .to_owned(),
        );
        fs::write(
            output.join(format!("vmmap-{pid}.txt")),
            [&map.stdout[..], &map.stderr[..]].concat(),
        )
        .unwrap();
    }
    let common = references
        .values()
        .filter(|owners| owners.len() == 4)
        .count() as u64
        * 16384;
    let duplicates = references
        .values()
        .map(|owners| owners.len().saturating_sub(1) as u64 * 16384)
        .sum::<u64>();
    if physical {
        assert!(
            common > 0 && duplicates > 32 * 1024 * 1024,
            "resident application-size sharing required: common={common} duplicates={duplicates}"
        );
    }
    let command = |name: &str, sequence: u32, operation: &str| {
        let control = output.join("controls").join(name);
        fs::write(control.join("pending"), format!("{sequence} {operation}")).unwrap();
        fs::rename(control.join("pending"), control.join("command")).unwrap();
    };
    let verify = |sequence: u32| {
        wait(
            &|| {
                names.iter().all(|name| {
                    fs::read_to_string(output.join("controls").join(name).join("result"))
                        .unwrap_or_default()
                        .starts_with(&format!("{sequence} passed "))
                })
            },
            "complete payload verification",
        );
    };
    let mut observer = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let initial_pool_bytes = observer.stats().unwrap().encoded_bytes;
    let initial: Vec<_> = names.iter().map(|name| shared_bytes(name)).collect();
    for name in names {
        command(name, 1, "read");
    }
    verify(1);
    command("vm-0", 2, "quarter");
    for name in &names[1..] {
        command(name, 2, "read");
    }
    verify(2);
    for name in names {
        command(name, 3, "all");
    }
    verify(3);
    // Every VM now holds a different nonzero 64 MiB payload. Full byte checks
    // above include its own writes and peer isolation, not just stored hashes.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let after: Vec<_> = names.iter().map(|name| shared_bytes(name)).collect();
    let first = store.resolve("vm-0").unwrap();
    fs::copy(
        home.join("instances").join(first.id).join("stderr.log"),
        output.join("stderr-vm-0.log"),
    )
    .unwrap();
    call(&["rm", "-f", "vm-0"]);
    for name in &names[1..] {
        command(name, 4, "read");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    for name in &names[1..] {
        while !fs::read_to_string(output.join("controls").join(name).join("result"))
            .unwrap_or_default()
            .starts_with("4 passed ")
        {
            assert!(
                Instant::now() < deadline,
                "surviving VM must retain full private contents"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let pool_stats = observer.stats().unwrap();
    let summary = serde_json::json!({"physical_sharing":physical,"ram_paged_out_reference_bytes":paged_out,"footprint_proxy_per_vm":footprint_proxy,"initial_pool_payload_bytes":initial_pool_bytes,"pool_payload_bytes_after_writes":pool_stats.encoded_bytes,"vms":4,"memory_mib_per_vm":512,"application_bytes_per_vm":64*1024*1024,"common_resident_backing_bytes_all_four":common,
        "ram_resident_reference_bytes":resident,"ram_unique_resident_backing_bytes":references.len() as u64*16384,"duplicate_resident_reference_bytes":duplicates,
        "initial_pinned_bytes_per_vm":initial,"after_full_private_writes_pinned_bytes_per_vm":after,"integrity":"full-byte read, quarter/all private write, peer and exit passed",
        "scope":"static diagnostic, native resident object+offset identity; not whole-host cost or a production density estimate"});
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("PVISOR_DYNAMIC_FOUR_VM_RESULT {summary}");
}

#[test]
fn daemon_pool_shares_objects_and_survives_owner_reopen_without_replacement() {
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
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
    let socket = directory.canonicalize().unwrap().join("pool.sock");
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

fn pool_service(
    config: PoolConfig,
) -> (
    tempfile::TempDir,
    OwnedChild,
    std::path::PathBuf,
    PoolConfig,
) {
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = state.path().join("memory-pool");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let mut child = OwnedChild(
        Command::new(
            std::env::var_os("PVISOR_MEMORY_POOL_TEST_DAEMON")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_pvisor-daemon").into()),
        )
        .args(["memory-pool", "--directory"])
        .arg(&directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap(),
    );
    let socket = directory.canonicalize().unwrap().join("pool.sock");
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
fn compressed_pool() -> (
    tempfile::TempDir,
    OwnedChild,
    std::path::PathBuf,
    PoolConfig,
) {
    pool_service(PoolConfig {
        mode: pvisor_daemon::memory_pool::PoolMode::Compressed,
        max_bytes: 8192,
        max_objects: 2,
        max_connections: 8,
        max_references: 2,
    })
}

#[test]
fn standalone_manager_reuses_pool_preserves_it_on_shutdown_and_reports_pool_loss() {
    standalone_manager_lifecycle(PoolConfig::default());
}

#[test]
fn physical_pages_manager_reuses_owner_and_preserves_exit_contract() {
    standalone_manager_lifecycle(PoolConfig {
        mode: pvisor_daemon::memory_pool::PoolMode::PhysicalPages,
        ..Default::default()
    });
}

fn standalone_manager_lifecycle(config: PoolConfig) {
    let mode = serde_json::to_value(config.mode)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    let (state, mut pool, socket, _) = pool_service(config.clone());
    let ready = state.path().join("manager.json");
    let start = || {
        OwnedChild(
            Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
                .args(["serve", "--memory-pool", "--state"])
                .arg(state.path())
                .args(["--memory-pool-mode", &mode])
                .env_remove("OPEN_SANDBOX_API_KEY")
                .stdout(fs::File::create(&ready).unwrap())
                .spawn()
                .unwrap(),
        )
    };
    let wait_ready = |manager: &mut OwnedChild| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                manager.0.try_wait().unwrap().is_none(),
                "standalone manager failed"
            );
            if let Ok(value) =
                serde_json::from_slice::<serde_json::Value>(&fs::read(&ready).unwrap())
            {
                assert_eq!(value["memory_pool"], socket.to_string_lossy().as_ref());
                assert_eq!(value["ready"], true);
                break;
            }
            assert!(Instant::now() < deadline, "manager readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let wait_exit = |manager: &mut OwnedChild, success: bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = manager.0.try_wait().unwrap() {
                assert_eq!(status.success(), success, "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "manager exit deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let mut manager = start();
    wait_ready(&mut manager);
    assert_eq!(
        unsafe { libc::kill(manager.0.id() as i32, libc::SIGTERM) },
        0
    );
    wait_exit(&mut manager, true);
    assert!(
        pool.0.try_wait().unwrap().is_none(),
        "manager stopped live RAM owner"
    );
    let mut client = PoolClient::new(
        UnixStream::connect(&socket).unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    client.stats().unwrap();
    let mut manager = start();
    wait_ready(&mut manager);
    pool.0.kill().unwrap();
    pool.0.wait().unwrap();
    wait_exit(&mut manager, false);
    assert!(
        ensure_service(
            state.path(),
            std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon")),
            config
        )
        .is_err()
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_compressed_pool_shares_bounds_and_fails_closed_after_service_loss() {
    let config = PoolConfig {
        mode: pvisor_daemon::memory_pool::PoolMode::Compressed,
        max_bytes: 8192,
        max_objects: 1,
        max_connections: 8,
        max_references: 2,
    };
    let (state, mut service, socket, config) = pool_service(config);
    let connect = || {
        PoolClient::new(
            UnixStream::connect(&socket).unwrap(),
            Duration::from_secs(2),
        )
        .unwrap()
    };
    let (mut a, mut b) = (connect(), connect());
    let bytes = vec![7; 65536];
    let first = a.put(&bytes).unwrap();
    let second = b.put(&bytes).unwrap();
    let stats = a.stats().unwrap();
    assert_eq!(stats.objects, 1);
    assert_eq!(stats.cross_session_objects, 1);
    assert!(stats.encoded_bytes < bytes.len() as u64);
    assert!(
        a.put(&vec![8; 65536]).is_err(),
        "object budget must reject unique content"
    );
    assert_eq!(
        ensure_service(
            state.path(),
            std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon")),
            config.clone()
        )
        .unwrap(),
        socket
    );
    let mut output = vec![0; bytes.len()];
    a.release(first).unwrap();
    b.restore(&second, &mut output).unwrap();
    assert_eq!(output, bytes);
    service.0.kill().unwrap();
    service.0.wait().unwrap();
    assert!(
        b.restore(&second, &mut output).is_err(),
        "legacy macOS restore needs its live owner"
    );
    assert!(
        ensure_service(
            state.path(),
            std::path::Path::new(env!("CARGO_BIN_EXE_pvisor-daemon")),
            config
        )
        .is_err()
    );
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
