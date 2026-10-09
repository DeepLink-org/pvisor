use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use sha2::{Digest, Sha256};
use std::fs;

fn fixture() -> (tempfile::TempDir, PortableCache, ImageStore, PreparedImage) {
    let tmp = tempfile::tempdir().unwrap();
    let cache = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("publisher")),
        false,
    );
    let store = ImageStore::new(Some(tmp.path().join("publisher"))).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("small"), b"small").unwrap();
    fs::write(root.join("large"), vec![42; MAX_READ as usize * 2 + 9]).unwrap();
    std::os::unix::fs::symlink("small", root.join("link")).unwrap();
    fs::hard_link(root.join("small"), root.join("hard")).unwrap();
    fs::create_dir(root.join("directory")).unwrap();
    let image = PreparedImage {
        rootfs: root,
        digest,
        env: BTreeMap::from([("EXAMPLE".into(), "value".into())]),
        entrypoint: vec!["/bin/sh".into()],
        cmd: vec!["-c".into()],
    };
    (tmp, cache, store, image)
}
fn read_handle(cache: &PortableCache) -> String {
    match cache.prepare("example:test", "amd64", false).unwrap().0 {
        Response::Prepared {
            image_handle: handle,
            ..
        } => handle,
        _ => panic!("missing v1 read handle"),
    }
}

struct MetadataSocket {
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<(String, u64, u32)>>>,
}

impl Drop for MetadataSocket {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

// A deliberately V1-only service exercises the adapter independently of pooling.
// Faults are rehashed at the transport layer to test revision/page authentication.
fn metadata_socket(
    root: &std::path::Path,
    cache: PortableCache,
    corrupt: Option<(&'static str, u64)>,
) -> (MetadataSocket, Arc<crate::image::cache::CacheClient>) {
    use crate::image::cache::protocol::{Envelope, read_frame, write_frame};
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    let path = root.join("metadata.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let worker_stop = stop.clone();
    let worker_requests = requests.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            let (mut socket, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let envelope: Envelope = read_frame(&mut socket).unwrap();
            if envelope.version != 1 {
                write_frame(
                    &mut socket,
                    &Response::Error {
                        code: "unsupported_version".into(),
                        message: "V1 only".into(),
                    },
                )
                .unwrap();
                continue;
            }
            let fault = match &envelope.request {
                Request::Metadata {
                    object_name,
                    offset,
                    length,
                    ..
                } => {
                    worker_requests
                        .lock()
                        .unwrap()
                        .push((object_name.clone(), *offset, *length));
                    corrupt == Some((object_name.as_str(), *offset))
                }
                Request::Ping => false,
                _ => panic!("socket metadata reader requested non-metadata operation"),
            };
            let (mut response, mut body) =
                cache.request(envelope.request).unwrap_or_else(|error| {
                    (
                        Response::Error {
                            code: "request_failed".into(),
                            message: format!("{error:#}"),
                        },
                        Vec::new(),
                    )
                });
            assert!(
                cache.blobs.lock().unwrap().is_empty(),
                "metadata fetched file content"
            );
            if fault && !body.is_empty() {
                body[0] ^= 1;
                response = Response::Data {
                    length: body.len() as u32,
                    sha256: hash(&body),
                };
            }
            write_frame(&mut socket, &response).unwrap();
            socket.write_all(&body).unwrap();
        }
    });
    let client = Arc::new(crate::image::cache::client::tests::use_server_reuse(
        crate::image::cache::CacheClient::new(format!("unix://{}", path.display()), None).unwrap(),
        false,
    ));
    (
        MetadataSocket {
            stop,
            worker: Some(worker),
            requests,
        },
        client,
    )
}

// Run the irreversible seccomp restriction in a fresh test process, not in the
// shared harness. Unlike counting /proc tasks after joins, this catches even
// short-lived workers deterministically and does not need namespace authority.
#[cfg(target_os = "linux")]
#[test]
fn root_metadata_loading_does_not_create_threads_before_user_namespace() {
    const CHILD: &str = "PVISOR_TEST_THREADLESS_METADATA";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "image::cache::portable::tests::root_metadata_loading_does_not_create_threads_before_user_namespace",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "threadless metadata child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    // The service is external to the client lifecycle under test. Start it before
    // restricting only this calling thread (no SECCOMP_FILTER_FLAG_TSYNC).
    let (_server, client) = metadata_socket(tmp.path(), cache, None);
    let local = tmp.path().join("binary-cache");
    let storage = Storage::filesystem(tmp.path().join("shared"), false).unwrap();
    let filter = [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0, // seccomp_data.nr
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 1,
            jf: 0,
            k: libc::SYS_clone as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_clone3 as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) },
        0,
        "install thread-creation guard: {}",
        std::io::Error::last_os_error()
    );
    for _ in 0..2 {
        // A new reader each time forces both cold loading and authenticated disk
        // cache reuse through the same path used by runner lower reconstruction.
        let reader =
            MetadataReader::new(client.clone(), handle.clone(), Some(local.clone())).unwrap();
        assert!(matches!(
            reader
                .request(Request::Stat { digest: handle.clone(), path: vec![] })
                .unwrap(),
            Response::Metadata { ref kind, .. } if kind == "directory"
        ));
        let objects = PortableCache::new(storage.clone(), None, true);
        assert!(matches!(
            objects
                .request(Request::Stat { digest: handle.clone(), path: vec![] })
                .unwrap().0,
            Response::Metadata { ref kind, .. } if kind == "directory"
        ));
    }
}

