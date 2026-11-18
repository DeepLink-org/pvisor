//! Image CLI and Rust cache client contracts against filesystem and SigV4 S3 fixtures.
use pvisor::cache::{CacheClient, CacheConfig, MAX_READ, Request, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    sync::atomic::Ordering,
};
#[path = "common/s3.rs"]
mod s3;
use s3::MockS3;
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn architecture() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}
struct Registry {
    child: std::process::Child,
    reference: String,
    digest: String,
}
impl Registry {
    fn start(root: &Path, target: &str, guest: Option<&Path>) -> Self {
        use std::io::{BufRead, BufReader};
        let python = std::env::var_os("UV_PYTHON").unwrap_or_else(|| "python3".into());
        let mut command = Command::new(python);
        command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/registry.py"))
            .arg(root.parent().unwrap().join("registry"))
            .arg(target)
            .stdout(std::process::Stdio::piped());
        if let Some(guest) = guest {
            command.arg(guest);
        }
        let mut registry = Self {
            child: command.spawn().unwrap(),
            reference: String::new(),
            digest: String::new(),
        };
        let mut line = String::new();
        BufReader::new(registry.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).expect("registry readiness");
        registry.reference = ready["reference"].as_str().unwrap().into();
        registry.digest = ready["digest"].as_str().unwrap().into();
        registry
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        self.stop();
    }
}
fn source_fixture(root: &Path) -> Registry {
    Registry::start(root, architecture(), None)
}
fn source_fixture_for_architecture(root: &Path, target: &str) -> Registry {
    Registry::start(root, target, None)
}
fn clean_imports(root: &Path) {
    for name in ["blobs", "rootfs-v3", "metadata"] {
        assert!(!root.join(name).exists(), "retained raw {name}");
    }
    assert_eq!(fs::read_dir(root.join("staging")).unwrap().count(), 0);
}
fn digest_bytes(bytes: &[u8]) -> String {
    digest(bytes)
}
#[derive(Serialize, Deserialize)]
enum Operation {
    Request(Request),
    ReadFile { digest: String, path: Vec<u8> },
}

fn prepare(image: &str, refresh: bool) -> Operation {
    prepare_for(image, architecture(), refresh)
}

fn prepare_for(image: &str, architecture: &str, refresh: bool) -> Operation {
    Operation::Request(Request::Prepare {
        image: image.into(),
        architecture: architecture.into(),
        refresh,
    })
}

fn read_file(digest: &str, path: &str) -> Operation {
    Operation::ReadFile {
        digest: digest.into(),
        path: path.as_bytes().to_vec(),
    }
}

#[test]
#[ignore = "invoked by the cache fixtures with an isolated environment"]
fn cache_client_child() -> anyhow::Result<()> {
    let output = std::env::var_os("PVISOR_TEST_CACHE_OUTPUT").expect("fixture output");
    let (backend, location, read_only, image_store, operation): (_, _, _, _, Operation) =
        serde_json::from_str::<(String, String, bool, Option<std::path::PathBuf>, Operation)>(
            &std::env::var("PVISOR_TEST_CACHE_REQUEST")?,
        )?;
    let client = CacheClient::from_config(CacheConfig::from_options(
        Some(
            backend
                .parse::<pvisor::cache::CacheBackend>()
                .map_err(anyhow::Error::msg)?,
        ),
        Some(location),
        Some(read_only),
        image_store,
    )?)?;
    let data = match operation {
        Operation::Request(request) => serde_json::to_vec(&client.request(request)?.0)?,
        Operation::ReadFile { digest, path } => {
            let mut data = Vec::new();
            loop {
                let (response, bytes) = client.request(Request::Read {
                    digest: digest.clone(),
                    path: path.clone(),
                    offset: data.len() as u64,
                    length: MAX_READ,
                })?;
                assert!(matches!(response, Response::Data { .. }));
                let done = bytes.len() < MAX_READ as usize;
                data.extend(bytes);
                if done {
                    break;
                }
                assert!(data.len() <= 4 * 1024 * 1024, "bounded fixture file");
            }
            data
        }
    };
    fs::write(output, data)?;
    Ok(())
}

