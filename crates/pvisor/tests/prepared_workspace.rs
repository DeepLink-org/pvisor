//! Workspace catalog ownership and authentication through the public runtime API.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]

use pvisor::environment_snapshot::{BaseReference, SnapshotBase, SnapshotStore};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn source(root: &Path) -> PathBuf {
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    source
}

fn entry(root: &Path, catalog: &str, base: &SnapshotBase) -> PathBuf {
    root.join(catalog).join(&base.reference().id)
}

fn base_marker(root: &Path, base: &SnapshotBase) -> PathBuf {
    root.join("bases")
        .join(&base.reference().id)
        .join("reference")
}

fn workspace_rejected(store: &SnapshotStore, base: &SnapshotBase) {
    assert!(store.open_prepared_workspace(base.reference()).is_err());
    // Retention must not repair a corrupt published entry by overwriting it.
    assert!(store.retain_prepared_workspace(base).is_err());
}

#[test]
fn prepared_workspace_survives_preparer_exit_source_changes_and_gc() {
    const ROLE: &str = "PVISOR_PREPARED_WORKSPACE_PREPARER";
    if let Some(root) = std::env::var_os(ROLE) {
        let root = Path::new(&root);
        let store = SnapshotStore::new(&root.join("store")).unwrap();
        let base = store.prepare_workspace(&root.join("source")).unwrap();
        fs::write(
            root.join("base.json"),
            serde_json::to_vec(base.reference()).unwrap(),
        )
        .unwrap();
        // A persistent pin cannot depend on running the preparer's destructors.
        std::process::exit(0);
    }
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "prepared_workspace_survives_preparer_exit_source_changes_and_gc",
                "--nocapture",
            ])
            .env(ROLE, temp.path())
            .status()
            .unwrap()
            .success()
    );
    let reference: BaseReference =
        serde_json::from_slice(&fs::read(temp.path().join("base.json")).unwrap()).unwrap();
    fs::write(source.join("file"), b"host changed the original").unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    store.collect_abandoned().unwrap();
    let base = store.open_prepared_workspace(&reference).unwrap();
    assert_eq!(fs::read(base.root().join("file")).unwrap(), b"original");
    drop(base);
    fs::remove_dir_all(source).unwrap();
    store.collect_abandoned().unwrap();
    let base = store.open_prepared_workspace(&reference).unwrap();
    base.verify().unwrap();
    assert_eq!(fs::read(base.root().join("file")).unwrap(), b"original");
    // Warm catalog opens authenticate bounded evidence, not a full inventory.
    fs::remove_file(
        temp.path()
            .join("store/bases")
            .join(&reference.id)
            .join("inventory.json"),
    )
    .unwrap();
    assert!(store.open_prepared_workspace(&reference).is_ok());
    assert!(base.verify().is_err());
}

#[test]
fn prepared_workspace_independent_process_lease_survives_lost_catalog_pin() {
    const ROLE: &str = "PVISOR_PREPARED_WORKSPACE_READER";
    const ABRUPT: &str = "PVISOR_PREPARED_WORKSPACE_ABRUPT";
    if let Some(root) = std::env::var_os(ROLE) {
        let root = Path::new(&root);
        let reference = serde_json::from_slice(&fs::read(root.join("base.json")).unwrap()).unwrap();
        let store = SnapshotStore::new(&root.join("store")).unwrap();
        let base = store.open_prepared_workspace(&reference).unwrap();
        println!("WORKSPACE_LEASE_READY");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_exact(&mut [0]).unwrap();
        assert_eq!(fs::read(base.root().join("file")).unwrap(), b"original");
        if std::env::var_os(ABRUPT).is_some() {
            std::process::exit(0);
        }
        return;
    }
    for abrupt in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let source = source(temp.path());
        let root = temp.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let base = store.prepare_workspace(&source).unwrap();
        let reference = base.reference().clone();
        let pin = entry(&root, "prepared-workspace", &base).join("reference");
        fs::write(
            temp.path().join("base.json"),
            serde_json::to_vec(&reference).unwrap(),
        )
        .unwrap();
        drop(base);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "prepared_workspace_independent_process_lease_survives_lost_catalog_pin",
                "--nocapture",
            ])
            .env(ROLE, temp.path())
            .env_remove(ABRUPT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if abrupt {
            command.env(ABRUPT, "1");
        }
        let mut child = command.spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "reader exited before acquiring its own lease"
            );
            if line.trim() == "WORKSPACE_LEASE_READY" {
                break;
            }
        }
        // Fault injection only, not a supported catalog removal operation.
        fs::remove_file(pin).unwrap();
        store.collect_abandoned().unwrap();
        assert!(store.open_base(&reference).is_ok());
        child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(child.wait().unwrap().success());
        store.collect_abandoned().unwrap();
        assert!(store.open_base(&reference).is_err());
    }
}