#[test]
fn metadata_service_serves_only_immutable_objects_with_bounded_data_bodies() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    let parsed = Handle::parse(&handle).unwrap();
    assert!(matches!(
        cache.load(&parsed).unwrap().prepared(),
        Response::Prepared {
            metadata_pages: true,
            ..
        }
    ));
    fs::remove_dir_all(&store.root).unwrap();
    for name in [
        "COMMIT.json",
        "manifest.json",
        "config.json",
        "checksums.bin",
        "files.bin",
        "contents.bin",
        "index.bin",
        "objects.bin",
    ] {
        let expected =
            fs::read(tmp.path().join("shared").join(parsed.prefix()).join(name)).unwrap();
        let (response, body) = cache
            .request(Request::Metadata {
                handle: handle.clone(),
                object_name: name.into(),
                offset: 0,
                length: MAX_READ,
            })
            .unwrap();
        assert_eq!(body, expected[..expected.len().min(MAX_READ as usize)]);
        assert!(
            matches!(response, Response::Data { length, sha256 } if length as usize == body.len() && sha256 == hash(&body))
        );
        let (_, tail) = cache
            .request(Request::Metadata {
                handle: handle.clone(),
                object_name: name.into(),
                offset: expected.len() as u64 - 1,
                length: 10,
            })
            .unwrap();
        assert_eq!(tail, expected[expected.len() - 1..]);
        let (_, eof) = cache
            .request(Request::Metadata {
                handle: handle.clone(),
                object_name: name.into(),
                offset: expected.len() as u64,
                length: 1,
            })
            .unwrap();
        assert!(eof.is_empty());
        assert!(
            cache
                .request(Request::Metadata {
                    handle: handle.clone(),
                    object_name: name.into(),
                    offset: expected.len() as u64 + 1,
                    length: 1,
                })
                .is_err()
        );
    }
    for name in [
        "HEAD.json",
        "format.json",
        "identity.json",
        "../COMMIT.json",
        "directory/files.bin",
        "/files.bin",
        "data/aa/content",
        "files.bin\0",
    ] {
        assert!(!metadata_object_name(name));
        assert!(
            cache
                .request(Request::Metadata {
                    handle: handle.clone(),
                    object_name: name.into(),
                    offset: 0,
                    length: 1,
                })
                .is_err()
        );
    }
    for (offset, length) in [(0, 0), (0, MAX_READ + 1), (u64::MAX, 1), (u64::MAX - 1, 3)] {
        assert!(
            cache
                .request(Request::Metadata {
                    handle: handle.clone(),
                    object_name: "index.bin".into(),
                    offset,
                    length,
                })
                .is_err()
        );
    }
    assert!(
        cache
            .request(Request::Metadata {
                handle: "example:test".into(),
                object_name: "COMMIT.json".into(),
                offset: 0,
                length: 1,
            })
            .is_err()
    );
    assert!(cache.blobs.lock().unwrap().is_empty());
}

#[test]
fn metadata_reader_and_storage_validate_without_io_and_reject_unbound_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let client = Arc::new(
        crate::image::cache::CacheClient::new(
            format!("unix://{}", tmp.path().join("absent.sock").display()),
            None,
        )
        .unwrap(),
    );
    let handle = format!(
        "pvisor-v1:{}:linux-amd64:{}",
        "a".repeat(64),
        "b".repeat(64)
    );
    let other = format!(
        "pvisor-v1:{}:linux-amd64:{}",
        "a".repeat(64),
        "c".repeat(64)
    );
    let local = tmp.path().join("not-created");
    let reader = MetadataReader::new(client.clone(), handle.clone(), Some(local.clone())).unwrap();
    assert!(!local.exists());
    for request in [
        Request::Stat {
            digest: other.clone(),
            path: Vec::new(),
        },
        Request::List {
            digest: other.clone(),
            path: Vec::new(),
            offset: 0,
        },
        Request::Read {
            digest: handle.clone(),
            path: b"small".to_vec(),
            offset: 0,
            length: 1,
        },
        Request::Metadata {
            handle: handle.clone(),
            object_name: "COMMIT.json".into(),
            offset: 0,
            length: 1,
        },
        Request::Open {
            handle: handle.clone(),
            architecture: "amd64".into(),
        },
        Request::Prepare {
            image: "example:test".into(),
            architecture: "amd64".into(),
            refresh: false,
        },
        Request::Ping,
    ] {
        let error = reader.request(request).unwrap_err();
        assert!(
            error.downcast_ref::<std::io::Error>().is_none(),
            "request reached absent socket"
        );
    }
    assert!(MetadataReader::new(client.clone(), "example:test".into(), None).is_err());
    assert!(Storage::metadata(client.clone(), "example:test".into()).is_err());
    let storage = Storage::metadata(client, handle.clone()).unwrap();
    let prefix = Handle::parse(&handle).unwrap().prefix();
    for key in [
        "format.json".into(),
        format!("{prefix}/HEAD.json"),
        format!("{prefix}/../COMMIT.json"),
        format!("{}/COMMIT.json", Handle::parse(&other).unwrap().prefix()),
    ] {
        assert!(storage.get(&key).is_err());
    }
    for name in BINARY_NAMES {
        assert!(storage.get(&format!("{prefix}/{name}")).is_err());
    }
    let key = format!("{prefix}/index.bin");
    for range in [
        0..0,
        std::ops::Range { start: 2, end: 1 },
        0..MAX_READ as u64 + 1,
    ] {
        assert!(storage.range(&key, range).is_err());
    }
    for result in [
        storage.put(&key, vec![1], true),
        storage.put(&key, vec![1], false),
        storage.compare_and_swap(&key, vec![1], None),
    ] {
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
    assert!(!local.exists());
}

