//! OpenSandbox lifecycle adapter, checked against OpenSandbox 1.1.0 at
//! b1a29cf93a823a95913f7943010febb3f29de05c (sandbox-lifecycle.yml and JS SDK).
//!
//! Integration security requirements:
//! - `proxy_client` MUST use redirect::Policy::none(), no_proxy(), a bounded
//!   connect_timeout, no total/read timeout, no default credentials, and no
//!   automatic response decompression. These policies cannot be inspected here.
//! - `upstream` MUST resolve only this sandbox's guest ports 44772/18080 to
//!   trusted loopback listeners; the host-side mapped port can be different.
//! - `proxy_guard` MUST acquire the same lifecycle mutex as deletion. The API
//!   holds it through port resolution and send (bounded to 120 seconds), then
//!   releases it before forwarding the response body, including long-lived SSE.
//! - `endpoint` MUST return the daemon proxy URL in both modes. In non-server
//!   mode its headers MUST include X-PVISOR-SANDBOX-TOKEN; the SDK does not send
//!   the lifecycle key in that mode. Server mode uses the lifecycle key.
//! - Parent authorization MUST fail closed, including when no key is configured,
//!   reject ambiguous duplicate credentials, and scope endpoint tokens to id.
//! - Parent request types validate unsupported creation features (snapshot/fsb,
//!   etc.) rather than silently ignoring them, and use the spec's serde names.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{
        DefaultBodyLimit, MatchedPath, Path, RawQuery, Request, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde_json::{Value, json};
use url::{Host, Url};

use super::{ApiError, CreateRequest, Daemon, RenewRequest, Sandbox};

const CONTROL_BODY_LIMIT: usize = 1024 * 1024;
const PROXY_HEADERS_TIMEOUT: Duration = Duration::from_secs(120);
const PROXY_ROUTE: &str = "/v1/sandboxes/{id}/proxy/{port}";
const PROXY_TRAILING_ROUTE: &str = "/v1/sandboxes/{id}/proxy/{port}/";
const PROXY_PATH_ROUTE: &str = "/v1/sandboxes/{id}/proxy/{port}/{*path}";

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/v1/sandboxes", get(list).post(create))
        .route("/v1/sandboxes/{id}", get(get_sandbox).delete(delete))
        .route("/v1/sandboxes/{id}/pause", post(pause))
        .route("/v1/sandboxes/{id}/resume", post(resume))
        .route("/v1/sandboxes/{id}/renew-expiration", post(renew))
        .route("/v1/sandboxes/{id}/endpoints/{port}", get(endpoint))
        .route(PROXY_ROUTE, any(proxy))
        .route(PROXY_TRAILING_ROUTE, any(proxy))
        .route(PROXY_PATH_ROUTE, any(proxy))
        .route("/v1/snapshots", get(unsupported))
        .route("/v1/snapshots/{id}", get(unsupported).delete(unsupported))
        .route("/v1/templates", get(unsupported).post(unsupported))
        .route("/v1/templates/{id}", get(unsupported).delete(unsupported))
        .route("/v1/sandboxes/{id}/snapshots", post(unsupported))
        .route(
            "/v1/sandboxes/{id}/metadata",
            axum::routing::patch(unsupported),
        )
        .route(
            "/v1/sandboxes/{id}/networkpolicy",
            get(unsupported)
                .put(unsupported)
                .patch(unsupported)
                .delete(unsupported),
        )
        .route("/v1/metrics/events", post(unsupported))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(
            daemon.clone(),
            request_policy,
        ))
        .with_state(daemon)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status)
            .ok()
            .filter(|status| status.is_client_error() || status.is_server_error())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({"code": self.code, "message": self.message})),
        )
            .into_response()
    }
}

async fn request_policy(
    State(daemon): State<Arc<Daemon>>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
        .expect("UUID is a valid header value");
    request
        .headers_mut()
        .insert("x-request-id", request_id.clone());
    let is_proxy = request
        .extensions()
        .get::<MatchedPath>()
        .is_some_and(|path| {
            matches!(
                path.as_str(),
                PROXY_ROUTE | PROXY_TRAILING_ROUTE | PROXY_PATH_ROUTE
            )
        });
    let mut response = if is_proxy {
        next.run(request).await
    } else if !control_authorized(&daemon, request.headers()) {
        unauthorized().into_response()
    } else {
        // Buffer only bounded control bodies. Proxy requests never enter this branch.
        let (parts, body) = request.into_parts();
        match to_bytes(body, CONTROL_BODY_LIMIT).await {
            Ok(bytes) => {
                next.run(Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
            Err(_) => {
                ApiError::bad_request("Control body is unreadable or exceeds 1 MiB").into_response()
            }
        }
    };
    response.headers_mut().insert("x-request-id", request_id);
    response
}

fn single_credential(headers: &HeaderMap, name: &'static str) -> bool {
    let mut values = headers.get_all(name).iter();
    values
        .next()
        .is_some_and(|value| value.to_str().is_ok_and(|value| !value.trim().is_empty()))
        && values.next().is_none()
}

fn control_authorized(daemon: &Daemon, headers: &HeaderMap) -> bool {
    single_credential(headers, "open-sandbox-api-key") && daemon.authorize(headers)
}

fn unauthorized() -> ApiError {
    ApiError::new(
        401,
        "UNAUTHORIZED",
        "Missing or invalid authentication credentials",
    )
}

async fn not_found() -> ApiError {
    ApiError::new(404, "NOT_FOUND", "Unknown API route")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        405,
        "METHOD_NOT_ALLOWED",
        "Method is not supported on this route",
    )
}

async fn unsupported() -> ApiError {
    ApiError::new(
        501,
        "NOT_IMPLEMENTED",
        "This OpenSandbox feature is not supported by pVisor",
    )
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(value)| value)
        .map_err(|error| ApiError::bad_request(error.body_text()))
}

