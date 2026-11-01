use super::*;
use pvisor_overlay_core::{OverlayCore, backend};
use std::{collections::BTreeMap, fs};

#[test]
fn public_export_rejects_private_predecessor_slots() {
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
    let error = super::super::portable::export_pool(&pool, &handle, &destination).unwrap_err();
    assert!(
        error.to_string().contains("content from another revision"),
        "{error:#}"
    );
    assert!(
        !destination.exists(),
        "reject before copying any private bytes"
    );
    let fresh = tmp.path().join("fresh");
    let handle =
        publish_local_image(&fresh, &source, &image, "example:visibility", "amd64").unwrap();
    super::super::portable::export_pool(&fresh, &handle, &destination).unwrap();
}

/// Explicit special runner: installed Kubo, isolated loopback-only test network.
#[test]
#[ignore = "requires PVISOR_TEST_IPFS and PVISOR_TEST_DAEMON executables"]
fn real_kubo_peers_serve_lazy_ranges_without_a_local_pool() {
    use std::{
        io::BufRead,
        process::{Child, Command, Stdio},
        time::Duration,
    };
    struct Kubo {
        executable: PathBuf,
        repo: PathBuf,
        gateway: u16,
        child: Option<Child>,
        log: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Kubo {
        fn drop(&mut self) {
            if let Some(child) = &mut self.child {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(log) = self.log.take() {
                let _ = log.join();
            }
        }
    }
    impl Kubo {
        fn command(&self, args: &[&str]) -> Vec<u8> {
            let result = Command::new(&self.executable)
                .env("IPFS_PATH", &self.repo)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            result.stdout
        }
        fn new(repo: PathBuf) -> Self {
            let executable = std::env::var_os("PVISOR_TEST_IPFS")
                .expect("set PVISOR_TEST_IPFS")
                .into();
            let port = || {
                std::net::TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
                    .port()
            };
            let node = Self {
                executable,
                repo,
                gateway: port(),
                child: None,
                log: None,
            };
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&node.repo)
                .unwrap();
            if !node.repo.join("config").exists() {
                node.command(&["init", "--profile=test"]);
            }
            node.command(&[
                "config",
                "Addresses.API",
                &format!("/ip4/127.0.0.1/tcp/{}", port()),
            ]);
            node.command(&[
                "config",
                "Addresses.Gateway",
                &format!("/ip4/127.0.0.1/tcp/{}", node.gateway),
            ]);
            node.command(&[
                "config",
                "--json",
                "Addresses.Swarm",
                r#"["/ip4/127.0.0.1/tcp/0"]"#,
            ]);
            node.command(&["config", "--json", "Bootstrap", "[]"]);
            node.command(&["config", "--json", "Discovery.MDNS.Enabled", "false"]);
            node
        }
        fn start(&mut self) {
            let mut child = Command::new(&self.executable)
                .env("IPFS_PATH", &self.repo)
                .args(["daemon", "--routing=none"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            self.child = Some(child);
            let (ready, initialized) = std::sync::mpsc::channel();
            self.log = Some(std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let line = line.unwrap();
                    if line.trim() == "Daemon is ready" {
                        let _ = ready.send(());
                    }
                }
            }));
            initialized
                .recv_timeout(Duration::from_secs(30))
                .expect("Kubo readiness");
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let publisher = temporary.path().join("publisher");
    drop(crate::docker::Store::open(publisher.clone()).unwrap());
    let location = publisher.join("images/pool-v2");
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let digest = format!("sha256:{}", "9".repeat(64));
    let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&rootfs).unwrap();
    fs::write(rootfs.join("hello"), b"hello from peer").unwrap();
    let mut random = 17u64;
    let untouched: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random as u8
        })
        .collect();
    fs::write(rootfs.join("untouched"), untouched).unwrap();
    let image = PreparedImage {
        rootfs,
        digest: digest.clone(),
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let handle =
        publish_local_image(&location, &source, &image, "example:real-ipfs", "amd64").unwrap();
    let publisher_images = temporary.path().join("publisher-images");
    fs::create_dir(&publisher_images).unwrap();
    let manifest = publisher_images.join("peer-image.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "rootfs": publisher, "entrypoint": [], "cmd": [], "env": {}, "native_guest": true,
            "pooled_image": {
                "reference": "example:real-ipfs", "id": format!("sha256:{}", "8".repeat(64)),
                "digest": digest, "platform": "linux/amd64",
                "cache": {"location": location, "handle": handle},
                "env": {}, "entrypoint": [], "cmd": [], "cwd": "/",
                "user": "", "stop_signal": 15, "volumes": [], "created_ms": 0
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let daemon = std::env::var_os("PVISOR_TEST_DAEMON").expect("set PVISOR_TEST_DAEMON");
    let executable = PathBuf::from(std::env::var_os("PVISOR_TEST_IPFS").unwrap());
    let mut paths = vec![executable.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();
    let run_daemon = |args: &[&str]| {
        let result = Command::new(&daemon)
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    };
    let status = run_daemon(&[
        "image",
        "peer-image",
        "--images-dir",
        publisher_images.to_str().unwrap(),
    ]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status).unwrap()["visibility"],
        "private"
    );
    assert!(
        !publisher_images.join(".ipfs").exists(),
        "inspection must not initialize Kubo"
    );
    let result = run_daemon(&[
        "image",
        "peer-image",
        "--images-dir",
        publisher_images.to_str().unwrap(),
        "--public",
    ]);
    let result: serde_json::Value = serde_json::from_slice(&result).unwrap();
    let cid = result["cid"].as_str().unwrap();
    fs::copy(&manifest, publisher_images.join("peer-alias.json")).unwrap();
    let alias = run_daemon(&[
        "image",
        "peer-alias",
        "--images-dir",
        publisher_images.to_str().unwrap(),
        "--public",
    ]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&alias).unwrap()["cid"],
        cid
    );
    // The first public operation initializes the hidden Kubo repository and
    // chooses local ports itself. Configure only this test's network before start.
    let mut seed = Kubo::new(publisher_images.join(".ipfs"));
    seed.start();
    let images = temporary.path().join("receiver-images");
    let mut receiver = Kubo::new(images.join(".ipfs"));
    receiver.start();
    assert!(
        String::from_utf8(seed.command(&["pin", "ls", "--type=recursive", "--quiet"]))
            .unwrap()
            .lines()
            .any(|line| line == cid)
    );
    // Kubo init may pin its welcome directory. Reads must add no new pins.
    let pins_before = receiver.command(&["pin", "ls", "--type=recursive", "--quiet"]);
    let id: serde_json::Value = serde_json::from_slice(&seed.command(&["id"])).unwrap();
    let peer = id["Addresses"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .find(|v| v.starts_with("/ip4/127.0.0.1/tcp/"))
        .expect("loopback peer");
    receiver.command(&["swarm", "connect", peer]);
    run_daemon(&[
        "image",
        "peer-image",
        "--images-dir",
        images.to_str().unwrap(),
        "--from",
        cid,
    ]);
    fs::remove_dir_all(&publisher).unwrap();
    fs::remove_dir_all(&source.root).unwrap();
    let installed: serde_json::Value =
        serde_json::from_slice(&fs::read(images.join("peer-image.json")).unwrap()).unwrap();
    let remote: crate::docker::Image =
        serde_json::from_value(installed["pooled_image"].clone()).unwrap();
    assert!(!remote.cache.location.exists());
    let store =
        crate::docker::Store::open(PathBuf::from(installed["rootfs"].as_str().unwrap())).unwrap();
    let owner = store.open_image(&remote).unwrap();
    assert_eq!(
        backend::read_at(&owner.rootfs().join("hello"), 6, 9)
            .unwrap()
            .unwrap(),
        b"from peer"
    );
    assert!(!owner.rootfs().join("untouched").exists());
    let stats: serde_json::Value =
        serde_json::from_slice(&receiver.command(&["bitswap", "stat", "--enc=json"])).unwrap();
    let received = stats["DataReceived"]
        .as_u64()
        .expect("Bitswap received-byte counter");
    assert!(
        received > 0 && received < 1024 * 1024,
        "must fetch from peer without fetching the 2 MiB unused file: {stats}"
    );
    assert_eq!(
        receiver.command(&["pin", "ls", "--type=recursive", "--quiet"]),
        pins_before,
        "receiver must not recursively pin the image"
    );
    // Publication keeps the local image local; visibility is revision-scoped.
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert!(original["pooled_image"]["cache"]["repository"].is_null());
    let mut replacement = original;
    replacement["pooled_image"]["cache"]["handle"] = format!(
        "pvisor-v2:{}:linux-amd64:{}",
        "6".repeat(64),
        "7".repeat(64)
    )
    .into();
    fs::write(&manifest, serde_json::to_vec(&replacement).unwrap()).unwrap();
    let status = run_daemon(&[
        "image",
        "peer-image",
        "--images-dir",
        publisher_images.to_str().unwrap(),
    ]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status).unwrap()["visibility"],
        "private",
        "new revisions must not inherit public permission"
    );
    let private = run_daemon(&[
        "image",
        "peer-image",
        "--images-dir",
        publisher_images.to_str().unwrap(),
        "--private",
    ]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&private).unwrap()["visibility"],
        "private"
    );
    assert!(
        String::from_utf8(seed.command(&["pin", "ls", "--type=recursive", "--quiet"]))
            .unwrap()
            .lines()
            .any(|line| line == cid),
        "another public name must retain the pin"
    );
    run_daemon(&[
        "image",
        "peer-alias",
        "--images-dir",
        publisher_images.to_str().unwrap(),
        "--private",
    ]);
    assert!(
        !String::from_utf8(seed.command(&["pin", "ls", "--type=recursive", "--quiet"]))
            .unwrap()
            .lines()
            .any(|line| line == cid)
    );
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
    fs::write(rootfs.join("hello"), b"hello from IPFS").unwrap();
    fs::write(rootfs.join("untouched"), vec![42; 2 * 1024 * 1024]).unwrap();
    let image = PreparedImage {
        rootfs,
        digest: digest.clone(),
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let handle = publish_local_image(&location, &source, &image, "example:ipfs", "amd64").unwrap();
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

    // A deterministic UnixFS gateway stand-in, not evidence of P2P transport.
    let requests = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
    let observed = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!(
        "http://{}/ipfs/{}",
        listener.local_addr().unwrap(),
        "b".repeat(59)
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
                                let key = path.splitn(3, '/').nth(2).unwrap();
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
    struct Gateway(
        Option<tokio::sync::oneshot::Sender<()>>,
        Option<std::thread::JoinHandle<()>>,
    );
    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
            self.1.take().unwrap().join().unwrap();
        }
    }
    let _gateway = Gateway(Some(stop), Some(task));
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
        2,
        "two revisions share the same two content objects"
    );
}