#[test]
fn v1_socket_metadata_reader_reuses_binary_pages_and_never_reads_content() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    fs::remove_dir_all(&store.root).unwrap();
    let (server, client) = metadata_socket(tmp.path(), cache, None);
    let storage = Storage::metadata(client.clone(), handle.clone()).unwrap();
    let prefix = Handle::parse(&handle).unwrap().prefix();
    let commit = storage
        .get(&format!("{prefix}/COMMIT.json"))
        .unwrap()
        .unwrap();
    assert!(
        storage
            .range(
                &format!("{prefix}/COMMIT.json"),
                commit.len() as u64 - 1..commit.len() as u64 + 1
            )
            .is_err(),
        "exact storage ranges must reject wire EOF clipping"
    );
    server.requests.lock().unwrap().clear();
    let local = tmp.path().join("client-pages");
    let reader = MetadataReader::new(client.clone(), handle.clone(), Some(local.clone())).unwrap();
    assert!(server.requests.lock().unwrap().is_empty());
    let stat = || Request::Stat {
        digest: handle.clone(),
        path: b"small".to_vec(),
    };
    assert!(matches!(
        reader.request(stat()).unwrap(),
        Response::Metadata { size: 5, .. }
    ));
    let count = server.requests.lock().unwrap().len();
    reader.request(stat()).unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), count);
    assert!(
        matches!(reader.request(Request::List { digest: handle.clone(), path: Vec::new(), offset: 0 }).unwrap(), Response::Entries { names, .. } if names.contains(&b"small".to_vec()))
    );
    let error = reader
        .request(Request::Stat {
            digest: handle.clone(),
            path: b"absent".to_vec(),
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(name, _, length)| metadata_object_name(name) && *length <= MAX_READ)
    );

    // A damaged persistent page is refetched rather than trusted or made ENOENT.
    let parsed = Handle::parse(&handle).unwrap();
    fs::write(
        local
            .join("pages")
            .join(&parsed.revision)
            .join("index.bin/0"),
        b"corrupt",
    )
    .unwrap();
    let count = server.requests.lock().unwrap().len();
    let remount = MetadataReader::new(client, handle, Some(local)).unwrap();
    assert!(matches!(
        remount
            .request(Request::Stat {
                digest: parsed.encode(),
                path: b"small".to_vec()
            })
            .unwrap(),
        Response::Metadata { size: 5, .. }
    ));
    assert!(server.requests.lock().unwrap().len() > count);
}

#[test]
fn socket_metadata_reader_authenticates_commit_catalog_and_binary_pages() {
    for name in ["COMMIT.json", "checksums.bin", "index.bin"] {
        let (tmp, cache, store, image) = fixture();
        let canonical = crate::image::oci::cache_reference("example:test")
            .unwrap()
            .0;
        cache.publish(&store, &image, "amd64", &canonical).unwrap();
        let handle = read_handle(&cache);
        let (_server, client) = metadata_socket(tmp.path(), cache, Some((name, 0)));
        let reader = MetadataReader::new(client, handle.clone(), None).unwrap();
        let error = reader
            .request(Request::Stat {
                digest: handle,
                path: b"small".to_vec(),
            })
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("digest mismatch"),
            "{name}: {error:#}"
        );
        assert!(
            !error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
    }
}

#[test]
fn immutable_open_survives_head_replacement_and_does_not_resolve_an_oci_tag() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let old = read_handle(&cache);
    let mut updated = image.clone();
    updated.env.insert("EXAMPLE".into(), "new-version".into());
    cache
        .publish(&store, &updated, "amd64", &canonical)
        .unwrap();
    assert_ne!(old, read_handle(&cache));
    fs::remove_file(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64")),
    )
    .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    let (response, _) = reader
        .request(Request::Open {
            handle: old.clone(),
            architecture: "amd64".into(),
        })
        .unwrap();
    assert!(
        matches!(response, Response::Prepared { image_handle: handle, env, .. } if handle == old && env["EXAMPLE"] == "value")
    );
    assert!(
        !tmp.path().join("reader").exists(),
        "immutable open never creates OCI extraction state"
    );
    let (_, bytes) = reader
        .request(Request::Read {
            digest: old.clone(),
            path: b"small".to_vec(),
            offset: 0,
            length: 64,
        })
        .unwrap();
    assert_eq!(bytes, b"small");
    assert!(
        reader
            .request(Request::Open {
                handle: old,
                architecture: "arm64".into()
            })
            .is_err()
    );
    assert!(
        reader
            .request(Request::Open {
                handle: "example:test".into(),
                architecture: "amd64".into()
            })
            .is_err()
    );
}
#[test]
fn filesystem_publishing_and_offline_readers_preserve_the_image_contract() {
    let (tmp, publisher, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    let (response, _) = reader
        .request(Request::Prepare {
            image: "docker.io/library/example:test".into(),
            architecture: "amd64".into(),
            refresh: false,
        })
        .unwrap();
    assert!(
        matches!(response, Response::Prepared { digest, metadata_generation, env, .. } if digest == image.digest && metadata_generation.starts_with("sha256:") && env["EXAMPLE"] == "value")
    );
    assert!(
        !tmp.path().join("reader").exists(),
        "warm cache must not initialize local OCI extraction"
    );
    let (_, bytes) = reader
        .request(Request::Read {
            digest: read_handle(&reader),
            path: b"large".to_vec(),
            offset: MAX_READ as u64 - 1,
            length: 25,
        })
        .unwrap();
    assert_eq!(bytes, vec![42; 25]);
    let (_, bytes) = reader
        .request(Request::Read {
            digest: read_handle(&reader),
            path: b"small".to_vec(),
            offset: 100,
            length: 10,
        })
        .unwrap();
    assert!(bytes.is_empty());
    assert!(
        reader
            .request(Request::Read {
                digest: read_handle(&reader),
                path: b"link".to_vec(),
                offset: 0,
                length: 10
            })
            .is_err()
    );
    assert!(
        reader
            .request(Request::Stat {
                digest: read_handle(&reader),
                path: b"link/child".to_vec()
            })
            .is_err()
    );
    for path in [
        b"../small".as_slice(),
        b"/small",
        b"directory/../small",
        b"small\0",
    ] {
        assert!(
            reader
                .request(Request::Stat {
                    digest: read_handle(&reader),
                    path: path.into()
                })
                .is_err()
        );
    }
    let inode = |name: &[u8]| match reader
        .request(Request::Stat {
            digest: read_handle(&reader),
            path: name.into(),
        })
        .unwrap()
        .0
    {
        Response::Metadata { inode, .. } => inode,
        _ => panic!(),
    };
    assert_eq!(inode(b"small"), inode(b"hard"));
}
#[test]
fn corrupt_content_is_not_served_and_read_only_never_prepares_missing_images() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let index = cache
        .load(&Handle::parse(&read_handle(&cache)).unwrap())
        .unwrap();
    let content = index
        .content(index.entry(b"small").unwrap().content.unwrap())
        .unwrap();
    let (blob, _) = index.chunk(content.first).unwrap();
    fs::write(
        tmp.path().join("shared").join(data_key(&blob).unwrap()),
        b"corrupt",
    )
    .unwrap();
    assert!(
        cache
            .request(Request::Read {
                digest: read_handle(&cache),
                path: b"small".to_vec(),
                offset: 0,
                length: 5
            })
            .is_err()
    );
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    for (name, refresh) in [("example:absent", false), ("example:test", true)] {
        let error = reader
            .request(Request::Prepare {
                image: name.into(),
                architecture: "amd64".into(),
                refresh,
            })
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));
    }
    assert!(!tmp.path().join("reader").exists());
}