fn path_value<T>(path: Result<Path<T>, PathRejection>) -> Result<T, ApiError> {
    path.map(|Path(value)| value)
        .map_err(|error| ApiError::bad_request(error.body_text()))
}

fn internal(message: &'static str) -> ApiError {
    ApiError::new(500, "INTERNAL_ERROR", message)
}

async fn create(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let sandbox = daemon.create(json_body(body)?).await?;
    let location = format!("/v1/sandboxes/{}", encode_segment(&sandbox.id));
    let mut value =
        serde_json::to_value(sandbox).map_err(|_| internal("Cannot serialize sandbox"))?;
    if let Some(object) = value.as_object_mut() {
        object.remove("image");
        object.remove("snapshotId");
        object.remove("updatedAt");
    }
    let mut response = (StatusCode::ACCEPTED, Json(value)).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| internal("Invalid sandbox Location"))?,
    );
    Ok(response)
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

async fn get_sandbox(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<Sandbox>, ApiError> {
    Ok(Json(daemon.get(&path_value(path)?).await?))
}

async fn delete(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    daemon.delete(&path_value(path)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pause(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    daemon.pause(&path_value(path)?).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn resume(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    daemon.resume(&path_value(path)?).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn renew(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<RenewRequest>, JsonRejection>,
) -> Result<Json<RenewRequest>, ApiError> {
    Ok(Json(
        daemon.renew(&path_value(path)?, json_body(body)?).await?,
    ))
}

#[derive(Debug, PartialEq)]
struct ListQuery {
    states: Vec<String>,
    metadata: Vec<(String, String)>,
    page: usize,
    page_size: usize,
    offset: usize,
}

fn positive_integer(value: &str, name: &str) -> Result<usize, ApiError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiError::bad_request(format!(
            "{name} must be a positive integer"
        )));
    }
    value
        .parse::<usize>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| ApiError::bad_request(format!("{name} is outside the supported range")))
}

fn parse_list_query(raw: Option<&str>) -> Result<ListQuery, ApiError> {
    let mut query = ListQuery {
        states: Vec::new(),
        metadata: Vec::new(),
        page: 1,
        page_size: 20,
        offset: 0,
    };
    let mut seen_page = false;
    let mut seen_size = false;
    let mut seen_metadata = false;
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "state" => query.states.push(value.into_owned()),
            "metadata" if !seen_metadata => {
                seen_metadata = true;
                // Outer query decoding is already done; parse exactly one inner layer.
                query.metadata = url::form_urlencoded::parse(value.as_bytes())
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
            }
            "page" if !seen_page => {
                seen_page = true;
                query.page = positive_integer(&value, "page")?;
            }
            "pageSize" if !seen_size => {
                seen_size = true;
                query.page_size = positive_integer(&value, "pageSize")?;
            }
            "metadata" | "page" | "pageSize" => {
                return Err(ApiError::bad_request(format!(
                    "Duplicate query parameter: {key}"
                )));
            }
            _ => {}
        }
    }
    query.offset = (query.page - 1)
        .checked_mul(query.page_size)
        .ok_or_else(|| ApiError::bad_request("Pagination offset exceeds the supported range"))?;
    Ok(query)
}

async fn list(
    State(daemon): State<Arc<Daemon>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let query = parse_list_query(raw.as_deref())?;
    let mut items: Vec<_> = daemon
        .list()
        .await?
        .into_iter()
        .filter(|sandbox| {
            (query.states.is_empty() || query.states.contains(&sandbox.status.state))
                && query
                    .metadata
                    .iter()
                    .all(|(key, value)| sandbox.metadata.get(key) == Some(value))
        })
        .collect();
    // Stable pages even if the parent's storage iteration order changes.
    items.sort_by(|left, right| left.id.cmp(&right.id));
    let total = items.len();
    let total_pages = total / query.page_size + usize::from(total % query.page_size != 0);
    let items: Vec<_> = items
        .into_iter()
        .skip(query.offset)
        .take(query.page_size)
        .collect();
    Ok(Json(json!({
        "items": items,
        "pagination": { "page": query.page, "pageSize": query.page_size,
            "totalItems": total, "totalPages": total_pages,
            "hasNextPage": query.page < total_pages }
    })))
}

