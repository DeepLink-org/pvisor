use anyhow::{Result, bail};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use pvisor_daemon::api::{
    Config, CreateError, Daemon, DaemonService, E2bTemplate, MemoryPoolService, NativeRuntime,
    NativeRuntimeConfig, NativeRuntimeService, PoolConfig, PoolMode, Runtime, RuntimeSpec,
    RuntimeState, RuntimeValidation,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tempfile::TempDir;
use tower::ServiceExt;

const KEY: &str = "01234567890123456789012345678901";
const MIB: u64 = 1024 * 1024;

fn spec() -> RuntimeSpec {
    RuntimeSpec {
        id: "sb-12345678-1234-4234-8234-123456789abc".into(),
        image: "base".into(),
        entrypoint: vec!["sh".into()],
        env: BTreeMap::new(),
        cpu_millis: 1000,
        memory_bytes: 64 * MIB,
        e2b: None,
    }
}
fn config(dir: &TempDir) -> Config {
    Config {
        state_dir: dir.path().into(),
        api_key: KEY.into(),
        public_endpoint: "localhost:8080".into(),
        max_sandboxes: 1,
        cpu_millis: 1000,
        memory_bytes: 64 * MIB,
        max_timeout_seconds: 86400,
    }
}
fn create() -> Value {
    json!({"image":{"uri":"base"}, "entrypoint":["sh"], "resourceLimits":{"cpu":"1","memory":"64Mi"}})
}

#[derive(Default)]
struct Backend {
    states: Mutex<BTreeMap<String, RuntimeState>>,
    specs: Mutex<Vec<RuntimeSpec>>,
    fail_preflight: AtomicBool,
    unknown_create_error: AtomicBool,
    create_failure: Mutex<Option<bool>>,
    fail_delete: AtomicBool,
}
#[async_trait::async_trait]
impl Runtime for Backend {
    async fn preflight(&self) -> Result<()> {
        anyhow::ensure!(
            !self.fail_preflight.load(Ordering::SeqCst),
            "preflight unavailable"
        );
        Ok(())
    }
    async fn create(&self, spec: &RuntimeSpec) -> Result<()> {
        spec.validate()?;
        anyhow::ensure!(
            !self.unknown_create_error.load(Ordering::SeqCst),
            "unknown create outcome"
        );
        self.specs.lock().unwrap().push(spec.clone());
        if let Some(uncertain) = *self.create_failure.lock().unwrap() {
            return Err(CreateError {
                message: "injected create failure".into(),
                requires_reconciliation: uncertain,
            }
            .into());
        }
        self.states
            .lock()
            .unwrap()
            .insert(spec.id.clone(), RuntimeState::Running);
        Ok(())
    }
    async fn inspect(&self, id: &str) -> Result<RuntimeState> {
        Ok(self
            .states
            .lock()
            .unwrap()
            .get(id)
            .copied()
            .unwrap_or(RuntimeState::Missing))
    }
    async fn pause(&self, id: &str) -> Result<RuntimeState> {
        self.states
            .lock()
            .unwrap()
            .insert(id.into(), RuntimeState::Paused);
        Ok(RuntimeState::Paused)
    }
    async fn resume(&self, id: &str) -> Result<RuntimeState> {
        self.states
            .lock()
            .unwrap()
            .insert(id.into(), RuntimeState::Running);
        Ok(RuntimeState::Running)
    }
    async fn delete(&self, id: &str) -> Result<()> {
        anyhow::ensure!(
            !self.fail_delete.load(Ordering::SeqCst),
            "cleanup uncertain"
        );
        self.states.lock().unwrap().remove(id);
        Ok(())
    }
    async fn endpoint(&self, _: &str, _: u16) -> Result<String> {
        bail!("unsupported endpoint")
    }
    async fn e2b_template(&self, _: &str) -> Result<E2bTemplate> {
        bail!("E2B prepared templates are not supported by this runtime")
    }
}
async fn open(dir: &TempDir, backend: &Arc<Backend>) -> Arc<Daemon> {
    let backend = backend.clone();
    Daemon::open(config(dir), move |_| Ok(backend as Arc<dyn Runtime>))
        .await
        .unwrap()
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    keys: &[&str],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    for key in keys {
        req = req.header("OPEN-SANDBOX-API-KEY", *key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert!(response.headers().contains_key("X-Request-ID"));
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[test]
fn runtime_resource_and_identity_boundaries() {
    for cpu in [10, 8000] {
        let mut s = spec();
        s.cpu_millis = cpu;
        assert!(s.validate().is_ok());
    }
    for cpu in [0, 9, 8001, u64::MAX] {
        let mut s = spec();
        s.cpu_millis = cpu;
        assert!(s.validate().is_err(), "{cpu}");
    }
    for memory in [0, 6 * MIB - 1, (u64::from(u32::MAX) + 1) * MIB, u64::MAX] {
        let mut s = spec();
        s.memory_bytes = memory;
        assert!(s.validate().is_err(), "{memory}");
    }
    let mut s = spec();
    s.memory_bytes = 6 * MIB;
    assert!(s.validate().is_ok());
    for id in [
        "",
        "../other",
        "sb-00000000-0000-0000-0000-000000000000",
        "sb-12345678-1234-4234-8234-123456789ABC",
    ] {
        let mut s = spec();
        s.id = id.into();
        assert!(s.validate().is_err());
    }
    for image in [
        "",
        ".hidden",
        "../escape",
        "docker.io/base",
        "https://host/base",
    ] {
        let mut s = spec();
        s.image = image.into();
        assert!(s.validate().is_err());
    }
    let mut s = spec();
    s.image = "a".repeat(128);
    assert!(s.validate().is_ok());
    s.image.push('a');
    assert!(s.validate().is_err());
}
#[test]
fn launch_inputs_are_bounded_without_mutation() {
    for (key, value) in [
        ("PVISOR_CONTROL", "secret"),
        ("AGENTCTL_TOKEN", "secret"),
        ("1BAD", "x"),
        ("GOOD", "a\0b"),
    ] {
        let mut s = spec();
        s.env.insert(key.into(), value.into());
        let before = serde_json::to_value(&s).unwrap();
        assert!(s.validate().is_err());
        assert_eq!(serde_json::to_value(&s).unwrap(), before);
    }
    let mut s = spec();
    s.entrypoint = vec!["x".repeat(65535)];
    assert!(s.validate().is_ok());
    s.entrypoint[0].push('x');
    assert!(s.validate().is_err());
    s.entrypoint = vec!["".into()];
    assert!(s.validate().is_err());
    s.entrypoint = vec!["a\0b".into()];
    assert!(s.validate().is_err());
}
#[test]
fn e2b_validation_checks_version_and_admitted_resources() {
    let template = E2bTemplate {
        cpu_millis: 1000,
        memory_bytes: 64 * MIB,
        envd_version: "0.10.2".into(),
    };
    assert!(template.validate().is_ok());
    for version in [
        "0.0.1",
        "0.0.0",
        "1.0",
        "1.0.0-beta",
        "1.0.4294967296",
        "1..0",
    ] {
        let mut t = template.clone();
        t.envd_version = version.into();
        assert!(t.validate().is_err(), "{version}");
    }
    let mut s = spec();
    s.e2b = Some(template);
    assert!(s.validate().is_err());
    s.env.insert("PVISOR_ENVD_ACCESS_TOKEN".into(), KEY.into());
    assert!(s.validate().is_ok());
    s.e2b.as_mut().unwrap().memory_bytes += MIB;
    assert!(s.validate().is_err());
}
#[test]
fn runtime_and_pool_serialization_preserves_defaults_and_rejects_unknown_fields() {
    let s = spec();
    let mut value = serde_json::to_value(&s).unwrap();
    assert!(value.get("e2b").is_none());
    assert!(serde_json::from_value::<RuntimeSpec>(value.clone()).is_ok());
    value["unknown"] = json!(true);
    assert!(serde_json::from_value::<RuntimeSpec>(value).is_err());
    let default = PoolConfig::default();
    assert_eq!(default.max_bytes, 512 * 1024 * 1024);
    assert_eq!(default.max_objects, 32768);
    assert_eq!(default.max_connections, 32);
    assert_eq!(default.max_references, 32768);
    assert_eq!(
        default.mode,
        if cfg!(target_os = "macos") {
            PoolMode::Compressed
        } else {
            PoolMode::Physical
        }
    );
    let mut value = serde_json::to_value(&default).unwrap();
    value.as_object_mut().unwrap().remove("mode");
    assert_eq!(
        serde_json::from_value::<PoolConfig>(value.clone())
            .unwrap()
            .mode,
        PoolMode::Physical
    );
    value["unknown"] = json!(true);
    assert!(serde_json::from_value::<PoolConfig>(value).is_err());
    for state in [
        RuntimeState::Running,
        RuntimeState::Paused,
        RuntimeState::Stopped,
        RuntimeState::Missing,
    ] {
        assert_eq!(
            serde_json::from_value::<RuntimeState>(serde_json::to_value(state).unwrap()).unwrap(),
            state
        );
    }
}
#[test]
fn create_error_is_downcastable_and_preserves_cleanup_certainty() {
    for requires_reconciliation in [false, true] {
        let err: anyhow::Error = CreateError {
            message: "failure".into(),
            requires_reconciliation,
        }
        .into();
        assert_eq!(err.to_string(), "failure");
        assert_eq!(
            err.downcast_ref::<CreateError>()
                .unwrap()
                .requires_reconciliation,
            requires_reconciliation
        );
    }
}
#[tokio::test]
async fn invalid_config_is_rejected_before_factory_or_disk_mutation() {
    let dir = TempDir::new().unwrap();
    for case in 0..9 {
        let mut c = config(&dir);
        match case {
            0 => c.api_key = "short".into(),
            1 => c.max_sandboxes = 0,
            2 => c.cpu_millis = 0,
            3 => c.memory_bytes = 0,
            4 => c.max_timeout_seconds = 59,
            5 => c.max_timeout_seconds = 365 * 86400 + 1,
            6 => c.public_endpoint = "host/path".into(),
            7 => c.public_endpoint = "user:pass@host".into(),
            _ => c.public_endpoint = "host?query".into(),
        };
        assert!(
            Daemon::open(c, |_| panic!("invalid input reached factory"))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
#[tokio::test]
async fn factory_and_preflight_failures_release_exclusive_lock() {
    let dir = TempDir::new().unwrap();
    assert!(
        Daemon::open(config(&dir), |_| bail!("factory rejected owner"))
            .await
            .is_err()
    );
    let backend = Arc::new(Backend::default());
    backend.fail_preflight.store(true, Ordering::SeqCst);
    let b = backend.clone();
    assert!(
        Daemon::open(config(&dir), move |_| Ok(b as Arc<dyn Runtime>))
            .await
            .is_err()
    );
    backend.fail_preflight.store(false, Ordering::SeqCst);
    let daemon = open(&dir, &backend).await;
    assert!(
        Daemon::open(config(&dir), |_| panic!("locked state reached factory"))
            .await
            .is_err()
    );
    drop(daemon);
    drop(open(&dir, &backend).await);
}
#[tokio::test]
async fn router_and_maintenance_have_explicit_owner_lifetimes() {
    let dir = TempDir::new().unwrap();
    let backend = Arc::new(Backend::default());
    let daemon = open(&dir, &backend).await;
    let maintenance = daemon.start_maintenance();
    maintenance.abort();
    assert!(maintenance.await.unwrap_err().is_cancelled());
    let weak = Arc::downgrade(&daemon);
    let app = daemon.router();
    assert!(weak.upgrade().is_some());
    assert!(
        Daemon::open(config(&dir), |_| panic!("router lost state lock"))
            .await
            .is_err()
    );
    drop(app);
    assert!(weak.upgrade().is_none());
    drop(open(&dir, &backend).await);
}
#[tokio::test]
async fn http_auth_and_validation_cannot_launch_work() {
    let dir = TempDir::new().unwrap();
    let b = Arc::new(Backend::default());
    let app = open(&dir, &b).await.router();
    for keys in [vec![], vec!["wrong"], vec![KEY, KEY]] {
        assert_eq!(
            request(&app, "POST", "/v1/sandboxes", create(), &keys)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    for (field, value, status) in [
        ("timeout", json!(59), 400),
        ("image", json!({"uri":"../other"}), 400),
        ("networkPolicy", json!({}), 501),
        ("unknown", json!(true), 400),
    ] {
        let mut body = create();
        body[field] = value;
        assert_eq!(
            request(&app, "POST", "/v1/sandboxes", body, &[KEY])
                .await
                .0
                .as_u16(),
            status
        );
    }
    assert!(b.specs.lock().unwrap().is_empty());
}
#[tokio::test]
async fn http_lifecycle_retains_capacity_until_confirmed_cleanup() {
    let dir = TempDir::new().unwrap();
    let b = Arc::new(Backend::default());
    let app = open(&dir, &b).await.router();
    let (status, body) = request(&app, "POST", "/v1/sandboxes", create(), &[KEY]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["id"].as_str().unwrap();
    let path = format!("/v1/sandboxes/{id}");
    assert_eq!(
        request(&app, "POST", &format!("{path}/pause"), Value::Null, &[KEY])
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        request(&app, "GET", &path, Value::Null, &[KEY]).await.1["status"]["state"],
        "Paused"
    );
    assert_eq!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(&app, "POST", &format!("{path}/resume"), Value::Null, &[KEY])
            .await
            .0,
        StatusCode::ACCEPTED
    );
    b.fail_delete.store(true, Ordering::SeqCst);
    assert!(
        request(&app, "DELETE", &path, Value::Null, &[KEY])
            .await
            .0
            .is_server_error()
    );
    assert_eq!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    b.fail_delete.store(false, Ordering::SeqCst);
    assert_eq!(
        request(&app, "DELETE", &path, Value::Null, &[KEY]).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0,
        StatusCode::ACCEPTED
    );
}
#[tokio::test]
async fn uncertain_create_retains_capacity_and_safe_failure_releases_it() {
    for uncertain in [false, true] {
        let dir = TempDir::new().unwrap();
        let b = Arc::new(Backend::default());
        *b.create_failure.lock().unwrap() = Some(uncertain);
        let app = open(&dir, &b).await.router();
        assert!(
            request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
                .await
                .0
                .is_server_error()
        );
        *b.create_failure.lock().unwrap() = None;
        assert_eq!(
            request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
                .await
                .0,
            if uncertain {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::ACCEPTED
            }
        );
    }
}
#[test]
fn pool_reentry_rejects_unsafe_state_and_invalid_budgets_without_binding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let c = PoolConfig::default();
    for case in 0..4 {
        let mut c = c.clone();
        match case {
            0 => c.max_bytes = 0,
            1 => c.max_objects = 0,
            2 => c.max_connections = 0,
            _ => c.max_references = 0,
        };
        std::fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        assert!(PoolConfig::run(dir.path()).is_err());
        assert!(!dir.path().join("pool.sock").exists());
    }
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        c.clone()
            .ensure_service(dir.path(), Path::new("/nonexistent"))
            .is_err()
    );
    assert!(!dir.path().join("memory-pool").exists());
    assert!(PoolConfig::run(dir.path()).is_err());
}
#[tokio::test]
async fn native_entrypoints_fail_explicitly_without_usable_platform_state() {
    let cfg = NativeRuntimeConfig {
        state_dir: "relative".into(),
        owner: "invalid".into(),
        cgroup_root: "relative".into(),
        images_dir: "relative".into(),
        executable: "relative".into(),
    };
    assert!(NativeRuntime::new(cfg).is_err());
    assert!(
        NativeRuntime::run_supervisor(Path::new("relative"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unknown_create_errors_retain_capacity() {
    let dir = TempDir::new().unwrap();
    let backend = Arc::new(Backend::default());
    backend.unknown_create_error.store(true, Ordering::SeqCst);
    let app = open(&dir, &backend).await.router();
    assert!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0
            .is_server_error()
    );
    backend.unknown_create_error.store(false, Ordering::SeqCst);
    assert_eq!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn reopen_preserves_durable_owner_live_identity_and_reservation() {
    let dir = TempDir::new().unwrap();
    let backend = Arc::new(Backend::default());
    let mut owners = Vec::new();
    let b = backend.clone();
    let daemon = Daemon::open(config(&dir), |owner| {
        owners.push(owner);
        Ok(b as Arc<dyn Runtime>)
    })
    .await
    .unwrap();
    let app = daemon.router();
    let (status, created) = request(&app, "POST", "/v1/sandboxes", create(), &[KEY]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    drop(app);
    let b = backend.clone();
    let daemon = Daemon::open(config(&dir), |owner| {
        owners.push(owner);
        Ok(b as Arc<dyn Runtime>)
    })
    .await
    .unwrap();
    assert_eq!(owners.len(), 2);
    assert_eq!(owners[0], owners[1]);
    let app = daemon.router();
    let path = format!("/v1/sandboxes/{}", created["id"].as_str().unwrap());
    let (status, observed) = request(&app, "GET", &path, Value::Null, &[KEY]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observed["id"], created["id"]);
    assert_eq!(observed["status"]["state"], "Running");
    assert_eq!(backend.specs.lock().unwrap().len(), 1);
    assert_eq!(
        request(&app, "POST", "/v1/sandboxes", create(), &[KEY])
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(&app, "DELETE", &path, Value::Null, &[KEY]).await.0,
        StatusCode::NO_CONTENT
    );
}

proptest::proptest! {
    #[test]
    fn pressure_admission_wire_preserves_owner_identity(
        id in "[a-z0-9]{1,32}", bytes in 1u64..u64::MAX,
    ) {
        use pvisor_daemon::api::{PressureEnvelope, PressureRequest};
        let envelope = PressureEnvelope {
            instance: Some("pinned-instance".into()),
            request: PressureRequest::Reserve { id: id.clone(), memory_bytes: bytes },
        };
        let wire = serde_json::to_value(&envelope).unwrap();
        proptest::prop_assert_eq!(&wire["request"]["operation"], "reserve");
        let decoded: PressureEnvelope = serde_json::from_value(wire.clone()).unwrap();
        proptest::prop_assert_eq!(decoded.instance.as_deref(), Some("pinned-instance"));
        match decoded.request {
            PressureRequest::Reserve { id: actual, memory_bytes } => {
                proptest::prop_assert_eq!(actual, id);
                proptest::prop_assert_eq!(memory_bytes, bytes);
            }
            _ => proptest::prop_assert!(false, "request variant changed"),
        }
        let mut unknown = wire;
        unknown["request"]["unrecognized"] = serde_json::json!(true);
        proptest::prop_assert!(serde_json::from_value::<PressureEnvelope>(unknown).is_err());
    }
}