#[test]
fn directory_pages_preserve_every_host_supported_name() {
    use std::os::unix::ffi::OsStrExt;
    let (tmp, publisher, store, image) = fixture();
    let mut expected = Vec::new();
    for i in 0..600 {
        let name = format!("entry-{i:04}").into_bytes();
        fs::write(
            image
                .rootfs
                .join("directory")
                .join(std::ffi::OsStr::from_bytes(&name)),
            [],
        )
        .unwrap();
        expected.push(name);
    }
    // Linux filesystems support arbitrary filename bytes; macOS APFS rejects
    // invalid UTF-8. The binary codec tests cover these bytes on every host.
    if cfg!(target_os = "linux") {
        let name = b"entry-\xff".to_vec();
        fs::write(
            image
                .rootfs
                .join("directory")
                .join(std::ffi::OsStr::from_bytes(&name)),
            [],
        )
        .unwrap();
        expected.push(name);
    }
    expected.sort();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let mut actual = Vec::new();
    let mut offset = 0;
    loop {
        let (response, _) = reader
            .request(Request::List {
                digest: read_handle(&reader),
                path: b"directory".to_vec(),
                offset,
            })
            .unwrap();
        let Response::Entries {
            names,
            metadata,
            next_offset,
        } = response
        else {
            panic!("missing directory metadata")
        };
        assert!(names.len() <= 256);
        assert_eq!(names.len(), metadata.len());
        assert!(metadata.iter().all(
            |meta| matches!(meta, Response::Metadata { kind, size: 0, .. } if kind == "file")
        ));
        actual.extend(names);
        let Some(next) = next_offset else { break };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(actual, expected);
}

#[test]
fn local_objects_survive_backend_loss_and_corruption_is_refetched() {
    let (tmp, publisher, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    let make_reader = || {
        PortableCache::new(
            Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
            None,
            true,
        )
        .with_local_objects(Some(tmp.path().join("objects")))
    };
    let read = |reader: &PortableCache| {
        reader
            .request(Request::Read {
                digest: read_handle(reader),
                path: b"small".to_vec(),
                offset: 0,
                length: 5,
            })
            .unwrap()
            .1
    };
    let reader = make_reader();
    assert_eq!(read(&reader), b"small");
    let index = reader
        .load(&Handle::parse(&read_handle(&reader)).unwrap())
        .unwrap();
    let content = index
        .content(index.entry(b"small").unwrap().content.unwrap())
        .unwrap();
    let (blob, _) = index.chunk(content.first).unwrap();
    let remote = tmp.path().join("shared").join(data_key(&blob).unwrap());
    let original = fs::read(&remote).unwrap();
    fs::remove_file(&remote).unwrap();
    drop(reader);
    assert_eq!(
        read(&make_reader()),
        b"small",
        "a new reader must reuse local objects"
    );
    fs::write(&remote, &original).unwrap();
    let local = tmp.path().join("objects/immutable").join(&blob[7..]);
    fs::write(&local, b"corrupt").unwrap();
    assert_eq!(read(&make_reader()), b"small");
    assert_eq!(fs::read(local).unwrap(), original);
}

#[test]
fn failed_publication_never_exposes_an_image_reference() {
    let (tmp, publisher, store, image) = fixture();
    fs::create_dir_all(tmp.path().join("shared")).unwrap();
    fs::create_dir(tmp.path().join("outside")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside"), tmp.path().join("shared/data")).unwrap();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    assert!(
        publisher
            .publish(&store, &image, "amd64", &canonical)
            .is_err()
    );
    assert!(
        !tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64"))
            .exists()
    );

    assert_eq!(fs::read_dir(tmp.path().join("outside")).unwrap().count(), 0);
}

#[test]
fn independent_images_share_file_chunks_and_old_revisions_remain_readable() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let old = read_handle(&cache);
    let object_paths = || {
        fn walk(root: &std::path::Path, paths: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(root).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, paths);
                } else {
                    paths.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&tmp.path().join("shared/data"), &mut paths);
        paths.sort();
        paths
    };
    let initial = object_paths();
    let other = crate::image::oci::cache_reference("other:test").unwrap().0;
    cache.publish(&store, &image, "amd64", &other).unwrap();
    cache.publish(&store, &image, "arm64", &canonical).unwrap();
    assert_eq!(
        object_paths(),
        initial,
        "identical files/chunks must be shared across images and platforms"
    );
    assert_ne!(image_key(&other), image_key(&canonical));
    assert!(
        tmp.path()
            .join("shared")
            .join(head_key(&other, "linux-amd64"))
            .exists()
    );
    assert!(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-arm64-v8"))
            .exists()
    );
    let mut image = image;
    fs::write(image.rootfs.join("small"), b"changed").unwrap();
    let next_digest = format!("sha256:{}", "b".repeat(64));
    let next_root = store.root.join("rootfs-v3/sha256").join(&next_digest[7..]);
    fs::rename(&image.rootfs, &next_root).unwrap();
    image.rootfs = next_root;
    image.digest = next_digest;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let new = read_handle(&cache);
    assert_ne!(old, new);
    assert_eq!(object_paths().len(), initial.len() + 1);
    let cold = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    for (handle, expected) in [(old, b"small".as_slice()), (new, b"changed".as_slice())] {
        let bytes = cold
            .request(Request::Read {
                digest: handle,
                path: b"small".into(),
                offset: 0,
                length: 64,
            })
            .unwrap()
            .1;
        assert_eq!(bytes, expected);
    }
    assert!(
        cold.request(Request::Stat {
            digest: image.digest,
            path: b"small".into()
        })
        .is_err(),
        "v1 must not introduce a global digest-only index"
    );
}

