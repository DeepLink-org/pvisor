//! Bounded offline integration/chaos tests. One loopback server, <=2 clients,
//! 128 KiB payloads, no real credentials, external buckets, VMs or stress loops.
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use pvisor_daemon::api::{PreparedCatalog, PreparedCatalogService, PreparedImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
enum CatalogOperation {
    Push(String, String),
    Pull(String, Option<String>, bool),
    Inspect(String),
    List,
}

#[test]
#[ignore = "isolated subprocess helper invoked by repository integration tests"]
fn catalog_child() -> anyhow::Result<()> {
    let images = std::env::var_os("PVISOR_TEST_CATALOG").expect("fixture catalog");
    let output = std::env::var_os("PVISOR_TEST_CATALOG_OUTPUT").expect("fixture output");
    let operation = serde_json::from_str(&std::env::var("PVISOR_TEST_CATALOG_OPERATION")?)?;
    let catalog = PreparedCatalog::new(images.into());
    let value = match operation {
        CatalogOperation::Push(name, destination) => catalog.push(&name, &destination)?,
        CatalogOperation::Pull(name, source, refresh) => {
            let image = catalog.pull(&name, source.as_deref(), refresh)?;
            serde_json::json!({"image":name, "source":image.cache.repository, "mirrors":image.cache.mirrors})
        }
        CatalogOperation::Inspect(name) => catalog.inspect(&name)?,
        CatalogOperation::List => serde_json::to_value(catalog.list()?)?,
    };
    std::fs::write(output, serde_json::to_vec(&value)?)?;
    Ok(())
}

struct Fixture {
    _root: tempfile::TempDir,
    images: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let images = root.path().join("images");
        std::fs::create_dir(&images).unwrap();
        let store_root = images.join("store");
        let store = pvisor::docker::Store::open(store_root.clone()).unwrap();
        let context = root.path().join("context");
        std::fs::create_dir(&context).unwrap();
        std::fs::write(context.join("hello"), b"hello from repository").unwrap();
        std::fs::write(context.join("unused"), vec![17; 128 * 1024]).unwrap();
        let file = context.join("Dockerfile");
        std::fs::write(
            &file,
            "FROM scratch\nCOPY hello /hello\nCOPY unused /unused\n",
        )
        .unwrap();
        let pooled = pvisor::docker::build::build(
            &store,
            &pvisor::docker::build::BuildOptions {
                context,
                file,
                tags: vec![],
                build_args: BTreeMap::new(),
                target: None,
                // Repository byte/metadata tests do not require CPU emulation.
                platform: None,
                pull: false,
                no_cache: false,
                cpus: 1,
                memory_mib: 128,
                network: false,
                timeout: Duration::from_secs(10),
                quiet: true,
                output: None,
            },
        )
        .unwrap();
        let image = PreparedImage {
            rootfs: store_root,
            entrypoint: vec![],
            cmd: vec![],
            env: BTreeMap::new(),
            library_dir: None,
            e2b: None,
            pooled_image: pooled,
            native_opensandbox: false,
            native_e2b: false,
            native_guest: true,
        };
        PreparedCatalog::new(images.clone())
            .prepare("tiny", |store| {
                Ok(PreparedImage {
                    rootfs: store,
                    ..image
                })
            })
            .unwrap();
        Self {
            _root: root,
            images,
        }
    }

    fn environment(
        &self,
        server: &Server,
        images: &Path,
        executable: impl AsRef<std::ffi::OsStr>,
    ) -> Command {
        let mut command = Command::new(executable);
        // Only synthetic credentials for the loopback S3 fixture.
        for (key, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("AWS_"))
        {
            command.env_remove(key);
        }
        command.envs([
            ("AWS_ACCESS_KEY_ID", "local-test-key"),
            ("AWS_SECRET_ACCESS_KEY", "local-test-secret"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("AWS_ENDPOINT", &server.url),
            ("AWS_ALLOW_HTTP", "true"),
            ("AWS_VIRTUAL_HOSTED_STYLE_REQUEST", "false"),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ]);
        // dirs::cache_dir uses HOME/Library/Caches on macOS, not XDG_CACHE_HOME.
        // Keep both platform paths private so prior verified bytes cannot bypass faults.
        command.env("HOME", images.join("home"));
        command.env("XDG_CACHE_HOME", images.join("cache"));
        command.env("NO_PROXY", "127.0.0.1");
        command
            .env_remove("SSL_CERT_FILE")
            .env_remove("SSL_CERT_DIR");
        command
    }

    fn pull(&self, server: &Server, images: &Path, source: Option<&str>, refresh: bool) -> Output {
        self.catalog(
            server,
            images,
            CatalogOperation::Pull("tiny".into(), source.map(str::to_owned), refresh),
        )
    }

    fn registry(&self, server: &Server, images: &Path, args: &[&str]) -> Output {
        let mut command = self.environment(server, images, env!("CARGO_BIN_EXE_pvisor-daemon"));
        command
            .arg("registry")
            .arg("--images-dir")
            .arg(images)
            .args(args);
        command.output().unwrap()
    }

    fn catalog(&self, server: &Server, images: &Path, operation: CatalogOperation) -> Output {
        self.catalog_with(server, images, operation, |_| {})
    }

    fn catalog_with(
        &self,
        server: &Server,
        images: &Path,
        operation: CatalogOperation,
        configure: impl FnOnce(&mut Command),
    ) -> Output {
        let capture = tempfile::NamedTempFile::new().unwrap();
        let mut command = self.environment(server, images, std::env::current_exe().unwrap());
        command
            .args(["--exact", "catalog_child", "--ignored"])
            .env("PVISOR_TEST_CATALOG", images)
            .env("PVISOR_TEST_CATALOG_OUTPUT", capture.path())
            .env(
                "PVISOR_TEST_CATALOG_OPERATION",
                serde_json::to_string(&operation).unwrap(),
            );
        configure(&mut command);
        let mut output = command.output().unwrap();
        if !output.status.success() {
            output.stderr.extend(&output.stdout);
        }
        output.stdout = std::fs::read(capture.path()).unwrap();
        output
    }

    fn reader(&self, server: &Server, images: &Path, cache_tag: &str) -> Command {
        let mut command = self.environment(server, images, std::env::current_exe().unwrap());
        command.args(["--exact", "repository_reader_child", "--ignored"]);
        command.env("PVISOR_TEST_IMAGE_RECORD", images.join("tiny.json"));
        command.env("HOME", images.join(cache_tag).join("home"));
        command.env("XDG_CACHE_HOME", images.join(cache_tag));
        command
    }

    fn publish(&self, server: &Server) -> Output {
        self.catalog(
            server,
            &self.images,
            CatalogOperation::Push("tiny".into(), "s3://images/releases".into()),
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    BusyOnce,
    Missing,
    Denied,
    WrongRange,
    IgnoreRange,
    Corrupt,
    Truncate,
    Redirect,
    FailUpload,
    LostWriteAck,
}

struct PublicationPause {
    reached: std::sync::mpsc::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[derive(Default)]
struct Data {
    objects: BTreeMap<String, Vec<u8>>,
    requests: Vec<(String, String, bool)>,
    fault: Option<(Fault, &'static str)>,
    pause_publication: Option<PublicationPause>,
    injected: usize,
    order: Option<(String, Arc<Mutex<Vec<String>>>)>,
}

struct Server {
    url: String,
    data: Arc<Mutex<Data>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().join().unwrap();
    }
}

impl Server {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let data = Arc::new(Mutex::new(Data::default()));
        let state = data.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let app = axum::Router::new()
                        .route("/{*key}", any(serve))
                        .with_state(state);
                    axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                        .with_graceful_shutdown(async {
                            let _ = stopped.await;
                        })
                        .await
                        .unwrap();
                });
        });
        Self {
            url,
            data,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn inject(&self, fault: Fault, suffix: &'static str) {
        let mut data = self.data.lock().unwrap();
        data.fault = Some((fault, suffix));
        data.injected = 0;
    }
}

async fn serve(State(state): State<Arc<Mutex<Data>>>, request: Request) -> Response {
    let method = request.method().to_string();
    let key = request.uri().path().trim_start_matches('/').to_string();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    if method == "PUT" && key.ends_with("/image.json") {
        let pause = state.lock().unwrap().pause_publication.take();
        if let Some(pause) = pause {
            let _ = pause.reached.send(());
            if !matches!(
                tokio::time::timeout(Duration::from_secs(20), pause.resume).await,
                Ok(Ok(()))
            ) {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    let mut data = state.lock().unwrap();
    if let Some((name, order)) = &data.order {
        order.lock().unwrap().push(format!("{name}:{method}:{key}"));
    }
    data.requests.push((
        method.clone(),
        key.clone(),
        headers.contains_key("authorization"),
    ));
    let fault = data
        .fault
        .filter(|(_, suffix)| {
            (key.ends_with(suffix) || key.contains(suffix))
                && (method == "GET"
                    || matches!(
                        data.fault,
                        Some((Fault::FailUpload | Fault::LostWriteAck, _))
                    ))
        })
        .map(|(fault, _)| fault);
    if fault.is_some() {
        data.injected += 1;
    }
    if let Some(Fault::BusyOnce) = fault {
        data.fault = None;
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("Retry-After", "0")],
            "busy",
        )
            .into_response();
    }
    if let Some(Fault::Missing) = fault {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(Fault::Denied) = fault {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(Fault::Redirect) = fault {
        return (
            StatusCode::FOUND,
            [("Location", "http://127.0.0.1:9/forbidden")],
        )
            .into_response();
    }
    if method == "PUT" {
        if let Some(Fault::FailUpload) = fault {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Retry-After", "0")],
                "busy",
            )
                .into_response();
        }
        if headers.get("if-none-match").is_some_and(|h| h == "*") && data.objects.contains_key(&key)
        {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        if let Some(expected) = headers.get("if-match")
            && !data
                .objects
                .get(&key)
                .is_some_and(|bytes| expected == etag(bytes).as_str())
        {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        data.objects.insert(key.clone(), body.to_vec());
        if let Some(Fault::LostWriteAck) = fault {
            data.fault = None;
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [("Retry-After", "0")],
                "lost acknowledgement",
            )
                .into_response();
        }
        return (StatusCode::OK, [("ETag", etag(&body))]).into_response();
    }
    let Some(value) = data.objects.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert("ETag", etag(value).parse().unwrap());
    response_headers.insert(
        "Last-Modified",
        "Wed, 01 Jan 2025 00:00:00 GMT".parse().unwrap(),
    );
    let (status, mut bytes) = if let Some(range) = headers
        .get("range")
        .filter(|_| !matches!(fault, Some(Fault::IgnoreRange)))
    {
        let (start, end) = range
            .to_str()
            .unwrap()
            .strip_prefix("bytes=")
            .unwrap()
            .split_once('-')
            .unwrap();
        let start: usize = start.parse().unwrap();
        let end: usize = end.parse().unwrap();
        if end >= value.len() {
            return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        }
        let reported = if matches!(fault, Some(Fault::WrongRange)) {
            start + 1
        } else {
            start
        };
        response_headers.insert(
            "Content-Range",
            format!("bytes {reported}-{end}/{}", value.len())
                .parse()
                .unwrap(),
        );
        (StatusCode::PARTIAL_CONTENT, value[start..=end].to_vec())
    } else {
        (StatusCode::OK, value.clone())
    };
    if matches!(fault, Some(Fault::Corrupt)) && !bytes.is_empty() {
        bytes[0] ^= 1;
    }
    if matches!(fault, Some(Fault::Truncate)) {
        response_headers.insert("Content-Length", bytes.len().to_string().parse().unwrap());
        let prefix = bytes[..bytes.len() / 2].to_vec();
        let stream = futures_util::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from(prefix)),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "injected body reset",
            )),
        ]);
        return (status, response_headers, Body::from_stream(stream)).into_response();
    }
    (status, response_headers, bytes).into_response()
}

