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
    let mut filesystem = RemoteFs::new(client, digest, temp.path().join("blocks"), None).unwrap();
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
            Some(temp.path().join("new-generation"))
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
    assert!(RemoteFs::new(client(), digest, blocks, Some(metadata)).is_err());
}

#[test]
fn reads_only_requested_blocks_and_reuses_verified_cache() {
    let (temp, server, client, digest) = fixture();
    let cache = temp.path().join("client");
    let mut filesystem = RemoteFs::new(client, digest, cache, None).unwrap();
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
    let mut fs = RemoteFs::new(client, digest, cache.path().to_path_buf(), None).unwrap();
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
    let mut filesystem = RemoteFs::new(client, digest, temp.path().join("blocks"), None).unwrap();
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
    let mut filesystem = RemoteFs::new(client, digest, temp.path().join("blocks"), None).unwrap();
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