#[test]
fn stale_publishers_cannot_replace_head_and_corrupt_existing_objects_are_not_reused() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    let first = cache.observe(&canonical, "linux-amd64").unwrap();
    let second = cache.observe(&canonical, "linux-amd64").unwrap();
    cache
        .publish_observed(&store, &image, "amd64", &canonical, first)
        .unwrap();
    let head = fs::read(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64")),
    )
    .unwrap();
    let error = cache
        .publish_observed(&store, &image, "amd64", &canonical, second)
        .unwrap_err();
    assert!(super::super::storage::is_conflict(&error));
    assert_eq!(
        fs::read(
            tmp.path()
                .join("shared")
                .join(head_key(&canonical, "linux-amd64"))
        )
        .unwrap(),
        head
    );
    let data = tmp
        .path()
        .join("shared")
        .join(data_key(&hash(b"small")).unwrap());
    fs::write(data, b"corrupt").unwrap();
    assert!(
        cache
            .publish(&store, &image, "amd64", &canonical)
            .unwrap_err()
            .to_string()
            .contains("digest mismatch")
    );
    assert_eq!(
        fs::read(
            tmp.path()
                .join("shared")
                .join(head_key(&canonical, "linux-amd64"))
        )
        .unwrap(),
        head
    );
}

#[test]
fn cold_start_fetches_pages_instead_of_deserializing_the_complete_file_tree() {
    let (tmp, cache, store, image) = fixture();
    for i in 0..5000 {
        fs::write(image.rootfs.join("directory").join(format!("f-{i:05}")), []).unwrap();
    }
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&Handle::parse(&handle).unwrap()).unwrap();
    assert!(
        loaded.cached_page_count() <= 5,
        "startup must only fetch headers, root metadata and index root"
    );
    assert!(loaded.commit.metadata["index.bin"].bytes > 20 * binary::PAGE_BYTES as u64);
    assert!(matches!(
        reader
            .request(Request::Stat {
                digest: handle.clone(),
                path: b"directory/f-04999".into()
            })
            .unwrap()
            .0,
        Response::Metadata { size: 0, .. }
    ));
    assert!(
        loaded.cached_page_count() <= 10,
        "one lookup must not populate a complete-image index"
    );
    let mut offset = 0;
    let mut count = 0;
    loop {
        let Response::Entries {
            names, next_offset, ..
        } = reader
            .request(Request::List {
                digest: handle.clone(),
                path: b"directory".into(),
                offset,
            })
            .unwrap()
            .0
        else {
            panic!()
        };
        count += names.len();
        let Some(next) = next_offset else { break };
        offset = next;
    }
    assert_eq!(count, 5000);
}

#[test]
fn decoded_index_nodes_are_reused_for_positive_and_negative_lookups() {
    let (tmp, cache, store, image) = fixture();
    for i in 0..600 {
        fs::write(image.rootfs.join("directory").join(format!("f-{i:05}")), []).unwrap();
    }
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = Handle::parse(&read_handle(&cache)).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&handle).unwrap();
    let check = || {
        for path in [
            b"directory/f-00000".as_slice(),
            b"directory/f-00599",
            b"link",
            b"hard",
        ] {
            assert_eq!(loaded.entry(path).unwrap().path, path);
        }
        for path in [
            b"directory/f-00232x".as_slice(),
            b"directory/z-missing",
            b"missing",
        ] {
            let error = loaded.entry(path).unwrap_err();
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::NotFound
            );
        }
        assert!(matches!(loaded.entry(b"link").unwrap().metadata,
            Response::Metadata { target: Some(target), .. } if target == b"small"));
        assert!(
            loaded
                .entry(b"small/child")
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
    };
    check();
    let before = loaded.cached_nodes();
    assert!(before.len() >= 3, "exercise internal and leaf nodes");
    check();
    let after = loaded.cached_nodes();
    assert_eq!(before.len(), after.len());
    for (id, node, _) in &before {
        let (_, reused, _) = after.iter().find(|(other, _, _)| other == id).unwrap();
        assert!(Arc::ptr_eq(node, reused), "node {id} was decoded again");
    }

    // Raw and decoded entries share this capacity, including during re-admission.
    loaded.resize_metadata_cache(1);
    for _ in 0..2 {
        check();
        assert!(loaded.cached_page_count() <= 1);
    }
    let after_eviction = loaded.cached_nodes();
    for (id, node, _) in after_eviction {
        if let Some((_, old, _)) = before.iter().find(|(other, _, _)| *other == id) {
            assert!(
                !Arc::ptr_eq(old, &node),
                "evicted node must be decoded anew"
            );
        }
    }
}

