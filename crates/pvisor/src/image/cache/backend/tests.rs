use super::*;
use crate::image::cache::protocol::{Envelope, read_frame, write_frame};
use crate::image::cache::source::{Request as SourceRequest, handle};
use crate::image::oci::ImageStore;
use std::os::unix::net::UnixListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) reads: Arc<AtomicUsize>,
    stats: Arc<AtomicUsize>,
    lists: Arc<AtomicUsize>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
pub(crate) fn fixture() -> (tempfile::TempDir, Server, CacheClient, String) {
    let temp = tempfile::tempdir().unwrap();
    let store = ImageStore::new(Some(temp.path().join("store"))).unwrap();
    let digest = format!("sha256:{}", "b".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("large"), vec![42; 3 * MAX_READ as usize]).unwrap();
    std::os::unix::fs::symlink("large", root.join("alias")).unwrap();
    let socket = temp.path().join("s");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let stats = Arc::new(AtomicUsize::new(0));
    let lists = Arc::new(AtomicUsize::new(0));
    let worker_stats = stats.clone();
    let worker_lists = lists.clone();
    let worker_stop = stop.clone();
    let worker_reads = reads.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            let (mut socket, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket.set_nonblocking(false).unwrap();
            let envelope: Envelope = read_frame(&mut socket).unwrap();
            if matches!(&envelope.request, CacheRequest::Stat { .. }) {
                worker_stats.fetch_add(1, Ordering::Relaxed);
            }
            if matches!(&envelope.request, CacheRequest::List { .. }) {
                worker_lists.fetch_add(1, Ordering::Relaxed);
            }
            if matches!(&envelope.request, CacheRequest::Read { .. }) {
                worker_reads.fetch_add(1, Ordering::Relaxed);
            }
            let result = match envelope.request {
                CacheRequest::Ping => Ok((Response::Ready, Vec::new())),
                CacheRequest::Stat { digest, path } => {
                    handle(&store, SourceRequest::Stat { digest, path })
                }
                CacheRequest::List {
                    digest,
                    path,
                    offset,
                } => handle(
                    &store,
                    SourceRequest::List {
                        digest,
                        path,
                        offset,
                    },
                ),
                CacheRequest::Read {
                    digest,
                    path,
                    offset,
                    length,
                } => handle(
                    &store,
                    SourceRequest::Read {
                        digest,
                        path,
                        offset,
                        length,
                    },
                ),
                _ => Err(anyhow::anyhow!("unsupported test request")),
            };
            let (response, bytes) = result.unwrap_or_else(|e| {
                let code = if e
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    "not_found"
                } else {
                    "request_failed"
                };
                (
                    Response::Error {
                        code: code.into(),
                        message: e.to_string(),
                    },
                    Vec::new(),
                )
            });
            write_frame(&mut socket, &response).unwrap();
            socket.write_all(&bytes).unwrap();
        }
    });
    let client = CacheClient::new(format!("unix://{}", socket.display()), None).unwrap();
    (
        temp,
        Server {
            stop,
            reads,
            stats,
            lists,
            worker: Some(worker),
        },
        client,
        digest,
    )
}

