//! OpenCode Responses API resume-transport bridge.
//!
//! `opencode run --session <id>` refuses to start without a message.  The
//! SandboxReplay continuation therefore passes a unique transport nonce as
//! that message, and this bridge removes the nonce from every request before
//! forwarding it upstream, so the first live model request still ends exactly
//! at the replayed boundary observation.  OpenCode may resend the full
//! conversation history (including the persisted nonce) on every request, so
//! the cleanup is exact-match and repeated.  The bridge also pins sampling:
//! OpenCode never forwards `temperature`/`top_p` to the Responses API, so the
//! continuation would otherwise drift away from the recorded sampling.
//!
//! Responses bodies are streamed through unchanged apart from the JSON
//! rewrite: OpenCode treats a stalled stream as a dead connection and retries.

use std::sync::{Arc, Mutex};

use super::host::{self, BridgeHost, Cancellation, WorkerRuntime, merged_no_proxy_environment};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use anyhow::Context;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::Value;

const BRIDGE_VERSION: &str = "sandbox-replay-opencode-responses-bridge/1";
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub struct OpencodeBridgeHandle {
    pub base_url: String,
    api_key: String,
    shared: Arc<BridgeShared>,
    host: BridgeHost,
}

struct BridgeShared {
    state: Mutex<BridgeState>,
    client: reqwest::Client,
    upstream_origin: String,
    upstream_api_key: String,
    routing_session_id: String,
    bridge_api_key: String,
    strip_prompt: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    disable_thinking: bool,
    cancellation: Arc<Cancellation>,
}

#[derive(Default)]
struct BridgeState {
    forwarded_requests: usize,
    removed_transport_prompt: bool,
    failed: bool,
    failure: Option<String>,
}

impl BridgeState {
    fn fail(&mut self, message: impl Into<String>) {
        self.failed = true;
        if self.failure.is_none() {
            self.failure = Some(message.into());
        }
    }
}

impl OpencodeBridgeHandle {
    /// Start the bridge.  `strip_prompt` is the transport nonce to remove
    /// from every request; `None` keeps an explicit `boundary_user_prompt`
    /// in the model input on purpose.
    pub fn start(
        routing_session_id: &str,
        strip_prompt: Option<String>,
        temperature: Option<f64>,
        top_p: Option<f64>,
        disable_thinking: bool,
    ) -> Result<Self, ReplayError> {
        let upstream_base = first_nonempty_env(&["OPENAI_BASE_URL", "OPENAI_API_BASE"])
            .ok_or_else(|| {
                ReplayError::configuration(
                    "OpenCode SandboxReplay bridge requires OPENAI_BASE_URL or OPENAI_API_BASE",
                )
            })?;
        let upstream_api_key =
            first_nonempty_env(&["OPENAI_API_KEY", "LLM_API_KEY"]).ok_or_else(|| {
                ReplayError::configuration(
                    "OpenCode SandboxReplay bridge requires OPENAI_API_KEY or LLM_API_KEY",
                )
            })?;
        let upstream_origin = url_origin(&upstream_base)?;
        // Preserve the API prefix; OpenCode appends /responses to this base URL.
        let upstream_prefix = url_path_prefix(&upstream_base)?;
        let bridge_api_key = host::bridge_api_key();
        let (listener, address) = host::bind_loopback("OpenCode")?;
        let client = host::upstream_client("OpenCode")?;
        let shared = Arc::new(BridgeShared {
            state: Mutex::new(BridgeState::default()),
            client,
            upstream_origin,
            upstream_api_key,
            routing_session_id: routing_session_id.to_owned(),
            bridge_api_key: bridge_api_key.clone(),
            strip_prompt,
            temperature,
            top_p,
            disable_thinking,
            cancellation: Arc::new(Cancellation::default()),
        });
        let host = BridgeHost::start(
            "OpenCode",
            listener,
            router(Arc::clone(&shared)),
            shared.cancellation.clone(),
            WorkerRuntime::CurrentThread,
            ReplayErrorKind::Executor,
        )?;
        Ok(Self {
            base_url: format!("http://{address}{upstream_prefix}"),
            api_key: bridge_api_key,
            shared,
            host,
        })
    }