#[test]
fn decoded_index_node_budget_is_bounded_and_survives_eviction_until_last_arc() {
    const CHILD: &str = "PVISOR_TEST_DECODED_NODE_BUDGET";
    let Some(mode) = std::env::var_os(CHILD) else {
        for mode in ["retained", "exhausted"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "image::cache::portable::tests::decoded_index_node_budget_is_bounded_and_survives_eviction_until_last_arc", "--nocapture"])
                .env(CHILD, mode).output().unwrap();
            assert!(
                output.status.success(),
                "{mode}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = Handle::parse(&read_handle(&cache)).unwrap();
    drop(cache);
    let limit = if mode == "exhausted" {
        1
    } else {
        8 * binary::PAGE_BYTES
    };
    crate::cache_budget::configure(limit).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&handle).unwrap();
    if mode == "exhausted" {
        for _ in 0..2 {
            assert_eq!(loaded.entry(b"small").unwrap().path, b"small");
            assert!(
                loaded
                    .entry(b"missing")
                    .unwrap_err()
                    .downcast_ref::<std::io::Error>()
                    .is_some()
            );
            assert_eq!(loaded.cached_page_count(), 0);
        }
        let (used, actual_limit, misses) = crate::cache_budget::stats();
        assert_eq!((used, actual_limit), (0, limit));
        assert!(misses > 0);
    } else {
        let mut nodes = loaded.cached_nodes();
        assert_eq!(nodes.len(), 1);
        let (_, held, bytes) = nodes.pop().unwrap();
        assert!(bytes > 0 && bytes < binary::PAGE_BYTES);
        loaded.resize_metadata_cache(1);
        assert_eq!(crate::cache_budget::stats().0, bytes);
        // Root file reads evict the index node, but the traversal's Arc still owns its charge.
        loaded.entry(b"").unwrap();
        assert!(loaded.cached_nodes().is_empty());
        let used = crate::cache_budget::stats().0;
        assert!(used >= bytes && used <= limit);
        loaded.entry(b"small").unwrap();
        assert!(crate::cache_budget::stats().0 <= limit);
        drop(loaded);
        drop(reader);
        assert_eq!(crate::cache_budget::stats().0, bytes);
        drop(held);
        assert_eq!(crate::cache_budget::stats().0, 0);
    }
}

#[test]
fn held_raw_pages_keep_budget_charge_after_eviction_and_decoded_replacement() {
    const CHILD: &str = "PVISOR_TEST_RAW_PAGE_CHARGE";
    let Some(mode) = std::env::var_os(CHILD) else {
        for mode in ["eviction", "replacement"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "image::cache::portable::tests::held_raw_pages_keep_budget_charge_after_eviction_and_decoded_replacement", "--nocapture"])
                .env(CHILD, mode).output().unwrap();
            assert!(
                output.status.success(),
                "{mode}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = Handle::parse(&read_handle(&cache)).unwrap();
    drop(cache);
    let limit = 8 * binary::PAGE_BYTES;
    crate::cache_budget::configure(limit).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&handle).unwrap();
    loaded.clear_metadata_cache();
    assert_eq!(crate::cache_budget::stats().0, 0);
    let (held, bytes) = loaded.hold_raw_page("index.bin", 1);
    let concurrent_reader = held.clone();
    assert!(bytes >= binary::PAGE_BYTES);
    assert_eq!(crate::cache_budget::stats().0, bytes);
    if mode == "eviction" {
        loaded.clear_metadata_cache();
        assert_eq!(loaded.cached_page_count(), 0);
        assert_eq!(crate::cache_budget::stats().0, bytes);
    } else {
        // Decode the same raw page while another reader retains its shared owner.
        loaded.entry(b"small").unwrap();
        let nodes = loaded.cached_nodes();
        assert_eq!(nodes.len(), 1);
        assert!(crate::cache_budget::stats().0 >= bytes + nodes[0].2);
    }
    assert!(crate::cache_budget::stats().0 <= limit);
    drop(loaded);
    drop(reader);
    assert_eq!(crate::cache_budget::stats().0, bytes);
    drop(held);
    assert_eq!(crate::cache_budget::stats().0, bytes);
    drop(concurrent_reader);
    assert_eq!(crate::cache_budget::stats().0, 0);
}

#[test]
fn evicted_decoded_index_nodes_reauthenticate_pages_before_reuse() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = Handle::parse(&read_handle(&cache)).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&handle).unwrap();
    loaded.entry(b"small").unwrap();
    let remote = tmp
        .path()
        .join("shared")
        .join(handle.prefix())
        .join("index.bin");
    let mut bytes = fs::read(&remote).unwrap();
    bytes[binary::PAGE_BYTES + 24] ^= 1;
    fs::write(remote, bytes).unwrap();
    // An already authenticated immutable node is unaffected by backing-file changes.
    loaded.entry(b"small").unwrap();
    loaded.resize_metadata_cache(1);
    loaded.entry(b"").unwrap();
    assert!(loaded.cached_nodes().is_empty());
    for _ in 0..2 {
        assert!(
            loaded
                .entry(b"small")
                .unwrap_err()
                .to_string()
                .contains("page digest mismatch")
        );
        assert!(
            loaded.cached_nodes().is_empty(),
            "failed authentication must not admit a node"
        );
    }
}