#[test]
fn cold_directory_batches_attributes_and_preserves_file_boundaries() {
    let (temp, server, client, digest) = fixture();
    let root = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    for index in 0..600 {
        fs::write(root.join(format!("file-{index:03}")), [index as u8]).unwrap();
    }
    let mut filesystem = RemoteFs::new(
        client,
        digest,
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    assert_eq!(filesystem.entries(1).unwrap().len(), 604);
    assert_eq!(server.stats.load(Ordering::Relaxed), 1, "root only");
    assert_eq!(
        server.lists.load(Ordering::Relaxed),
        3,
        "three metadata pages"
    );
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    for index in [0, 1, 599] {
        let node = filesystem
            .child(1, OsStr::new(&format!("file-{index:03}")))
            .unwrap();
        assert_eq!(
            filesystem.read_range(node.attr.ino, 0, 100).unwrap(),
            [index as u8]
        );
        assert_eq!(
            filesystem.read_range(node.attr.ino, 0, 100).unwrap(),
            [index as u8]
        );
    }
    assert_eq!(
        server.stats.load(Ordering::Relaxed),
        1,
        "lookup reuses listed attributes"
    );
    assert_eq!(server.reads.load(Ordering::Relaxed), 3);
    assert!(filesystem.child(1, OsStr::new("absent")).is_err());
    assert_eq!(
        server.stats.load(Ordering::Relaxed),
        1,
        "complete listing proves absence"
    );
    assert_eq!(
        filesystem
            .downloads
            .lock()
            .unwrap()
            .snapshot()
            .downloaded_bytes,
        3,
        "small files are not padded to 1 MiB"
    );
    assert_eq!(
        filesystem.downloads.lock().unwrap().snapshot().cached_bytes,
        3
    );
}

#[test]
fn directory_cookies_resume_after_eviction_without_retaining_child_nodes() {
    let (temp, server, client, digest) = fixture();
    let root = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    for index in 0..2500 {
        fs::write(root.join(format!("file-{index:04}")), [index as u8]).unwrap();
    }
    fs::hard_link(root.join("file-0000"), root.join("linked")).unwrap();
    let mut filesystem =
        RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
    let first = filesystem.entries_page(1, 0).unwrap();
    assert_eq!(first.len(), 258);
    assert_eq!(server.lists.load(Ordering::Relaxed), 1);
    assert_eq!(
        filesystem.nodes.len(),
        1,
        "listing must not pin child nodes"
    );
    let resumed = filesystem.entries_page(1, 17).unwrap();
    assert_eq!(resumed, first[17..]);
    assert_eq!(server.lists.load(Ordering::Relaxed), 1);
    let mut cookie = first.len();
    loop {
        let page = filesystem.entries_page(1, cookie).unwrap();
        assert!(filesystem.directories.len() <= RemoteFs::DIRECTORY_PAGES);
        assert_eq!(filesystem.nodes.len(), 1);
        if page.is_empty() {
            break;
        }
        cookie += page.len();
    }
    assert_eq!(cookie, 2505);
    let again = filesystem.entries_page(1, 0).unwrap();
    assert_eq!(
        again, first,
        "inode identities and cookies survive eviction"
    );
    let file = filesystem.child(1, OsStr::new("file-0000")).unwrap();
    let linked = filesystem.child(1, OsStr::new("linked")).unwrap();
    assert_eq!(file.attr.ino, linked.attr.ino);
    assert!(filesystem.child(1, OsStr::new("missing")).is_err());
}

#[test]
fn directory_pages_reject_nonprogressing_cursors_and_invalid_names() {
    let (temp, _server, client, digest) = fixture();
    let metadata = temp.path().join("metadata");
    let mut filesystem = RemoteFs::new(
        client,
        digest.clone(),
        temp.path().join("blocks"),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    let request = CacheRequest::List {
        digest,
        path: vec![],
        offset: 0,
    };
    let path = metadata.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    let attr = filesystem
        .metadata_request(CacheRequest::Stat {
            digest: filesystem.digest.clone(),
            path: b"large".to_vec(),
        })
        .unwrap();
    for (name, next_offset) in [(b"large".to_vec(), Some(0)), (b"../escape".to_vec(), None)] {
        let bytes = serde_json::to_vec(&Response::Entries {
            names: vec![name],
            metadata: vec![attr.clone()],
            next_offset,
        })
        .unwrap();
        let mut encoded = Sha256::digest(&bytes).to_vec();
        encoded.extend(bytes);
        fs::write(&path, encoded).unwrap();
        assert!(filesystem.entries_page(1, 0).is_err());
        assert!(filesystem.directories.is_empty());
        assert_eq!(filesystem.nodes.len(), 1);
    }
}

#[test]
fn hot_blocks_bound_memory_and_reuse_verified_buffers() {
    let mut hot = HotBlocks::default();
    let bytes: Arc<[u8]> = vec![42; MAX_READ as usize].into();
    hot.insert(1, 0, bytes.clone());
    assert!(Arc::ptr_eq(&hot.get(1, 0).unwrap(), &bytes));
    for file in 2..=66 {
        hot.insert(file, 0, bytes.clone());
    }
    assert_eq!(hot.bytes, HotBlocks::MAX_BYTES);
    assert_eq!(hot.files.len(), 64);
    assert!(hot.get(1, 0).is_none());
    let mut tiny = HotBlocks::default();
    let byte: Arc<[u8]> = vec![1].into();
    for file in 0..=HotBlocks::MAX_ENTRIES as u64 {
        tiny.insert(file, 0, byte.clone());
    }
    assert_eq!(tiny.order.len(), HotBlocks::MAX_ENTRIES);
    assert_eq!(tiny.files.len(), HotBlocks::MAX_ENTRIES);
}

#[test]
fn persistent_metadata_survives_remount_and_rejects_corruption() {
    let (temp, server, client, digest) = fixture();
    let blocks = temp.path().join("blocks");
    let metadata = temp.path().join("metadata");
    let endpoint = client.address().to_owned();
    let mut cold = RemoteFs::new(
        client,
        digest.clone(),
        blocks.clone(),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    let expected = cold.child(1, OsStr::new("large")).unwrap().attr.size;
    let count = cold.entries(1).unwrap().len();
    assert!(cold.child(1, OsStr::new("missing")).is_err());
    drop(server);
    let client = || CacheClient::new(endpoint.clone(), None).unwrap();
    let mut warm = RemoteFs::new(
        client(),
        digest.clone(),
        blocks.clone(),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    assert_eq!(
        warm.child(1, OsStr::new("large")).unwrap().attr.size,
        expected
    );
    assert_eq!(warm.entries(1).unwrap().len(), count);
    let error = warm.child(1, OsStr::new("missing")).err().unwrap();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
    // A different generation cannot borrow entries from the old snapshot.
    assert!(
        RemoteFs::new(
            client(),
            digest.clone(),
            blocks.clone(),
            Some(temp.path().join("new-generation")),
            false
        )
        .is_err()
    );
    let root = CacheRequest::Stat {
        digest: digest.clone(),
        path: vec![],
    };
    fs::write(
        metadata.join(&hash(&serde_json::to_vec(&root).unwrap())[7..]),
        b"corrupt",
    )
    .unwrap();
    assert!(RemoteFs::new(client(), digest, blocks, Some(metadata), false).is_err());
}

#[test]
fn reads_only_requested_blocks_and_reuses_verified_cache() {
    let (temp, server, client, digest) = fixture();
    let cache = temp.path().join("client");
    let mut filesystem = RemoteFs::new(client, digest, cache, None, false).unwrap();
    let file = filesystem.child(1, OsStr::new("large")).unwrap();
    assert_eq!(filesystem.entries(1).unwrap().len(), 4);
    assert_eq!(
        server.reads.load(Ordering::Relaxed),
        0,
        "metadata must not download content"
    );
    let offset = MAX_READ as u64 - 4;
    assert_eq!(
        filesystem.read_range(file.attr.ino, offset, 16).unwrap(),
        [42; 16]
    );
    assert_eq!(
        server.reads.load(Ordering::Relaxed),
        2,
        "only two intersecting blocks are fetched"
    );
    assert_eq!(
        filesystem.read_range(file.attr.ino, offset, 16).unwrap(),
        [42; 16]
    );
    assert_eq!(server.reads.load(Ordering::Relaxed), 2);
    let progress = filesystem.downloads.lock().unwrap().snapshot();
    assert_eq!(progress.downloaded_files, 1);
    assert_eq!(progress.downloaded_bytes, 2 * MAX_READ as u64);
    let path = filesystem.cache.join(&hash(&file.path)[7..]).join("0");
    fs::write(path, b"corrupt").unwrap();
    assert_eq!(filesystem.read_range(file.attr.ino, 0, 1).unwrap(), [42]);
    assert_eq!(server.reads.load(Ordering::Relaxed), 2);
    *filesystem.reader.hot.lock().unwrap() = HotBlocks::default();
    assert_eq!(filesystem.read_range(file.attr.ino, 0, 1).unwrap(), [42]);
    assert_eq!(
        server.reads.load(Ordering::Relaxed),
        3,
        "corrupt cache must be replaced"
    );
    assert_eq!(
        filesystem
            .downloads
            .lock()
            .unwrap()
            .snapshot()
            .downloaded_files,
        1
    );
    assert_eq!(
        filesystem
            .downloads
            .lock()
            .unwrap()
            .snapshot()
            .downloaded_bytes,
        3 * MAX_READ as u64
    );
    let warm_client = CacheClient::new(filesystem.client.address().to_owned(), None).unwrap();
    let mut warm = RemoteFs::new(
        warm_client,
        filesystem.digest.clone(),
        filesystem.cache.clone(),
        None,
        false,
    )
    .unwrap();
    let warm_file = warm.child(1, OsStr::new("large")).unwrap();
    assert_eq!(
        warm.read_range(warm_file.attr.ino, offset, 16).unwrap(),
        [42; 16]
    );
    assert_eq!(
        warm.downloads.lock().unwrap().snapshot().downloaded_files,
        0
    );
    assert_eq!(
        warm.downloads.lock().unwrap().snapshot().downloaded_bytes,
        0
    );
    assert_eq!(warm.downloads.lock().unwrap().snapshot().cached_files, 1);
    assert_eq!(warm.downloads.lock().unwrap().snapshot().cached_bytes, 16);
    warm.read_range(warm_file.attr.ino, offset, 16).unwrap();
    assert_eq!(warm.downloads.lock().unwrap().snapshot().cached_files, 1);
    assert_eq!(warm.downloads.lock().unwrap().snapshot().cached_bytes, 32);
    drop(server);
    assert_eq!(filesystem.read_range(file.attr.ino, 0, 1).unwrap(), [42]);
    assert!(
        filesystem
            .read_range(file.attr.ino, 2 * MAX_READ as u64, 1)
            .is_err(),
        "uncached data must fail, never become zeroes"
    );
}

#[test]
fn auto_probe_distinguishes_absence_from_explicit_failure() {
    let temp = tempfile::tempdir().unwrap();
    let address = format!("unix://{}", temp.path().join("missing").display());
    assert!(
        CacheClient::probe(address.clone(), None, false)
            .unwrap()
            .is_none()
    );
    assert!(CacheClient::probe(address, None, true).is_err());
    let socket = temp.path().join("stale");
    drop(UnixListener::bind(&socket).unwrap());
    assert!(
        CacheClient::probe(format!("unix://{}", socket.display()), None, false)
            .unwrap()
            .is_none()
    );
    let (_temp, _server, client, _) = fixture();
    assert!(
        CacheClient::probe(client.address().to_owned(), None, false)
            .unwrap()
            .is_some()
    );
}

#[test]
fn remote_linux_identity_reaches_the_override_stat_contract() {
    let (_temp, _server, client, digest) = fixture();
    let cache = tempfile::tempdir().unwrap();
    let mut fs = RemoteFs::new(client, digest, cache.path().to_path_buf(), None, false).unwrap();
    let node = fs
        .insert_node(
            b"owned".to_vec(),
            Response::Metadata {
                kind: "file".into(),
                size: 0,
                mode: 0o100640,
                uid: 123,
                gid: 456,
                inode: u64::MAX,
                nlink: 1,
                mtime: 0,
                mtime_nsec: 0,
                target: None,
            },
        )
        .unwrap();
    assert_eq!(node.override_stat, b"123:456:0100640");
    if cfg!(target_os = "linux") {
        assert_eq!((node.attr.uid, node.attr.gid), (123, 456));
    }
}

#[test]
fn v2_warm_positive_and_negative_stat_hits_never_trigger_lists() {
    let (temp, server, client, digest) = fixture();
    let endpoint = client.address().to_owned();
    let metadata = temp.path().join("metadata");
    let mut cold = RemoteFs::new(
        client,
        digest.clone(),
        temp.path().join("blocks"),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    cold.prefetch_enabled = false;
    cold.child(1, OsStr::new("large")).unwrap();
    assert!(cold.child(1, OsStr::new("absent")).is_err());
    let stats = server.stats.load(Ordering::Relaxed);
    let mut warm = RemoteFs::new(
        CacheClient::new(endpoint.clone(), None).unwrap(),
        digest.clone(),
        temp.path().join("blocks"),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    warm.prefetch_enabled = true;
    warm.child(1, OsStr::new("large")).unwrap();
    assert!(warm.child(1, OsStr::new("absent")).is_err());
    assert_eq!(server.stats.load(Ordering::Relaxed), stats);
    assert_eq!(server.lists.load(Ordering::Relaxed), 0);
    assert!(warm.prefetch_probes.is_empty());
    let request = CacheRequest::Stat {
        digest: digest.clone(),
        path: b"large".to_vec(),
    };
    let key = metadata.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    fs::write(key, b"corrupt").unwrap();
    let mut recovered = RemoteFs::new(
        CacheClient::new(endpoint, None).unwrap(),
        digest,
        temp.path().join("blocks"),
        Some(metadata),
        false,
    )
    .unwrap();
    recovered.prefetch_enabled = true;
    recovered.child(1, OsStr::new("large")).unwrap();
    assert_eq!(server.stats.load(Ordering::Relaxed), stats + 1);
    assert_eq!(server.lists.load(Ordering::Relaxed), 0);
}

#[test]
fn v2_inventory_negative_stat_survives_offline_remount() {
    // Exercise both complete-inventory branches: absence discovered by the
    // triggering prefetch, and a later miss in an already complete inventory.
    for missing_triggers_prefetch in [true, false] {
        let (temp, server, client, digest) = fixture();
        let endpoint = client.address().to_owned();
        let metadata = temp.path().join("metadata");
        let blocks = temp.path().join("blocks");
        let mut cold = RemoteFs::new(
            client,
            digest.clone(),
            blocks.clone(),
            Some(metadata.clone()),
            false,
        )
        .unwrap();
        cold.prefetch_enabled = true;
        cold.child(1, OsStr::new("large")).unwrap();
        if !missing_triggers_prefetch {
            cold.child(1, OsStr::new("alias")).unwrap();
        }
        let error = cold.child(1, OsStr::new("missing")).err().unwrap();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(cold.directory_complete(1));
        assert_eq!(
            server.stats.load(Ordering::Relaxed),
            2,
            "root and first child only"
        );
        assert_eq!(
            server.lists.load(Ordering::Relaxed),
            1,
            "V2 must populate the inventory"
        );
        let request = CacheRequest::Stat {
            digest: digest.clone(),
            path: b"missing".to_vec(),
        };
        assert!(
            matches!(cold.cached_metadata(&request), Some(Response::Error { code, .. }) if code == "not_found")
        );
        drop(cold);
        drop(server);

        let mut warm = RemoteFs::new(
            CacheClient::new(endpoint, None).unwrap(),
            digest,
            blocks,
            Some(metadata),
            false,
        )
        .unwrap();
        warm.prefetch_enabled = true;
        assert!(warm.directories.is_empty());
        // The missing name is the very first child probe after remount, with
        // no server available and no resident inventory to prove its absence.
        let error = warm.child(1, OsStr::new("missing")).err().unwrap();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(warm.prefetch_probes.is_empty());
        assert_eq!(warm.prefetch_pages, 0);
        assert!(warm.directories.is_empty());
    }
}

#[test]
fn v2_inventory_negative_receipt_write_failure_is_not_enoent() {
    let (temp, server, client, digest) = fixture();
    let metadata = temp.path().join("metadata");
    let mut filesystem = RemoteFs::new(
        client,
        digest.clone(),
        temp.path().join("blocks"),
        Some(metadata.clone()),
        false,
    )
    .unwrap();
    filesystem.prefetch_enabled = true;
    filesystem.child(1, OsStr::new("large")).unwrap();
    filesystem.child(1, OsStr::new("alias")).unwrap();
    assert!(filesystem.directory_complete(1));
    let request = CacheRequest::Stat {
        digest,
        path: b"missing".to_vec(),
    };
    let key = metadata.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    // A directory at the receipt path deterministically prevents publication,
    // including when tests run as root (unlike permission-bit-only fixtures).
    fs::create_dir(key).unwrap();
    let error = filesystem.child(1, OsStr::new("missing")).err().unwrap();
    let io_error = error
        .chain()
        .find_map(|error| error.downcast_ref::<std::io::Error>())
        .unwrap();
    assert_ne!(io_error.kind(), std::io::ErrorKind::NotFound);
    assert!(filesystem.cached_metadata(&request).is_none());
    assert_eq!(server.stats.load(Ordering::Relaxed), 2);
    assert_eq!(server.lists.load(Ordering::Relaxed), 1);
}

#[test]
fn v2_override_disables_only_exact_zero() {
    assert!(lazy_image_v2(None));
    assert!(!lazy_image_v2(Some(OsStr::new("0"))));
    for value in ["", "false", "00", " 0", "1"] {
        assert!(lazy_image_v2(Some(OsStr::new(value))));
    }
}

#[test]
fn v2_second_cold_probe_batches_metadata_but_never_content() {
    let (temp, server, client, digest) = fixture();
    let mut filesystem = RemoteFs::new(
        client,
        digest,
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    filesystem.prefetch_enabled = true;
    let large = filesystem.child(1, OsStr::new("large")).unwrap();
    assert_eq!(server.stats.load(Ordering::Relaxed), 2);
    assert_eq!(server.lists.load(Ordering::Relaxed), 0);
    let alias = filesystem.child(1, OsStr::new("alias")).unwrap();
    assert_eq!(alias.target.as_deref(), Some(b"large".as_slice()));
    assert_eq!(alias.attr.kind, FileType::Symlink);
    assert_eq!(server.stats.load(Ordering::Relaxed), 2);
    assert_eq!(server.lists.load(Ordering::Relaxed), 1);
    assert!(filesystem.child(1, OsStr::new("missing")).is_err());
    assert_eq!(server.stats.load(Ordering::Relaxed), 2);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    assert_eq!(filesystem.nodes.len(), 3);
    assert_eq!(
        filesystem.child(1, OsStr::new("large")).unwrap().attr.ino,
        large.attr.ino
    );
    let endpoint = filesystem.client.address().to_owned();
    let digest = filesystem.digest.clone();
    drop(server);
    let mut warm = RemoteFs::new(
        CacheClient::new(endpoint, None).unwrap(),
        digest,
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    warm.prefetch_enabled = true;
    warm.child(1, OsStr::new("large")).unwrap();
    warm.child(1, OsStr::new("alias")).unwrap();
    assert!(
        warm.prefetch_probes.is_empty(),
        "persistent stat hits must precede triggers"
    );
}

#[test]
fn v2_oversized_inventory_falls_back_and_does_not_retrigger_after_eviction() {
    let (temp, server, client, digest) = fixture();
    let root = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    for index in 0..900 {
        fs::write(root.join(format!("file-{index:04}")), []).unwrap();
    }
    let mut filesystem =
        RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
    filesystem.prefetch_enabled = true;
    filesystem.child(1, OsStr::new("file-0899")).unwrap();
    filesystem.child(1, OsStr::new("file-0898")).unwrap();
    assert_eq!(server.lists.load(Ordering::Relaxed), 2);
    assert_eq!(
        server.stats.load(Ordering::Relaxed),
        3,
        "outside prefix still requires stat"
    );
    assert!(!filesystem.directory_complete(1));
    filesystem.child(1, OsStr::new("file-0000")).unwrap();
    assert_eq!(server.stats.load(Ordering::Relaxed), 3);
    assert!(filesystem.child(1, OsStr::new("absent")).is_err());
    assert_eq!(
        server.stats.load(Ordering::Relaxed),
        4,
        "partial inventory cannot prove absence"
    );
    filesystem.directories.clear();
    filesystem.directory_order.clear();
    filesystem.child(1, OsStr::new("file-0001")).unwrap();
    assert_eq!(
        server.lists.load(Ordering::Relaxed),
        2,
        "eviction cannot replenish allowance"
    );
    assert_eq!(filesystem.prefetch_pages, RemoteFs::PREFETCH_PAGES);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn v2_trigger_admission_is_bounded_and_disabled_mode_remains_exact_stat() {
    let (temp, server, client, digest) = fixture();
    let root = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    for index in 0..RemoteFs::PREFETCH_DIRECTORIES + 4 {
        fs::create_dir(root.join(format!("dir-{index}"))).unwrap();
    }
    let mut filesystem =
        RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
    filesystem.prefetch_enabled = false;
    filesystem.child(1, OsStr::new("large")).unwrap();
    filesystem.child(1, OsStr::new("alias")).unwrap();
    assert_eq!(server.lists.load(Ordering::Relaxed), 0);
    assert!(filesystem.prefetch_probes.is_empty());
    filesystem.prefetch_enabled = true;
    for index in 0..RemoteFs::PREFETCH_DIRECTORIES + 4 {
        let directory = filesystem
            .child(1, OsStr::new(&format!("dir-{index}")))
            .unwrap();
        assert!(
            filesystem
                .child(directory.attr.ino, OsStr::new("missing-a"))
                .is_err()
        );
        assert!(
            filesystem
                .child(directory.attr.ino, OsStr::new("missing-b"))
                .is_err()
        );
        assert!(filesystem.directories.len() <= RemoteFs::DIRECTORY_PAGES);
        assert!(filesystem.prefetch_probes.len() <= RemoteFs::PREFETCH_DIRECTORIES);
    }
    assert_eq!(
        filesystem.prefetch_probes.len(),
        RemoteFs::PREFETCH_DIRECTORIES
    );
    assert!(filesystem.prefetch_pages <= RemoteFs::PREFETCH_DIRECTORIES * RemoteFs::PREFETCH_PAGES);
    assert!(server.lists.load(Ordering::Relaxed) <= RemoteFs::PREFETCH_DIRECTORIES + 1);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn v2_speculative_errors_fall_back_without_inventing_absence() {
    let (temp, server, client, digest) = fixture();
    let mut filesystem = RemoteFs::new(
        client,
        digest,
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    filesystem.prefetch_enabled = true;
    filesystem.child(1, OsStr::new("large")).unwrap();
    let request = CacheRequest::List {
        digest: filesystem.digest.clone(),
        path: vec![],
        offset: 0,
    };
    let directory = filesystem.metadata_cache.as_ref().unwrap();
    let key = directory.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    filesystem
        .store_metadata_at(
            directory,
            &key,
            &Response::Entries {
                names: vec![],
                metadata: vec![],
                next_offset: Some(0),
            },
        )
        .unwrap();
    let alias = filesystem.child(1, OsStr::new("alias")).unwrap();
    assert_eq!(alias.target.as_deref(), Some(b"large".as_slice()));
    assert_eq!(
        server.stats.load(Ordering::Relaxed),
        3,
        "invalid prefetch falls back to stat"
    );
    assert!(!filesystem.directory_complete(1));
    assert!(
        filesystem.entries_page(1, 0).is_err(),
        "explicit listing still reports corruption"
    );
    assert!(filesystem.directories.is_empty());
    assert_eq!(filesystem.prefetch_pages, 1);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn v2_non_utf8_listed_metadata_preserves_the_exact_contract() {
    let (temp, server, client, digest) = fixture();
    let mut filesystem = RemoteFs::new(
        client,
        digest,
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    filesystem.prefetch_enabled = true;
    filesystem.child(1, OsStr::new("large")).unwrap();
    let name = b"name-\xff";
    let attr = Response::Metadata {
        kind: "file".into(),
        size: 123,
        mode: 0o100751,
        uid: 123,
        gid: 456,
        inode: u64::MAX,
        nlink: 2,
        mtime: -1,
        mtime_nsec: 123456789,
        target: None,
    };
    let request = CacheRequest::List {
        digest: filesystem.digest.clone(),
        path: vec![],
        offset: 0,
    };
    let directory = filesystem.metadata_cache.as_ref().unwrap();
    let key = directory.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    // Byte-only fixture works even on hosts whose filesystem rejects this name.
    filesystem
        .store_metadata_at(
            directory,
            &key,
            &Response::Entries {
                names: vec![name.to_vec()],
                metadata: vec![attr.clone()],
                next_offset: None,
            },
        )
        .unwrap();
    let listed = filesystem.child(1, OsStr::from_bytes(name)).unwrap();
    assert_eq!(listed.path, name);
    assert_eq!(listed.attr.size, 123);
    assert_eq!(
        (
            listed.attr.uid,
            listed.attr.gid,
            listed.attr.perm,
            listed.attr.nlink
        ),
        (123, 456, 0o751, 2)
    );
    assert_eq!(listed.override_stat, b"123:456:0100751");
    assert_eq!(
        listed.attr.mtime,
        UNIX_EPOCH - Duration::from_secs(1) + Duration::from_nanos(123456789)
    );
    let alias = filesystem.insert_node(b"hardlink".to_vec(), attr).unwrap();
    assert_eq!(alias.attr.ino, listed.attr.ino);
    assert_eq!(server.stats.load(Ordering::Relaxed), 2);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn v2_corrupt_cached_pages_refetch_and_metadata_matches_exact_stat() {
    let (temp, server, client, digest) = fixture();
    let root = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    fs::hard_link(root.join("large"), root.join("linked")).unwrap();
    let mut filesystem = RemoteFs::new(
        client,
        digest.clone(),
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        false,
    )
    .unwrap();
    filesystem.prefetch_enabled = true;
    let expected = filesystem.child(1, OsStr::new("large")).unwrap();
    let request = CacheRequest::List {
        digest,
        path: vec![],
        offset: 0,
    };
    let key = temp
        .path()
        .join("metadata")
        .join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
    fs::write(key, b"corrupt").unwrap();
    let linked = filesystem.child(1, OsStr::new("linked")).unwrap();
    assert_eq!(server.lists.load(Ordering::Relaxed), 1);
    assert_eq!(linked.attr.ino, expected.attr.ino);
    assert_eq!(linked.object_id, expected.object_id);
    assert_eq!(linked.override_stat, expected.override_stat);
    assert_eq!(linked.attr.mtime, expected.attr.mtime);
    assert_eq!(linked.attr.size, expected.attr.size);
    assert_eq!(linked.attr.nlink, expected.attr.nlink);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

// APFS rejects invalid UTF-8; keep byte-name coverage on Linux.
const PORTABLE_RAW_NAME: &[u8] = if cfg!(target_os = "linux") {
    b"raw-\xff"
} else {
    b"raw-utf8"
};

pub(crate) fn portable_fixture(
    tcp: bool,
) -> (
    tempfile::TempDir,
    Server,
    CacheClient,
    String,
    Arc<AtomicUsize>,
) {
    use crate::image::cache::{portable::PortableCache, storage::Storage, transport::Stream};
    use crate::image::oci::PreparedImage;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let store = ImageStore::new(Some(temp.path().join("publisher"))).unwrap();
    let digest = format!("sha256:{}", "c".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("large"), vec![42; MAX_READ as usize + 9]).unwrap();
    fs::set_permissions(root.join("large"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::hard_link(root.join("large"), root.join("linked")).unwrap();
    fs::write(root.join(OsStr::from_bytes(PORTABLE_RAW_NAME)), b"raw").unwrap();
    std::os::unix::fs::symlink("large", root.join("alias")).unwrap();
    let image = PreparedImage {
        rootfs: root,
        digest,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let cache = PortableCache::new(
        Storage::filesystem(temp.path().join("shared"), true).unwrap(),
        None,
        false,
    );
    let (response, _) = cache.publish(&store, &image, "amd64", "fixture").unwrap();
    let Response::Prepared {
        image_handle,
        metadata_pages,
        ..
    } = response
    else {
        panic!()
    };
    assert!(metadata_pages);
    fs::remove_dir_all(&store.root).unwrap();

    enum Listener {
        Unix(UnixListener),
        Tcp(TcpListener),
    }
    let (listener, address) = if tcp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("tcp://{}", listener.local_addr().unwrap());
        (Listener::Tcp(listener), address)
    } else {
        let path = temp.path().join("portable.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        (
            Listener::Unix(listener),
            format!("unix://{}", path.display()),
        )
    };
    let token = tcp.then(|| "portable-fixture-token".to_owned());
    let worker_token = token.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let stats = Arc::new(AtomicUsize::new(0));
    let lists = Arc::new(AtomicUsize::new(0));
    let pages = Arc::new(AtomicUsize::new(0));
    let worker_stop = stop.clone();
    let worker_reads = reads.clone();
    let worker_stats = stats.clone();
    let worker_lists = lists.clone();
    let worker_pages = pages.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            let accepted = match &listener {
                Listener::Unix(listener) => listener.accept().map(|(s, _)| Stream::Unix(s)),
                Listener::Tcp(listener) => listener.accept().map(|(s, _)| Stream::Tcp(s)),
            };
            let mut socket = match accepted {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            // Accepted sockets inherit nonblocking mode on macOS.
            match &socket {
                Stream::Unix(socket) => socket.set_nonblocking(false).unwrap(),
                Stream::Tcp(socket) => socket.set_nonblocking(false).unwrap(),
            }
            socket.timeouts(Duration::from_secs(5)).unwrap();
            let envelope: Envelope = read_frame(&mut socket).unwrap();
            assert_eq!(envelope.token, worker_token);
            let counter = match &envelope.request {
                CacheRequest::Read { .. } => Some(&worker_reads),
                CacheRequest::Stat { .. } => Some(&worker_stats),
                CacheRequest::List { .. } => Some(&worker_lists),
                CacheRequest::Metadata { .. } => Some(&worker_pages),
                _ => None,
            };
            if let Some(counter) = counter {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            let (response, bytes) = cache.request(envelope.request).unwrap_or_else(|error| {
                let code = match error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind)
                {
                    Some(std::io::ErrorKind::NotFound) => "not_found",
                    Some(std::io::ErrorKind::PermissionDenied) => "permission_denied",
                    _ => "request_failed",
                };
                (
                    Response::Error {
                        code: code.into(),
                        message: error.to_string(),
                    },
                    vec![],
                )
            });
            write_frame(&mut socket, &response).unwrap();
            socket.write_all(&bytes).unwrap();
        }
    });
    let client = crate::image::cache::client::tests::use_server_reuse(
        CacheClient::new(address, token).unwrap(),
        false,
    );
    (
        temp,
        Server {
            stop,
            worker: Some(worker),
            reads,
            stats,
            lists,
        },
        client,
        image_handle,
        pages,
    )
}

#[test]
fn socket_index_pages_preserve_metadata_without_guest_rpcs_or_content() {
    for tcp in [false, true] {
        let (temp, server, client, handle, pages) = portable_fixture(tcp);
        let binding = client.binding();
        let blocks = temp.path().join("blocks");
        let metadata = temp.path().join("metadata");
        let mut source = portable_backend(
            client,
            handle.clone(),
            blocks.clone(),
            metadata.clone(),
            true,
        )
        .unwrap();
        assert!(source.metadata_reader.is_some());
        assert!(!source.prefetch_enabled);
        let large = source.child(1, OsStr::new("large")).unwrap();
        let linked = source.child(1, OsStr::new("linked")).unwrap();
        assert_eq!(large.attr.ino, linked.attr.ino);
        assert_eq!(large.attr.nlink, 2);
        assert_eq!(large.attr.perm, 0o640);
        assert_eq!(
            source
                .child(1, OsStr::from_bytes(PORTABLE_RAW_NAME))
                .unwrap()
                .attr
                .size,
            3
        );
        assert_eq!(
            source
                .child(1, OsStr::new("alias"))
                .unwrap()
                .target
                .as_deref(),
            Some(b"large".as_slice())
        );
        assert!(source.lookup_path(b"alias/child".to_vec()).is_err());
        for path in [b"../large".as_slice(), b"/large", b"large\0"] {
            assert!(source.lookup_path(path.to_vec()).is_err());
        }
        assert!(source.child(1, OsStr::new("absent")).is_err());
        assert_eq!(source.entries(1).unwrap().len(), 6);
        let fetched = pages.load(Ordering::Relaxed);
        assert!(fetched > 0);
        assert_eq!(server.stats.load(Ordering::Relaxed), 0);
        assert_eq!(server.lists.load(Ordering::Relaxed), 0);
        assert_eq!(server.reads.load(Ordering::Relaxed), 0);
        drop(source);
        drop(server);
        let offline = || {
            crate::image::cache::client::tests::use_server_reuse(
                CacheClient::from_binding(binding.clone()).unwrap(),
                false,
            )
        };
        let mut warm = portable_backend(
            offline(),
            handle.clone(),
            blocks.clone(),
            metadata.clone(),
            true,
        )
        .unwrap();
        assert_eq!(warm.child(1, OsStr::new("large")).unwrap().attr.perm, 0o640);
        assert!(warm.child(1, OsStr::new("absent")).is_err());
        assert_eq!(warm.entries(1).unwrap().len(), 6);
        // Exercise the persistent binary subtree rather than exact-response receipts.
        for entry in fs::read_dir(&metadata).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                fs::remove_file(entry.path()).unwrap();
            }
        }
        let mut binary_warm = portable_backend(offline(), handle, blocks, metadata, true).unwrap();
        assert_eq!(
            binary_warm
                .child(1, OsStr::from_bytes(PORTABLE_RAW_NAME))
                .unwrap()
                .attr
                .size,
            3
        );
        assert!(binary_warm.child(1, OsStr::new("absent")).is_err());
        assert_eq!(binary_warm.entries(1).unwrap().len(), 6);
    }
}

fn portable_backend(
    client: CacheClient,
    handle: String,
    blocks: PathBuf,
    metadata: PathBuf,
    pages: bool,
) -> anyhow::Result<RemoteFs> {
    RemoteFs::new_with_overrides(client, handle, blocks, Some(metadata), pages, None, None)
}

fn json_receipts(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut receipts: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| (entry.path(), fs::read(entry.path()).unwrap()))
        .collect();
    receipts.sort_by(|a, b| a.0.cmp(&b.0));
    receipts
}

#[test]
fn index_pages_ignore_rechecksummed_positive_and_negative_receipts_after_reconstruction() {
    for child_lookup in [false, true] {
        let (temp, server, client, handle, pages) = portable_fixture(false);
        let binding = client.binding();
        let blocks = temp.path().join("blocks");
        let metadata = temp.path().join("metadata");
        let mut legacy = portable_backend(
            client,
            handle.clone(),
            blocks.clone(),
            metadata.clone(),
            false,
        )
        .unwrap();
        legacy.prefetch_enabled = false;
        let expected = legacy.child(1, OsStr::new("large")).unwrap();
        legacy.entries(1).unwrap();
        let list_request = CacheRequest::List {
            digest: handle.clone(),
            path: Vec::new(),
            offset: 0,
        };
        let list = metadata.join(&hash(&serde_json::to_vec(&list_request).unwrap())[7..]);
        legacy
            .store_metadata_at(
                &metadata,
                &list,
                &Response::Entries {
                    names: Vec::new(),
                    metadata: Vec::new(),
                    next_offset: None,
                },
            )
            .unwrap();
        let request = CacheRequest::Stat {
            digest: handle.clone(),
            path: b"large".to_vec(),
        };
        let mut forged = legacy.cached_metadata(&request).unwrap();
        let Response::Metadata { size, mode, .. } = &mut forged else {
            panic!()
        };
        *size = 1;
        *mode = 0o100777;
        let positive = metadata.join(&hash(&serde_json::to_vec(&request).unwrap())[7..]);
        legacy
            .store_metadata_at(&metadata, &positive, &forged)
            .unwrap();
        let negative_request = CacheRequest::Stat {
            digest: handle.clone(),
            path: b"linked".to_vec(),
        };
        let negative = metadata.join(&hash(&serde_json::to_vec(&negative_request).unwrap())[7..]);
        legacy
            .store_metadata_at(
                &metadata,
                &negative,
                &Response::Error {
                    code: "not_found".into(),
                    message: "forged absence".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            legacy.cached_metadata(&request),
            Some(Response::Metadata { size: 1, .. })
        ));
        assert!(
            matches!(legacy.cached_metadata(&negative_request), Some(Response::Error { code, .. }) if code == "not_found")
        );
        let receipts = json_receipts(&metadata);
        let stats = server.stats.load(Ordering::Relaxed);
        let lists = server.lists.load(Ordering::Relaxed);
        drop(legacy);
        let client = crate::image::cache::client::tests::use_server_reuse(
            CacheClient::from_binding(binding).unwrap(),
            false,
        );
        let mut reconstructed =
            portable_backend(client, handle, blocks, metadata.clone(), true).unwrap();
        assert!(reconstructed.metadata_reader.is_some());
        let large = if child_lookup {
            reconstructed.child(1, OsStr::new("large"))
        } else {
            reconstructed.lookup_path(b"large".to_vec())
        }
        .unwrap();
        let linked = if child_lookup {
            reconstructed.child(1, OsStr::new("linked"))
        } else {
            reconstructed.lookup_path(b"linked".to_vec())
        }
        .unwrap();
        assert_eq!(large.attr.size, expected.attr.size);
        assert_eq!(large.attr.perm, expected.attr.perm);
        assert_eq!(large.attr.ino, linked.attr.ino);
        assert_eq!(large.attr.nlink, linked.attr.nlink);
        assert_eq!(reconstructed.entries(1).unwrap().len(), 6);
        // Listed positives and complete-inventory negatives must not write receipts either.
        reconstructed.child(1, OsStr::new("alias")).unwrap();
        let error = reconstructed
            .child(1, OsStr::new("absent"))
            .err()
            .expect("missing path");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(json_receipts(&metadata), receipts);
        assert!(pages.load(Ordering::Relaxed) > 0);
        assert_eq!(server.stats.load(Ordering::Relaxed), stats);
        assert_eq!(server.lists.load(Ordering::Relaxed), lists);
        assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn index_pages_old_receipts_cannot_mask_corrupt_remote_metadata() {
    for name in ["COMMIT.json", "checksums.bin", "index.bin"] {
        let (temp, server, client, handle, pages) = portable_fixture(false);
        let binding = client.binding();
        let blocks = temp.path().join("blocks");
        let metadata = temp.path().join("metadata");
        let mut legacy = portable_backend(
            client,
            handle.clone(),
            blocks.clone(),
            metadata.clone(),
            false,
        )
        .unwrap();
        legacy.prefetch_enabled = false;
        legacy.child(1, OsStr::new("large")).unwrap();
        legacy.entries(1).unwrap();
        let receipts = json_receipts(&metadata);
        assert!(!receipts.is_empty());
        assert!(!metadata.join("binary").exists());
        let stats = server.stats.load(Ordering::Relaxed);
        let lists = server.lists.load(Ordering::Relaxed);
        drop(legacy);
        let prefix = crate::image::cache::portable::metadata_prefix(&handle).unwrap();
        let object = temp.path().join("shared").join(prefix).join(name);
        let mut bytes = fs::read(&object).unwrap();
        bytes[0] ^= 1;
        fs::write(object, bytes).unwrap();
        let client = crate::image::cache::client::tests::use_server_reuse(
            CacheClient::from_binding(binding).unwrap(),
            false,
        );
        let error = portable_backend(client, handle, blocks, metadata.clone(), true)
            .err()
            .expect("old receipts must not mask remote metadata corruption");
        assert!(
            format!("{error:#}").contains("digest mismatch"),
            "{name}: {error:#}"
        );
        assert!(
            !error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
        assert_eq!(json_receipts(&metadata), receipts);
        assert!(pages.load(Ordering::Relaxed) > 0);
        assert_eq!(server.stats.load(Ordering::Relaxed), stats);
        assert_eq!(server.lists.load(Ordering::Relaxed), lists);
        assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn unknown_metadata_capability_keeps_socket_metadata_on_rpc_without_probe() {
    let (temp, server, client, handle, pages) = portable_fixture(false);
    let mut source =
        RemoteFs::new(client, handle, temp.path().join("blocks"), None, false).unwrap();
    source.prefetch_enabled = false;
    assert!(source.metadata_reader.is_none());
    source.child(1, OsStr::new("large")).unwrap();
    source.entries(1).unwrap();
    assert!(server.stats.load(Ordering::Relaxed) >= 2);
    assert!(server.lists.load(Ordering::Relaxed) > 0);
    assert_eq!(pages.load(Ordering::Relaxed), 0);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn index_page_rollout_requires_all_gates_and_only_exact_zero_disables() {
    assert!(index_pages_enabled(true, true, true, None));
    assert!(index_pages_enabled(
        true,
        true,
        true,
        Some(OsStr::new("false"))
    ));
    assert!(index_pages_enabled(true, true, true, Some(OsStr::new(""))));
    assert!(!index_pages_enabled(
        true,
        true,
        true,
        Some(OsStr::new("0"))
    ));
    assert!(!index_pages_enabled(false, true, true, None));
    assert!(!index_pages_enabled(true, false, true, None));
    assert!(!index_pages_enabled(true, true, false, None));
}

#[test]
fn index_page_corruption_and_transport_failure_never_fall_back_to_guest_rpcs() {
    let (temp, server, client, handle, pages) = portable_fixture(false);
    let prefix = crate::image::cache::portable::metadata_prefix(&handle).unwrap();
    let index = temp.path().join("shared").join(prefix).join("index.bin");
    let mut bytes = fs::read(&index).unwrap();
    bytes[0] ^= 1;
    fs::write(index, bytes).unwrap();
    assert!(
        RemoteFs::new_with_overrides(
            client,
            handle.clone(),
            temp.path().join("blocks"),
            None,
            true,
            None,
            None,
        )
        .is_err()
    );
    assert!(pages.load(Ordering::Relaxed) > 0);
    assert_eq!(server.stats.load(Ordering::Relaxed), 0);
    assert_eq!(server.lists.load(Ordering::Relaxed), 0);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    drop(server);
    let client = crate::image::cache::client::tests::use_server_reuse(
        CacheClient::new(
            format!("unix://{}", temp.path().join("portable.sock").display()),
            None,
        )
        .unwrap(),
        false,
    );
    assert!(
        RemoteFs::new_with_overrides(
            client,
            handle,
            temp.path().join("blocks"),
            None,
            true,
            None,
            None,
        )
        .is_err()
    );
}

#[test]
fn index_page_authorization_failure_is_not_a_legacy_downgrade_signal() {
    let (temp, server, client, handle, _pages) = portable_fixture(false);
    drop(client);
    drop(server);
    let path = temp.path().join("denied.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let envelope: Envelope = read_frame(&mut socket).unwrap();
        assert!(matches!(envelope.request, CacheRequest::Metadata { .. }));
        write_frame(
            &mut socket,
            &Response::Error {
                code: "permission_denied".into(),
                message: "denied metadata".into(),
            },
        )
        .unwrap();
    });
    let client = crate::image::cache::client::tests::use_server_reuse(
        CacheClient::new(format!("unix://{}", path.display()), None).unwrap(),
        false,
    );
    let error = RemoteFs::new_with_overrides(
        client,
        handle,
        temp.path().join("blocks"),
        None,
        true,
        None,
        None,
    )
    .err()
    .expect("metadata authorization must fail");
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    worker.join().unwrap();
}
