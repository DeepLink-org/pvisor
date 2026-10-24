//! E2B 2.59.0 lifecycle adapter. The data plane is the real, pinned envd.
//! Local SDKs set api_url to this daemon and sandbox_url to its /e2b path.
use super::{
    ApiError, Daemon,
    models::{self, Record, Validated},
};
use crate::runtime::{ENVD_PORT, RuntimeSpec};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, Path, RawQuery, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/sandboxes", post(create).get(list))
        .route("/v2/sandboxes", post(create).get(list))
        .route("/sandboxes/{id}", get(info).delete(delete))
        .route("/sandboxes/{id}/timeout", post(set_timeout))
        .route("/sandboxes/{id}/connect", post(connect))
        .route("/v2/sandboxes/{id}/connect", post(connect))
        .route("/sandboxes/{id}/resume", post(connect))
        .route("/sandboxes/{id}/pause", post(pause))
        .route("/sandboxes/{id}/{*feature}", any(unsupported))
        .route("/v2/sandboxes/{id}/{*feature}", any(unsupported))
        .route("/snapshots", any(unsupported))
        .route("/snapshots/{*feature}", any(unsupported))
        .route("/volumes", any(unsupported))
        .route("/volumes/{*feature}", any(unsupported))
        .route("/templates", any(unsupported))
        .route("/templates/{*feature}", any(unsupported))
        .route("/e2b", any(proxy))
        .route("/e2b/", any(proxy))
        .route("/e2b/{*path}", any(proxy))
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(daemon.clone(), policy))
        .with_state(daemon)
}