#[test]
fn cached_index_nodes_do_not_bypass_index_file_relationship_validation() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = Handle::parse(&read_handle(&cache)).unwrap();
    let storage = Storage::filesystem(tmp.path().join("shared"), false).unwrap();
    let mut commit: Commit = serde_json::from_slice(
        &storage
            .get(&format!("{}/COMMIT.json", handle.prefix()))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let mut objects: BTreeMap<String, Vec<u8>> = commit
        .metadata
        .keys()
        .map(|name| {
            (
                name.clone(),
                storage
                    .get(&format!("{}/{name}", handle.prefix()))
                    .unwrap()
                    .unwrap(),
            )
        })
        .collect();
    let index = objects.get_mut("index.bin").unwrap();
    let count = u32::from_le_bytes(
        index[binary::PAGE_BYTES + 4..binary::PAGE_BYTES + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    let find_record = |name: &[u8]| {
        (0..count)
            .map(|slot| binary::PAGE_BYTES + 16 + slot * 280)
            .find(|start| &index[*start + 18..*start + 18 + name.len()] == name)
            .unwrap()
    };
    let small = find_record(b"small");
    let large = find_record(b"large");
    let wrong_file: [u8; 8] = index[large + 8..large + 16].try_into().unwrap();
    index[small + 8..small + 16].copy_from_slice(&wrong_file);
    // Re-seal the malicious, structurally valid index through the real trust chain.
    let mut checksums = objects["checksums.bin"][..80].to_vec();
    for name in BINARY_NAMES {
        for page in objects[name].chunks(binary::PAGE_BYTES) {
            checksums.extend_from_slice(&Sha256::digest(page));
        }
    }
    objects.insert("checksums.bin".into(), checksums);
    for (name, bytes) in &objects {
        commit.metadata.get_mut(name).unwrap().sha256 = hash(bytes);
    }
    let commit_bytes = serde_json::to_vec(&commit).unwrap();
    let malicious = Handle {
        revision: hash(&commit_bytes)[7..].into(),
        ..handle
    };
    for (name, bytes) in objects {
        storage
            .put(&format!("{}/{name}", malicious.prefix()), bytes, true)
            .unwrap();
    }
    storage
        .put(
            &format!("{}/COMMIT.json", malicious.prefix()),
            commit_bytes,
            true,
        )
        .unwrap();
    let reader = PortableCache::new(storage, None, true);
    let loaded = reader.load_revision(&malicious).unwrap();
    let before = loaded.cached_nodes();
    assert_eq!(before.len(), 1);
    for _ in 0..2 {
        assert!(
            loaded
                .entry(b"small")
                .unwrap_err()
                .to_string()
                .contains("index/file relationship")
        );
        assert!(
            loaded
                .list(b"", 0)
                .unwrap_err()
                .to_string()
                .contains("directory index relationship"),
            "warm listings must recheck the cached index against file records"
        );
    }
    let after = loaded.cached_nodes();
    assert!(Arc::ptr_eq(&before[0].1, &after[0].1));
}

#[test]
fn corrupt_remote_pages_fail_and_corrupt_local_pages_are_refetched() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    let parsed = Handle::parse(&handle).unwrap();
    let make = || {
        PortableCache::new(
            Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
            None,
            true,
        )
        .with_local_objects(Some(tmp.path().join("pages")))
    };
    let read = |reader: &PortableCache| {
        reader.request(Request::Stat {
            digest: handle.clone(),
            path: b"small".into(),
        })
    };
    read(&make()).unwrap();
    let local = tmp
        .path()
        .join("pages/pages")
        .join(&parsed.revision)
        .join("index.bin/1");
    fs::write(&local, b"bad").unwrap();
    read(&make()).unwrap();
    assert_eq!(
        fs::metadata(&local).unwrap().len(),
        binary::PAGE_BYTES as u64
    );
    fs::remove_dir_all(tmp.path().join("pages")).unwrap();
    let remote = tmp
        .path()
        .join("shared")
        .join(parsed.prefix())
        .join("index.bin");
    use std::io::{Seek, SeekFrom, Write};
    let mut file = fs::OpenOptions::new().write(true).open(remote).unwrap();
    file.seek(SeekFrom::Start(binary::PAGE_BYTES as u64 + 24))
        .unwrap();
    file.write_all(b"corrupt").unwrap();
    assert!(
        read(&make())
            .unwrap_err()
            .to_string()
            .contains("page digest mismatch")
    );
}

#[test]
fn publication_uses_v1_for_controls_handles_and_binary_magic() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    assert!(handle.starts_with("pvisor-v1:"));
    let parsed = Handle::parse(&handle).unwrap();
    assert_eq!(parsed.encode(), handle);
    let root = tmp.path().join("shared");
    for key in [
        "format.json".to_string(),
        format!("meta/{}/identity.json", parsed.image_key),
        head_key(&canonical, &parsed.platform),
        format!("{}/COMMIT.json", parsed.prefix()),
        format!("{}/manifest.json", parsed.prefix()),
        format!("{}/config.json", parsed.prefix()),
    ] {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(key)).unwrap()).unwrap();
        assert_eq!(record["format_version"], 1);
    }
    for name in BINARY_NAMES {
        let bytes = fs::read(root.join(parsed.prefix()).join(name)).unwrap();
        assert_eq!(&bytes[..8], b"PVICB1\0\0");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
    }
    let checksums = fs::read(root.join(parsed.prefix()).join("checksums.bin")).unwrap();
    assert_eq!(&checksums[..8], b"PVICH1\0\0");
    let old_handle = handle.replacen("pvisor-v1:", "pvisor-v2:", 1);
    assert!(
        cache
            .request(Request::Stat {
                digest: old_handle,
                path: b"small".into(),
            })
            .unwrap_err()
            .to_string()
            .contains("expected immutable pvisor-v1")
    );
    let head_bytes = fs::read(root.join(head_key(&canonical, &parsed.platform))).unwrap();
    let mut head: serde_json::Value = serde_json::from_slice(&head_bytes).unwrap();
    head["format_version"] = 2.into();
    assert!(
        decode_head(
            &serde_json::to_vec(&head).unwrap(),
            &parsed.image_key,
            &parsed.platform,
        )
        .is_err()
    );
    let mut format: serde_json::Value = serde_json::from_slice(FORMAT).unwrap();
    format["format_version"] = 2.into();
    fs::write(
        root.join("format.json"),
        serde_json::to_vec(&format).unwrap(),
    )
    .unwrap();
    assert!(
        cache
            .request(Request::Ping)
            .unwrap_err()
            .to_string()
            .contains("unsupported cache format")
    );
}