fn etag(bytes: &[u8]) -> String {
    format!(
        "\"{}\"",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn success(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "isolated subprocess helper invoked by repository integration tests"]
fn repository_reader_child() {
    let path =
        std::env::var_os("PVISOR_TEST_IMAGE_RECORD").expect("internal helper requires a fixture");
    let image: PreparedImage = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let location = image.pooled_image.cache.repository.unwrap();
    let mut repositories = vec![location];
    repositories.extend(image.pooled_image.cache.mirrors);
    let client = pvisor::cache::CacheClient::from_repositories(&repositories).unwrap();
    let (_, bytes) = client
        .request(pvisor::cache::Request::Read {
            digest: image.pooled_image.cache.handle,
            path: b"hello".to_vec(),
            offset: 6,
            length: 4,
        })
        .unwrap();
    assert_eq!(bytes, b"from");
}

#[test]
fn s3_publication_http_mirror_and_signed_lazy_reads() {
    let fixture = Fixture::new();
    let server = Server::new();
    let published = success(fixture.publish(&server));
    let source = published["source"].as_str().unwrap();
    assert!(source.starts_with("s3://images/releases/"));
    assert_eq!(
        success(fixture.publish(&server))["source"],
        source,
        "idempotent publication"
    );
    assert!(!fixture.images.join(".ipfs").exists());
    let http = format!("{}/{}", server.url, source.strip_prefix("s3://").unwrap());
    let import_start = server.data.lock().unwrap().requests.len();
    for (name, repository) in [("s3-reader", source), ("http-reader", http.as_str())] {
        let images = fixture._root.path().join(name);
        success(fixture.pull(&server, &images, Some(repository), false));
        let installed: PreparedImage =
            serde_json::from_slice(&std::fs::read(images.join("tiny.json")).unwrap()).unwrap();
        assert_eq!(
            installed.pooled_image.cache.repository.as_deref(),
            Some(repository)
        );
        assert!(
            !installed.pooled_image.cache.location.exists(),
            "must not pull a complete local pool"
        );
        assert!(!images.join(".ipfs").exists());
        assert!(
            !fixture
                .pull(&server, &images, Some(repository), false)
                .status
                .success(),
            "never overwrite"
        );
    }
    let before_read = server.data.lock().unwrap().requests.len();
    for name in ["s3-reader", "http-reader"] {
        let output = fixture
            .reader(&server, &fixture._root.path().join(name), "read-cache")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let guard = server.data.lock().unwrap();
    let requests = &guard.requests;
    assert!(
        requests
            .iter()
            .filter(|(method, _, _)| method == "PUT")
            .all(|(_, _, signed)| *signed)
    );
    assert!(
        !requests[import_start..before_read]
            .iter()
            .any(|(method, key, _)| method == "GET" && key.contains("/data/")),
        "import may fetch metadata, never complete file contents"
    );
    let data_reads = requests[before_read..]
        .iter()
        .filter(|(method, key, _)| method == "GET" && key.contains("/data/blobs/"))
        .map(|(_, key, _)| key)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        data_reads.len(),
        1,
        "only the requested small-file blob is read"
    );
    assert!(
        guard
            .objects
            .keys()
            .filter(|key| key.contains("/data/blobs/"))
            .count()
            > 1,
        "fixture must contain additional unread data blobs"
    );
    assert!(
        requests[import_start..]
            .iter()
            .any(|(_, _, signed)| *signed),
        "S3 reader must authenticate"
    );
    assert!(
        requests[import_start..]
            .iter()
            .any(|(_, _, signed)| !signed),
        "HTTP reader must be anonymous"
    );
}

#[test]
fn prepared_image_commands_retain_template_metadata_and_ignore_retired_policy() {
    let fixture = Fixture::new();
    let server = Server::new();
    let path = fixture.images.join("tiny.json");
    let mut original: PreparedImage =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    original.e2b = Some(pvisor_daemon::api::E2bTemplate {
        cpu_millis: 1500,
        memory_bytes: 768 * 1024 * 1024,
        envd_version: "0.10.2".into(),
    });
    original.env.insert("EXAMPLE".into(), "template".into());
    original.cmd = vec!["/bin/sleep".into(), "infinity".into()];
    std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let catalog = PreparedCatalog::new(fixture.images.clone());
    assert!(
        catalog
            .prepare("tiny", |_| anyhow::bail!("fixture build failed"))
            .is_err()
    );
    assert_eq!(before, std::fs::read(&path).unwrap());
    let published = success(fixture.catalog(
        &server,
        &fixture.images,
        CatalogOperation::Push("tiny".into(), "s3://images/releases".into()),
    ));
    assert_eq!(before, std::fs::read(&path).unwrap());
    let receiver = fixture._root.path().join("receiver");
    let source = published["source"].as_str().unwrap();
    let start = server.data.lock().unwrap().requests.len();
    success(fixture.catalog(
        &server,
        &receiver,
        CatalogOperation::Pull("copy".into(), Some(source.into()), false),
    ));
    let imported_path = receiver.join("copy.json");
    let installed = std::fs::read(&imported_path).unwrap();
    let imported: PreparedImage = serde_json::from_slice(&installed).unwrap();
    let mut expected = original;
    expected.rootfs = receiver.join("store");
    expected.pooled_image.cache.location = receiver.join("store/images/image-pack-v1");
    expected.pooled_image.cache.repository = Some(source.into());
    expected.library_dir = None;
    assert_eq!(
        serde_json::to_value(imported).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert!(
        !server.data.lock().unwrap().requests[start..]
            .iter()
            .any(|(_, key, _)| key.contains("/data/"))
    );
    let reads = server.data.lock().unwrap().requests.len();
    let inspected =
        success(fixture.catalog(&server, &receiver, CatalogOperation::Inspect("copy".into())));
    assert_eq!(
        inspected["Pvisor"]["ImageHandle"],
        published["image_handle"]
    );
    assert_eq!(inspected["RepoTags"], serde_json::json!(["copy"]));
    let listed = success(fixture.catalog(&server, &receiver, CatalogOperation::List));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(server.data.lock().unwrap().requests.len(), reads);
    assert!(
        !fixture
            .catalog(
                &server,
                &receiver,
                CatalogOperation::Pull("copy".into(), Some(source.into()), false)
            )
            .status
            .success()
    );
    server.inject(Fault::Corrupt, "root.bin");
    let fresh = fixture._root.path().join("refresh-home");
    let failed = fixture.catalog_with(
        &server,
        &receiver,
        CatalogOperation::Pull("copy".into(), Some(source.into()), true),
        |command| {
            command
                .env("HOME", fresh.join("home"))
                .env("XDG_CACHE_HOME", fresh.join("cache"));
        },
    );
    assert!(!failed.status.success());
    assert_eq!(installed, std::fs::read(&imported_path).unwrap());
    server.data.lock().unwrap().fault = None;
    std::fs::write(receiver.join(".distribution/copy.json"), b"invalid JSON").unwrap();
    success(fixture.catalog(
        &server,
        &receiver,
        CatalogOperation::Pull("copy".into(), Some(source.into()), true),
    ));
    assert_eq!(installed, std::fs::read(imported_path).unwrap());
    let missing = fixture._root.path().join("absent");
    assert_eq!(
        success(fixture.catalog(&server, &missing, CatalogOperation::List)),
        serde_json::json!([])
    );
    assert!(
        !fixture
            .catalog(
                &server,
                &missing,
                CatalogOperation::Inspect("unknown".into())
            )
            .status
            .success()
    );
    assert!(
        !missing.exists(),
        "queries must not create a catalog or resolve names"
    );
}

#[test]
fn invalid_template_refresh_leaves_the_installed_record_unchanged() {
    let fixture = Fixture::new();
    let server = Server::new();
    let good = success(fixture.publish(&server));
    let receiver = fixture._root.path().join("receiver");
    success(fixture.pull(&server, &receiver, good["source"].as_str(), false));
    let installed = receiver.join("tiny.json");
    let before = std::fs::read(&installed).unwrap();

    let path = fixture.images.join("tiny.json");
    let mut template: PreparedImage =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(template.native_guest);
    template.native_opensandbox = true;
    std::fs::write(path, serde_json::to_vec(&template).unwrap()).unwrap();
    let invalid = success(fixture.publish(&server));
    assert_ne!(invalid["source"], good["source"]);
    let rejected = fixture.pull(&server, &receiver, invalid["source"].as_str(), true);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("exclusive pooled engine profile"));
    assert_eq!(std::fs::read(installed).unwrap(), before);
}

#[test]
fn older_prepared_publications_without_oci_config_remain_importable() {
    let fixture = Fixture::new();
    let server = Server::new();
    let publication = success(fixture.publish(&server));
    server
        .data
        .lock()
        .unwrap()
        .objects
        .retain(|key, _| !key.contains("/configs/"));
    let receiver = fixture._root.path().join("legacy-reader");
    success(fixture.pull(&server, &receiver, publication["source"].as_str(), false));
    assert!(receiver.join("tiny.json").exists());
}

#[test]
fn chaos_publication_failure_and_lost_ack_reconcile_without_partial_visibility() {
    let fixture = Fixture::new();
    let server = Server::new();
    server.inject(Fault::FailUpload, "format.json");
    assert!(!fixture.publish(&server).status.success());
    assert!(
        !server
            .data
            .lock()
            .unwrap()
            .objects
            .keys()
            .any(|key| key.ends_with("/image.json"))
    );
    assert!(!fixture.images.join(".distribution/tiny.json").exists());
    server.inject(Fault::LostWriteAck, "image.json");
    let publication = success(fixture.publish(&server));
    assert!(
        publication["image_handle"]
            .as_str()
            .unwrap()
            .starts_with("pvisor-image-pack:v1:")
    );
    assert!(!fixture.images.join(".distribution/tiny.json").exists());
    assert!(server.data.lock().unwrap().injected > 0);
}

#[test]
fn filesystem_export_can_be_served_by_an_ordinary_static_mirror() {
    let fixture = Fixture::new();
    let server = Server::new();
    let destination = format!("file://{}", fixture._root.path().join("mirror").display());
    let result = success(fixture.catalog(
        &server,
        &fixture.images,
        CatalogOperation::Push("tiny".into(), destination),
    ));
    let root = PathBuf::from(
        result["source"]
            .as_str()
            .unwrap()
            .strip_prefix("file://")
            .unwrap(),
    );
    fn mirror(root: &Path, dir: &Path, data: &mut Data) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                mirror(root, &path, data);
            } else {
                data.objects.insert(
                    format!("cdn/{}", path.strip_prefix(root).unwrap().display()),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    mirror(&root, &root, &mut server.data.lock().unwrap());
    let images = fixture._root.path().join("mirror-reader");
    success(fixture.pull(
        &server,
        &images,
        Some(&format!("{}/cdn", server.url)),
        false,
    ));
    let output = fixture
        .reader(&server, &images, "mirror-cache")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.images.join(".ipfs").exists());
}

#[test]
fn chaos_import_faults_fail_closed_and_recover_without_poisoning_cache() {
    let fixture = Fixture::new();
    let server = Server::new();
    let published = success(fixture.publish(&server));
    let source = published["source"].as_str().unwrap();
    let http = format!("{}/{}", server.url, source.strip_prefix("s3://").unwrap());
    // Fixed fault inventory, not an unbounded randomized load generator.
    for (round, (fault, suffix)) in [
        (Fault::Missing, "COMMIT.json"),
        (Fault::Denied, "COMMIT.json"),
        (Fault::WrongRange, "index.bin"),
        (Fault::IgnoreRange, "index.bin"),
        (Fault::Corrupt, "root.bin"),
        (Fault::Corrupt, "image.json"),
        (Fault::Truncate, "index.bin"),
        (Fault::Redirect, "COMMIT.json"),
    ]
    .into_iter()
    .enumerate()
    {
        let images = fixture._root.path().join(format!("fault-{round}"));
        server.inject(fault, suffix);
        let output = fixture.pull(&server, &images, Some(&http), false);
        assert!(!output.status.success(), "{fault:?} unexpectedly accepted");
        assert!(
            server.data.lock().unwrap().injected > 0,
            "fault did not reach transport"
        );
        assert!(
            !images.join("tiny.json").exists(),
            "failed import became visible"
        );
        server.data.lock().unwrap().fault = None;
        success(fixture.pull(&server, &images, Some(&http), false));
    }
    let images = fixture._root.path().join("transient");
    server.inject(Fault::BusyOnce, "COMMIT.json");
    success(fixture.pull(&server, &images, Some(&http), false));
}

#[test]
fn chaos_file_content_faults_are_verified_and_recover_for_both_transports() {
    let fixture = Fixture::new();
    let server = Server::new();
    let published = success(fixture.publish(&server));
    let s3 = published["source"].as_str().unwrap();
    let http = format!("{}/{}", server.url, s3.strip_prefix("s3://").unwrap());
    for (transport, source) in [("http", http.as_str()), ("s3", s3)] {
        let images = fixture._root.path().join(transport);
        success(fixture.pull(&server, &images, Some(source), false));
        for (round, fault) in [
            Fault::Corrupt,
            Fault::WrongRange,
            Fault::Truncate,
            Fault::Denied,
        ]
        .into_iter()
        .enumerate()
        {
            let cache = format!("isolated-{round}");
            server.inject(fault, "data/blobs/");
            let output = fixture.reader(&server, &images, &cache).output().unwrap();
            assert!(!output.status.success(), "{transport} accepted {fault:?}");
            assert!(server.data.lock().unwrap().injected > 0);
            server.data.lock().unwrap().fault = None;
            let output = fixture.reader(&server, &images, &cache).output().unwrap();
            assert!(
                output.status.success(),
                "{transport} did not recover: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn copy_registry(from: &Server, to: &Server) {
    let objects = from.data.lock().unwrap().objects.clone();
    let mut data = to.data.lock().unwrap();
    for (key, bytes) in objects {
        if let Some(key) = key.strip_prefix("images/releases/") {
            data.objects.insert(format!("registry/{key}"), bytes);
        }
    }
}

#[test]
fn publishing_an_image_named_registries_cannot_replace_registry_configuration() {
    let fixture = Fixture::new();
    let origin = Server::new();
    std::fs::copy(
        fixture.images.join("tiny.json"),
        fixture.images.join("registries.json"),
    )
    .unwrap();
    success(fixture.registry(
        &origin,
        &fixture.images,
        &["add", "origin", "s3://images/releases"],
    ));
    let config = fixture.images.join(".distribution/config/registries.json");
    let before = std::fs::read(&config).unwrap();
    success(fixture.catalog(
        &origin,
        &fixture.images,
        CatalogOperation::Push("registries".into(), "s3://images/releases".into()),
    ));
    assert_eq!(std::fs::read(config).unwrap(), before);
    assert!(
        origin
            .data
            .lock()
            .unwrap()
            .objects
            .contains_key("images/releases/refs/registries.json")
    );
    assert!(
        !fixture
            .images
            .join(".distribution/registries.json")
            .exists()
    );
    let listed = success(fixture.registry(&origin, &fixture.images, &["list"]));
    assert_eq!(listed["registries"][0]["name"], "origin");
}

#[test]
fn configured_registry_resolves_once_and_fails_over_verified_downloads_in_order() {
    let fixture = Fixture::new();
    let origin = Server::new();
    let mirror = Server::new();
    success(fixture.registry(
        &origin,
        &fixture.images,
        &["add", "origin", "s3://images/releases"],
    ));
    let publication = success(fixture.publish(&origin));
    let source = publication["source"].as_str().unwrap();
    copy_registry(&origin, &mirror);
    let images = fixture._root.path().join("configured-reader");
    let mirror_root = format!("{}/registry", mirror.url);
    success(fixture.registry(
        &origin,
        &images,
        &["add", "cdn", &mirror_root, "--download-only"],
    ));
    success(fixture.registry(&origin, &images, &["add", "origin", "s3://images/releases"]));
    let order = Arc::new(Mutex::new(vec![]));
    mirror.data.lock().unwrap().order = Some(("cdn".into(), order.clone()));
    origin.data.lock().unwrap().order = Some(("origin".into(), order.clone()));
    let result = success(fixture.pull(&origin, &images, None, false));
    assert_eq!(result["mirrors"][0], source);
    let trace = order.lock().unwrap().clone();
    assert!(
        !trace
            .iter()
            .any(|s| s.starts_with("cdn:") && s.contains("refs/")),
        "download-only CDN cannot resolve names"
    );
    let resolved = trace
        .iter()
        .position(|s| s.starts_with("origin:GET:") && s.contains("refs/tiny.json"))
        .unwrap();
    let downloaded = trace
        .iter()
        .position(|s| s.starts_with("cdn:GET:") && s.ends_with("image.json"))
        .unwrap();
    assert!(resolved < downloaded);
    assert!(
        !trace.iter().any(|s| s.contains("/data/")),
        "resolution must not download full contents"
    );
    for (round, fault) in [
        Fault::Corrupt,
        Fault::WrongRange,
        Fault::Truncate,
        Fault::Missing,
    ]
    .into_iter()
    .enumerate()
    {
        order.lock().unwrap().clear();
        mirror.inject(fault, "data/blobs/");
        let output = fixture
            .reader(&origin, &images, &format!("ordered-{round}"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mirror fallback failed for {fault:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let trace = order.lock().unwrap().clone();
        let first = trace
            .iter()
            .position(|s| s.starts_with("cdn:GET:") && s.contains("/data/blobs/"))
            .unwrap();
        let second = trace
            .iter()
            .position(|s| s.starts_with("origin:GET:") && s.contains("/data/blobs/"))
            .unwrap();
        assert!(first < second);
        assert!(
            !trace.iter().any(|s| s.contains("/refs/")),
            "file reads must not re-resolve a mutable name"
        );
    }
    mirror.inject(Fault::Corrupt, "data/blobs/");
    origin.inject(Fault::Corrupt, "data/blobs/");
    let output = fixture
        .reader(&origin, &images, "all-corrupt")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "all corrupt mirrors must fail closed"
    );
    origin.data.lock().unwrap().fault = None;
    let output = fixture
        .reader(&origin, &images, "all-corrupt")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "recovery must not be poisoned by unverified cache bytes"
    );
    for (round, suffix) in ["image.json", "root.bin", "index.bin"]
        .into_iter()
        .enumerate()
    {
        mirror.inject(Fault::Corrupt, suffix);
        let metadata = fixture
            ._root
            .path()
            .join(format!("metadata-mirror-{round}"));
        success(fixture.registry(
            &origin,
            &metadata,
            &["add", "cdn", &mirror_root, "--download-only"],
        ));
        success(fixture.registry(
            &origin,
            &metadata,
            &["add", "origin", "s3://images/releases"],
        ));
        success(fixture.pull(&origin, &metadata, None, false));
        assert!(
            mirror.data.lock().unwrap().injected > 0,
            "metadata corruption was not exercised"
        );
    }
}

#[test]
fn registry_reference_commit_failure_keeps_old_name_and_lost_ack_is_reconciled() {
    let fixture = Fixture::new();
    let origin = Server::new();
    let initial = success(fixture.publish(&origin));
    let reference = origin.data.lock().unwrap().objects["images/releases/refs/tiny.json"].clone();
    let manifest = fixture.images.join("tiny.json");
    let mut image: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    image["cmd"] = serde_json::json!(["updated"]);
    std::fs::write(&manifest, serde_json::to_vec(&image).unwrap()).unwrap();
    let before = std::fs::read(&manifest).unwrap();
    origin.inject(Fault::FailUpload, "refs/tiny.json");
    assert!(!fixture.publish(&origin).status.success());
    assert_eq!(
        origin.data.lock().unwrap().objects["images/releases/refs/tiny.json"],
        reference
    );
    assert_eq!(std::fs::read(manifest).unwrap(), before);
    assert!(!fixture.images.join(".distribution/tiny.json").exists());
    origin.inject(Fault::LostWriteAck, "refs/tiny.json");
    let next = success(fixture.publish(&origin));
    assert_ne!(initial["source"], next["source"]);
    let reference: serde_json::Value = serde_json::from_slice(
        &origin.data.lock().unwrap().objects["images/releases/refs/tiny.json"],
    )
    .unwrap();
    assert_eq!(
        reference["publication"].as_str().unwrap(),
        next["source"].as_str().unwrap().rsplit('/').next().unwrap()
    );
}

#[test]
fn slower_publisher_cannot_overwrite_a_concurrent_reference_winner() {
    let initial = Fixture::new();
    let first = Fixture::new();
    let second = Fixture::new();
    let origin = Server::new();
    let prior = success(initial.publish(&origin));
    for (fixture, command) in [(&first, "first"), (&second, "second")] {
        let path = fixture.images.join("tiny.json");
        let mut image: PreparedImage =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        image.cmd = vec![command.into()];
        std::fs::write(path, serde_json::to_vec(&image).unwrap()).unwrap();
    }
    let winner = std::thread::scope(|scope| {
        let (reached, waiting) = std::sync::mpsc::channel();
        let (resume, paused) = tokio::sync::oneshot::channel();
        origin.data.lock().unwrap().pause_publication = Some(PublicationPause {
            reached,
            resume: paused,
        });
        let slow = scope.spawn(|| first.publish(&origin));
        waiting.recv_timeout(Duration::from_secs(15)).unwrap();
        let fast = second.publish(&origin);
        resume.send(()).unwrap();
        let output = slow.join().unwrap();
        let winner = success(fast);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("registry reference commit failed")
        );
        winner
    });
    assert_ne!(winner["source"], prior["source"]);
    let reference: serde_json::Value = serde_json::from_slice(
        &origin.data.lock().unwrap().objects["images/releases/refs/tiny.json"],
    )
    .unwrap();
    assert_eq!(
        reference["publication"],
        winner["source"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
    );
}

#[test]
fn registry_fallback_never_changes_the_selected_publication_and_failed_pull_is_atomic() {
    let fixture = Fixture::new();
    let origin = Server::new();
    let first = Server::new();
    let published = success(fixture.publish(&origin));
    let old = published["source"].as_str().unwrap();
    let old_hash = old.rsplit('/').next().unwrap();
    copy_registry(&origin, &first);
    let old_objects = origin.data.lock().unwrap().objects.clone();
    let images = fixture._root.path().join("pinned-reader");
    success(fixture.registry(
        &origin,
        &images,
        &["add", "first", &format!("{}/registry", first.url)],
    ));
    success(fixture.registry(&origin, &images, &["add", "origin", "s3://images/releases"]));
    success(fixture.pull(&origin, &images, None, false));
    let installed = std::fs::read(images.join("tiny.json")).unwrap();
    // Publish a new launch revision through the origin's mutable index.
    let path = fixture.images.join("tiny.json");
    let mut template: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    template["cmd"] = serde_json::json!(["new-release"]);
    std::fs::write(path, serde_json::to_vec(&template).unwrap()).unwrap();
    let new = success(fixture.publish(&origin));
    assert_ne!(new["source"], old);
    // The first registry still resolves A, but neither source can deliver A.
    first
        .data
        .lock()
        .unwrap()
        .objects
        .retain(|key, _| key.contains("/refs/"));
    origin
        .data
        .lock()
        .unwrap()
        .objects
        .retain(|key, _| !key.contains(&format!("/{old_hash}/")));
    let order = Arc::new(Mutex::new(vec![]));
    origin.data.lock().unwrap().order = Some(("origin".into(), order.clone()));
    let output = fixture.pull(&origin, &images, None, true);
    assert!(
        !output.status.success(),
        "must fail rather than silently use origin's different release"
    );
    assert_eq!(std::fs::read(images.join("tiny.json")).unwrap(), installed);
    assert!(
        !order
            .lock()
            .unwrap()
            .iter()
            .any(|key| key.contains("/refs/")),
        "must not re-resolve after selecting A"
    );
    // Restoring A makes the already installed, pinned image usable again even
    // though origin's mutable name now identifies B.
    {
        let mut data = first.data.lock().unwrap();
        for (key, bytes) in old_objects {
            if let Some(key) = key.strip_prefix("images/releases/") {
                data.objects.insert(format!("registry/{key}"), bytes);
            }
        }
    }
    let output = fixture
        .reader(&origin, &images, "restored")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    order.lock().unwrap().clear();
    success(fixture.catalog(&origin, &images, CatalogOperation::Inspect("tiny".into())));
    assert!(order.lock().unwrap().is_empty());
    // Explicit refresh can choose B after the higher-priority name is absent.
    first
        .data
        .lock()
        .unwrap()
        .objects
        .remove("registry/refs/tiny.json");
    let refreshed = success(fixture.pull(&origin, &images, None, true));
    assert_eq!(
        refreshed["source"].as_str().unwrap().rsplit('/').next(),
        new["source"].as_str().unwrap().rsplit('/').next()
    );
}

#[test]
fn registry_configuration_is_ordered_atomic_and_rejects_unsafe_or_duplicate_entries() {
    let fixture = Fixture::new();
    let server = Server::new();
    let images = fixture._root.path().join("settings");
    assert!(
        success(fixture.registry(&server, &images, &["list"]))["registries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!images.exists(), "read-only listing must not create state");
    success(fixture.registry(
        &server,
        &images,
        &["add", "first", "https://first.example/pvisor"],
    ));
    success(fixture.registry(&server, &images, &["add", "origin", "s3://images/releases"]));
    let moved = success(fixture.registry(&server, &images, &["move", "origin", "1"]));
    assert_eq!(moved["registries"][0]["name"], "origin");
    let before = std::fs::read(images.join(".distribution/config/registries.json")).unwrap();
    for args in [
        vec!["add", "origin", "https://other.example/root"],
        vec!["add", "duplicate", "https://first.example/pvisor/"],
        vec!["add", "unsafe", "https://user:placeholder@example/root"],
        vec!["move", "origin", "0"],
        vec!["remove", "unknown"],
    ] {
        assert!(!fixture.registry(&server, &images, &args).status.success());
        assert_eq!(
            std::fs::read(images.join(".distribution/config/registries.json")).unwrap(),
            before
        );
    }
    let removed = success(fixture.registry(&server, &images, &["remove", "first"]));
    assert_eq!(removed["registries"].as_array().unwrap().len(), 1);
}

#[test]
fn concurrent_registry_configuration_changes_do_not_lose_updates() {
    let fixture = Fixture::new();
    let server = Server::new();
    let images = fixture._root.path().join("concurrent-settings");
    let mut tasks = vec![];
    for (name, location) in [("one", "s3://images/one"), ("two", "s3://images/two")] {
        let mut command =
            fixture.environment(&server, &images, env!("CARGO_BIN_EXE_pvisor-daemon"));
        command
            .arg("registry")
            .arg("--images-dir")
            .arg(&images)
            .args(["add", name, location]);
        tasks.push(std::thread::spawn(move || command.output().unwrap()));
    }
    for task in tasks {
        success(task.join().unwrap());
    }
    let result = success(fixture.registry(&server, &images, &["list"]));
    let mut names: Vec<_> = result["registries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["one", "two"]);
    assert!(
        server.data.lock().unwrap().requests.is_empty(),
        "configuration must never probe the network"
    );
}

#[test]
fn offline_registry_is_skipped_and_explicit_publication_ignores_registry_settings() {
    let fixture = Fixture::new();
    let origin = Server::new();
    let offline = Server::new();
    let offline_url = format!("{}/registry", offline.url);
    drop(offline);
    let publication = success(fixture.publish(&origin));
    let images = fixture._root.path().join("offline-reader");
    success(fixture.registry(&origin, &images, &["add", "offline", &offline_url]));
    success(fixture.registry(&origin, &images, &["add", "origin", "s3://images/releases"]));
    let resolved = success(fixture.pull(&origin, &images, None, false));
    assert_eq!(resolved["mirrors"][0], publication["source"]);
    let output = fixture
        .reader(&origin, &images, "offline")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    success(fixture.registry(
        &origin,
        &fixture.images,
        &["add", "wrong", "s3://other-bucket/ignored"],
    ));
    let selected = success(fixture.publish(&origin));
    assert_eq!(selected["source"], publication["source"]);
    assert!(
        !origin
            .data
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(_, key, _)| key.contains("other-bucket"))
    );
}

/// Explicit special runner: stdlib Python TLS/Range server and OpenSSL create an
/// ephemeral self-signed certificate. No disabled validation or committed keys.
#[test]
#[ignore = "requires python3 and openssl; run explicitly with nextest"]
fn https_repository_checks_certificates_and_reads_ranges() {
    use std::io::BufRead;
    let fixture = Fixture::new();
    let server = Server::new();
    let published = success(fixture.publish(&server));
    let source = published["source"]
        .as_str()
        .unwrap()
        .strip_prefix("s3://")
        .unwrap();
    let web = fixture._root.path().join("web");
    for (key, bytes) in &server.data.lock().unwrap().objects {
        let path = web.join(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let cert = fixture._root.path().join("test-ca.pem");
    let key = fixture._root.path().join("test-key.pem");
    let output = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "ed25519",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-days",
            "1",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let script = r#"
import http.server, pathlib, ssl, sys, urllib.parse
root = pathlib.Path(sys.argv[1]).resolve()
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        path = (root / urllib.parse.unquote(self.path).lstrip('/')).resolve()
        if not path.is_relative_to(root) or not path.is_file():
            self.send_error(404); return
        data = path.read_bytes()
        start, end = 0, len(data) - 1
        partial = self.headers.get('Range')
        if partial:
            start, end = map(int, partial.removeprefix('bytes=').split('-'))
            if start < 0 or end >= len(data):
                self.send_error(416); return
        self.send_response(206 if partial else 200)
        self.send_header('Content-Length', str(end - start + 1))
        if partial: self.send_header('Content-Range', f'bytes {start}-{end}/{len(data)}')
        self.end_headers()
        self.wfile.write(data[start:end+1])
server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[2], sys.argv[3])
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_port, flush=True)
server.serve_forever()
"#;
    let mut child = Command::new("python3")
        .args(["-u", "-c", script])
        .arg(&web)
        .arg(&cert)
        .arg(&key)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _process = Process(child);
    let (send, receive) = std::sync::mpsc::channel();
    let readiness = std::thread::spawn(move || {
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .unwrap();
        let _ = send.send(line);
    });
    let port: u16 = receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .trim()
        .parse()
        .expect("TLS server did not publish a valid port");
    readiness.join().unwrap();
    let location = format!("https://127.0.0.1:{port}/{source}");
    let images = fixture._root.path().join("tls-reader");
    assert!(
        !fixture
            .pull(&server, &images, Some(&location), false)
            .status
            .success(),
        "untrusted CA accepted"
    );
    assert!(!images.join("tiny.json").exists());
    let mismatch = fixture._root.path().join("tls-hostname");
    let rejected = fixture.catalog_with(
        &server,
        &mismatch,
        CatalogOperation::Pull(
            "tiny".into(),
            Some(location.replace("127.0.0.1", "localhost")),
            false,
        ),
        |command| {
            command.env("SSL_CERT_FILE", &cert);
        },
    );
    assert!(
        !rejected.status.success(),
        "hostname verification was bypassed"
    );
    assert!(!mismatch.join("tiny.json").exists());
    success(fixture.catalog_with(
        &server,
        &images,
        CatalogOperation::Pull("tiny".into(), Some(location), false),
        |command| {
            command.env("SSL_CERT_FILE", &cert);
        },
    ));
    let output = fixture
        .reader(&server, &images, "tls-cache")
        .env("SSL_CERT_FILE", &cert)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
