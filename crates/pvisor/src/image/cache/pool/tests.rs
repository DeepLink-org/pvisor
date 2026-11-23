use super::*;
use pvisor_overlay_core::{OverlayCore, backend};
use std::{collections::BTreeMap, fs};

#[test]
fn public_export_never_copies_private_predecessor_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let source = ImageStore::new(Some(tmp.path().join("source"))).unwrap();
    let pool = tmp.path().join("pool");
    let mut current = None;
    for (revision, secret) in [('a', true), ('b', false)] {
        let digest = format!("sha256:{}", revision.to_string().repeat(64));
        let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::create_dir(&rootfs).unwrap();
        fs::write(rootfs.join("keep"), b"public content").unwrap();
        if secret {
            fs::write(rootfs.join("removed-secret"), b"private predecessor").unwrap();
        }
        let image = PreparedImage {
            rootfs,
            digest,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
        };
        let handle =
            publish_local_image(&pool, &source, &image, "example:visibility", "amd64").unwrap();
        current = Some((handle, image));
    }
    let (handle, image) = current.unwrap();
    let destination = tmp.path().join("export");
    super::super::portable::export_pool(&pool, &handle, &destination).unwrap();
    let mut data = vec![destination.join("data")];
    let mut objects = 0;
    while let Some(path) = data.pop() {
        if path.is_dir() {
            data.extend(
                fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else {
            let payload = zstd::stream::decode_all(fs::read(path).unwrap().as_slice()).unwrap();
            assert!(payload.ends_with(b"public content"));
            assert!(
                !payload
                    .windows(b"private predecessor".len())
                    .any(|w| w == b"private predecessor")
            );
            objects += 1;
        }
    }
    assert_eq!(objects, 1);
    let fresh = tmp.path().join("fresh");
    let fresh_handle =
        publish_local_image(&fresh, &source, &image, "example:visibility", "amd64").unwrap();
    assert_eq!(handle, fresh_handle);
    super::super::portable::export_pool(&fresh, &fresh_handle, &tmp.path().join("fresh-export"))
        .unwrap();
}

#[test]
fn http_repository_opens_and_reads_lazily_without_the_publishers_pool() {
    use axum::{
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    use std::sync::{Arc, Mutex};
    let temporary = tempfile::tempdir().unwrap();
    let location = temporary.path().join("pool");
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let digest = format!("sha256:{}", "e".repeat(64));
    let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&rootfs).unwrap();
    fs::write(rootfs.join("hello"), b"hello from HTTP").unwrap();
    fs::write(rootfs.join("untouched"), vec![42; 2 * 1024 * 1024]).unwrap();
    let image = PreparedImage {
        rootfs,
        digest: digest.clone(),
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let handle = publish_local_image(&location, &source, &image, "example:http", "amd64").unwrap();
    let secret_digest = format!("sha256:{}", "f".repeat(64));
    let secret_root = source
        .root
        .join("rootfs-v3/sha256")
        .join(&secret_digest[7..]);
    fs::create_dir(&secret_root).unwrap();
    fs::write(secret_root.join("secret"), b"not for publication").unwrap();
    let secret = PreparedImage {
        rootfs: secret_root,
        digest: secret_digest,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let secret_handle =
        publish_local_image(&location, &source, &secret, "private:secret", "amd64").unwrap();
    let export = temporary.path().join("export");
    super::super::portable::export_pool(&location, &handle, &export).unwrap();
    assert!(
        !export
            .join(super::super::portable::metadata_prefix(&secret_handle).unwrap())
            .exists()
    );
    fs::remove_dir_all(&location).unwrap();
    fs::remove_dir_all(&source.root).unwrap();

    let requests = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
    let observed = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!(
        "http://{}/{}",
        listener.local_addr().unwrap(),
        uuid::Uuid::new_v4()
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let app = axum::Router::new().route(
                    "/{*path}",
                    get(
                        move |axum::extract::Path(path): axum::extract::Path<String>,
                              headers: HeaderMap| {
                            let export = export.clone();
                            let observed = observed.clone();
                            async move {
                                // A private repository namespace keeps the persistent cache cold.
                                let key = path.split_once('/').unwrap().1;
                                let bytes = fs::read(export.join(key)).unwrap();
                                let range = headers
                                    .get("range")
                                    .map(|h| h.to_str().unwrap().strip_prefix("bytes=").unwrap());
                                observed
                                    .lock()
                                    .unwrap()
                                    .push((key.to_string(), range.is_some()));
                                if let Some(range) = range {
                                    let (start, end) = range.split_once('-').unwrap();
                                    let start: usize = start.parse().unwrap();
                                    let end: usize = end.parse().unwrap();
                                    (
                                        StatusCode::PARTIAL_CONTENT,
                                        [(
                                            "Content-Range",
                                            format!("bytes {start}-{end}/{}", bytes.len()),
                                        )],
                                        bytes[start..=end].to_vec(),
                                    )
                                        .into_response()
                                } else {
                                    (StatusCode::OK, bytes).into_response()
                                }
                            }
                        },
                    ),
                );
                axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
    });
    struct Server(
        Option<tokio::sync::oneshot::Sender<()>>,
        Option<std::thread::JoinHandle<()>>,
    );
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
            self.1.take().unwrap().join().unwrap();
        }
    }
    let _server = Server(Some(stop), Some(task));
    let scratch = temporary.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let image = open_local_image(
        &location,
        Some(&address),
        &[],
        &handle,
        "amd64",
        &digest,
        &scratch,
    )
    .unwrap();
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|(key, _)| !key.starts_with("data/") && !key.ends_with("objects.bin"))
    );
    assert_eq!(
        backend::read_at(&image.root().join("hello"), 6, 4)
            .unwrap()
            .unwrap(),
        b"from"
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|(key, range)| key.starts_with("data/") && *range)
    );
    assert!(!image.root().join("untouched").exists());
}