struct Error(ApiError);
impl From<ApiError> for Error {
    fn from(value: ApiError) -> Self {
        Self(value)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({"code": self.0.status, "message": self.0.message, "error_code": self.0.code}))).into_response()
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad(message: &str) -> Error {
    ApiError::bad_request(message).into()
}
fn unsupported_error() -> Error {
    ApiError::new(
        501,
        "NOT_SUPPORTED",
        "This E2B feature is not supported by the native local profile",
    )
    .into()
}
async fn unsupported() -> Error {
    unsupported_error()
}
fn one<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    if headers.get_all(name).iter().count() != 1 {
        return None;
    }
    headers.get(name)?.to_str().ok().filter(|s| !s.is_empty())
}
async fn policy(State(daemon): State<Arc<Daemon>>, request: Request, next: Next) -> Response {
    let http1 = request.version() <= axum::http::Version::HTTP_11;
    let request_id = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()).unwrap();
    let data_plane = request.uri().path() == "/e2b" || request.uri().path().starts_with("/e2b/");
    let mut response = if data_plane {
        next.run(request).await
    } else if !one(request.headers(), "x-api-key")
        .is_some_and(|key| super::secret_eq(key.as_bytes(), daemon.config.api_key.as_bytes()))
    {
        Error(ApiError::new(
            401,
            "UNAUTHORIZED",
            "Missing or invalid E2B API key",
        ))
        .into_response()
    } else {
        let (parts, body) = request.into_parts();
        match to_bytes(body, 1024 * 1024).await {
            Ok(body) => next.run(Request::from_parts(parts, Body::from(body))).await,
            Err(_) => bad("Control body is unreadable or exceeds 1 MiB").into_response(),
        }
    };
    response.headers_mut().insert("x-request-id", request_id);
    // An early data-plane rejection may leave a streamed request body unread.
    // Tell HTTP/1 clients to discard this connection before their next request.
    if http1 && (response.status().is_client_error() || response.status().is_server_error()) {
        response.headers_mut().insert(
            axum::http::header::CONNECTION,
            HeaderValue::from_static("close"),
        );
    }
    response
}
fn body<T>(value: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    value
        .map(|Json(v)| v)
        .map_err(|_| bad("Invalid E2B JSON request"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    #[serde(rename = "templateID")]
    template: String,
    timeout: Option<u64>,
    #[serde(rename = "envVars", default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    #[serde(rename = "autoPause")]
    auto_pause: Option<bool>,
    #[serde(rename = "autoPauseMemory")]
    auto_pause_memory: Option<bool>,
    #[serde(rename = "autoResume")]
    auto_resume: Option<Value>,
    secure: Option<bool>,
    allow_internet_access: Option<bool>,
    network: Option<Value>,
    mcp: Option<Value>,
    iam: Option<Value>,
    #[serde(rename = "volumeMounts")]
    volume_mounts: Option<Value>,
}
fn timeout(daemon: &Daemon, seconds: Option<u64>) -> Result<u64> {
    let seconds = seconds.unwrap_or(300);
    if seconds == 0 || seconds > daemon.config.max_timeout_seconds {
        return Err(bad("timeout must be positive and within the node limit"));
    }
    Ok(seconds)
}
async fn create(
    State(daemon): State<Arc<Daemon>>,
    request: std::result::Result<Json<Create>, JsonRejection>,
) -> Result<impl IntoResponse> {
    let request = body(request)?;
    if request.auto_pause == Some(true)
        || request.auto_pause_memory == Some(false)
        || request
            .auto_resume
            .as_ref()
            .is_some_and(|value| *value != json!({"enabled":false}))
        || request.secure == Some(false)
        || request.allow_internet_access == Some(false)
        || request.network.is_some()
        || request.mcp.is_some()
        || request.iam.is_some()
        || request.volume_mounts.is_some()
    {
        return Err(unsupported_error());
    }
    let seconds = timeout(&daemon, request.timeout)?;
    models::validate_map(&request.env, true)?;
    models::validate_map(&request.metadata, false)?;
    // Validate the local key before any manifest path construction or runtime call.
    crate::runtime::validate_spec(&RuntimeSpec {
        id: "sb-12345678-1234-4234-8234-123456789abc".into(),
        image: request.template.clone(),
        entrypoint: vec![],
        env: request.env.clone(),
        cpu_millis: 1000,
        memory_bytes: 64 * 1024 * 1024,
        e2b: None,
    })
    .map_err(|_| bad("Invalid template key or environment"))?;
    let profile = daemon
        .runtime
        .e2b_template(&request.template)
        .await
        .map_err(|_| {
            Error(ApiError::new(
                404,
                "TEMPLATE_NOT_FOUND",
                "Prepared E2B template is unavailable",
            ))
        })?;
    profile
        .validate()
        .map_err(|_| bad("Invalid prepared E2B profile"))?;
    let validated = Validated {
        image: models::Image {
            uri: request.template,
            auth: None,
        },
        entrypoint: vec![],
        timeout: Some(seconds),
        env: request.env,
        metadata: request.metadata,
        cpu_millis: profile.cpu_millis,
        memory_bytes: profile.memory_bytes,
        e2b: Some(profile),
    };
    // Accepted native creation persists even when the HTTP client disconnects.
    let created = tokio::spawn(async move {
        let sandbox = daemon.create_validated(validated).await?;
        daemon.record(&sandbox.id).await
    })
    .await
    .map_err(|_| ApiError::internal("E2B creation task failed"))??;
    Ok((StatusCode::CREATED, Json(wire(&created, true)?)))
}
fn wire(record: &Record, credentials: bool) -> Result<Value> {
    let template = record.e2b.as_ref().ok_or_else(|| {
        Error(ApiError::new(
            404,
            "SANDBOX_NOT_FOUND",
            "E2B sandbox does not exist",
        ))
    })?;
    let sandbox = &record.sandbox;
    let state = match sandbox.status.state.as_str() {
        "Running" => "running",
        "Paused" => "paused",
        _ => {
            return Err(ApiError::new(
                409,
                "INVALID_SANDBOX_STATE",
                "E2B sandbox is not running or paused",
            )
            .into());
        }
    };
    let mut value = json!({
        "sandboxID": sandbox.id, "templateID": sandbox.image.uri, "clientID": "pvisor-local",
        "envdVersion": template.envd_version, "startedAt": sandbox.created_at, "endAt": sandbox.expires_at,
        "cpuCount": template.cpu_millis.div_ceil(1000), "memoryMB": template.memory_bytes / (1024 * 1024),
        // No fixed-size block disk or disk quota is allocated by the shared pool.
        "diskSizeMB": 0, "state": state, "metadata": sandbox.metadata,
        "lifecycle": {"onTimeout": "kill", "autoResume": false},
    });
    if credentials {
        value["envdAccessToken"] = json!(record.endpoint_token);
    }
    Ok(value)
}
async fn record(daemon: &Daemon, id: &str) -> Result<Record> {
    let record = daemon.record(id).await?;
    if record.e2b.is_none() {
        return Err(ApiError::new(404, "SANDBOX_NOT_FOUND", "E2B sandbox does not exist").into());
    }
    Ok(record)
}
async fn info(State(daemon): State<Arc<Daemon>>, Path(id): Path<String>) -> Result<Json<Value>> {
    record(&daemon, &id).await?;
    daemon.get(&id).await?;
    Ok(Json(wire(&record(&daemon, &id).await?, true)?))
}
async fn delete(State(daemon): State<Arc<Daemon>>, Path(id): Path<String>) -> Result<StatusCode> {
    record(&daemon, &id).await?;
    daemon.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    timeout: Option<u64>,
    memory: Option<bool>,
}
async fn connect(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<String>,
    request: std::result::Result<Json<Control>, JsonRejection>,
) -> Result<Json<Value>> {
    let request = body(request)?;
    if request.memory == Some(false) {
        return Err(unsupported_error());
    }
    let seconds = timeout(&daemon, request.timeout)?;
    let result = tokio::spawn(async move {
        let gate = daemon.operation(&id).await?;
        let _guard = gate.write().await;
        record(&daemon, &id).await?;
        // Same gate covers native resume and TTL replacement, so deletion/expiry cannot race it.
        daemon.control_locked(&id, false).await?;
        replace_timeout(&daemon, &id, seconds).await?;
        wire(&record(&daemon, &id).await?, true)
    })
    .await
    .map_err(|_| ApiError::internal("E2B connect task failed"))??;
    Ok(Json(result))
}
async fn pause(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<String>,
    request: std::result::Result<Json<Control>, JsonRejection>,
) -> Result<StatusCode> {
    let request = body(request)?;
    if request.memory == Some(false) || request.timeout.is_some() {
        return Err(unsupported_error());
    }
    tokio::spawn(async move {
        let gate = daemon.operation(&id).await?;
        let _guard = gate.write().await;
        if record(&daemon, &id).await?.sandbox.status.state == "Paused" {
            return Err(Error(ApiError::new(
                409,
                "ALREADY_PAUSED",
                "Sandbox is already paused",
            )));
        }
        daemon.control_locked(&id, true).await?;
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| ApiError::internal("E2B pause task failed"))??;
    Ok(StatusCode::NO_CONTENT)
}
async fn replace_timeout(
    daemon: &Daemon,
    id: &str,
    seconds: u64,
) -> std::result::Result<(), ApiError> {
    daemon
        .update(id, |registry| {
            let mut record =
                registry.sandboxes.get(id).cloned().ok_or_else(|| {
                    ApiError::new(404, "SANDBOX_NOT_FOUND", "Sandbox does not exist")
                })?;
            super::check_expiration(record.sandbox.expires_at)?;
            if !matches!(record.sandbox.status.state.as_str(), "Running" | "Paused") {
                return Err(ApiError::new(
                    409,
                    "INVALID_SANDBOX_STATE",
                    "Cannot set timeout in this state",
                ));
            }
            record.sandbox.expires_at = Some(Utc::now() + Duration::seconds(seconds as i64));
            Ok((Some(record), ()))
        })
        .await
}
async fn set_timeout(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<String>,
    request: std::result::Result<Json<Control>, JsonRejection>,
) -> Result<StatusCode> {
    let request = body(request)?;
    if request.memory.is_some() || request.timeout.is_none() {
        return Err(bad("timeout is required"));
    }
    let seconds = timeout(&daemon, request.timeout)?;
    tokio::spawn(async move {
        let gate = daemon.operation(&id).await?;
        let _guard = gate.write().await;
        record(&daemon, &id).await?;
        replace_timeout(&daemon, &id, seconds).await?;
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| ApiError::internal("E2B timeout task failed"))??;
    Ok(StatusCode::NO_CONTENT)
}
async fn list(State(daemon): State<Arc<Daemon>>, RawQuery(raw): RawQuery) -> Result<Response> {
    let mut values = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes()) {
        if values
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(bad("Duplicate list query field"));
        }
    }
    if values.keys().any(|k| {
        ![
            "limit",
            "nextToken",
            "state",
            "metadata",
            "order",
            "template",
            "startedAfter",
        ]
        .contains(&k.as_str())
    }) {
        return Err(bad("Unknown list query field"));
    }
    let limit: usize = values
        .get("limit")
        .map_or(Ok(100), |s| s.parse())
        .map_err(|_| bad("Invalid list limit"))?;
    if !(1..=100).contains(&limit) {
        return Err(bad("List limit must be 1..100"));
    }
    let offset: usize = values
        .get("nextToken")
        .map_or(Ok(0), |s| s.parse())
        .map_err(|_| bad("Invalid nextToken"))?;
    let states: BTreeSet<_> = values
        .get("state")
        .map(|s| s.split(',').collect())
        .unwrap_or_else(|| BTreeSet::from(["running", "paused"]));
    if states.iter().any(|s| !["running", "paused"].contains(s)) {
        return Err(bad("Invalid state"));
    }
    let descending = match values.get("order").map(String::as_str) {
        None | Some("desc") => true,
        Some("asc") => false,
        _ => return Err(bad("Invalid order")),
    };
    let after = values
        .get("startedAfter")
        .map(|v| chrono::DateTime::parse_from_rfc3339(v))
        .transpose()
        .map_err(|_| bad("Invalid startedAfter"))?;
    let metadata: BTreeMap<_, _> =
        url::form_urlencoded::parse(values.get("metadata").map_or("", String::as_str).as_bytes())
            .map(|(k, v)| (decode_component(&k), decode_component(&v)))
            .collect();
    daemon.ensure_storage()?;
    let registry = daemon.registry.lock().await;
    let mut records: Vec<_> = registry
        .sandboxes
        .values()
        .filter(|r| {
            r.e2b.is_some()
                && states.contains(if r.sandbox.status.state == "Running" {
                    "running"
                } else if r.sandbox.status.state == "Paused" {
                    "paused"
                } else {
                    ""
                })
                && metadata
                    .iter()
                    .all(|(k, v)| r.sandbox.metadata.get(k) == Some(v))
                && values
                    .get("template")
                    .is_none_or(|t| *t == r.sandbox.image.uri)
                && after.is_none_or(|date| r.sandbox.created_at > date)
        })
        .collect();
    records.sort_by_key(|r| (r.sandbox.created_at, &r.sandbox.id));
    if descending {
        records.reverse();
    }
    let page = records
        .iter()
        .skip(offset)
        .take(limit)
        .map(|r| wire(r, false))
        .collect::<Result<Vec<_>>>()?;
    let mut response = Json(page).into_response();
    if offset
        .checked_add(limit)
        .is_some_and(|next| next < records.len())
    {
        response.headers_mut().insert(
            "x-next-token",
            HeaderValue::from_str(&(offset + limit).to_string()).unwrap(),
        );
    }
    Ok(response)
}
async fn proxy(State(daemon): State<Arc<Daemon>>, request: Request) -> Result<Response> {
    let id = one(request.headers(), "e2b-sandbox-id")
        .ok_or_else(|| bad("Exactly one E2b-Sandbox-Id is required"))?
        .to_owned();
    if one(request.headers(), "e2b-sandbox-port") != Some("49983") {
        return Err(unsupported_error());
    }
    let record = match record(&daemon, &id).await {
        Ok(record) => record,
        Err(Error(error)) if error.status == 404 && request.uri().path() == "/e2b/health" => {
            return Err(ApiError::new(502, "SANDBOX_UNAVAILABLE", "Sandbox is unavailable").into());
        }
        Err(error) => return Err(error),
    };
    if !one(request.headers(), "x-access-token")
        .is_some_and(|value| super::secret_eq(value.as_bytes(), record.endpoint_token.as_bytes()))
    {
        return Err(ApiError::new(401, "UNAUTHORIZED", "Invalid sandbox access token").into());
    }
    let guest_path = request.uri().path().strip_prefix("/e2b").unwrap_or("/");
    let guest_path = if guest_path.is_empty() {
        "/"
    } else {
        guest_path
    };
    let guest_path = guest_path.to_owned();
    match super::api::forward(&daemon, &id, ENVD_PORT, request, guest_path).await {
        Ok(response) => Ok(response),
        Err(error) if error.status == 409 || error.code == "ENDPOINT_UNAVAILABLE" => {
            Err(ApiError::new(502, "SANDBOX_UNAVAILABLE", "Sandbox is unavailable").into())
        }
        Err(error) => Err(error.into()),
    }
}

// Decode once, as the envd router does; never let either public proxy reach
// control-plane mutation routes, even with a valid sandbox token.
pub(super) fn private_path(path: &str) -> bool {
    let decoded = decode_component(path);
    ["init", "upgrade", "freeze", "unfreeze", "thaw"].contains(
        &decoded
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or(""),
    )
}

fn decode_component(value: &str) -> String {
    let encoded = format!("v={}", value.replace('+', "%2B").replace('&', "%26"));
    url::form_urlencoded::parse(encoded.as_bytes())
        .next()
        .unwrap()
        .1
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{E2bTemplate, Runtime, RuntimeState};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::TempDir;
    use tower::ServiceExt;
    const KEY: &str = "e2b-test-key-at-least-thirty-two-bytes";
    #[derive(Default)]
    struct Fake {
        states: Mutex<BTreeMap<String, RuntimeState>>,
        specs: Mutex<Vec<RuntimeSpec>>,
        lookups: AtomicUsize,
        endpoint_calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Runtime for Fake {
        async fn preflight(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn e2b_template(&self, image: &str) -> anyhow::Result<E2bTemplate> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            anyhow::ensure!(image == "base", "unknown");
            Ok(E2bTemplate {
                cpu_millis: 1000,
                memory_bytes: 512 * 1024 * 1024,
                envd_version: "0.10.2".into(),
            })
        }
        async fn create(&self, spec: &RuntimeSpec) -> anyhow::Result<()> {
            self.specs.lock().unwrap().push(spec.clone());
            self.states
                .lock()
                .unwrap()
                .insert(spec.id.clone(), RuntimeState::Running);
            Ok(())
        }
        async fn inspect(&self, id: &str) -> anyhow::Result<RuntimeState> {
            Ok(self
                .states
                .lock()
                .unwrap()
                .get(id)
                .copied()
                .unwrap_or(RuntimeState::Missing))
        }
        async fn pause(&self, id: &str) -> anyhow::Result<RuntimeState> {
            self.states
                .lock()
                .unwrap()
                .insert(id.into(), RuntimeState::Paused);
            Ok(RuntimeState::Paused)
        }
        async fn resume(&self, id: &str) -> anyhow::Result<RuntimeState> {
            self.states
                .lock()
                .unwrap()
                .insert(id.into(), RuntimeState::Running);
            Ok(RuntimeState::Running)
        }
        async fn delete(&self, id: &str) -> anyhow::Result<()> {
            self.states.lock().unwrap().remove(id);
            Ok(())
        }
        async fn endpoint(&self, _: &str, _: u16) -> anyhow::Result<String> {
            self.endpoint_calls.fetch_add(1, Ordering::SeqCst);
            Ok("http://127.0.0.1:1".into())
        }
    }
    async fn open(dir: &TempDir, runtime: &Arc<Fake>) -> Arc<Daemon> {
        Daemon::open(
            super::super::Config {
                state_dir: dir.path().into(),
                api_key: KEY.into(),
                public_endpoint: "127.0.0.1:8080".into(),
                max_sandboxes: 4,
                cpu_millis: 4000,
                memory_bytes: 4 * 512 * 1024 * 1024,
                max_timeout_seconds: 86400,
            },
            |_| Ok(runtime.clone()),
        )
        .await
        .unwrap()
    }
    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        value: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut req = axum::http::Request::builder().method(method).uri(path);
        for (key, value) in headers {
            req = req.header(*key, *value);
        }
        let body = match value {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let response = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let (parts, body) = response.into_parts();
        assert!(parts.headers.contains_key("x-request-id"));
        let bytes = to_bytes(body, 2 * 1024 * 1024).await.unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (parts.status, parts.headers, json)
    }
    fn quote(value: &str) -> String {
        value.bytes().map(|b| format!("%{b:02X}")).collect()
    }
    async fn create_one(app: &Router) -> Value {
        let (status, _, value) = call(app, "POST", "/v2/sandboxes", &[("x-api-key", KEY)], Some(json!({"templateID":"base", "timeout":600,"metadata":{"a&=雪":"literal %20+✓"}, "envVars":{"PVISOR_ENVD_ACCESS_TOKEN":"untrusted","TEST":"value"}}))).await;
        assert_eq!(status, StatusCode::CREATED, "{value}");
        value
    }
    #[tokio::test]
    async fn denied_data_plane_posts_close_http1_before_connection_reuse() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(Fake::default());
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon);
        let sandbox = create_one(&app).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for _ in 0..20 {
            let denied = client
                .post(format!("{url}/e2b/init"))
                .header("e2b-sandbox-id", sandbox["sandboxID"].as_str().unwrap())
                .header("e2b-sandbox-port", "49983")
                .header(
                    "x-access-token",
                    sandbox["envdAccessToken"].as_str().unwrap(),
                )
                .json(&json!({"input":"x".repeat(24000)}))
                .send()
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::FORBIDDEN);
            assert_eq!(denied.headers().get("connection").unwrap(), "close");
            assert!(denied.json::<Value>().await.unwrap()["message"].is_string());
            assert_eq!(
                client
                    .get(format!("{url}/sandboxes"))
                    .header("x-api-key", KEY)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        assert_eq!(runtime.endpoint_calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn official_sdk_wire_lifecycle_pagination_and_restart() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(Fake::default());
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon.clone());
        let first = create_one(&app).await;
        let second = create_one(&app).await;
        let id = first["sandboxID"].as_str().unwrap();
        let token = first["envdAccessToken"].as_str().unwrap();
        assert_ne!(token, "untrusted");
        assert_ne!(token, KEY);
        assert_ne!(first["envdAccessToken"], second["envdAccessToken"]);
        let spec = runtime.specs.lock().unwrap()[0].clone();
        assert_eq!(spec.env[crate::runtime::ENVD_TOKEN_ENV], token);
        assert_eq!(spec.cpu_millis, 1000);
        assert_eq!(spec.memory_bytes, 512 * 1024 * 1024);
        let auth = [("x-api-key", KEY)];
        let (status, _, info) = call(&app, "GET", &format!("/sandboxes/{id}"), &auth, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(info["state"], "running");
        assert_eq!(info["memoryMB"], 512);
        let (status, headers, page) =
            call(&app, "GET", "/v2/sandboxes?limit=1&order=asc", &auth, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page[0]["sandboxID"], first["sandboxID"]);
        assert!(page[0].get("envdAccessToken").is_none());
        assert_eq!(headers["x-next-token"], "1");
        let (_, headers, page) = call(
            &app,
            "GET",
            "/v2/sandboxes?limit=1&order=asc&nextToken=1",
            &auth,
            None,
        )
        .await;
        assert_eq!(page[0]["sandboxID"], second["sandboxID"]);
        assert!(!headers.contains_key("x-next-token"));
        // The Python SDK percent-quotes each component, urlencodes the inner map,
        // then encodes that map again as the outer metadata query field.
        let inner = url::form_urlencoded::Serializer::new(String::new())
            .append_pair(&quote("a&=雪"), &quote("literal %20+✓"))
            .finish();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("metadata", &inner)
            .finish();
        let (_, _, page) = call(&app, "GET", &format!("/v2/sandboxes?{query}"), &auth, None).await;
        assert_eq!(page.as_array().unwrap().len(), 2);
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/sandboxes/{id}/pause"),
                &auth,
                Some(json!({}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        let (_, _, page) = call(&app, "GET", "/v2/sandboxes?state=paused", &auth, None).await;
        assert_eq!(page.as_array().unwrap().len(), 1);
        drop(app);
        drop(daemon);
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon.clone());
        let (status, _, connected) = call(
            &app,
            "POST",
            &format!("/v2/sandboxes/{id}/connect"),
            &auth,
            Some(json!({"timeout":120})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(connected["envdAccessToken"], token);
        assert_eq!(connected["state"], "running");
        let before = daemon.record(id).await.unwrap().sandbox.expires_at.unwrap();
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/sandboxes/{id}/timeout"),
                &auth,
                Some(json!({"timeout":1}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert!(daemon.record(id).await.unwrap().sandbox.expires_at.unwrap() < before);
        for id in [id, second["sandboxID"].as_str().unwrap()] {
            assert_eq!(
                call(&app, "DELETE", &format!("/sandboxes/{id}"), &auth, None)
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
        }
        assert!(runtime.states.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn rejects_unsupported_and_invalid_options_before_admission() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(Fake::default());
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon);
        for (field, value, status) in [
            ("autoPause", json!(true), 501),
            ("autoResume", json!({}), 501),
            ("autoPauseMemory", json!(false), 501),
            ("secure", json!(false), 501),
            ("allow_internet_access", json!(false), 501),
            ("network", json!({}), 501),
            ("iam", json!({}), 501),
            ("mcp", json!({}), 501),
            ("volumeMounts", json!([]), 501),
            ("timeout", json!(0), 400),
            ("timeout", json!(86401), 400),
            ("templateID", json!("../host"), 400),
            ("unknown", json!(true), 400),
            ("envVars", json!({"AGENTCTL_BAD":"x"}), 400),
        ] {
            let mut payload = json!({"templateID":"base"});
            payload[field] = value;
            let (actual, _, error) = call(
                &app,
                "POST",
                "/v2/sandboxes",
                &[("x-api-key", KEY)],
                Some(payload),
            )
            .await;
            assert_eq!(actual.as_u16(), status, "{field}: {error}");
            assert_eq!(error["code"], status);
        }
        assert!(runtime.specs.lock().unwrap().is_empty());
        assert_eq!(runtime.lookups.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn credentials_are_scoped_and_private_envd_routes_are_blocked_in_both_proxies() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(Fake::default());
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon);
        for auth in [
            vec![],
            vec![("open-sandbox-api-key", KEY)],
            vec![("x-api-key", KEY), ("x-api-key", KEY)],
        ] {
            let (status, _, error) = call(&app, "GET", "/sandboxes", &auth, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(error["code"], 401);
        }
        let a = create_one(&app).await;
        let b = create_one(&app).await;
        let id = a["sandboxID"].as_str().unwrap();
        let token = a["envdAccessToken"].as_str().unwrap();
        for auth in [
            vec![
                ("e2b-sandbox-id", id),
                ("e2b-sandbox-port", "49983"),
                ("x-access-token", "wrong"),
            ],
            vec![
                ("e2b-sandbox-id", b["sandboxID"].as_str().unwrap()),
                ("e2b-sandbox-port", "49983"),
                ("x-access-token", token),
            ],
            vec![
                ("e2b-sandbox-id", id),
                ("e2b-sandbox-port", "49983"),
                ("x-access-token", token),
                ("x-access-token", token),
            ],
        ] {
            assert_eq!(
                call(&app, "GET", "/e2b/envs", &auth, None).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        let auth = [
            ("e2b-sandbox-id", id),
            ("e2b-sandbox-port", "49983"),
            ("x-access-token", token),
        ];
        for path in [
            "/init",
            "/%69nit",
            "/%2Finit",
            "/upgrade",
            "/freeze",
            "/unfreeze",
            "/thaw",
        ] {
            assert_eq!(
                call(&app, "POST", &format!("/e2b{path}"), &auth, Some(json!({})))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
            assert_eq!(
                call(
                    &app,
                    "POST",
                    &format!("/v1/sandboxes/{id}/proxy/49983{path}"),
                    &[("x-pvisor-sandbox-token", token)],
                    Some(json!({}))
                )
                .await
                .0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
        assert_eq!(runtime.endpoint_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            call(&app, "GET", "/sandboxes", &[("x-api-key", token)], None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn expired_e2b_identity_cannot_be_resurrected_by_connect_or_timeout() {
        let dir = TempDir::new().unwrap();
        let runtime = Arc::new(Fake::default());
        let daemon = open(&dir, &runtime).await;
        let app = super::super::api::router(daemon.clone());
        let a = create_one(&app).await;
        let id = a["sandboxID"].as_str().unwrap();
        daemon
            .update(id, |registry| {
                let mut record = registry.sandboxes[id].clone();
                record.sandbox.expires_at = Some(Utc::now() - Duration::seconds(1));
                Ok((Some(record), ()))
            })
            .await
            .unwrap();
        for path in [
            format!("/v2/sandboxes/{id}/connect"),
            format!("/sandboxes/{id}/timeout"),
        ] {
            assert_eq!(
                call(
                    &app,
                    "POST",
                    &path,
                    &[("x-api-key", KEY)],
                    Some(json!({"timeout":600}))
                )
                .await
                .0,
                StatusCode::CONFLICT
            );
        }
        daemon.delete(id).await.unwrap();
    }
}
