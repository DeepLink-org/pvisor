//! Bounded concurrency regressions: ordinary tests need no KVM or FUSE.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]

use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
use std::{fs, io::Write, sync::Barrier, thread};

#[test]
fn concurrent_first_use_creates_one_usable_store() {
    for round in 0..12 {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("store");
        let barrier = Barrier::new(16);
        thread::scope(|scope| {
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        SnapshotStore::new(&root)
                    })
                })
                .collect();
            for worker in workers {
                assert!(worker.join().unwrap().is_ok(), "round {round}");
            }
        });
        for name in ["objects", "pending", "deleted", "content"] {
            assert!(root.join(name).is_dir());
        }
        assert_eq!(
            SnapshotStore::new(&root)
                .unwrap()
                .collect_abandoned()
                .unwrap(),
            0
        );
        assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 0);
    }
}

#[test]
fn concurrent_faults_evict_caches_after_delete_and_gc_without_losing_pins() {
    let binding = Compatibility {
        host_boot: "stress".into(),
        build: "stress".into(),
        firmware: "stress".into(),
        profile: "stress".into(),
    };
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("store");
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let store = SnapshotStore::new(&root).unwrap();
        let bytes: Vec<_> = (0..16 * 65536 + 17)
            .map(|i| ((i * 17 + i / 65536) % 251) as u8)
            .collect();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(&bytes).unwrap();
        let id = if compressed {
            pending.publish_compressed(&source, b"state", binding.clone())
        } else {
            pending.publish(&source, b"state", binding.clone())
        }
        .unwrap();
        let published = store.open_for_restore(&id, &binding).unwrap();
        let readers: Vec<_> = (0..8).map(|_| published.ram_reader().unwrap()).collect();
        drop(published);
        store.delete(&id).unwrap();
        thread::scope(|scope| {
            let mut workers = Vec::new();
            for (index, mut reader) in readers.into_iter().enumerate() {
                let bytes = &bytes;
                workers.push(scope.spawn(move || {
                    let mut seed = index as u64 + 1;
                    for _ in 0..256 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let offset = seed as usize % (bytes.len() + 100);
                        let mut output = vec![0xcc; 1 + (seed >> 32) as usize % 100_000];
                        let count = reader.read_at(offset as u64, &mut output).unwrap();
                        let expected = bytes.len().saturating_sub(offset).min(output.len());
                        assert_eq!(count, expected);
                        if count > 0 {
                            assert_eq!(&output[..count], &bytes[offset..offset + count]);
                        }
                        assert!(output[count..].iter().all(|byte| *byte == 0xcc));
                    }
                }));
            }
            // Actual collection races reader Drop and cache eviction. Readers
            // may disappear at any point; none of their remaining pins may be lost.
            for _ in 0..32 {
                store.collect_abandoned().unwrap();
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        store.collect_abandoned().unwrap();
        assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 0);
    }
}
