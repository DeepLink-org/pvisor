use super::*;
use crate::image::cache::protocol::{Envelope, read_frame, write_frame};
use crate::image::cache::server::handle;
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
            let (response, bytes) = handle(&store, envelope.request).unwrap_or_else(|e| {
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
    let endpoint = client.endpoint.clone();
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
    let warm_client = CacheClient::new(filesystem.client.endpoint.clone(), None).unwrap();
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
        CacheClient::probe(client.endpoint.clone(), None, false)
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