fn cache_client(
    root: &Path,
    backend: &str,
    location: &str,
    s3: Option<&MockS3>,
    read_only: bool,
    image_store: Option<&Path>,
    operation: Operation,
) -> Output {
    let capture = tempfile::NamedTempFile::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("client-cache"))
        .env("PVISOR_CACHE_BACKEND", "s3")
        .env("PVISOR_CACHE_LOCATION", "s3://unused-bucket")
        .env("PVISOR_TEST_CACHE_OUTPUT", capture.path())
        .env(
            "PVISOR_TEST_CACHE_REQUEST",
            serde_json::to_string(&(backend, location, read_only, image_store, operation)).unwrap(),
        )
        .args(["--exact", "cache_client_child", "--ignored"]);
    if let Some(s3) = s3 {
        command
            .env("AWS_ACCESS_KEY_ID", "AKIATEST")
            .env("AWS_SECRET_ACCESS_KEY", "test-secret")
            .env("AWS_SESSION_TOKEN", "test-token")
            .env("AWS_DEFAULT_REGION", "us-east-1")
            .env("AWS_ENDPOINT", &s3.endpoint)
            .env("AWS_ALLOW_HTTP", "true");
    }
    let ca = root.join("registry/ca.pem");
    if ca.exists() {
        command.env("SSL_CERT_FILE", ca);
    }
    let mut output = command.output().unwrap();
    if !output.status.success() {
        output.stderr.extend(&output.stdout);
    }
    output.stdout = fs::read(capture.path()).unwrap();
    output
}
fn successful(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn image_cli(root: &Path, s3: &MockS3, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("PVISOR_DOCKER_HOME", root.join("docker"))
        .env("PVISOR_IMAGE_STORE", root.join("docker/images"))
        .env("PVISOR_DAEMON_HOME", root.join("daemon"))
        .env("AWS_ACCESS_KEY_ID", "AKIATEST")
        .env("AWS_SECRET_ACCESS_KEY", "test-secret")
        .env("AWS_SESSION_TOKEN", "test-token")
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .env("AWS_ENDPOINT", &s3.endpoint)
        .env("AWS_ALLOW_HTTP", "true")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn image_push_aliases_and_s3_import_preserve_config_builds_and_failed_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let s3 = MockS3::start();
    let producer = tmp.path().join("producer");
    let reader = tmp.path().join("reader");
    let context = tmp.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), b"published payload").unwrap();
    fs::write(context.join("Dockerfile"), "FROM scratch\nCOPY payload /payload\nLABEL app.role=worker\nENV FEATURE=lazy\nHEALTHCHECK CMD [\"/probe\"]\nCMD [\"/payload\"]\n").unwrap();
    successful(image_cli(
        &producer,
        &s3,
        &["build", "-q", "-t", "local:test", context.to_str().unwrap()],
    ));
    let before = successful(image_cli(
        &producer,
        &s3,
        &["image", "inspect", "local:test"],
    ));
    let destination = "s3://cache-bucket/team";
    let first = successful(image_cli(
        &producer,
        &s3,
        &["push", "local:test", "--to", destination],
    ));
    let second = successful(image_cli(
        &producer,
        &s3,
        &["image", "push", "local:test", "--to", destination],
    ));
    assert_eq!(
        first, second,
        "both spellings pin and publish identical bytes"
    );
    assert_eq!(
        before,
        successful(image_cli(
            &producer,
            &s3,
            &["image", "inspect", "local:test"]
        ))
    );
    let receipt: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let source = receipt["source"].as_str().unwrap();
    let objects = s3.objects.lock().unwrap().clone();
    assert!(objects.keys().any(|key| key.contains("/configs/")));
    // With every data object absent, only a metadata-only import can succeed.
    s3.objects
        .lock()
        .unwrap()
        .retain(|key, _| !key.contains("/data/"));
    s3.read_only.store(true, Ordering::Relaxed);
    let puts = s3.puts.load(Ordering::Relaxed);
    successful(image_cli(
        &reader,
        &s3,
        &["image", "pull", "copy:test", "--from", source],
    ));
    let installed = successful(image_cli(&reader, &s3, &["image", "inspect", "copy:test"]));
    let original: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let imported: serde_json::Value = serde_json::from_slice(&installed).unwrap();
    for field in ["Id", "Config", "RootFS", "Architecture", "Os"] {
        assert_eq!(original[0][field], imported[0][field], "{field}");
    }
    assert_eq!(imported[0]["Pvisor"]["Source"], source);
    assert_eq!(
        imported[0]["Pvisor"]["ImageHandle"],
        receipt["image_handle"]
    );
    let reads = s3.gets.load(Ordering::Relaxed);
    successful(image_cli(
        &reader,
        &s3,
        &["image", "ls", "--filter", "label=app.role=worker"],
    ));
    assert_eq!(s3.gets.load(Ordering::Relaxed), reads, "queries are local");
    assert!(
        !image_cli(&reader, &s3, &["pull", "copy:test", "--from", source])
            .status
            .success()
    );
    let (config_key, _) = objects
        .iter()
        .find(|(key, _)| key.contains("/configs/"))
        .unwrap();
    s3.objects
        .lock()
        .unwrap()
        .insert(config_key.clone(), b"{}".to_vec());
    assert!(
        !image_cli(
            &reader,
            &s3,
            &["image", "pull", "copy:test", "--from", source, "--refresh"]
        )
        .status
        .success()
    );
    assert_eq!(
        installed,
        successful(image_cli(&reader, &s3, &["image", "inspect", "copy:test"]))
    );
    *s3.objects.lock().unwrap() = objects;
    successful(image_cli(
        &reader,
        &s3,
        &["pull", "copy:test", "--from", source, "--refresh"],
    ));
    fs::write(
        context.join("Dockerfile"),
        "FROM copy:test\nLABEL imported=yes\n",
    )
    .unwrap();
    successful(image_cli(
        &reader,
        &s3,
        &[
            "build",
            "-q",
            "-t",
            "derived:test",
            context.to_str().unwrap(),
        ],
    ));
    let derived: serde_json::Value = serde_json::from_slice(&successful(image_cli(
        &reader,
        &s3,
        &["image", "inspect", "derived:test"],
    )))
    .unwrap();
    assert_eq!(derived[0]["Config"]["Labels"]["app.role"], "worker");
    assert_eq!(derived[0]["Config"]["Labels"]["imported"], "yes");
    assert_eq!(
        derived[0]["Config"]["Healthcheck"],
        original[0]["Config"]["Healthcheck"]
    );
    assert_eq!(
        s3.puts.load(Ordering::Relaxed),
        puts,
        "readers never write remote objects"
    );
    assert!(!reader.join("daemon").exists());
}

