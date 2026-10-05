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
fn lazy_reads_cross_blocks_and_eof_after_snapshot_deletion_and_gc() {
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&directory.path().join("store")).unwrap();
        let bytes = bytes();
        let id = publish(&store, &source, &bytes, compressed);
        let published = store.open_for_restore(&id, &compatibility()).unwrap();
        assert_eq!(published.manifest().version, if compressed { 2 } else { 3 });
        let mut reader = published.ram_reader().unwrap();
        let mut other = published.ram_reader().unwrap();
        drop(published);
        store.delete(&id).unwrap();
        // Pins must survive collection without blocking new snapshots.
        assert_eq!(store.collect_abandoned().unwrap(), 0);
        let new_id = publish(&store, &source, b"new RAM", false);
        store.delete(&new_id).unwrap();
        assert_eq!(reader.len(), bytes.len() as u64);
        for (offset, size) in [
            (0, 1),
            (65533, 14),
            (2 * 65536 - 2, 9),
            (3 * 65536 - 1, 100),
            (bytes.len(), 10),
            (usize::MAX, 4),
            (10, 0),
        ] {
            let mut output = vec![0xcc; size];
            let count = reader.read_at(offset as u64, &mut output).unwrap();
            let expected = bytes.len().saturating_sub(offset).min(size);
            assert_eq!(count, expected);
            if count != 0 {
                assert_eq!(&output[..count], &bytes[offset..offset + count]);
            }
            assert!(output[count..].iter().all(|b| *b == 0xcc));
        }
        let mut output = vec![0; bytes.len()];
        other.read_at(0, &mut output).unwrap();
        assert_eq!(output, bytes);
        drop(reader);
        assert_eq!(store.collect_abandoned().unwrap(), 0);
        drop(other);
        assert_eq!(
            store.collect_abandoned().unwrap(),
            if compressed { 4 } else { 0 }
        );
    }
}

#[test]
fn lazy_restore_defers_corrupt_payload_detection_until_that_block_is_read() {
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let root = directory.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let bytes = bytes();
        let id = publish(&store, &source, &bytes, compressed);
        let published = store.open(&id, &compatibility()).unwrap();
        let path = if compressed {
            root.join("objects")
                .join(&id)
                .join("ram-blocks")
                .join(&published.manifest().ram_blocks.as_ref().unwrap().blocks[2].id)
        } else {
            root.join("objects").join(&id).join("ram.bin")
        };
        drop(published);
        let mut corrupted = fs::read(&path).unwrap();
        let offset = if compressed { 45 + 19 } else { 2 * 65536 + 19 };
        corrupted[offset] ^= 1;
        fs::write(path, corrupted).unwrap();
        assert!(store.open(&id, &compatibility()).is_err());
        // No all-RAM scan or decompression at open or reader construction.
        let published = store.open_for_restore(&id, &compatibility()).unwrap();
        let mut reader = published.ram_reader().unwrap();
        let mut output = [0; 4];
        reader.read_at(0, &mut output).unwrap();
        assert_eq!(output, [0; 4]);
        assert!(reader.read_at(2 * 65536, &mut output).is_err());
    }
}

#[test]
fn indexed_raw_truncation_and_missing_compressed_blocks_are_rejected() {
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let root = directory.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let id = publish(&store, &source, &bytes(), compressed);
        let published = store.open(&id, &compatibility()).unwrap();
        if compressed {
            let block = published.manifest().ram_blocks.as_ref().unwrap().blocks[0]
                .id
                .clone();
            drop(published);
            fs::remove_file(
                root.join("objects")
                    .join(&id)
                    .join("ram-blocks")
                    .join(block),
            )
            .unwrap();
            let published = store.open_for_restore(&id, &compatibility()).unwrap();
            assert!(published.ram_reader().is_err());
        } else {
            drop(published);
            fs::OpenOptions::new()
                .write(true)
                .open(root.join("objects").join(&id).join("ram.bin"))
                .unwrap()
                .set_len(1)
                .unwrap();
            assert!(store.open_for_restore(&id, &compatibility()).is_err());
        }
    }
}

fn rewrite_manifest(
    root: &std::path::Path,
    id: &str,
    update: impl FnOnce(&mut serde_json::Value),
) -> String {
    use sha2::{Digest, Sha256};
    let object = root.join("objects").join(id);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(object.join("manifest.json")).unwrap()).unwrap();
    update(&mut manifest);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let new_id: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(object.join("manifest.json"), bytes).unwrap();
    fs::rename(object, root.join("objects").join(&new_id)).unwrap();
    new_id
}

#[test]
fn legacy_raw_restore_keeps_whole_file_validation_before_lazy_mapping() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let bytes = bytes();
    let id = publish(&store, &source, &bytes, false);
    let id = rewrite_manifest(&root, &id, |manifest| {
        manifest["version"] = 1.into();
        manifest.as_object_mut().unwrap().remove("ram_index");
    });
    let published = store.open_for_restore(&id, &compatibility()).unwrap();
    let mut reader = published.ram_reader().unwrap();
    let mut output = [0; 4];
    reader.read_at(65536, &mut output).unwrap();
    assert_eq!(&output, &bytes[65536..65540]);
    drop(published);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(root.join("objects").join(&id).join("ram.bin"))
        .unwrap();
    use std::os::unix::fs::FileExt;
    file.write_all_at(b"changed", 2 * 65536).unwrap();
    assert!(store.open_for_restore(&id, &compatibility()).is_err());
}

#[test]
fn lazy_restore_rejects_invalid_index_lengths_digests_and_format_combinations() {
    for case in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let root = directory.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let id = publish(&store, &source, &bytes(), case == 4);
        let id = rewrite_manifest(&root, &id, |manifest| match case {
            0 => manifest["ram_index"]["sha256"] = serde_json::json!([]),
            1 => manifest["ram_index"]["sha256"][0] = "../invalid".into(),
            2 => manifest["ram_index"]["length"] = 1.into(),
            3 => manifest["version"] = 2.into(),
            _ => manifest["ram_blocks"]["blocks"][0]["length"] = 1.into(),
        });
        assert!(
            store.open_for_restore(&id, &compatibility()).is_err(),
            "case {case}"
        );
        assert!(store.open(&id, &compatibility()).is_err(), "case {case}");
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
        let (mount, file) = published
            .ram_mount(
                std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")),
                directory.path(),
            )
            .unwrap();
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
        store.collect_abandoned().unwrap();
    }
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
        let id = publish(&store, &source, &bytes(), true);
        let published = store.open_for_restore(&id, &compatibility()).unwrap();
        let (_mount, file) = published
            .ram_mount(
                std::path::Path::new(env!("CARGO_BIN_EXE_pvisor")),
                &directory,
            )
            .unwrap();
        drop(published);
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
    // The crashed pager's content pins are now eligible for normal gc.
    assert_eq!(store.collect_abandoned().unwrap(), 1);
}
