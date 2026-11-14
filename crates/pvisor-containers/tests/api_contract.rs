use pvisor_containers::api::{Containers, Network, Run, Store};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

fn digest(data: &[u8]) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

struct Registry {
    image: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Registry {
    fn new(corrupt: bool) -> Self {
        Self::with_stop_signal(corrupt, "SIGTERM")
    }
    fn with_stop_signal(corrupt: bool, stop_signal: &str) -> Self {
        let config=serde_json::to_vec(&serde_json::json!({
            "architecture":"amd64","os":"linux",
            "config":{"Entrypoint":["/bin/echo"],"Cmd":["image default"],"Env":["PATH=/bin"],"WorkingDir":"/","StopSignal":stop_signal}
        })).unwrap();
        let layer = b"compressed layer bytes are verified, not extracted by the host".to_vec();
        let cd = digest(&config);
        let ld = digest(&layer);
        let manifest=serde_json::to_vec(&serde_json::json!({
            "schemaVersion":2,
            "config":{"digest":cd,"size":config.len(),"mediaType":"application/vnd.oci.image.config.v1+json"},
            "layers":[{"digest":ld,"size":layer.len(),"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip"}]
        })).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let image = format!("{}/fixture:latest", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let ended = stop.clone();
        let worker = thread::spawn(move || {
            while !ended.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Winsock inherits the listener's nonblocking mode;
                        // Linux accept does not. This fixture handles one full
                        // request synchronously before closing the connection.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                            .unwrap();
                        let mut data = Vec::new();
                        let mut byte = [0; 1];
                        while !data.ends_with(b"\r\n\r\n") && data.len() < 8192 {
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            data.push(byte[0]);
                        }
                        let request = String::from_utf8_lossy(&data);
                        let (status, body) = if request
                            .starts_with("GET /v2/fixture/manifests/latest ")
                            || request.starts_with("GET /v2/fixture/manifests/alias ")
                        {
                            ("200 OK", manifest.clone())
                        } else if request.contains(&format!("/blobs/{cd} ")) {
                            ("200 OK", config.clone())
                        } else if request.contains(&format!("/blobs/{ld} ")) {
                            (
                                "200 OK",
                                if corrupt {
                                    vec![b'!'; layer.len()]
                                } else {
                                    layer.clone()
                                },
                            )
                        } else {
                            ("404 Not Found", vec![])
                        };
                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            image,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn defaults_and_empty_catalog_require_no_vm() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(dir.path().join("store"))).unwrap();
    assert!(store.images().unwrap().is_empty());
    assert!(store.list(true).unwrap().is_empty());
    assert!(
        store
            .stop("missing", std::time::Duration::MAX, Some("SIGUNKNOWN"))
            .unwrap_err()
            .to_string()
            .contains("unsupported image stop signal")
    );
    assert!(
        store
            .stop("missing", std::time::Duration::MAX, Some("0"))
            .unwrap_err()
            .to_string()
            .contains("invalid signal")
    );
    let request = Run::default();
    assert_eq!(request.network, Network::Bridge);
    assert!(!request.interactive && request.tty.is_none());
    let mut legacy = serde_json::to_value(&request).unwrap();
    let fields = legacy.as_object_mut().unwrap();
    for key in ["network", "interactive", "tty"] {
        fields.remove(key);
    }
    let legacy: Run = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.network, Network::None);
    assert!(!legacy.interactive && legacy.tty.is_none());
    assert_eq!((request.cpus, request.memory_mib), (2, 512));
    assert!(!request.detach && !request.auto_remove && request.env.is_empty());
    assert!(store.run(request).is_err());
    assert!(store.list(true).unwrap().is_empty());
}

#[test]
fn verified_pull_publishes_identity_and_rechecks_cached_bytes() {
    let registry = Registry::new(false);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(Some(root.clone())).unwrap();
    let image = store.pull(&registry.image).unwrap();
    assert_eq!(image.reference, registry.image);
    assert!(image.id.starts_with("sha256:") && image.digest.starts_with("sha256:"));
    assert_eq!(store.images().unwrap().len(), 1);
    assert_eq!(store.pull(&registry.image).unwrap().id, image.id);
    let blob = fs::read_dir(root.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let size = fs::metadata(&blob).unwrap().len();
    fs::write(blob, vec![0; size as usize]).unwrap();
    assert!(store.pull(&registry.image).is_err());
    assert_eq!(store.images().unwrap().len(), 1);
}

#[test]
fn corrupt_download_never_publishes_an_image_or_partial_blob() {
    let registry = Registry::new(true);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(Some(root.clone())).unwrap();
    assert!(store.pull(&registry.image).is_err());
    assert!(store.images().unwrap().is_empty());
    // Only the independently verified config may remain in the immutable cache.
    assert_eq!(fs::read_dir(root.join("blobs")).unwrap().count(), 1);
    assert!(store.list(true).unwrap().is_empty());
}

#[test]
fn image_stop_signal_is_validated_before_catalog_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(dir.path().join("store"))).unwrap();
    let valid = Registry::with_stop_signal(false, "SIGUSR1");
    store.pull(&valid.image).unwrap();
    let before = store.images().unwrap();
    let invalid = Registry::with_stop_signal(false, "SIGUNKNOWN");
    assert!(
        store
            .pull(&invalid.image)
            .unwrap_err()
            .to_string()
            .contains("unsupported image stop signal")
    );
    assert_eq!(store.images().unwrap().len(), before.len());
    assert_eq!(store.images().unwrap()[0].id, before[0].id);
}

#[test]
fn malformed_references_and_selectors_fail_without_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(Some(dir.path().join("store"))).unwrap();
    for reference in [
        "",
        "alpine?secret=bad",
        "alpine\\x",
        "repo@sha256:bad",
        "host/../image",
    ] {
        assert!(store.pull(reference).is_err(), "{reference}");
    }
    for selector in ["", "../escape", "missing"] {
        assert!(store.remove(selector, false).is_err());
    }
    assert!(store.images().unwrap().is_empty());
    assert!(store.list(true).unwrap().is_empty());
}

#[test]
fn image_removal_selects_tags_or_unique_ids_without_deleting_shared_blobs() {
    let registry = Registry::new(false);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("store");
    let store = Store::open(Some(root.clone())).unwrap();
    let image = store.pull(&registry.image).unwrap();
    let alias = registry.image.replace(":latest", ":alias");
    assert_eq!(store.pull(&alias).unwrap().id, image.id);
    let before = fs::read_dir(root.join("blobs")).unwrap().count();
    assert!(store.remove_image(&image.id, false).is_err());
    assert_eq!(store.images().unwrap().len(), 2);
    assert_eq!(
        store.remove_image(&alias, false).unwrap()[0].reference,
        alias
    );
    let prefix = &image.id["sha256:".len()..][..12];
    assert_eq!(store.remove_image(prefix, false).unwrap()[0].id, image.id);
    assert!(store.images().unwrap().is_empty());
    assert_eq!(fs::read_dir(root.join("blobs")).unwrap().count(), before);
    for selector in ["", "../escape", "sha256:bad", "missing"] {
        assert!(store.remove_image(selector, true).is_err(), "{selector}");
    }
    assert!(
        store.pull(&registry.image).is_ok(),
        "untag must not corrupt cached layers"
    );
}