#[test]
fn s3_pool_is_readable_without_registry_or_daemon_after_preparation() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let mut registry = source_fixture(&source);
    let digest = registry.digest.clone();
    let s3 = MockS3::start();
    let prepared = successful(cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        false,
        Some(&source),
        prepare(&registry.reference, false),
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(prepared["digest"], digest);
    let objects = s3.objects.lock().unwrap();
    let blobs = objects.keys().filter(|key| key.contains("/data/")).count();
    drop(objects);
    assert!(
        blobs == 3,
        "one small-file pack plus two distinct chunks of the large file"
    );
    clean_imports(&source);
    registry.stop();
    fs::remove_dir_all(&source).unwrap();
    s3.read_only.store(true, Ordering::Relaxed);
    let before_puts = s3.puts.load(Ordering::Relaxed);
    let prepared = successful(cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        None,
        prepare(&registry.reference, false),
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    assert_eq!(prepared["digest"], digest);
    let handle = prepared["image_handle"].as_str().unwrap();
    let before_gets = s3.gets.load(Ordering::Relaxed);
    let contents = successful(cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        None,
        read_file(handle, "large"),
    ));
    assert_eq!(contents, vec![42; 2 * 1024 * 1024 + 13]);
    assert!(
        s3.gets.load(Ordering::Relaxed) - before_gets <= 12,
        "small control objects and required metadata pages plus two distinct data chunks"
    );
    assert_eq!(
        s3.puts.load(Ordering::Relaxed),
        before_puts,
        "readers must never execution PutObject"
    );
    let missing = cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        None,
        prepare("example:missing", false),
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("read-only"));
    let alias = cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        None,
        read_file(handle, "alias"),
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
    let corrupt = cache_client(
        &tmp.path().join("cold-reader"),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        None,
        read_file(handle, "large"),
    );
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("digest mismatch"));
    s3.deny_reads.store(true, Ordering::Relaxed);
    let blocked_staging = tmp.path().join("blocked-staging");
    let denied = cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        false,
        Some(&blocked_staging),
        prepare(&registry.reference, false),
    );
    assert!(!denied.status.success());
    assert!(
        !blocked_staging.exists(),
        "S3 authorization errors must not cause registry fallback"
    );
}
#[test]
fn refreshing_s3_cache_rebuilds_missing_objects_and_conditionally_updates_its_own_head() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let target = if architecture() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let mut registry = source_fixture_for_architecture(&source, target);
    let image_digest = registry.digest.clone();
    let s3 = MockS3::start();
    let publish_revision = || {
        let output = cache_client(
            tmp.path(),
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            false,
            Some(&source),
            prepare_for(&registry.reference, target, true),
        );
        let response: serde_json::Value = serde_json::from_slice(&successful(output)).unwrap();
        assert_eq!(response["digest"], image_digest);
        assert_eq!(response["architecture"], target);
        assert!(response["totals"]["files"].as_u64().unwrap() > 100);
        response
    };
    let first = publish_revision();
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
    let second = publish_revision();
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
        "V2 CAS and immutable reuse verification require GetObject"
    );
    let head_key = original
        .keys()
        .find(|key| key.ends_with("/HEAD.json"))
        .unwrap();
    let head: serde_json::Value =
        serde_json::from_slice(&s3.objects.lock().unwrap()[head_key]).unwrap();
    assert_eq!(head["generation"], 2);
    assert_eq!(first["image_handle"], second["image_handle"]);
    clean_imports(&source);
    let old_head = s3.objects.lock().unwrap()[head_key].clone();
    s3.read_only.store(true, Ordering::Relaxed);
    let failed = cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        false,
        Some(&source),
        prepare_for(&registry.reference, target, true),
    );
    assert!(!failed.status.success(), "injected upload refusal");
    clean_imports(&source);
    assert_eq!(s3.objects.lock().unwrap()[head_key], old_head);
    registry.stop();
    fs::remove_dir_all(&source).unwrap();
    s3.deny_reads.store(false, Ordering::Relaxed);
    s3.read_only.store(true, Ordering::Relaxed);
    let reader = tmp.path().join("independent-reader");
    assert_eq!(
        successful(cache_client(
            &reader,
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            true,
            None,
            read_file(second["image_handle"].as_str().unwrap(), "small-003")
        )),
        b"value-3"
    );
    let before = s3.puts.load(Ordering::Relaxed);
    let staging = tmp.path().join("forbidden-staging");
    let rejected = cache_client(
        tmp.path(),
        "s3",
        "s3://cache-bucket/team",
        Some(&s3),
        true,
        Some(&staging),
        prepare(&registry.reference, true),
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("read-only"));
    assert!(!staging.exists());
    assert_eq!(s3.puts.load(Ordering::Relaxed), before);
}

