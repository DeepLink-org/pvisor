//! Public prepared-base ownership, authentication and independent-process leases.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
use pvisor::RunExecutor;
use pvisor::environment_snapshot::{BaseReference, SnapshotStore};
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
use pvisor_vm::api::RuntimeSupport;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, symlink},
    process::{Command, Stdio},
};

#[test]
fn prepared_rootfs_survives_preparer_exit_and_gc_without_rescanning_source() {
    const ROLE: &str = "PVISOR_PREPARED_ROOTFS_PREPARER";
    if let Some(root) = std::env::var_os(ROLE) {
        let root = std::path::Path::new(&root);
        let store = SnapshotStore::new(&root.join("store")).unwrap();
        let base = store.prepare_rootfs(&root.join("source")).unwrap();
        fs::write(
            root.join("base.json"),
            serde_json::to_vec(base.reference()).unwrap(),
        )
        .unwrap();
        // Abrupt process exit closes the lease without calling Rust Drop.
        std::process::exit(0);
    }
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("source")).unwrap();
    fs::write(temp.path().join("source/file"), b"original").unwrap();
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "prepared_rootfs_survives_preparer_exit_and_gc_without_rescanning_source",
                "--nocapture"
            ])
            .env(ROLE, temp.path())
            .status()
            .unwrap()
            .success()
    );
    let reference: BaseReference =
        serde_json::from_slice(&fs::read(temp.path().join("base.json")).unwrap()).unwrap();
    fs::remove_dir_all(temp.path().join("source")).unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    store.collect_abandoned().unwrap();
    let base = store.open_prepared_rootfs(&reference).unwrap();
    assert_eq!(fs::read(base.root().join("file")).unwrap(), b"original");
    let marker = temp
        .path()
        .join("store/bases")
        .join(&reference.id)
        .join("reference");
    assert_eq!(fs::metadata(&marker).unwrap().nlink(), 2);
    store.retain_prepared_rootfs(&base).unwrap();
    store.retain_prepared_rootfs(&base).unwrap();
    assert_eq!(
        fs::metadata(&marker).unwrap().nlink(),
        2,
        "idempotent retain cannot leak pins"
    );
    let other = SnapshotStore::new(&temp.path().join("other")).unwrap();
    assert!(other.retain_prepared_rootfs(&base).is_err());
    // A warm open authenticates small records; full content audit is explicit.
    fs::remove_file(
        temp.path()
            .join("store/bases")
            .join(&reference.id)
            .join("inventory.json"),
    )
    .unwrap();
    assert!(store.open_prepared_rootfs(&reference).is_ok());
    assert!(base.verify().is_err());
}

#[test]
fn prepared_rootfs_rejects_unpinned_substituted_symlinked_and_overlapping_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"data").unwrap();
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    assert!(store.prepare_rootfs(temp.path()).is_err());
    assert!(store.prepare_rootfs(&root).is_err());
    let base = store.import_base(&source).unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    store.retain_prepared_rootfs(&base).unwrap();
    let entry = root.join("prepared-rootfs").join(&base.reference().id);
    let marker = entry.join("reference");
    fs::remove_file(&marker).unwrap();
    fs::write(&marker, b"fake").unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    assert!(store.retain_prepared_rootfs(&base).is_err());
    fs::remove_file(&marker).unwrap();
    symlink(
        root.join("bases")
            .join(&base.reference().id)
            .join("reference"),
        &marker,
    )
    .unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    fs::remove_file(&marker).unwrap();
    fs::hard_link(
        root.join("bases")
            .join(&base.reference().id)
            .join("reference"),
        &marker,
    )
    .unwrap();
    let saved = fs::read(entry.join("record.json")).unwrap();
    fs::write(entry.join("record.json"), b"{\"version\":2}").unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    fs::write(entry.join("record.json"), saved).unwrap();
    let moved = root.join("saved-entry");
    fs::rename(&entry, &moved).unwrap();
    symlink(&moved, &entry).unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    assert!(
        store
            .open_prepared_rootfs(&BaseReference {
                id: "../escape".into()
            })
            .is_err()
    );
}