#[test]
fn prepared_workspace_rejects_unpinned_foreign_invalid_and_overlapping_sources() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let nested = root.join("nested-source");
    fs::create_dir(&nested).unwrap();
    let alias = temp.path().join("store-alias");
    symlink(&root, &alias).unwrap();
    for overlap in [temp.path(), &root, &nested, &alias] {
        assert!(store.prepare_workspace(overlap).is_err());
    }
    assert_eq!(fs::read_dir(root.join("bases")).unwrap().count(), 0);
    let base = store.import_base(&source).unwrap();
    assert!(store.open_prepared_workspace(base.reference()).is_err());
    let other = SnapshotStore::new(&temp.path().join("other")).unwrap();
    assert!(other.retain_prepared_workspace(&base).is_err());
    assert!(other.open_prepared_workspace(base.reference()).is_err());
    for id in ["../escape".to_owned(), "f".repeat(64)] {
        assert!(
            store
                .open_prepared_workspace(&BaseReference { id })
                .is_err()
        );
    }
    store.retain_prepared_workspace(&base).unwrap();
    assert!(store.open_prepared_workspace(base.reference()).is_ok());
}

#[test]
fn rootfs_and_workspace_catalogs_are_independent_and_domain_bound() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let base = store.prepare_rootfs(&source).unwrap();
    assert!(store.open_prepared_workspace(base.reference()).is_err());
    let root_record = entry(&root, "prepared-rootfs", &base).join("record.json");
    let root_bytes = fs::read(&root_record).unwrap();
    assert_eq!(
        root_bytes,
        format!(
            "{{\"version\":1,\"base\":{}}}",
            serde_json::to_string(base.reference()).unwrap()
        )
        .as_bytes(),
        "existing rootfs v1 record bytes must stay compatible"
    );
    store.retain_prepared_workspace(&base).unwrap();
    let workspace_record = entry(&root, "prepared-workspace", &base).join("record.json");
    let workspace_bytes = fs::read(&workspace_record).unwrap();
    for _ in 0..3 {
        store.retain_prepared_rootfs(&base).unwrap();
        store.retain_prepared_workspace(&base).unwrap();
    }
    assert_eq!(fs::metadata(base_marker(&root, &base)).unwrap().nlink(), 3);
    // Both records identify this same base and both markers are authentic.
    // The record's catalog domain must still prevent cross-purpose adoption.
    fs::write(&workspace_record, &root_bytes).unwrap();
    workspace_rejected(&store, &base);
    assert_eq!(fs::read(&workspace_record).unwrap(), root_bytes);
    fs::write(&workspace_record, &workspace_bytes).unwrap();
    fs::write(&root_record, &workspace_bytes).unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    assert!(store.retain_prepared_rootfs(&base).is_err());
    assert_eq!(fs::read(&root_record).unwrap(), workspace_bytes);
    let mut unknown_null: serde_json::Value = serde_json::from_slice(&root_bytes).unwrap();
    unknown_null["purpose"] = serde_json::Value::Null;
    fs::write(&root_record, serde_json::to_vec(&unknown_null).unwrap()).unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_err());
    assert!(store.retain_prepared_rootfs(&base).is_err());
    fs::write(&root_record, root_bytes).unwrap();
    assert!(store.open_prepared_rootfs(base.reference()).is_ok());
    assert!(store.open_prepared_workspace(base.reference()).is_ok());
    let workspace_only = store.prepare_workspace(&source).unwrap();
    assert!(
        store
            .open_prepared_rootfs(workspace_only.reference())
            .is_err()
    );
}

#[test]
fn prepared_workspace_substituted_records_and_markers_are_not_repaired() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let base = store.prepare_workspace(&source).unwrap();
    let other = store.prepare_workspace(&source).unwrap();
    let directory = entry(&root, "prepared-workspace", &base);
    let record = directory.join("record.json");
    let original = fs::read(&record).unwrap();
    let foreign = fs::read(entry(&root, "prepared-workspace", &other).join("record.json")).unwrap();
    let mut invalid_version: serde_json::Value = serde_json::from_slice(&original).unwrap();
    invalid_version["version"] = 2.into();
    let mut unknown_field: serde_json::Value = serde_json::from_slice(&original).unwrap();
    unknown_field["immutable"] = true.into();
    for bytes in [
        foreign,
        serde_json::to_vec(&invalid_version).unwrap(),
        serde_json::to_vec(&unknown_field).unwrap(),
        vec![b' '; 1025],
    ] {
        fs::write(&record, &bytes).unwrap();
        workspace_rejected(&store, &base);
        assert_eq!(fs::read(&record).unwrap(), bytes);
    }
    fs::write(&record, original).unwrap();
    let marker = directory.join("reference");
    fs::remove_file(&marker).unwrap();
    workspace_rejected(&store, &base);
    fs::write(&marker, b"substituted marker").unwrap();
    workspace_rejected(&store, &base);
    assert_eq!(fs::read(&marker).unwrap(), b"substituted marker");
    fs::remove_file(&marker).unwrap();
    fs::hard_link(base_marker(&root, &other), &marker).unwrap();
    workspace_rejected(&store, &base);
    assert_eq!(
        fs::metadata(&marker).unwrap().ino(),
        fs::metadata(base_marker(&root, &other)).unwrap().ino()
    );
    fs::remove_file(&marker).unwrap();
    fs::hard_link(base_marker(&root, &base), &marker).unwrap();
    assert!(store.open_prepared_workspace(base.reference()).is_ok());
}