#[test]
fn filesystem_client_and_explicit_options_use_the_same_daemonless_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let mut registry = source_fixture(&source);
    let shared = tmp.path().join("shared");
    let prepared = successful(cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        false,
        Some(&source),
        prepare(&registry.reference, false),
    ));
    let prepared: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
    let handle = prepared["image_handle"].as_str().unwrap();
    clean_imports(&source);
    // Fail after the config and manifest have been downloaded, so cleanup must
    // remove a partially populated import rather than just an empty directory.
    fs::write(tmp.path().join("registry/fail"), b"layer").unwrap();
    let failed = cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        false,
        Some(&source),
        prepare(&registry.reference, true),
    );
    assert!(!failed.status.success(), "injected registry failure");
    clean_imports(&source);
    let previous = successful(cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        true,
        None,
        prepare(&registry.reference, false),
    ));
    let previous: serde_json::Value = serde_json::from_slice(&previous).unwrap();
    assert_eq!(
        previous["image_handle"], handle,
        "failed import cannot replace HEAD"
    );
    fs::write(tmp.path().join("registry/ca.pem"), b"invalid CA bundle").unwrap();
    let failed = cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        false,
        Some(&source),
        prepare(&registry.reference, true),
    );
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("SSL_CERT_FILE"));
    clean_imports(&source);
    registry.stop();
    fs::remove_dir_all(source).unwrap();
    let contents = successful(cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        true,
        None,
        read_file(handle, "small-003"),
    ));
    assert_eq!(contents, b"value-3");
    let output = cache_client(
        tmp.path(),
        "filesystem",
        shared.to_str().unwrap(),
        None,
        true,
        None,
        Operation::Request(Request::Stat {
            digest: handle.into(),
            path: b"small-003".to_vec(),
        }),
    );
    let value: serde_json::Value = serde_json::from_slice(&successful(output)).unwrap();
    assert_eq!(value["kind"], "file");
    let absent = tmp.path().join("absent-shared");
    let output = cache_client(
        tmp.path(),
        "filesystem",
        absent.to_str().unwrap(),
        None,
        true,
        None,
        prepare(&registry.reference, false),
    );
    assert!(!output.status.success());
    assert!(
        !absent.exists(),
        "read-only client must not create shared storage"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "requires /dev/kvm and the static musl guest target"]