fn parse_endpoint_query(raw: Option<&str>) -> Result<bool, ApiError> {
    let mut server_proxy = None;
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "expires" => return Err(ApiError::bad_request("Signed endpoints are not supported")),
            "use_server_proxy" => {
                if server_proxy.is_some() {
                    return Err(ApiError::bad_request("Duplicate use_server_proxy"));
                }
                server_proxy = Some(match value.as_ref() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(ApiError::bad_request(
                            "use_server_proxy must be true or false",
                        ));
                    }
                });
            }
            _ => {}
        }
    }
    Ok(server_proxy.unwrap_or(false))
}

fn parse_port(value: &str) -> Result<u16, ApiError> {
    let port = positive_integer(value, "port")?;
    u16::try_from(port).map_err(|_| ApiError::bad_request("port must be between 1 and 65535"))
}

async fn endpoint(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<(String, String)>, PathRejection>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let (id, port) = path_value(path)?;
    let server_proxy = parse_endpoint_query(raw.as_deref())?;
    let endpoint = daemon
        .endpoint(&id, parse_port(&port)?, server_proxy)
        .await?;
    // Token issuance and the public proxy authority belong to the parent.
    let object = endpoint
        .as_object()
        .ok_or_else(|| internal("Invalid endpoint schema"))?;
    if !object
        .get("endpoint")
        .is_some_and(|value| value.as_str().is_some_and(|s| !s.is_empty()))
        || object
            .keys()
            .any(|key| key != "endpoint" && key != "headers")
        || object.get("headers").is_some_and(|value| {
            !value
                .as_object()
                .is_some_and(|headers| headers.values().all(Value::is_string))
        })
    {
        return Err(internal("Invalid endpoint schema"));
    }
    if !server_proxy
        && !object
            .get("headers")
            .and_then(Value::as_object)
            .is_some_and(|headers| {
                headers.iter().any(|(key, value)| {
                    key.eq_ignore_ascii_case("x-pvisor-sandbox-token")
                        && value.as_str().is_some_and(|token| !token.is_empty())
                })
            })
    {
        return Err(internal(
            "Non-server endpoint is missing sandbox authentication",
        ));
    }
    Ok(Json(endpoint))
}

// Connection can be repeated, and each value can nominate several additional
// hop-by-hop headers. Collect nominations before removing Connection itself.
fn strip_headers(headers: &mut HeaderMap, request: bool) {
    let nominated: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .filter_map(|name| {
            let name = std::str::from_utf8(name).ok()?.trim();
            HeaderName::from_bytes(name.as_bytes()).ok()
        })
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "open-sandbox-api-key",
        "x-pvisor-sandbox-token",
        "authorization",
    ] {
        headers.remove(name);
    }
    if request {
        // Only the explicitly supplied guest X-EXECD-ACCESS-TOKEN is allowed
        // through as an authentication header. Never synthesize guest tokens.
        for name in [
            "host",
            "cookie",
            "forwarded",
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "x-real-ip",
            "sec-websocket-key",
            "sec-websocket-version",
            "sec-websocket-protocol",
            "sec-websocket-extensions",
        ] {
            headers.remove(name);
        }
    }
}

fn proxy_url(base: &str, uri: &Uri) -> Result<Url, ApiError> {
    let mut url = Url::parse(base).map_err(|_| internal("Invalid proxy upstream"))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !loopback
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || base.contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port_or_known_default() == Some(0)
    {
        return Err(internal(
            "Proxy upstream must be a credential-free loopback origin",
        ));
    }
    // Use the original escaped URI, not the decoded wildcard extractor. Never
    // join a user path as a URL reference (//host could replace the authority).
    let suffix = uri.path().splitn(7, '/').nth(6).unwrap_or_default();
    let path = format!("/{suffix}");
    url.set_path(&path);
    url.set_query(uri.query());
    // URL parsing normalizes dot segments and backslashes. Reject these rather
    // than silently changing the requested guest resource or an encoded suffix.
    if url.path() != path || url.query() != uri.query() {
        return Err(ApiError::bad_request(
            "Proxy path/query cannot be forwarded without normalization",
        ));
    }
    Ok(url)
}

