//! Real CLI backend contracts. The S3 fixture verifies SigV4 independently and
//! implements atomic PUT/GET, including read-only authorization and corruption.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = if key.len() > 64 {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    let mut inner = [0x36; 64];
    let mut outer = [0x5c; 64];
    for (i, byte) in key.iter().enumerate() {
        inner[i] ^= byte;
        outer[i] ^= byte;
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(data);
    let hashed = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(hashed);
    hash.finalize().to_vec()
}
fn verify_signature(method: &str, path: &str, headers: &BTreeMap<String, String>, body: &[u8]) {
    assert_eq!(
        headers.get("x-amz-security-token").map(String::as_str),
        Some("test-token")
    );
    let authorization = headers
        .get("authorization")
        .expect("S3 request must be signed");
    let parts: BTreeMap<_, _> = authorization
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .unwrap()
        .split(", ")
        .map(|part| part.split_once('=').unwrap())
        .collect();
    let (credential, scope) = parts["Credential"].split_once('/').unwrap();
    assert_eq!(credential, "AKIATEST");
    let scope_parts: Vec<_> = scope.split('/').collect();
    assert_eq!(&scope_parts[1..], &["us-east-1", "s3", "aws4_request"]);
    let signed = parts["SignedHeaders"];
    let canonical_headers: String = signed
        .split(';')
        .map(|name| {
            format!(
                "{name}:{}\n",
                headers[name]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    let payload = &headers["x-amz-content-sha256"];
    if payload != "UNSIGNED-PAYLOAD" {
        assert_eq!(payload, &digest(body));
    }
    let (uri, query) = path.split_once('?').unwrap_or((path, ""));
    let canonical = format!("{method}\n{uri}\n{query}\n{canonical_headers}\n{signed}\n{payload}");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        headers["x-amz-date"],
        digest(canonical.as_bytes())
    );
    let date = hmac(b"AWS4test-secret", scope_parts[0].as_bytes());
    let region = hmac(&date, b"us-east-1");
    let service = hmac(&region, b"s3");
    let key = hmac(&service, b"aws4_request");
    assert_eq!(parts["Signature"], hex(&hmac(&key, to_sign.as_bytes())));
}
struct MockS3 {
    endpoint: String,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    read_only: Arc<AtomicBool>,
    deny_reads: Arc<AtomicBool>,
    lost_head_ack: Arc<AtomicBool>,
    puts: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl MockS3 {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let read_only = Arc::new(AtomicBool::new(false));
        let deny_reads = Arc::new(AtomicBool::new(false));
        let lost_head_ack = Arc::new(AtomicBool::new(false));
        let puts = Arc::new(AtomicUsize::new(0));
        let gets = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_objects = objects.clone();
        let worker_read_only = read_only.clone();
        let worker_deny_reads = deny_reads.clone();
        let worker_lost_head_ack = lost_head_ack.clone();
        let worker_puts = puts.clone();
        let worker_gets = gets.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        serve_s3(
                            &mut stream,
                            &worker_objects,
                            &worker_read_only,
                            &worker_deny_reads,
                            &worker_puts,
                            &worker_gets,
                            &worker_lost_head_ack,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("S3 fixture: {error}"),
                }
            }
        });
        Self {
            endpoint,
            objects,
            read_only,
            deny_reads,
            lost_head_ack,
            puts,
            gets,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for MockS3 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn serve_s3(
    stream: &mut TcpStream,
    objects: &Mutex<BTreeMap<String, Vec<u8>>>,
    read_only: &AtomicBool,
    deny_reads: &AtomicBool,
    puts: &AtomicUsize,
    gets: &AtomicUsize,
    lost_head_ack: &AtomicBool,
) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 8192];
        let size = stream.read(&mut chunk).unwrap();
        assert!(size > 0);
        bytes.extend_from_slice(&chunk[..size]);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(bytes.len() < 1024 * 1024);
    };
    let text = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = text.lines();
    let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().into()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map(|v| v.parse().unwrap())
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let mut chunk = [0; 16384];
        let size = stream.read(&mut chunk).unwrap();
        assert!(size > 0);
        bytes.extend_from_slice(&chunk[..size]);
    }
    let body = &bytes[header_end..header_end + length];
    verify_signature(first[0], first[1], &headers, body);
    assert!(first[1].starts_with("/cache-bucket/team/"));
    let mut objects = objects.lock().unwrap();
    let mut content_range = String::new();
    let etag = objects
        .get(first[1])
        .map(|body| digest(body))
        .unwrap_or_else(|| digest(body));
    let (status, response) = match first[0] {
        "PUT" => {
            puts.fetch_add(1, Ordering::Relaxed);
            if read_only.load(Ordering::Relaxed) {
                (
                    "403 Forbidden",
                    b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
                )
            } else if (headers.get("if-none-match").map(String::as_str) == Some("*")
                && objects.contains_key(first[1]))
                || headers.get("if-match").is_some_and(|expected| {
                    objects
                        .get(first[1])
                        .is_none_or(|body| *expected != format!("\"{}\"", digest(body)))
                })
            {
                (
                    "412 Precondition Failed",
                    b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                )
            } else {
                objects.insert(first[1].into(), body.to_vec());
                if first[1].ends_with("/HEAD.json") && lost_head_ack.swap(false, Ordering::Relaxed)
                {
                    // Commit succeeded but the acknowledgement never arrived.
                    return;
                }
                ("200 OK", Vec::new())
            }
        }
        "GET" => {
            gets.fetch_add(1, Ordering::Relaxed);
            if deny_reads.load(Ordering::Relaxed) {
                (
                    "403 Forbidden",
                    b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
                )
            } else {
                match objects.get(first[1]) {
                    Some(body) => {
                        if read_only.load(Ordering::Relaxed)
                            && first[1].ends_with(".bin")
                            && !first[1].ends_with("checksums.bin")
                        {
                            assert!(
                                headers.contains_key("range"),
                                "binary metadata must use ranged reads"
                            );
                        }
                        if let Some(range) = headers.get("range") {
                            let (start, end) = range
                                .strip_prefix("bytes=")
                                .unwrap()
                                .split_once('-')
                                .unwrap();
                            let start: usize = start.parse().unwrap();
                            let end: usize = end.parse().unwrap();
                            assert!(start <= end && end < body.len());
                            content_range =
                                format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len());
                            ("206 Partial Content", body[start..=end].to_vec())
                        } else {
                            ("200 OK", body.clone())
                        }
                    }
                    None => (
                        "404 Not Found",
                        b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                    ),
                }
            }
        }
        method => {
            panic!("unexpected S3 operation {method}; cache should only need GetObject/PutObject")
        }
    };
    write!(stream, "HTTP/1.1 {status}\r\n{content_range}Content-Length: {}\r\nETag: \"{}\"\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", response.len(), etag).unwrap();
    stream.write_all(&response).unwrap();
}
fn architecture() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}
fn source_fixture(root: &Path) -> String {
    source_fixture_for_architecture(root, architecture())
}
fn source_fixture_for_architecture(root: &Path, target: &str) -> String {
    let digest = format!("sha256:{}", "c".repeat(64));
    let image = root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir_all(&image).unwrap();
    fs::write(image.join("large"), vec![42u8; 2 * 1024 * 1024 + 13]).unwrap();
    for n in 0..100 {
        fs::write(image.join(format!("small-{n:03}")), format!("value-{n}")).unwrap();
    }
    std::os::unix::fs::symlink("large", image.join("alias")).unwrap();
    let metadata = root.join("metadata/prepared-v1");
    fs::create_dir_all(&metadata).unwrap();
    let key =
        serde_json::to_vec(&["registry-1.docker.io", "library/example", "test", target]).unwrap();
    fs::write(metadata.join(format!("{}.json", digest_bytes(&key))), serde_json::to_vec(&serde_json::json!({
        "checked_at": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "prepared": {"digest": digest, "env": {"EXAMPLE":"value"}, "entrypoint":["/bin/sh"], "cmd":["-c"]}
    })).unwrap()).unwrap();
    digest
}
fn digest_bytes(bytes: &[u8]) -> String {
    digest(bytes)
}
fn cli(root: &Path, backend: &str, location: &str, s3: Option<&MockS3>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor-cache"));
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("client-cache"))
        .args(["--backend", backend, "--location", location])
        .args(args);
    if let Some(s3) = s3 {
        command
            .env("AWS_ACCESS_KEY_ID", "AKIATEST")
            .env("AWS_SECRET_ACCESS_KEY", "test-secret")
            .env("AWS_SESSION_TOKEN", "test-token")
            .env("AWS_DEFAULT_REGION", "us-east-1")
            .env("AWS_ENDPOINT", &s3.endpoint)
            .env("AWS_ALLOW_HTTP", "true");
    }
    command.output().unwrap()
}
fn successful(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn s3_cli_publishes_once_then_independent_read_only_clients_need_no_registry_or_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let digest = source_fixture(&source);
    let s3 = MockS3::start();
    let prepared = successful(cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &[
            "--image-store",
            source.to_str().unwrap(),
            "prepare",
            "example:test",
        ],
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(prepared["digest"], digest);
    let objects = s3.objects.lock().unwrap();
    let blobs = objects
        .keys()
        .filter(|key| key.contains("/data/sha256/"))
        .count();
    drop(objects);
    assert!(
        blobs == 102,
        "100 independent small files plus two distinct chunks of the large file"
    );
    fs::remove_dir_all(&source).unwrap();
    s3.read_only.store(true, Ordering::Relaxed);
    let before_puts = s3.puts.load(Ordering::Relaxed);
    let prepared = successful(cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &["--read-only", "prepare", "example:test"],
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(prepared["digest"], digest);
    let handle = prepared["image_handle"].as_str().unwrap();
    let before_gets = s3.gets.load(Ordering::Relaxed);
    let contents = successful(cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &["--read-only", "read", handle, "large"],
    ));
    assert_eq!(contents, vec![42; 2 * 1024 * 1024 + 13]);
    assert!(
        s3.gets.load(Ordering::Relaxed) - before_gets <= 12,
        "small control objects and required metadata pages plus two distinct data chunks"
    );
    assert_eq!(
        s3.puts.load(Ordering::Relaxed),
        before_puts,
        "readers must never attempt PutObject"
    );
    let missing = cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &["--read-only", "prepare", "example:missing"],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("read-only"));
    let alias = cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &["--read-only", "read", handle, "alias"],
    );
    assert!(!alias.status.success());
    let blob = s3
        .objects
        .lock()
        .unwrap()
        .keys()
        .find(|key| key.ends_with(&digest_bytes(&vec![42u8; 1024 * 1024])))
        .unwrap()
        .clone();
    s3.objects
        .lock()
        .unwrap()
        .insert(blob, b"corrupted".to_vec());
    let corrupt = cli(
        &tmp.path().join("cold-reader"),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &["--read-only", "read", handle, "large"],
    );
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("digest mismatch"));
    s3.deny_reads.store(true, Ordering::Relaxed);
    let blocked_staging = tmp.path().join("blocked-staging");
    let denied = cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &[
            "--image-store",
            blocked_staging.to_str().unwrap(),
            "prepare",
            "example:test",
        ],
    );
    assert!(!denied.status.success());
    assert!(
        !blocked_staging.exists(),
        "S3 authorization errors must not cause registry fallback"
    );
}
#[test]
fn explicit_s3_publisher_rebuilds_missing_objects_and_conditionally_updates_its_own_head() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let target = if architecture() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let image_digest = source_fixture_for_architecture(&source, target);
    let s3 = MockS3::start();
    let publish = || {
        let output = cli(
            tmp.path(),
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            &[
                "publish",
                "example:test",
                "--architecture",
                target,
                "--image-store",
                source.to_str().unwrap(),
            ],
        );
        let response: serde_json::Value = serde_json::from_slice(&successful(output)).unwrap();
        assert_eq!(response["digest"], image_digest);
        assert_eq!(response["architecture"], target);
        assert!(response["totals"]["files"].as_u64().unwrap() > 100);
        response
    };
    let first = publish();
    let original = s3.objects.lock().unwrap().clone();
    for part in ["/meta/", "/revisions/", "/data/sha256/", "/uploads/"] {
        assert!(
            original.keys().any(|key| key.contains(part)),
            "missing {part}"
        );
    }
    let missing_blob = original
        .keys()
        .find(|key| key.contains("/data/sha256/"))
        .unwrap();
    s3.objects.lock().unwrap().remove(missing_blob);
    let second = publish();
    assert_eq!(first["metadata_generation"], second["metadata_generation"]);
    let restored = s3.objects.lock().unwrap();
    assert_eq!(
        restored
            .keys()
            .filter(|key| !key.contains("/uploads/"))
            .count(),
        original
            .keys()
            .filter(|key| !key.contains("/uploads/"))
            .count(),
        "republication must reuse existing content objects"
    );
    assert_eq!(restored[missing_blob], original[missing_blob]);
    drop(restored);
    assert!(
        s3.gets.load(Ordering::Relaxed) > 0,
        "v1 CAS and immutable reuse verification require GetObject"
    );
    let head_key = original
        .keys()
        .find(|key| key.ends_with("/HEAD.json"))
        .unwrap();
    let head: serde_json::Value =
        serde_json::from_slice(&s3.objects.lock().unwrap()[head_key]).unwrap();
    assert_eq!(head["generation"], 2);
    assert_eq!(first["image_handle"], second["image_handle"]);
    fs::remove_dir_all(&source).unwrap();
    s3.deny_reads.store(false, Ordering::Relaxed);
    s3.read_only.store(true, Ordering::Relaxed);
    let reader = tmp.path().join("independent-reader");
    assert_eq!(
        successful(cli(
            &reader,
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            &[
                "--read-only",
                "read",
                second["image_handle"].as_str().unwrap(),
                "small-003"
            ],
        )),
        b"value-3"
    );
    let before = s3.puts.load(Ordering::Relaxed);
    let staging = tmp.path().join("forbidden-staging");
    let rejected = cli(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        &[
            "--read-only",
            "--image-store",
            staging.to_str().unwrap(),
            "publish",
            "example:test",
        ],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("read-only"));
    assert!(!staging.exists());
    assert_eq!(s3.puts.load(Ordering::Relaxed), before);
}

