#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
use pvisor_vm::api::RestoreState;
use std::{fs, io::Write};

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "test-boot".into(),
        build: "test-build".into(),
        firmware: "test-firmware".into(),
        profile: "test-profile".into(),
    }
}
fn bytes() -> Vec<u8> {
    let mut bytes = vec![0; 65536];
    bytes.extend((0..65536).map(|n| (n % 251) as u8));
    let mut seed = 123456789u64;
    bytes.extend((0..65536).map(|_| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as u8
    }));
    bytes.extend_from_slice(b"short tail");
    bytes
}
fn publish(
    store: &SnapshotStore,
    source: &std::path::Path,
    bytes: &[u8],
    compressed: bool,
) -> String {
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(bytes).unwrap();
    if compressed {
        pending
            .publish_compressed(source, b"machine", compatibility())
            .unwrap()
    } else {
        pending
            .publish(source, b"machine", compatibility())
            .unwrap()
    }
}

#[test]
#[ignore = "requires /dev/fuse or macFUSE; run with nextest --run-ignored only"]
fn fuse_faults_restore_ram_and_private_mappings_isolate_forks() {
    use pvisor_vm::api::{MachineRestore, MachineSnapshot};
    use std::sync::Arc;
    use vm_memory::{Bytes, GuestAddress};
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&directory.path().join("store")).unwrap();
        let bytes = bytes()[..3 * 65536].to_vec();
        let id = publish(&store, &source, &bytes, compressed);
        let published = store.open_for_restore(&id, &compatibility()).unwrap();
        // Exercise both public entry points with a legacy state-root hint.
        let legacy_parent = directory.path().join("ram-mounts");
        let (mount, file) = if compressed {
            published.ram_mount(
                std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")),
                &legacy_parent,
            )
        } else {
            pvisor::environment_snapshot::SnapshotRamMount::new(
                published.ram_reader().unwrap(),
                &legacy_parent,
            )
        }
        .unwrap();
        assert!(!legacy_parent.exists());
        #[cfg(target_os = "linux")]
        let mount_path = {
            use std::os::fd::AsRawFd;
            let path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
            let parent = path.parent().unwrap().to_path_buf();
            assert!(!parent.starts_with(directory.path()));
            parent
        };
        let restore = MachineRestore {
            ram_file: Arc::new(file),
            state: serde_json::from_value::<MachineSnapshot>(serde_json::json!({
                "version": 1, "cpus": [], "devices": [],
                "ram": [{ "base": 0, "len": bytes.len(), "file_offset": 0 }]
            }))
            .unwrap(),
        };
        let first = restore.map_ram(&[(GuestAddress(0), bytes.len())]).unwrap();
        let second = restore.map_ram(&[(GuestAddress(0), bytes.len())]).unwrap();
        drop(published);
        store.delete(&id).unwrap();
        store.collect_abandoned().unwrap();
        // First access happens after deleting the source environment.
        let mut output = vec![0; bytes.len()];
        first.read_slice(&mut output, GuestAddress(0)).unwrap();
        assert_eq!(output, bytes);
        first.write_slice(b"private", GuestAddress(65536)).unwrap();
        second.read_slice(&mut output, GuestAddress(0)).unwrap();
        assert_eq!(output, bytes);
        drop(first);
        drop(second);
        drop(restore);
        drop(mount);
        #[cfg(target_os = "linux")]
        assert!(fs::symlink_metadata(&mount_path).is_err());
        store.collect_abandoned().unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires /dev/fuse; exercises pager SIGKILL cleanup"]
fn fuse_mount_is_reaped_after_helper_is_killed() {
    use std::os::fd::AsRawFd;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let store = SnapshotStore::new(&directory.path().join("store")).unwrap();
    let id = publish(&store, &source, &bytes(), true);
    let published = store.open_for_restore(&id, &compatibility()).unwrap();
    let children = || -> Vec<i32> {
        fs::read_to_string("/proc/thread-self/children")
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect()
    };
    let before = children();
    let (mount, file) = published
        .ram_mount(
            std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")),
            directory.path(),
        )
        .unwrap();
    let path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
    let path = path.parent().unwrap();
    assert!(!path.starts_with(directory.path()));
    let helpers: Vec<_> = children()
        .into_iter()
        .filter(|pid| !before.contains(pid))
        .collect();
    assert_eq!(helpers.len(), 1);
    assert_eq!(unsafe { libc::kill(helpers[0], libc::SIGKILL) }, 0);
    drop(file);
    drop(mount); // Must detach even when the pager cannot handle EOF.
    assert!(fs::symlink_metadata(path).is_err());
    assert!(
        !fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(path.to_str().unwrap())
    );
    // Runtime cleanup must not remove the persistent publication or its RAM.
    store.open(&id, &compatibility()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires /dev/fuse; exercises runner SIGKILL cleanup"]
fn fuse_mount_is_reaped_after_runner_is_killed() {
    use std::{
        os::fd::AsRawFd,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    const ROLE: &str = "PVISOR_RAM_WATCHDOG_TEST";
    if let Some(directory) = std::env::var_os(ROLE) {
        let directory = std::path::PathBuf::from(directory);
        let source = directory.join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&directory.join("store")).unwrap();
        // One unique block makes the exact post-exit GC count deterministic.
        let id = publish(&store, &source, &[0; 65536], true);
        let published = store.open_for_restore(&id, &compatibility()).unwrap();
        let (_mount, file) = published
            .ram_mount(
                std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")),
                &directory,
            )
            .unwrap();
        drop(published);
        // Make content eligible for GC once the independent pager releases its
        // pin; a still-published snapshot correctly keeps it referenced forever.
        store.delete(&id).unwrap();
        let path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        fs::write(
            directory.join("ready"),
            path.parent().unwrap().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        loop {
            std::thread::park();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fuse_mount_is_reaped_after_runner_is_killed",
            "--ignored",
            "--nocapture",
        ])
        .env(ROLE, directory.path())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.path().join("ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "runner exited before readiness"
        );
        assert!(Instant::now() < deadline, "runner did not mount RAM");
        std::thread::sleep(Duration::from_millis(10));
    }
    let path =
        std::path::PathBuf::from(fs::read_to_string(directory.path().join("ready")).unwrap());
    assert!(
        !path.starts_with(directory.path()),
        "RAM mount was projected with source/store state"
    );
    child.kill().unwrap(); // SIGKILL bypasses all Rust cleanup in the runner.
    child.wait().unwrap();
    while fs::symlink_metadata(&path).is_ok()
        || fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(path.to_str().unwrap())
    {
        assert!(
            Instant::now() < deadline,
            "RAM mount survived runner SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let store = SnapshotStore::new(&directory.path().join("store")).unwrap();
    // Detach precedes server-thread teardown and release of its content pins.
    // Wait for that independent process, retaining the exact GC expectation.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let collected = store.collect_abandoned().unwrap();
        if collected != 0 {
            assert_eq!(collected, 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "crashed pager retained content pins"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