fn vm_reads_daemonless_filesystem_and_s3_images_without_projecting_storage_credentials() {
    assert!(Path::new("/dev/kvm").exists(), "KVM is required");
    let guest_temp = tempfile::tempdir().unwrap();
    let guest_source = guest_temp.path().join("guest.rs");
    fs::write(
        &guest_source,
        r#"
fn main() {
    let data = vec![42u8; 2 * 1024 * 1024 + 13];
    // Exercise guest page faults through both packed and multi-chunk lowers,
    // retaining mappings after closing their file descriptors.
    unsafe extern "C" {
        fn mmap(address: *mut std::ffi::c_void, length: usize, protection: i32,
                flags: i32, fd: i32, offset: i64) -> *mut std::ffi::c_void;
        fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
    }
    use std::os::fd::AsRawFd;
    for (path, expected) in [("/large", data), ("/small-099", b"value-99".to_vec())] {
        let file = std::fs::File::open(path).unwrap();
        unsafe {
            let address = mmap(std::ptr::null_mut(), expected.len(), 1, 2, file.as_raw_fd(), 0);
            assert_ne!(address as isize, -1);
            drop(file);
            assert_eq!(std::slice::from_raw_parts(address.cast::<u8>(), expected.len()), expected);
            assert_eq!(munmap(address, expected.len()), 0);
        }
    }
    assert_eq!(std::fs::read("/large").unwrap(), vec![42u8; 2 * 1024 * 1024 + 13]);
    for n in 0..100 {
        assert_eq!(std::fs::read(format!("/small-{n:03}")).unwrap(), format!("value-{n}").as_bytes());
    }
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
        let mut registry = Registry::start(&source, architecture(), Some(&guest));
        let s3 = MockS3::start();
        let shared = tmp.path().join("shared");
        let location = if backend == "s3" {
            "s3://cache-bucket/team"
        } else {
            shared.to_str().unwrap()
        };
        successful(cache_client(
            tmp.path(),
            backend,
            location,
            (backend == "s3").then_some(&s3),
            false,
            Some(&source),
            prepare(&registry.reference, false),
        ));
        clean_imports(&source);
        registry.stop();
        fs::remove_dir_all(source).unwrap();
        s3.read_only.store(true, Ordering::Relaxed);
        let config = tmp.path().join("vm.toml");
        let mut configuration = "[run]\ninherit_env = true\n".to_owned();
        if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
            configuration.push_str(&format!(
                "[vm]\nlibrary_dir = {}\n",
                serde_json::to_string(&directory.to_string_lossy()).unwrap()
            ));
        }
        fs::write(&config, configuration).unwrap();
        let workspace = tmp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let session = tmp.path().join("session");
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
            .args(["spawn", "--stage"])
            .arg(&session)
            .args(["--config"])
            .arg(&config)
            .args([
                "--executor",
                "vm",
                "--no-agent-defaults",
                "--gateway-mode",
                "off",
                "--rootfs",
                &format!("image={}", registry.reference),
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
            serde_json::from_slice(&fs::read(session.join("run-bundle.json")).unwrap()).unwrap();
        assert!(
            bundle["run"]["output"]["stdout"]
                .as_str()
                .unwrap()
                .contains("DAEMONLESS_VM_CACHE_OK"),
            "{bundle}"
        );
        assert_eq!(bundle["run"]["exit_code"], 0);
        assert!(
            !tmp.path().join("reader-staging").exists(),
            "reader must not extract a local OCI root"
        );
    }
}

#[test]
fn a_lost_head_acknowledgement_is_reconciled_after_sdk_conditional_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let registry = source_fixture(&source);
    let s3 = MockS3::start();
    let mut previous = None;
    for generation in 1..=2 {
        s3.lost_head_ack.store(true, Ordering::Relaxed);
        let response = successful(cache_client(
            tmp.path(),
            "s3",
            "s3://cache-bucket/team",
            Some(&s3),
            false,
            Some(&source),
            prepare(&registry.reference, true),
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
