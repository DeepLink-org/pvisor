//! Retired snapshot entry and native RAM helper regressions.
use std::process::Command;

#[test]
fn retired_snapshot_is_rejected_without_creating_a_store() {
    let temporary = tempfile::tempdir().unwrap();
    let store = temporary.path().join("must-not-be-created");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["snapshot", "--store"])
        .arg(&store)
        .args(["run", "--name", "a", "--rootfs", "/unused", "--", "bash"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("was removed"));
    assert!(!store.exists());
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn ram_watchdog_cleans_an_already_detached_private_directory() {
    let temp = tempfile::tempdir().unwrap();
    let mount = temp.path().join("ram-mount-watchdog-test");
    std::fs::create_dir(&mount).unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env(
            "PVISOR_VM_RESTORE_RAM_WATCHDOG",
            mount.canonicalize().unwrap(),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!mount.exists());
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
#[ignore = "real FUSE pager gate, no VMs"]
fn external_ram_pager_survives_object_deletion_without_snapshot_cli() {
    use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
    use std::{
        fs,
        io::{Read, Write},
    };
    for compressed in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&temporary.path().join("store")).unwrap();
        let compatibility = Compatibility {
            host_boot: "test".into(),
            build: "test".into(),
            firmware: "test".into(),
            profile: "test".into(),
        };
        let bytes: Vec<_> = (0..131072).map(|offset| (offset % 251) as u8).collect();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(&bytes).unwrap();
        let id = if compressed {
            pending
                .publish_compressed(&source, b"machine", compatibility.clone())
                .unwrap()
        } else {
            pending
                .publish(&source, b"machine", compatibility.clone())
                .unwrap()
        };
        let published = store.open_for_restore(&id, &compatibility).unwrap();
        let mounts = temporary.path().join("mounts");
        fs::create_dir(&mounts).unwrap();
        let (mount, mut file) = published
            .ram_mount(std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")), &mounts)
            .unwrap();
        drop(published);
        store.delete(&id).unwrap();
        store.collect_abandoned().unwrap();
        let mut output = Vec::new();
        file.read_to_end(&mut output).unwrap();
        assert_eq!(output, bytes);
        drop(file);
        drop(mount);
        assert_eq!(fs::read_dir(&mounts).unwrap().count(), 0);
    }
}