#[test]
fn remote_backend_uses_opaque_portable_cursors_with_local_resume_and_eviction() {
    use crate::image::cache::backend::RemoteFs;
    use crate::image::cache::{CacheBackend, CacheClient, CacheConfig};
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let (tmp, publisher, store, image) = fixture();
    let mut expected = vec![OsString::from("."), OsString::from("..")];
    for index in 0..2500 {
        let name = format!("f-{index:04}");
        fs::write(image.rootfs.join("directory").join(&name), [index as u8]).unwrap();
        expected.push(OsString::from(name));
    }
    if cfg!(target_os = "linux") {
        let name = OsString::from_vec(b"f-\xff".to_vec());
        fs::write(image.rootfs.join("directory").join(&name), b"bytes").unwrap();
        expected.push(name);
    }
    fs::hard_link(
        image.rootfs.join("directory/f-0000"),
        image.rootfs.join("directory/linked"),
    )
    .unwrap();
    expected.push(OsString::from("linked"));
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    let handle = read_handle(&publisher);
    // This is the production binary reader, not the ordinal source test server.
    let client = CacheClient::from_config(CacheConfig {
        backend: CacheBackend::Filesystem,
        location: tmp.path().join("shared").display().to_string(),
        read_only: true,
        image_store: None,
    })
    .unwrap();
    let mut remote = RemoteFs::new(
        client,
        handle.clone(),
        tmp.path().join("blocks"),
        Some(tmp.path().join("metadata")),
        false,
    )
    .unwrap();
    let directory = remote.child(1, OsStr::new("directory")).unwrap();
    let (response, _) = remote
        .client
        .request(Request::List {
            digest: handle,
            path: b"directory".to_vec(),
            offset: 0,
        })
        .unwrap();
    let Response::Entries {
        names,
        next_offset: Some(cursor),
        ..
    } = response
    else {
        panic!("expected multipage inventory");
    };
    assert_ne!(
        cursor,
        names.len(),
        "portable continuation is not an ordinal"
    );
    let first = remote.entries_page(directory.attr.ino, 0).unwrap();
    assert_eq!(
        remote.entries_page(directory.attr.ino, 17).unwrap(),
        first[17..]
    );
    let all = remote.entries(directory.attr.ino).unwrap();
    assert_eq!(
        all.iter()
            .map(|(_, _, name)| name.clone())
            .collect::<Vec<_>>(),
        expected
    );
    // More than eight pages have evicted the prefix. Local cookie zero and a
    // cookie inside an evicted page must replay valid remote boundaries.
    assert_eq!(remote.entries_page(directory.attr.ino, 0).unwrap(), first);
    let resumed = remote.entries_page(directory.attr.ino, 333).unwrap();
    assert_eq!(resumed, all[333..333 + resumed.len()]);
    assert!(
        remote
            .entries_page(directory.attr.ino, all.len())
            .unwrap()
            .is_empty()
    );
    let original = remote
        .child(directory.attr.ino, OsStr::new("f-0000"))
        .unwrap();
    let linked = remote
        .child(directory.attr.ino, OsStr::new("linked"))
        .unwrap();
    assert_eq!(original.attr.ino, linked.attr.ino);
    assert_eq!(original.override_stat, linked.override_stat);
    assert_eq!(original.attr.mtime, linked.attr.mtime);
    assert_eq!(
        remote.downloads.lock().unwrap().snapshot().downloaded_bytes,
        0,
        "portable metadata enumeration must not fetch content"
    );
    if cfg!(target_os = "linux") {
        let bytes = remote
            .child(directory.attr.ino, OsStr::from_bytes(b"f-\xff"))
            .unwrap();
        assert_eq!(remote.read_range(bytes.attr.ino, 0, 10).unwrap(), b"bytes");
    }
    assert!(
        remote
            .child(directory.attr.ino, OsStr::new("missing"))
            .is_err()
    );
    assert!(remote.child(1, OsStr::new("link")).is_ok());
}

#[test]
fn removed_packed_layout_is_not_a_readable_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::filesystem(tmp.path().into(), false).unwrap();
    storage
        .put("v1/format", b"pvisor-cache-v1\n".to_vec(), true)
        .unwrap();
    let cache = PortableCache::new(storage, None, true);
    assert!(
        cache
            .prepare("example:test", "amd64", false)
            .unwrap_err()
            .to_string()
            .contains("image absent from read-only cache")
    );
    assert!(
        cache
            .request(Request::Stat {
                digest: format!("sha256:{}", "a".repeat(64)),
                path: b"small".into(),
            })
            .unwrap_err()
            .to_string()
            .contains("expected immutable pvisor-v1")
    );
}