    /// Environment for the OpenCode child so it talks only to this bridge.
    pub fn child_environment(&self) -> Vec<(String, String)> {
        let no_proxy = merged_no_proxy_environment();
        vec![
            ("OPENAI_BASE_URL".to_owned(), self.base_url.clone()),
            ("OPENAI_API_BASE".to_owned(), self.base_url.clone()),
            ("OPENAI_API_KEY".to_owned(), self.api_key.clone()),
            ("NO_PROXY".to_owned(), no_proxy.clone()),
            ("no_proxy".to_owned(), no_proxy),
        ]
    }

    pub fn finish(mut self) -> Result<usize, ReplayError> {
        self.host.stop()?;
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| ReplayError::continuation("OpenCode bridge state lock poisoned"))?;
        if state.failed {
            return Err(ReplayError::continuation(format!(
                "OpenCode resume transport bridge failed closed: {}",
                state
                    .failure
                    .as_deref()
                    .unwrap_or("unknown protocol failure")
            )));
        }
        if state.forwarded_requests == 0 {
            return Err(ReplayError::continuation(
                "OpenCode continuation made no model request through the SandboxReplay bridge",
            ));
        }
        if self.shared.strip_prompt.is_some() && !state.removed_transport_prompt {
            return Err(ReplayError::continuation(
                "OpenCode transport nonce was not removed from any model request",
            ));
        }
        Ok(state.forwarded_requests)
    }
}

fn router(shared: Arc<BridgeShared>) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health))
        .fallback(forward_handler)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(shared)
}

async fn health(State(shared): State<Arc<BridgeShared>>) -> Response {
    let (failed, forwarded) = shared
        .state
        .lock()
        .map(|state| (state.failed, state.forwarded_requests))
        .unwrap_or((true, 0));
    json_response(StatusCode::OK, json_health(failed, forwarded))
}

fn json_health(failed: bool, forwarded: usize) -> Bytes {
    serde_json::to_vec(&serde_json::json!({
        "status": "healthy",
        "bridge_version": BRIDGE_VERSION,
        "failed": failed,
        "forwarded_requests": forwarded,
    }))
    .unwrap_or_default()
    .into()
}