#[test]
fn import_rejects_unrepresentable_xattrs_without_publishing_a_revision() {
    let temporary = tempfile::tempdir().unwrap();
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let digest = format!("sha256:{}", "d".repeat(64));
    let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&rootfs).unwrap();
    fs::write(rootfs.join("file"), b"keep attributes").unwrap();
    pvisor_overlay_core::sys::set_xattr(
        &rootfs.join("file"),
        std::ffi::OsStr::new("user.required"),
        b"preserve",
        0,
    )
    .unwrap();
    let image = PreparedImage {
        rootfs,
        digest,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let pool = temporary.path().join("pool");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert!(
        super::super::supports_extracted_run(&source, &image.digest)
            .unwrap_err()
            .to_string()
            .contains("does not support xattr user.required")
    );
    let error = publish_local_image(&pool, &source, &image, "example:xattr", "amd64").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not support xattr user.required")
    );
    assert!(!pool.join("meta").exists());
}

#[test]
fn encoding_roundtrips_bounds_and_rejects_corruption_without_changing_old_cache() {
    let temporary = tempfile::tempdir().unwrap();
    for (index, bytes) in [vec![42; super::super::MAX_READ as usize], (0..37).collect()]
        .into_iter()
        .enumerate()
    {
        let root = temporary.path().join(format!("pool-{index}"));
        let source =
            ImageStore::new(Some(temporary.path().join(format!("source-{index}")))).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::create_dir(&rootfs).unwrap();
        fs::write(rootfs.join("file"), &bytes).unwrap();
        let image = PreparedImage {
            rootfs,
            digest,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
        };
        let handle =
            publish_local_image(&root, &source, &image, "example:encoding", "amd64").unwrap();
        let read = || {
            CacheClient::configured_pool(&root, true)
                .unwrap()
                .request(Request::Read {
                    digest: handle.clone(),
                    path: b"file".to_vec(),
                    offset: 0,
                    length: super::super::MAX_READ,
                })
        };
        assert_eq!(read().unwrap().1, bytes);
        let export = temporary.path().join(format!("export-{index}"));
        super::super::portable::export_pool(&root, &handle, &export).unwrap();
        assert!(!export.join("pool-format.json").exists());
        let exported = CacheClient::from_config(super::super::CacheConfig {
            backend: super::super::CacheBackend::Filesystem,
            location: export.display().to_string(),
            read_only: true,
            image_store: None,
        })
        .unwrap();
        assert_eq!(
            exported
                .request(Request::Read {
                    digest: handle.clone(),
                    path: b"file".to_vec(),
                    offset: 0,
                    length: super::super::MAX_READ,
                })
                .unwrap()
                .1,
            bytes
        );
        fn find(root: &Path) -> PathBuf {
            if let Some(entry) = fs::read_dir(root).unwrap().next() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    return find(&path);
                }
                return path;
            }
            panic!("missing content")
        }
        let path = find(&root.join("data"));
        let encoded = fs::read(&path).unwrap();
        assert_eq!(&encoded[encoded.len() - 4..], &0x8f92_eab1u32.to_le_bytes());
        if bytes.len() > 100 {
            assert!(encoded.len() < bytes.len() / 10);
        }
        let mut corrupt = encoded.clone();
        corrupt[0] ^= 1;
        fs::write(&path, corrupt).unwrap();
        assert!(read().is_err());
        fs::write(&path, &encoded).unwrap();
        assert_eq!(read().unwrap().1, bytes);
    }
    let old = temporary.path().join("old");
    let storage = Storage::filesystem(old.clone(), true).unwrap();
    storage
        .put("format.json", b"existing V1".to_vec(), true)
        .unwrap();
    assert!(PoolStorage::open(old, true).is_err());
    assert_eq!(storage.get("format.json").unwrap().unwrap(), b"existing V1");
}