#[test]
fn prepared_workspace_rejects_record_marker_and_entry_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let base = store.prepare_workspace(&source).unwrap();
    let directory = entry(&root, "prepared-workspace", &base);
    let record = directory.join("record.json");
    let saved_record = root.join("saved-record.json");
    fs::rename(&record, &saved_record).unwrap();
    symlink(&saved_record, &record).unwrap();
    workspace_rejected(&store, &base);
    fs::remove_file(&record).unwrap();
    fs::rename(&saved_record, &record).unwrap();
    let marker = directory.join("reference");
    fs::remove_file(&marker).unwrap();
    symlink(base_marker(&root, &base), &marker).unwrap();
    workspace_rejected(&store, &base);
    fs::remove_file(&marker).unwrap();
    fs::hard_link(base_marker(&root, &base), &marker).unwrap();
    let saved_entry = root.join("saved-entry");
    fs::rename(&directory, &saved_entry).unwrap();
    symlink(&saved_entry, &directory).unwrap();
    workspace_rejected(&store, &base);
    fs::remove_file(&directory).unwrap();
    fs::rename(&saved_entry, &directory).unwrap();
    assert!(store.open_prepared_workspace(base.reference()).is_ok());
}

#[test]
fn legacy_store_adds_workspace_catalog_without_changing_rootfs_v1() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let base = store.prepare_rootfs(&source).unwrap();
    let reference = base.reference().clone();
    let record = entry(&root, "prepared-rootfs", &base).join("record.json");
    let original = fs::read(&record).unwrap();
    drop(base);
    drop(store);
    // Model an existing store created before workspace catalogs existed.
    fs::remove_dir(root.join("prepared-workspace")).unwrap();
    let store = SnapshotStore::new(&root).unwrap();
    store.collect_abandoned().unwrap();
    let base = store.open_prepared_rootfs(&reference).unwrap();
    assert_eq!(fs::read(&record).unwrap(), original);
    assert!(store.open_prepared_workspace(&reference).is_err());
    store.retain_prepared_workspace(&base).unwrap();
    drop(base);
    drop(store);
    let store = SnapshotStore::new(&root).unwrap();
    store.collect_abandoned().unwrap();
    assert!(store.open_prepared_workspace(&reference).is_ok());
    let catalog = root.join("prepared-workspace");
    let saved_catalog = root.join("saved-workspace-catalog");
    fs::rename(&catalog, &saved_catalog).unwrap();
    symlink(&saved_catalog, &catalog).unwrap();
    assert!(SnapshotStore::new(&root).is_err());
    assert!(store.open_prepared_workspace(&reference).is_err());
    assert_eq!(fs::read(record).unwrap(), original);
    assert!(store.open_prepared_rootfs(&reference).is_ok());
}

#[test]
fn workspace_import_preserves_owned_metadata_links_and_rejects_external_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path());
    fs::set_permissions(source.join("file"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::hard_link(source.join("file"), source.join("alias")).unwrap();
    symlink("file", source.join("link")).unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    let base = store.prepare_workspace(&source).unwrap();
    let file = fs::metadata(base.root().join("file")).unwrap();
    assert_eq!(file.mode() & 0o777, 0o640);
    assert_eq!((file.uid(), file.gid()), {
        let original = fs::metadata(source.join("file")).unwrap();
        (original.uid(), original.gid())
    });
    assert_eq!(
        file.ino(),
        fs::metadata(base.root().join("alias")).unwrap().ino()
    );
    assert_eq!(
        fs::read_link(base.root().join("link")).unwrap(),
        Path::new("file")
    );
    base.verify().unwrap();
    fs::hard_link(source.join("file"), temp.path().join("outside")).unwrap();
    assert!(store.prepare_workspace(&source).is_err());
    assert_eq!(fs::read(base.root().join("file")).unwrap(), b"original");
    base.verify().unwrap();
}