#[test]
fn prepared_rootfs_lease_is_independent_in_another_process() {
    const ROLE: &str = "PVISOR_PREPARED_ROOTFS_READER";
    if let Some(root) = std::env::var_os(ROLE) {
        let root = std::path::Path::new(&root);
        let reference = serde_json::from_slice(&fs::read(root.join("base.json")).unwrap()).unwrap();
        let store = SnapshotStore::new(&root.join("store")).unwrap();
        let _base = store.open_prepared_rootfs(&reference).unwrap();
        println!("LEASE_READY");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("source")).unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    let base = store.prepare_rootfs(&temp.path().join("source")).unwrap();
    let reference = base.reference().clone();
    fs::write(
        temp.path().join("base.json"),
        serde_json::to_vec(&reference).unwrap(),
    )
    .unwrap();
    drop(base);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "prepared_rootfs_lease_is_independent_in_another_process",
            "--nocapture",
        ])
        .env(ROLE, temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before acquiring lease"
        );
        if line.trim() == "LEASE_READY" {
            break;
        }
    }
    // Fault injection: losing a persistent catalog pin must not defeat an
    // already acquired independent process lease. This is not a removal API.
    fs::remove_file(
        temp.path()
            .join("store/prepared-rootfs")
            .join(&reference.id)
            .join("reference"),
    )
    .unwrap();
    store.collect_abandoned().unwrap();
    assert!(store.open_base(&reference).is_ok());
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(child.wait().unwrap().success());
    store.collect_abandoned().unwrap();
    assert!(store.open_base(&reference).is_err());
}

#[test]
fn prepared_rootfs_selector_roundtrips_and_legacy_defaults_stay_unowned() {
    let legacy: pvisor::VmSettings = serde_json::from_str("{}").unwrap();
    assert!(legacy.prepared_rootfs.is_none());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("prepared_rootfs")
            .is_none()
    );
    let selector = pvisor::PreparedRootfs {
        store: "/private/owned-store".into(),
        base: BaseReference { id: "a".repeat(64) },
    };
    let encoded = serde_json::to_string(&selector).unwrap();
    assert_eq!(
        serde_json::from_str::<pvisor::PreparedRootfs>(&encoded).unwrap(),
        selector
    );
    assert!(
        serde_json::from_str::<pvisor::PreparedRootfs>("{\"store\":\"/tmp\",\"immutable\":true}")
            .is_err()
    );
}

#[test]
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn sdk_executor_consumes_the_prepared_selector_and_rejects_source_override() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    let base = store.prepare_rootfs(&source).unwrap();
    let mut settings = pvisor::VmSettings {
        prepared_rootfs: Some(pvisor::PreparedRootfs {
            store: temp.path().join("store"),
            base: base.reference().clone(),
        }),
        ..Default::default()
    };
    if pvisor_vm::api::VmPlatform::embedded_kernel().is_none() {
        fs::write(
            temp.path()
                .join(pvisor_vm::api::VmPlatform::firmware_name()),
            b"fixture firmware",
        )
        .unwrap();
        settings.library_dir = Some(temp.path().to_owned());
    }
    let executor = pvisor::VmExecutor::new(settings.clone()).unwrap();
    assert!(!executor.descriptor().supports_checkpoint);
    assert!(
        pvisor::VmExecutor::checkpoint_compatibility(&settings)
            .unwrap_err()
            .to_string()
            .contains("CAPABILITY_UNSUPPORTED")
    );
    assert_eq!(executor.settings().rootfs, Some(base.root()));
    assert!(executor.settings().rootfs_immutable);
    let mut writable_alias = settings.clone();
    writable_alias.control_socket = Some(base.root().join("control.sock"));
    let error = pvisor::VmExecutor::new(writable_alias).unwrap_err();
    assert!(error.to_string().contains("overlaps mutable VM backing"));
    assert!(!base.root().join("control.sock").exists());
    base.verify().unwrap();
    settings.rootfs = Some(source);
    assert!(pvisor::VmExecutor::new(settings.clone()).is_err());
    settings.rootfs = None;
    settings.image = Some("example:latest".into());
    assert!(pvisor::VmExecutor::new(settings).is_err());
}