#[test]
fn images_share_content_survive_source_removal_and_copy_up_without_raw_mirror() {
    let temporary = tempfile::tempdir().unwrap();
    let location = temporary.path().join("pool");
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let scratch = temporary.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let bytes = vec![17; super::super::MAX_READ as usize + 123];
    let mut handles = Vec::new();
    for (revision, reference) in [('a', "example:first"), ('b', "example:second")] {
        let digest = format!("sha256:{}", revision.to_string().repeat(64));
        let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::create_dir(&rootfs).unwrap();
        fs::write(rootfs.join("file"), &bytes).unwrap();
        fs::hard_link(rootfs.join("file"), rootfs.join("alias")).unwrap();
        std::os::unix::fs::symlink("file", rootfs.join("link")).unwrap();
        let image = PreparedImage {
            rootfs,
            digest,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
        };
        handles.push((
            publish_local_image(&location, &source, &image, reference, "amd64").unwrap(),
            image.digest,
        ));
    }
    fs::remove_dir_all(&source.root).unwrap();
    for (handle, digest) in handles {
        // Repeated local reads must not become transfer progress or diagnostics
        // on the Docker workload's stderr. Remote readers retain their counters.
        let client = CacheClient::configured_pool(&location, true).unwrap();
        let mut filesystem = RemoteFs::new(
            client,
            handle.clone(),
            scratch.join("unused-progress-cache"),
            None,
            false,
        )
        .unwrap();
        let node = filesystem.child(1, std::ffi::OsStr::new("file")).unwrap();
        filesystem.read_range(node.attr.ino, 0, 16).unwrap();
        filesystem.read_range(node.attr.ino, 0, 16).unwrap();
        let progress = filesystem.downloads.lock().unwrap().snapshot();
        assert_eq!((progress.downloaded_bytes, progress.cached_bytes), (0, 0));
        let raw = CacheClient::new(format!("file://{}", location.display()), None).unwrap();
        assert!(
            matches!(
                raw.request(Request::Open {
                    handle: handle.clone(),
                    architecture: "amd64".into()
                })
                .unwrap()
                .0,
                Response::Prepared { .. }
            ),
            "V2 pool and filesystem share one physical encoding"
        );
        assert!(
            open_local_image(&location, None, &[], &handle, "arm64", &digest, &scratch).is_err()
        );
        assert!(
            open_local_image(
                &location,
                None,
                &[],
                &handle,
                "amd64",
                &format!("sha256:{}", "c".repeat(64)),
                &scratch
            )
            .is_err()
        );
        let image =
            open_local_image(&location, None, &[], &handle, "amd64", &digest, &scratch).unwrap();
        let path = image.root().join("file");
        assert_eq!(
            backend::read_at(&path, super::super::MAX_READ as u64 - 2, 5)
                .unwrap()
                .unwrap(),
            vec![17; 5]
        );
        assert!(!scratch.join("unused-block-cache").exists());
        backend::prepare_metadata(&path).unwrap();
        backend::prepare_metadata(&image.root().join("alias")).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            fs::metadata(image.root().join("alias")).unwrap().ino()
        );
        assert_eq!(
            backend::read_link(image.root().join("link")).unwrap(),
            Path::new("file")
        );
        let upper = temporary.path().join(format!("upper-{handle}"));
        let core = OverlayCore::new(vec![image.root().into()], upper, None).unwrap();
        let writable = core.copy_up(Path::new("file")).unwrap();
        assert_eq!(fs::read(&writable).unwrap(), bytes);
        fs::write(writable, b"changed").unwrap();
        assert_eq!(backend::read_at(&path, 0, 5).unwrap().unwrap(), vec![17; 5]);
        let projection = image.root().parent().unwrap().to_owned();
        drop(core);
        drop(image);
        assert!(!projection.exists());
    }
    fn objects(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    objects(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }
    assert_eq!(
        objects(&location.join("data")).len(),
        1,
        "two revisions share one blob with both frames"
    );
}

#[test]
fn failed_content_directory_barrier_never_advances_pool_head() {
    // Run with PVISOR_IMAGE_BATCH_DIRECTORY_SYNC=1 to exercise this boundary.
    if std::env::var_os("PVISOR_IMAGE_BATCH_DIRECTORY_SYNC").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = ImageStore::new(Some(tmp.path().join("source"))).unwrap();
    let pool = tmp.path().join("pool");
    let mut old = None;
    for revision in ['a', 'b'] {
        let digest = format!("sha256:{}", revision.to_string().repeat(64));
        let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::create_dir(&rootfs).unwrap();
        fs::write(rootfs.join("file"), revision.to_string()).unwrap();
        let image = PreparedImage {
            rootfs,
            digest,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
        };
        super::super::storage::FAIL_BATCH_DIRECTORY_SYNC.with(|fail| fail.set(true));
        let result = publish_local_image(&pool, &source, &image, "example:barrier", "amd64");
        super::super::storage::FAIL_BATCH_DIRECTORY_SYNC.with(|fail| fail.set(false));
        assert!(result.is_err());
        let current = published_local_image(&pool, "example:barrier", "amd64").unwrap();
        match (&old, current) {
            (None, None) => {}
            (Some(expected), Some(Response::Prepared { image_handle, .. })) => {
                assert_eq!(expected, &image_handle)
            }
            _ => panic!("failed barrier advanced HEAD"),
        }
        old =
            Some(publish_local_image(&pool, &source, &image, "example:barrier", "amd64").unwrap());
    }
}