#[test]
fn publisher_rejects_server_backend_before_connecting() {
    let tmp = tempfile::tempdir().unwrap();
    let staging = tmp.path().join("unused-staging");
    let output = cli(
        tmp.path(),
        "server",
        "unix:///nonexistent/pvisor-publish.sock",
        None,
        &[
            "--image-store",
            staging.to_str().unwrap(),
            "publish",
            "example:test",
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("publish requires a filesystem or S3")
    );
    assert!(!staging.exists());
}

#[test]
fn filesystem_cli_and_explicit_options_use_the_same_daemonless_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    source_fixture(&source);
    let shared = tmp.path().join("shared");
    let prepared = successful(cli(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        &[
            "--image-store",
            source.to_str().unwrap(),
            "publish",
            "example:test",
        ],
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    let handle = prepared["image_handle"].as_str().unwrap();
    fs::remove_dir_all(source).unwrap();
    let contents = successful(cli(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        &["--read-only", "read", handle, "small-003"],
    ));
    assert_eq!(contents, b"value-3");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-cache"))
        .env_clear()
        .env("PVISOR_CACHE_BACKEND", "s3")
        .env("PVISOR_CACHE_LOCATION", "s3://unused-bucket")
        .args([
            "--backend",
            "filesystem",
            "--location",
            shared.to_str().unwrap(),
            "--read-only",
            "stat",
            handle,
            "small-003",
        ])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&successful(output)).unwrap();
    assert_eq!(value["kind"], "file");
    let absent = tmp.path().join("absent-shared");
    let output = cli(
        tmp.path(),
        "filesystem",
        absent.to_str().unwrap(),
        None,
        &["--read-only", "prepare", "example:test"],
    );
    assert!(!output.status.success());
    assert!(
        !absent.exists(),
        "read-only client must not create shared storage"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "requires /dev/kvm, /dev/fuse and the static musl guest target"]
fn vm_reads_daemonless_filesystem_and_s3_images_without_projecting_storage_credentials() {
    assert!(Path::new("/dev/kvm").exists(), "KVM is required");
    assert!(Path::new("/dev/fuse").exists(), "FUSE is required");
    let guest_temp = tempfile::tempdir().unwrap();
    let guest_source = guest_temp.path().join("guest.rs");
    fs::write(
        &guest_source,
        r#"
fn main() {
    let data = std::fs::read("/large").unwrap();
    assert_eq!(data, vec![42u8; 2 * 1024 * 1024 + 13]);
    assert_eq!(std::env::var("EXAMPLE").unwrap(), "value");
    for key in ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN",
        "PVISOR_CACHE_BACKEND", "PVISOR_CACHE_LOCATION", "PVISOR_CACHE_READ_ONLY"] {
        assert!(std::env::var(key).is_err(), "host cache setting leaked: {key}");
    }
    println!("DAEMONLESS_VM_CACHE_OK");
}
"#,
    )
    .unwrap();
    let guest = guest_temp.path().join("guest");
    let compilation = Command::new("rustc")
        .args([
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "opt-level=1",
            "-C",
            "panic=abort",
        ])
        .arg(&guest_source)
        .arg("-o")
        .arg(&guest)
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    for backend in ["filesystem", "s3"] {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let digest = source_fixture(&source);
        fs::copy(
            &guest,
            source
                .join("rootfs-v3/sha256")
                .join(&digest[7..])
                .join("guest"),
        )
        .unwrap();
        let s3 = MockS3::start();
        let shared = tmp.path().join("shared");
        let location = if backend == "s3" {
            "s3://cache-bucket/team"
        } else {
            shared.to_str().unwrap()
        };
        successful(cli(
            tmp.path(),
            backend,
            location,
            (backend == "s3").then_some(&s3),
            &[
                "--image-store",
                source.to_str().unwrap(),
                "prepare",
                "example:test",
            ],
        ));
        fs::remove_dir_all(source).unwrap();
        s3.read_only.store(true, Ordering::Relaxed);
        let config = tmp.path().join("vm.toml");
        fs::write(&config, "[run]\ninherit_env = true\n").unwrap();
        let workspace = tmp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let job = tmp.path().join("job");
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", tmp.path().join("home"))
            .env("XDG_CACHE_HOME", tmp.path().join("local-cache"))
            .env("PVISOR_RUN_HOME", tmp.path().join("runs"))
            .env("PVISOR_IMAGE_STORE", tmp.path().join("reader-staging"))
            .env("PVISOR_CACHE_BACKEND", backend)
            .env("PVISOR_CACHE_LOCATION", location)
            .env("PVISOR_CACHE_READ_ONLY", "true")
            .current_dir(&workspace)
            .args(["run", "--stage"])
            .arg(&job)
            .args(["--config"])
            .arg(&config)
            .args([
                "--executor",
                "vm",
                "--no-agent-defaults",
                "--gateway-mode",
                "off",
                "--rootfs",
                "image=example:test",
                "--memory",
                "128MiB",
                "--timeout",
                "30s",
                "--stdio",
                "capture",
                "--",
                "/guest",
            ]);
        if backend == "s3" {
            command
                .env("AWS_ACCESS_KEY_ID", "AKIATEST")
                .env("AWS_SECRET_ACCESS_KEY", "test-secret")
                .env("AWS_SESSION_TOKEN", "test-token")
                .env("AWS_DEFAULT_REGION", "us-east-1")
                .env("AWS_ENDPOINT", &s3.endpoint)
                .env("AWS_ALLOW_HTTP", "true");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(job.join("run-bundle.json")).unwrap()).unwrap();
        assert!(
            bundle["run"]["output"]["stdout"]
                .as_str()
                .unwrap()
                .contains("DAEMONLESS_VM_CACHE_OK"),
            "{bundle}"
        );
        assert_eq!(bundle["run"]["exit_code"], 0);
        assert_eq!(
            fs::read_dir(tmp.path().join("reader-staging/rootfs-v3/sha256"))
                .unwrap()
                .count(),
            0,
            "reader must not extract a local OCI root"
        );
    }
}

#[test]
fn a_lost_head_acknowledgement_is_reconciled_after_sdk_conditional_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    source_fixture(&source);
    let s3 = MockS3::start();
    let mut previous = None;
    for generation in 1..=2 {
        s3.lost_head_ack.store(true, Ordering::Relaxed);
        let response = successful(cli(
            tmp.path(),
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            &[
                "publish",
                "example:test",
                "--image-store",
                source.to_str().unwrap(),
            ],
        ));
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        let objects = s3.objects.lock().unwrap();
        let bytes = &objects
            .iter()
            .find(|(key, _)| key.ends_with("/HEAD.json"))
            .unwrap()
            .1;
        let head: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            head["generation"], generation,
            "SDK retry must not create an extra generation"
        );
        let id = head["publication_id"].as_str().unwrap().to_owned();
        assert_ne!(previous.as_ref(), Some(&id));
        previous = Some(id);
        assert!(
            response["image_handle"].as_str().unwrap().ends_with(
                head["revision"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("sha256:")
                    .unwrap()
            )
        );
    }
}