/// Forward any request upstream, rewriting JSON bodies: strip the transport
/// nonce and pin sampling.  Non-JSON requests (catalog fetches and probes)
/// pass through untouched.
async fn forward_handler(
    State(shared): State<Arc<BridgeShared>>,
    request: axum::extract::Request,
) -> Response {
    if !authorized(&shared, request.headers()) {
        return error_response(StatusCode::UNAUTHORIZED, "invalid bridge API key");
    }
    let method = request.method().clone();
    let path = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str().to_owned());
    let headers = request.headers().clone();
    let body = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            fail(
                &shared,
                format!("read OpenCode bridge request body: {error}"),
            );
            return error_response(StatusCode::BAD_REQUEST, "invalid request body");
        }
    };
    let Some(path) = path else {
        return error_response(StatusCode::BAD_REQUEST, "request has no path");
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_json = method == axum::http::Method::POST && content_type.contains("application/json");
    let body: Bytes = if is_json && !body.is_empty() {
        match rewrite_request(&shared, &body) {
            Ok(rewritten) => rewritten,
            Err(error) => {
                fail(&shared, error.to_string());
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string());
            }
        }
    } else {
        body
    };
    let upstream_url = format!("{}{}", shared.upstream_origin, path);
    let mut upstream = shared.client.request(method, &upstream_url);
    for (name, value) in headers.iter() {
        let name = name.as_str();
        if is_hop_by_hop_header(name) || name == "host" {
            continue;
        }
        if name == "authorization" || name == "x-api-key" {
            continue;
        }
        // The JSON rewrite changes the body length (nonce removal, sampling
        // injection), so the incoming framing headers must never be trusted;
        // reqwest re-frames the full Bytes body itself.
        if name == "content-length" || name == "transfer-encoding" {
            continue;
        }
        upstream = upstream.header(name, value.clone());
    }
    upstream = upstream
        .header(
            axum::http::header::AUTHORIZATION.as_str(),
            format!("Bearer {}", shared.upstream_api_key),
        )
        .header("X-LiteLLM-Session-ID", &shared.routing_session_id);
    if !body.is_empty() {
        upstream = upstream.body(reqwest::Body::from(body));
    }
    let forwarding = async {
        let response = match upstream.send().await {
            Ok(response) => response,
            Err(error) => {
                fail(&shared, format!("forward OpenCode request: {error}"));
                return error_response(StatusCode::BAD_GATEWAY, &error.to_string());
            }
        };
        let status = response.status();
        let mut response_headers = HeaderMap::new();
        for (name, value) in response.headers().iter() {
            if is_hop_by_hop_header(name.as_str()) {
                continue;
            }
            if let Ok(header_value) = HeaderValue::from_bytes(value.as_bytes()) {
                response_headers.insert(name.clone(), header_value);
            }
        }
        // Buffer before delivery: OpenCode can hang on an interrupted response stream.
        let body = match super::read_response_limited(response).await {
            Ok(bytes) => bytes,
            Err(error) => {
                fail(&shared, format!("read upstream OpenCode response: {error}"));
                return error_response(StatusCode::BAD_GATEWAY, &error.to_string());
            }
        };
        let mut builder = Response::builder()
            .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
        for (name, value) in response_headers.iter() {
            builder = builder.header(name.clone(), value.clone());
        }
        match builder.body(Body::from(body)) {
            Ok(response) => {
                if let Ok(mut state) = shared.state.lock() {
                    state.forwarded_requests += 1;
                }
                response
            }
            Err(error) => {
                fail(&shared, format!("build OpenCode bridge response: {error}"));
                error_response(StatusCode::BAD_GATEWAY, "bridge response failure")
            }
        }
    };
    tokio::select! {
        biased;
        _ = shared.cancellation.cancelled() => {
            let message = "OpenCode SandboxReplay bridge was cancelled";
            fail(&shared, message.into());
            error_response(StatusCode::BAD_GATEWAY, message)
        }
        response = forwarding => response,
    }
}

fn rewrite_request(shared: &BridgeShared, body: &[u8]) -> anyhow::Result<Bytes> {
    let mut payload: Value =
        serde_json::from_slice(body).context("OpenCode bridge request is not valid JSON")?;
    if !payload.is_object() {
        anyhow::bail!("OpenCode bridge request must be a JSON object");
    }
    if let Some(nonce) = &shared.strip_prompt {
        let removed = remove_exact_user_input(&mut payload, nonce)?;
        if removed > 0
            && let Ok(mut state) = shared.state.lock()
        {
            state.removed_transport_prompt = true;
        }
    }
    let mut changed = false;
    if let Some(temperature) = shared.temperature {
        payload["temperature"] = serde_json::json!(temperature);
        changed = true;
    }
    if let Some(top_p) = shared.top_p {
        payload["top_p"] = serde_json::json!(top_p);
        changed = true;
    }
    if shared.disable_thinking {
        // Greedy reasoning models can loop until the output cap and emit an
        // empty turn; the endpoint only disables thinking through the chat
        // template, which OpenCode cannot express.
        payload["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        changed = true;
    }
    if changed || shared.strip_prompt.is_some() {
        Ok(serde_json::to_vec(&payload)?.into())
    } else {
        Ok(Bytes::copy_from_slice(body))
    }
}

/// Remove the exact nonce user message from a request.  Handles both the
/// Responses `input` array (items typed `user` or roled `user` with
/// `input_text` content) and the Chat Completions `messages` array.
fn remove_exact_user_input(payload: &mut Value, expected: &str) -> anyhow::Result<usize> {
    let mut removed = 0;
    for key in ["input", "messages"] {
        let Some(items) = payload.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut index = 0;
        while index < items.len() {
            let is_user = items[index].get("role").and_then(Value::as_str) == Some("user")
                || items[index].get("type").and_then(Value::as_str) == Some("user");
            if is_user && exact_message_text(&items[index]) == Some(expected) {
                items.remove(index);
                removed += 1;
            } else {
                index += 1;
            }
        }
    }
    Ok(removed)
}

fn exact_message_text(message: &Value) -> Option<&str> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    let blocks = content.as_array()?;
    if blocks.len() != 1 {
        return None;
    }
    let block = &blocks[0];
    let kind = block.get("type").and_then(Value::as_str)?;
    if kind != "input_text" && kind != "text" {
        return None;
    }
    block.get("text").and_then(Value::as_str)
}