async fn proxy(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<BTreeMap<String, String>>, PathRejection>,
    request: Request,
) -> Result<Response, ApiError> {
    let path = path_value(path)?;
    let id = path
        .get("id")
        .ok_or_else(|| ApiError::bad_request("Missing sandbox id"))?;
    let lifecycle_auth = control_authorized(&daemon, request.headers());
    let sandbox_auth = !lifecycle_auth
        && single_credential(request.headers(), "x-pvisor-sandbox-token")
        && daemon.authorize_proxy(id, request.headers()).await;
    if !lifecycle_auth && !sandbox_auth {
        return Err(unauthorized());
    }
    if request.method() == Method::CONNECT
        || request.headers().contains_key(header::UPGRADE)
        || request.headers().contains_key("sec-websocket-key")
    {
        return Err(ApiError::new(
            501,
            "NOT_IMPLEMENTED",
            "WebSocket upgrades and CONNECT are not supported",
        ));
    }
    let port = parse_port(
        path.get("port")
            .ok_or_else(|| ApiError::bad_request("Missing port"))?,
    )?;
    // Fence port resolution and connection establishment against delete/rebind.
    // The response stream must not retain this lifecycle lock.
    let guard = daemon.proxy_guard(id).await?;
    let upstream = daemon.upstream(id, port).await?;
    let url = proxy_url(&upstream, request.uri())?;
    let (mut parts, body) = request.into_parts();
    strip_headers(&mut parts.headers, true);
    let send = daemon
        .proxy_client()
        .request(parts.method, url)
        .headers(parts.headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send();
    // Includes request upload and waiting for response headers, but not SSE/body
    // consumption. Cancellation or timeout also releases the lifecycle lock.
    let response = tokio::time::timeout(PROXY_HEADERS_TIMEOUT, send).await;
    drop(guard);
    let response = response
        .map_err(|_| {
            ApiError::new(
                504,
                "UPSTREAM_TIMEOUT",
                "Sandbox upload or response headers timed out after 120 seconds",
            )
        })?
        .map_err(|error| {
            // Do not expose internal URLs or credentials embedded in client errors.
            if error.is_timeout() {
                ApiError::new(
                    504,
                    "UPSTREAM_TIMEOUT",
                    "Timed out connecting to sandbox service",
                )
            } else {
                ApiError::new(502, "BAD_GATEWAY", "Sandbox service request failed")
            }
        })?;
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        return Err(ApiError::new(
            502,
            "BAD_GATEWAY",
            "Sandbox unexpectedly attempted a protocol upgrade",
        ));
    }
    // No status rewriting: guest 404/405 bodies, SSE, and multipart remain streams.
    let status = response.status();
    let mut headers = response.headers().clone();
    strip_headers(&mut headers, false);
    let mut result = Response::new(Body::from_stream(response.bytes_stream()));
    *result.status_mut() = status;
    *result.headers_mut() = headers;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::Config;
    use super::*;
    use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tempfile::TempDir;
    use tower::ServiceExt;

    const TEST_API_KEY: &str = "route-test-key-at-least-thirty-two-bytes-long";
    const AUTH: &[(&str, &str)] = &[("open-sandbox-api-key", TEST_API_KEY)];

    #[derive(Default)]
    struct RouteRuntime {
        sandboxes: StdMutex<BTreeMap<String, RuntimeState>>,
        creates: AtomicUsize,
        endpoints: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Runtime for RouteRuntime {
        async fn create(&self, spec: &RuntimeSpec) -> anyhow::Result<()> {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            anyhow::ensure!(!sandboxes.contains_key(&spec.id), "duplicate sandbox");
            sandboxes.insert(spec.id.clone(), RuntimeState::Running);
            self.creates.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn inspect(&self, id: &str) -> anyhow::Result<RuntimeState> {
            Ok(self
                .sandboxes
                .lock()
                .unwrap()
                .get(id)
                .copied()
                .unwrap_or(RuntimeState::Missing))
        }

        async fn pause(&self, id: &str) -> anyhow::Result<()> {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
            anyhow::ensure!(*state == RuntimeState::Running, "sandbox not running");
            *state = RuntimeState::Paused;
            Ok(())
        }

        async fn resume(&self, id: &str) -> anyhow::Result<()> {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
            anyhow::ensure!(*state == RuntimeState::Paused, "sandbox not paused");
            *state = RuntimeState::Running;
            Ok(())
        }

        async fn delete(&self, id: &str) -> anyhow::Result<()> {
            self.sandboxes.lock().unwrap().remove(id);
            Ok(())
        }

        async fn endpoint(&self, id: &str, port: u16) -> anyhow::Result<String> {
            anyhow::ensure!(
                self.sandboxes.lock().unwrap().get(id) == Some(&RuntimeState::Running),
                "sandbox not running"
            );
            self.endpoints.fetch_add(1, Ordering::SeqCst);
            // Route tests never connect here: successful proxy auth is tested
            // with an explicitly unsupported upgrade, rejected before resolution.
            Ok(format!("http://127.0.0.1:{port}"))
        }
    }

    struct RouteFixture {
        app: Router,
        runtime: Arc<RouteRuntime>,
        // Keep private durable state alive until the router/daemon is dropped.
        _directory: TempDir,
    }

    impl RouteFixture {
        async fn open() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let runtime = Arc::new(RouteRuntime::default());
            let factory_runtime = runtime.clone();
            let daemon = Daemon::open(
                Config {
                    state_dir: directory.path().to_path_buf(),
                    api_key: TEST_API_KEY.to_owned(),
                    public_endpoint: "localhost:8080".to_owned(),
                    max_sandboxes: 8,
                    cpu_millis: 8000,
                    memory_bytes: 512 * 1024 * 1024,
                    max_timeout_seconds: 86400,
                },
                move |_owner| Ok(factory_runtime as Arc<dyn Runtime>),
            )
            .await
            .expect("open HTTP route fixture");
            Self {
                app: router(daemon),
                runtime,
                _directory: directory,
            }
        }
    }

    fn create_payload(metadata: Value) -> Value {
        json!({
            "image": {"uri": "registry.example.test/opensandbox/execd:fixture"},
            "entrypoint": ["tail", "-f", "/dev/null"],
            "resourceLimits": {"cpu": "1", "memory": "64Mi"},
            "timeout": 600,
            "metadata": metadata,
        })
    }

    async fn exchange(
        app: &Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Body,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let request_id = parts
            .headers
            .get("x-request-id")
            .expect("all routes have a request ID")
            .to_str()
            .unwrap();
        uuid::Uuid::parse_str(request_id).expect("request ID is a UUID");
        let bytes = to_bytes(body, CONTROL_BODY_LIMIT).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("HTTP response is JSON")
        };
        (parts.status, parts.headers, value)
    }

    async fn create_over_http(app: &Router, metadata: Value) -> Value {
        let (status, _, sandbox) = exchange(
            app,
            Method::POST,
            "/v1/sandboxes",
            AUTH,
            Body::from(create_payload(metadata).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sandbox}");
        sandbox
    }

    fn assert_error(value: &Value, code: &str) {
        assert_eq!(value["code"], code);
        assert!(
            value["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
        assert_eq!(value.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn http_auth_fallbacks_and_request_ids() {
        let fixture = RouteFixture::open().await;
        let mut ids = std::collections::BTreeSet::new();
        for credentials in [
            vec![],
            vec![("open-sandbox-api-key", "wrong")],
            vec![("open-sandbox-api-key", "")],
            vec![
                ("open-sandbox-api-key", TEST_API_KEY),
                ("open-sandbox-api-key", TEST_API_KEY),
            ],
            vec![("x-pvisor-sandbox-token", "not-a-lifecycle-key")],
        ] {
            let (status, headers, error) = exchange(
                &fixture.app,
                Method::POST,
                "/v1/sandboxes",
                &credentials,
                Body::from("malformed JSON"),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_error(&error, "UNAUTHORIZED");
            assert!(ids.insert(headers["x-request-id"].to_str().unwrap().to_owned()));
        }
        for (method, uri, expected, code) in [
            (
                Method::GET,
                "/v1/unknown",
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
            ),
            (
                Method::PUT,
                "/v1/sandboxes",
                StatusCode::METHOD_NOT_ALLOWED,
                "METHOD_NOT_ALLOWED",
            ),
        ] {
            let (status, headers, error) =
                exchange(&fixture.app, method, uri, AUTH, Body::empty()).await;
            assert_eq!(status, expected);
            assert_error(&error, code);
            assert!(ids.insert(headers["x-request-id"].to_str().unwrap().to_owned()));
        }
        assert_eq!(fixture.runtime.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn http_create_get_and_lifecycle_statuses() {
        let fixture = RouteFixture::open().await;
        let payload = create_payload(json!({"project": "route-test"}));
        let (status, headers, sandbox) = exchange(
            &fixture.app,
            Method::POST,
            "/v1/sandboxes",
            AUTH,
            Body::from(payload.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = sandbox["id"].as_str().unwrap();
        let resource = format!("/v1/sandboxes/{id}");
        assert_eq!(headers[header::LOCATION].to_str().unwrap(), resource);
        assert_eq!(sandbox["status"]["state"], "Running");
        assert_eq!(sandbox["metadata"], payload["metadata"]);
        assert_eq!(sandbox["entrypoint"], payload["entrypoint"]);
        assert!(sandbox["createdAt"].is_string());
        assert!(sandbox.as_object().unwrap().keys().all(|key| matches!(
            key.as_str(),
            "id" | "status"
                | "metadata"
                | "extensions"
                | "platform"
                | "expiresAt"
                | "createdAt"
                | "entrypoint"
        )));
        for excluded in ["image", "snapshotId", "updatedAt"] {
            assert!(
                sandbox.get(excluded).is_none(),
                "{excluded} is not a create response field"
            );
        }
        let (status, _, full) =
            exchange(&fixture.app, Method::GET, &resource, AUTH, Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(full["image"], payload["image"]);
        for (action, state) in [("pause", "Paused"), ("resume", "Running")] {
            let (status, _, empty) = exchange(
                &fixture.app,
                Method::POST,
                &format!("{resource}/{action}"),
                AUTH,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert!(empty.is_null());
            let (status, _, full) =
                exchange(&fixture.app, Method::GET, &resource, AUTH, Body::empty()).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(full["status"]["state"], state);
        }
        let renewal = json!({"expiresAt": (chrono::Utc::now()
            + chrono::Duration::seconds(1200)).to_rfc3339()});
        let (status, _, renewed) = exchange(
            &fixture.app,
            Method::POST,
            &format!("{resource}/renew-expiration"),
            AUTH,
            Body::from(renewal.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renewed.as_object().unwrap().len(), 1);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(renewed["expiresAt"].as_str().unwrap()).unwrap(),
            chrono::DateTime::parse_from_rfc3339(renewal["expiresAt"].as_str().unwrap()).unwrap()
        );
        let (status, _, empty) =
            exchange(&fixture.app, Method::DELETE, &resource, AUTH, Body::empty()).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(empty.is_null());
        let (status, _, error) =
            exchange(&fixture.app, Method::GET, &resource, AUTH, Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(error["code"].is_string());
        assert!(error["message"].is_string());
    }

    #[tokio::test]
    async fn http_list_repeated_states_metadata_and_pagination() {
        let fixture = RouteFixture::open().await;
        let metadata = json!({"project": "a&=+%20雪"});
        let running = create_over_http(&fixture.app, metadata.clone()).await;
        let paused = create_over_http(&fixture.app, metadata).await;
        create_over_http(&fixture.app, json!({"project": "other"})).await;
        let (status, _, _) = exchange(
            &fixture.app,
            Method::POST,
            &format!("/v1/sandboxes/{}/pause", paused["id"].as_str().unwrap()),
            AUTH,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let inner = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("project", "a&=+%20雪")
            .finish();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("state", "Running")
            .append_pair("state", "Paused")
            .append_pair("metadata", &inner)
            .append_pair("pageSize", "1")
            .finish();
        let mut found = std::collections::BTreeSet::new();
        for page in 1..=2 {
            let (status, _, list) = exchange(
                &fixture.app,
                Method::GET,
                &format!("/v1/sandboxes?{query}&page={page}"),
                AUTH,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list["items"].as_array().unwrap().len(), 1);
            found.insert(list["items"][0]["id"].as_str().unwrap().to_owned());
            assert_eq!(
                list["pagination"],
                json!({"page": page, "pageSize": 1,
                "totalItems": 2, "totalPages": 2, "hasNextPage": page == 1})
            );
        }
        assert_eq!(
            found,
            [
                running["id"].as_str().unwrap().to_owned(),
                paused["id"].as_str().unwrap().to_owned()
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
        );
        let (status, _, list) = exchange(
            &fixture.app,
            Method::GET,
            "/v1/sandboxes?state=Failed",
            AUTH,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            list["pagination"],
            json!({"page": 1, "pageSize": 20,
            "totalItems": 0, "totalPages": 0, "hasNextPage": false})
        );
        assert!(list["items"].as_array().unwrap().is_empty());
        let (status, _, error) = exchange(
            &fixture.app,
            Method::GET,
            "/v1/sandboxes?page=0",
            AUTH,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&error, "INVALID_REQUEST");
    }

    #[tokio::test]
    async fn http_unsupported_creation_fields_are_501_without_runtime_creation() {
        let fixture = RouteFixture::open().await;
        for (field, value) in [
            ("snapshotId", json!("snapshot-fixture")),
            ("templateId", json!("template-fixture")),
            ("networkPolicy", json!({"defaultAction": "deny"})),
            ("credentialProxy", json!({"enabled": true})),
            ("secureAccess", json!(true)),
            ("volumes", json!([])),
            ("lifecycle", json!({"preStart": {"command": ["true"]}})),
            ("resourceRequests", json!({"cpu": "1"})),
            ("extensions", json!({"poolRef": "fixture"})),
            (
                "image",
                json!({"uri": "fixture", "auth": {"username": "user", "password": "secret"}}),
            ),
        ] {
            let mut payload = create_payload(json!({}));
            payload[field] = value;
            let (status, _, error) = exchange(
                &fixture.app,
                Method::POST,
                "/v1/sandboxes",
                AUTH,
                Body::from(payload.to_string()),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{field}: {error}");
            assert_error(&error, "NOT_SUPPORTED");
        }
        assert_eq!(fixture.runtime.creates.load(Ordering::SeqCst), 0);
        for (method, path) in [
            (Method::GET, "/v1/snapshots"),
            (Method::GET, "/v1/templates/fixture"),
            (Method::POST, "/v1/sandboxes/fixture/snapshots"),
            (Method::PATCH, "/v1/sandboxes/fixture/metadata"),
            (Method::GET, "/v1/sandboxes/fixture/networkpolicy"),
            (Method::POST, "/v1/metrics/events"),
        ] {
            let (status, _, error) =
                exchange(&fixture.app, method, path, AUTH, Body::empty()).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
            assert_error(&error, "NOT_IMPLEMENTED");
        }
        let (status, _, error) = exchange(
            &fixture.app,
            Method::GET,
            "/v1/templates/fixture/unknown",
            AUTH,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_error(&error, "NOT_FOUND");
    }

    #[tokio::test]
    async fn http_json_rejections_and_control_body_limit_use_400_envelopes() {
        let fixture = RouteFixture::open().await;
        for body in [
            String::from("{"),
            String::from("[]"),
            String::from(r#"{"image":42}"#),
            "x".repeat(CONTROL_BODY_LIMIT + 1),
        ] {
            let (status, _, error) = exchange(
                &fixture.app,
                Method::POST,
                "/v1/sandboxes",
                AUTH,
                Body::from(body),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_error(&error, "INVALID_REQUEST");
        }
        assert_eq!(fixture.runtime.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn http_sdk_endpoint_schema_and_proxy_token_auth() {
        let fixture = RouteFixture::open().await;
        let sandbox = create_over_http(&fixture.app, json!({})).await;
        let id = sandbox["id"].as_str().unwrap();
        let endpoint_path = format!("/v1/sandboxes/{id}/endpoints/44772");
        let expected_address = format!("localhost:8080/v1/sandboxes/{id}/proxy/44772");
        let mut token = String::new();
        for flag in ["", "?use_server_proxy=false", "?use_server_proxy=true"] {
            let (status, _, endpoint) = exchange(
                &fixture.app,
                Method::GET,
                &format!("{endpoint_path}{flag}"),
                AUTH,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(endpoint["endpoint"], expected_address);
            assert!(
                endpoint
                    .as_object()
                    .unwrap()
                    .keys()
                    .all(|key| key == "endpoint" || key == "headers")
            );
            if flag.ends_with("true") {
                assert!(endpoint.get("headers").is_none());
            } else {
                let headers = endpoint["headers"].as_object().unwrap();
                assert!(headers.values().all(Value::is_string));
                token = headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("x-pvisor-sandbox-token"))
                    .unwrap()
                    .1
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert!(!token.is_empty());
                assert!(
                    !headers
                        .keys()
                        .any(|key| key.eq_ignore_ascii_case("open-sandbox-api-key"))
                );
            }
        }
        let calls = fixture.runtime.endpoints.load(Ordering::SeqCst);
        let proxy_path = format!("/v1/sandboxes/{id}/proxy/44772");
        for suffix in ["", "/", "/files/upload?part=1&part=2"] {
            for credentials in [
                vec![],
                vec![("x-pvisor-sandbox-token", "wrong")],
                vec![
                    ("x-pvisor-sandbox-token", token.as_str()),
                    ("x-pvisor-sandbox-token", token.as_str()),
                ],
            ] {
                let (status, _, error) = exchange(
                    &fixture.app,
                    Method::POST,
                    &format!("{proxy_path}{suffix}"),
                    &credentials,
                    Body::empty(),
                )
                .await;
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                assert_error(&error, "UNAUTHORIZED");
            }
        }
        // A valid token or lifecycle key reaches the explicit upgrade rejection,
        // without opening a guest connection or requiring a native runtime.
        for credentials in [
            vec![
                ("x-pvisor-sandbox-token", token.as_str()),
                ("upgrade", "websocket"),
            ],
            vec![
                ("open-sandbox-api-key", TEST_API_KEY),
                ("upgrade", "websocket"),
            ],
        ] {
            let (status, _, error) = exchange(
                &fixture.app,
                Method::GET,
                &proxy_path,
                &credentials,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
            assert_error(&error, "NOT_IMPLEMENTED");
        }
        assert_eq!(fixture.runtime.endpoints.load(Ordering::SeqCst), calls);
        let other = create_over_http(&fixture.app, json!({})).await;
        let (status, _, error) = exchange(
            &fixture.app,
            Method::GET,
            &format!(
                "/v1/sandboxes/{}/proxy/44772",
                other["id"].as_str().unwrap()
            ),
            &[("x-pvisor-sandbox-token", token.as_str())],
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error(&error, "UNAUTHORIZED");
        for query in [
            "?expires=",
            "?expires=123",
            "?use_server_proxy=true&expires=123",
        ] {
            let (status, _, error) = exchange(
                &fixture.app,
                Method::GET,
                &format!("{endpoint_path}{query}"),
                AUTH,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_error(&error, "INVALID_REQUEST");
        }
    }

    #[test]
    fn authentication_requires_one_nonempty_credential() {
        let mut headers = HeaderMap::new();
        assert!(!single_credential(&headers, "open-sandbox-api-key"));
        headers.insert("open-sandbox-api-key", HeaderValue::from_static(" "));
        assert!(!single_credential(&headers, "open-sandbox-api-key"));
        headers.insert("open-sandbox-api-key", HeaderValue::from_static("key"));
        assert!(single_credential(&headers, "open-sandbox-api-key"));
        headers.append("open-sandbox-api-key", HeaderValue::from_static("key"));
        assert!(!single_credential(&headers, "open-sandbox-api-key"));
        assert!(!single_credential(&headers, "x-pvisor-sandbox-token"));
    }

    #[test]
    fn list_defaults_and_repeated_states() {
        let defaults = parse_list_query(None).unwrap_or_else(|_| panic!("defaults"));
        assert_eq!(
            (defaults.page, defaults.page_size, defaults.offset),
            (1, 20, 0)
        );
        let query = parse_list_query(Some("state=Running&state=Paused&page=3&pageSize=7"))
            .unwrap_or_else(|_| panic!("valid query"));
        assert_eq!(query.states, ["Running", "Paused"]);
        assert_eq!(query.offset, 14);
    }

    #[test]
    fn metadata_decodes_exactly_two_layers() {
        let inner = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("a&=+", "literal%20 &+=雪")
            .finish();
        let outer = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("metadata", &inner)
            .finish();
        let query = parse_list_query(Some(&outer)).unwrap_or_else(|_| panic!("metadata"));
        assert_eq!(
            query.metadata,
            [(String::from("a&=+"), String::from("literal%20 &+=雪"))]
        );
    }

    #[test]
    fn pagination_rejects_invalid_and_overflow() {
        for raw in [
            "page=0",
            "page=-1",
            "page=1&page=2",
            "pageSize=0",
            "page=1.5",
            "page=999999999999999999999999999999999999",
        ] {
            assert!(parse_list_query(Some(raw)).is_err(), "{raw}");
        }
        let raw = format!("page={}&pageSize=2", usize::MAX);
        assert!(parse_list_query(Some(&raw)).is_err());
        let raw = format!("page=1&pageSize={}", usize::MAX);
        assert!(parse_list_query(Some(&raw)).is_ok());
    }

    #[test]
    fn signed_endpoints_and_ambiguous_flags_are_rejected() {
        assert_eq!(parse_endpoint_query(None).ok(), Some(false));
        assert_eq!(
            parse_endpoint_query(Some("use_server_proxy=true")).ok(),
            Some(true)
        );
        for raw in [
            "expires=",
            "expires=123",
            "use_server_proxy=1",
            "use_server_proxy=false&use_server_proxy=true",
        ] {
            assert!(parse_endpoint_query(Some(raw)).is_err());
        }
    }

    #[test]
    fn removes_all_connection_nominations_and_control_secrets() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::CONNECTION,
            HeaderValue::from_static("X-One, keep-alive"),
        );
        headers.append(header::CONNECTION, HeaderValue::from_static("x-two"));
        for name in [
            "x-one",
            "x-two",
            "open-sandbox-api-key",
            "x-pvisor-sandbox-token",
            "authorization",
            "cookie",
            "x-execd-access-token",
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("secret"),
            );
        }
        headers.append("x-repeat", HeaderValue::from_static("first"));
        headers.append("x-repeat", HeaderValue::from_static("second"));
        strip_headers(&mut headers, true);
        for name in [
            "connection",
            "x-one",
            "x-two",
            "open-sandbox-api-key",
            "x-pvisor-sandbox-token",
            "authorization",
            "cookie",
        ] {
            assert!(!headers.contains_key(name), "{name}");
        }
        assert_eq!(headers["x-execd-access-token"], "secret");
        assert_eq!(headers.get_all("x-repeat").iter().count(), 2);
    }

    #[test]
    fn response_headers_preserve_content_and_repeated_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        headers.append(header::SET_COOKIE, HeaderValue::from_static("a=1"));
        headers.append(header::SET_COOKIE, HeaderValue::from_static("b=2"));
        headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        strip_headers(&mut headers, false);
        assert!(!headers.contains_key(header::TRANSFER_ENCODING));
        assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
        assert_eq!(headers.get_all(header::SET_COOKIE).iter().count(), 2);
    }

    #[test]
    fn proxy_preserves_escaped_path_and_repeated_query_without_authority_join() {
        let uri: Uri = "/v1/sandboxes/id/proxy/44772//evil.example/a%2Fb?x=1&x=2&v=%2520"
            .parse()
            .unwrap();
        let url = proxy_url("http://127.0.0.1:32123", &uri)
            .unwrap_or_else(|_| panic!("valid loopback proxy"));
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "//evil.example/a%2Fb");
        assert_eq!(url.query(), Some("x=1&x=2&v=%2520"));
        for uri in [
            "/v1/sandboxes/id/proxy/44772",
            "/v1/sandboxes/id/proxy/44772/",
        ] {
            let url = proxy_url("http://[::1]:32123", &uri.parse().unwrap())
                .unwrap_or_else(|_| panic!("root proxy"));
            assert_eq!(url.path(), "/");
        }
    }

    #[test]
    fn proxy_rejects_non_loopback_credentials_and_normalization() {
        let uri: Uri = "/v1/sandboxes/id/proxy/44772/test".parse().unwrap();
        for base in [
            "http://localhost:80",
            "http://example.com",
            "http://10.0.0.1",
            "http://user:secret@127.0.0.1",
            "http://@127.0.0.1",
            "http://127.0.0.1/base",
            "http://127.0.0.1/?secret=x",
            "ftp://127.0.0.1",
        ] {
            assert!(proxy_url(base, &uri).is_err(), "{base}");
        }
        let uri: Uri = "/v1/sandboxes/id/proxy/44772/%2e%2e/private"
            .parse()
            .unwrap();
        assert!(proxy_url("http://127.0.0.1:32123", &uri).is_err());
    }

    #[test]
    fn location_segment_cannot_inject_a_path_or_header() {
        assert_eq!(encode_segment("a/b?c\r\n"), "a%2Fb%3Fc%0D%0A");
    }
}