fn authorized(shared: &BridgeShared, headers: &HeaderMap) -> bool {
    let supplied = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });
    supplied == Some(shared.bridge_api_key.as_str())
}

fn fail(shared: &BridgeShared, message: String) {
    if let Ok(mut state) = shared.state.lock() {
        state.fail(message);
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    json_response(
        status,
        serde_json::to_vec(&serde_json::json!({"error": {"message": message}}))
            .unwrap_or_default()
            .into(),
    )
}

fn json_response(status: StatusCode, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn url_path_prefix(base: &str) -> Result<String, ReplayError> {
    let parsed = reqwest::Url::parse(base).replay_context(
        ReplayErrorKind::Configuration,
        "parse OpenCode upstream URL",
    )?;
    let path = parsed.path().trim_end_matches('/');
    Ok(path.to_owned())
}

fn url_origin(base: &str) -> Result<String, ReplayError> {
    let parsed = reqwest::Url::parse(base).replay_context(
        ReplayErrorKind::Configuration,
        "parse OpenCode upstream URL",
    )?;
    let origin = match parsed.port() {
        Some(port) => format!(
            "{}://{}:{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default(),
            port
        ),
        None => format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default()
        ),
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ReplayError::configuration(
            "OpenCode upstream URL must be HTTP(S)",
        ));
    }
    Ok(origin)
}

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeHost, BridgeShared, OpencodeBridgeHandle, ReplayErrorKind, StatusCode, WorkerRuntime,
        host, remove_exact_user_input, rewrite_request, router,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finish_cancels_stalled_upstreams_and_preserves_failure_state() {
        for partial_body in [false, true] {
            let (upstream_url, started, upstream) =
                super::super::test_support::stalled_upstream(partial_body).await;
            let key = host::bridge_api_key();
            let mut state = shared(Some("nonce"), None, None);
            state.client = host::upstream_client("OpenCode").unwrap();
            state.upstream_origin = upstream_url;
            state.upstream_api_key = "upstream-test-key".into();
            state.bridge_api_key = key.clone();
            let shared = Arc::new(state);
            let (listener, address) = host::bind_loopback("OpenCode").unwrap();
            let bridge = OpencodeBridgeHandle {
                base_url: format!("http://{address}/v1"),
                api_key: key.clone(),
                shared: shared.clone(),
                host: BridgeHost::start(
                    "OpenCode",
                    listener,
                    router(shared.clone()),
                    shared.cancellation.clone(),
                    WorkerRuntime::CurrentThread,
                    ReplayErrorKind::Executor,
                )
                .unwrap(),
            };
            let client = host::upstream_client("test").unwrap();
            let endpoint = format!("{}/responses", bridge.base_url);
            for credential in [None, Some("foreign-key")] {
                let mut denied = client.post(&endpoint).body("{}");
                if let Some(credential) = credential {
                    denied = denied.bearer_auth(credential);
                }
                assert_eq!(
                    denied.send().await.unwrap().status(),
                    StatusCode::UNAUTHORIZED
                );
            }
            let request_key = key.clone();
            let request = tokio::spawn(async move {
                client
                    .post(endpoint)
                    .bearer_auth(request_key)
                    .header("content-type", "application/json")
                    .body(
                        json!({"input":[
                            {"role":"user","content":"task"},
                            {"role":"user","content":"nonce"}
                        ]})
                        .to_string(),
                    )
                    .send()
                    .await
            });
            let headers = tokio::time::timeout(Duration::from_secs(2), started)
                .await
                .unwrap()
                .unwrap();
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer upstream-test-key"));
            assert!(!headers.contains(&key));
            let error = bridge.finish().unwrap_err();
            assert_eq!(error.kind, ReplayErrorKind::Continuation);
            assert!(error.message.contains("failed closed"));
            let response = tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert!(response.text().await.unwrap().contains("cancelled"));
            let state = shared.state.lock().unwrap();
            assert!(state.failed);
            assert_eq!(state.forwarded_requests, 0);
            drop(state);
            upstream.abort();
            assert!(std::net::TcpListener::bind(address).is_ok());
        }
    }

    fn shared(strip: Option<&str>, temperature: Option<f64>, top_p: Option<f64>) -> BridgeShared {
        shared_full(strip, temperature, top_p, false)
    }

    fn shared_full(
        strip: Option<&str>,
        temperature: Option<f64>,
        top_p: Option<f64>,
        disable_thinking: bool,
    ) -> BridgeShared {
        BridgeShared {
            state: Mutex::new(Default::default()),
            client: reqwest::Client::new(),
            upstream_origin: "http://127.0.0.1:9".into(),
            upstream_api_key: "upstream".into(),
            routing_session_id: "ses".into(),
            bridge_api_key: "bridge".into(),
            strip_prompt: strip.map(str::to_owned),
            temperature,
            top_p,
            disable_thinking,
            cancellation: Default::default(),
        }
    }

    #[test]
    fn disables_thinking_through_the_chat_template() {
        let bridge = shared_full(None, Some(0.0), Some(1.0), true);
        let request = json!({"model": "m", "input": [{"role": "user", "content": "task"}]});
        let rewritten: serde_json::Value = serde_json::from_slice(
            &rewrite_request(&bridge, serde_json::to_vec(&request).unwrap().as_slice()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            rewritten["chat_template_kwargs"],
            serde_json::json!({"enable_thinking": false})
        );
    }

    #[test]
    fn strips_nonce_and_pins_sampling_in_responses_shape() {
        let bridge = shared(Some("pvisor-opencode-resume-nonce"), Some(0.0), Some(1.0));
        let request = json!({
            "model": "m",
            "input": [
                {"type": "system"},
                {"role": "user", "content": "the task"},
                {"role": "assistant", "content": [{"type": "output_text", "text": "working"}]},
                {"type": "function_call", "call_id": "c1"},
                {"type": "function_call_output", "call_id": "c1"},
                {"type": "user", "content": [{"type": "input_text", "text": "pvisor-opencode-resume-nonce"}]}
            ]
        });
        let rewritten: serde_json::Value = serde_json::from_slice(
            &rewrite_request(&bridge, serde_json::to_vec(&request).unwrap().as_slice()).unwrap(),
        )
        .unwrap();
        let kinds: Vec<&str> = rewritten["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                item.get("type")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("role").and_then(|v| v.as_str()))
                    .unwrap_or("?")
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "system",
                "user",
                "assistant",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(rewritten["temperature"], 0.0);
        assert_eq!(rewritten["top_p"], 1.0);
        assert!(bridge.state.lock().unwrap().removed_transport_prompt);
    }

    #[test]
    fn strips_nonce_from_chat_completions_shape() {
        let mut request = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "task"},
                {"role": "user", "content": "nonce-value"},
                {"role": "assistant", "content": "ok"}
            ]
        });
        assert_eq!(
            remove_exact_user_input(&mut request, "nonce-value").unwrap(),
            1
        );
        assert_eq!(request["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn keeps_explicit_boundary_prompt() {
        let bridge = shared(None, None, None);
        let request = json!({
            "model": "m",
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "continue from here"}]}
            ]
        });
        let rewritten: serde_json::Value = serde_json::from_slice(
            &rewrite_request(&bridge, serde_json::to_vec(&request).unwrap().as_slice()).unwrap(),
        )
        .unwrap();
        assert_eq!(rewritten["input"].as_array().unwrap().len(), 1);
        assert!(!bridge.state.lock().unwrap().removed_transport_prompt);
    }
}
