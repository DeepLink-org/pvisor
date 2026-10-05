//! A delegated model route must not become a credentialed administrative proxy.
//! All services bind loopback and all keys in this file are fictitious.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pvisor_gateway::config::ProxyConfig;
use pvisor_gateway::serve_with_listeners_and_shutdown;
use pvisor_gateway::sink::NoopCaptureObserver;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

const KEY: &str = "fictitious-gateway-action-scope-key";
const MODEL: &str = "allowed-model";

#[derive(Clone, Debug)]
struct Received {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Value,
}

struct Service {
    address: std::net::SocketAddr,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Service {
    async fn shutdown(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("service shutdown timeout")
            .expect("join service")
            .expect("service shutdown");
    }
}

async fn mock_upstream() -> (Service, Arc<Mutex<Vec<Received>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::clone(&captured);
    let app = Router::new().fallback(move |request: Request| {
        let requests = Arc::clone(&requests);
        async move {
            let (parts, body) = request.into_parts();
            let body: Value = serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap();
            let stream = parts.uri.path().ends_with(":streamGenerateContent")
                || body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let native_gemini = parts.uri.path().contains("/models/");
            requests.lock().unwrap().push(Received {
                method: parts.method.to_string(), path: parts.uri.to_string(),
                headers: parts.headers, body,
            });
            if stream && native_gemini {
                return Response::builder().status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .body(Body::from("data: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n"))
                    .unwrap();
            }
            // Valid minimal bodies for the supported passthrough protocols.
            axum::Json(json!({
                "id": "mock", "object": "chat.completion", "model": MODEL,
                "choices": [{"index":0, "message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                "type":"message", "role":"assistant", "content":[{"type":"text","text":"ok"}], "stop_reason":"end_turn",
                "status":"completed", "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],
                "candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],
                "input_tokens":1,"totalTokens":1,"data":[],
                "usage":{"prompt_tokens":1,"completion_tokens":1}
            })).into_response()
        }
    });
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await?;
        Ok(())
    });
    (
        Service {
            address,
            stop,
            task,
        },
        captured,
    )
}

async fn gateway(mock: &Service, gemini: bool, forward: bool) -> (Service, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let admin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_address = admin.local_addr().unwrap();
    let provider = if gemini { "provider = 'gemini'\n" } else { "" };
    let forwarding = if forward {
        format!("[[models]]\nname = '{MODEL}'\nforward = 'gemini-target'\n")
    } else {
        String::new()
    };
    let target = if forward { "gemini-target" } else { MODEL };
    let config = ProxyConfig::from_toml_str(&format!(
        "listen = '{address}'\nadmin_listen = '{admin_address}'\nagent_id = 'action-scope-test'\n\
         [network]\nmode = 'no-network'\n\
         {forwarding}[[models]]\nname = '{target}'\n{provider}\
         upstream = 'http://{}/trusted-prefix#api.openai.com'\n\
         upstream_anthropic = 'http://{}/trusted-prefix'\napi_key = '{KEY}'\n",
        mock.address, mock.address
    ))
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let path = storage.path().to_path_buf();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        serve_with_listeners_and_shutdown(
            config,
            path,
            Arc::new(NoopCaptureObserver::new()),
            listener,
            admin,
            async {
                let _ = stopped.await;
            },
        )
        .await
    });
    (
        Service {
            address,
            stop,
            task,
        },
        storage,
    )
}

// Use a raw origin-form request so reqwest cannot normalize the attack path
// before Gateway receives it. Read to EOF to establish the denial was delivered.
async fn raw_request(
    gateway: &Service,
    method: &str,
    path: &str,
    headers: &str,
    body: &Value,
) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(gateway.address)
        .await
        .unwrap();
    let body = body.to_string();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        gateway.address,
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .expect("response timeout")
        .unwrap();
    String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn request_body() -> Value {
    json!({"model": MODEL, "messages":[{"role":"user","content":"test"}],
        "input":"test", "contents":[{"role":"user","parts":[{"text":"test"}]}], "max_tokens":8})
}

#[tokio::test]
async fn administrative_methods_paths_and_normalization_bypasses_never_reach_upstream() {
    let (mock, received) = mock_upstream().await;
    let (gateway, _storage) = gateway(&mock, false, false).await;
    for (method, path) in [
        ("DELETE", "/v1/files/review-file"),
        ("POST", "/v1/files"),
        ("GET", "/v1/files"),
        ("PATCH", "/v1/responses"),
        ("DELETE", "/v1/chat/completions"),
        ("GET", "/v1/messages"),
        ("PUT", "/v1/embeddings"),
        ("OPTIONS", "/v1/chat/completions"),
        ("POST", "/v1/models"),
        ("POST", "/v1/responses/id/cancel"),
        ("POST", "/v1/messages/batches"),
        ("DELETE", "/v1/messages/batches/id"),
        ("POST", "/v1/realtime/sessions"),
        ("POST", "/v1/detect"),
        ("POST", "/v1/fine_tuning/jobs"),
        ("POST", "/prefix/v1/chat/completions"),
        ("POST", "/v1/files/messages"),
        ("POST", "/v1/messages/delete"),
        ("POST", "/v1/responses/../files"),
        ("POST", "/v1/files/../chat/completions"),
        ("POST", "/v1/./chat/completions"),
        ("POST", "/v1//chat/completions"),
        ("POST", "/v1/chat/completions//"),
        ("POST", "/v1/%2e%2e/chat/completions"),
        ("POST", "/v1/%63hat/completions"),
        ("POST", "/v1/chat%2fcompletions"),
        ("POST", "/v1/chat%252fcompletions"),
        ("POST", "/v1/chat\\completions"),
        ("POST", "/v1/chat/completions;action=delete"),
        ("POST", "/v1beta/models/.:generateContent"),
        ("POST", "/v1beta/models/..:generateContent"),
        ("POST", "/v1beta/models/a%2fb:generateContent"),
        ("POST", "/v1beta/models/a/b:generateContent"),
        ("POST", "/v1beta/models/a:delete"),
        ("DELETE", "/v1beta/models/allowed-model:generateContent"),
    ] {
        assert_eq!(
            raw_request(&gateway, method, path, "", &request_body()).await,
            403,
            "{method} {path}"
        );
        assert!(
            received.lock().unwrap().is_empty(),
            "upstream received {method} {path}"
        );
    }
    for header in [
        "X-HTTP-Method-Override: DELETE",
        "X-Method-Override: PUT",
        "X-HTTP-Method: PATCH",
        "X-Original-URL: /v1/files",
        "X-Rewrite-URL: /v1/files",
    ] {
        assert_eq!(
            raw_request(
                &gateway,
                "POST",
                "/v1/chat/completions",
                &format!("{header}\r\n"),
                &request_body()
            )
            .await,
            403,
            "{header}"
        );
        assert!(
            received.lock().unwrap().is_empty(),
            "override reached upstream"
        );
    }
    for query in [
        "_method=DELETE",
        "method=DELETE",
        "%5fmethod=DELETE",
        "METHOD=DELETE",
        "http_method=DELETE",
        "$httpMethod=DELETE",
        "path=%2fv1%2ffiles",
        "url=%2fv1%2ffiles",
        "x-original-url=%2fv1%2ffiles",
        "alt=sse&_method=DELETE",
    ] {
        let path = format!("/v1/chat/completions?{query}");
        assert_eq!(
            raw_request(&gateway, "POST", &path, "", &request_body()).await,
            403,
            "{query}"
        );
        assert!(
            received.lock().unwrap().is_empty(),
            "query override reached upstream"
        );
    }
    // A positive control proves the mock is reachable and no-network intentionally
    // retains the separate delegated model grant.
    assert_eq!(
        raw_request(
            &gateway,
            "POST",
            "/v1/chat/completions",
            "",
            &request_body()
        )
        .await,
        200
    );
    let records = received.lock().unwrap().clone();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].headers["authorization"], format!("Bearer {KEY}"));
    gateway.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test]
async fn supported_model_actions_and_local_discovery_keep_their_delegated_behavior() {
    let (mock, received) = mock_upstream().await;
    let (gateway, _storage) = gateway(&mock, false, false).await;
    for path in ["/models", "/v1/models", "/v1/models/", "/v1beta/models"] {
        assert_eq!(
            raw_request(&gateway, "GET", path, "", &json!({})).await,
            200,
            "{path}"
        );
        assert!(
            received.lock().unwrap().is_empty(),
            "discovery contacted upstream"
        );
    }
    for path in [
        "/v1/chat/completions",
        "/chat/completions",
        "/v1/chat/completions/",
        "/v1/responses",
        "/responses",
        "/v1/messages",
        "/messages",
        "/v1/embeddings",
        "/embeddings",
        "/v1/messages/count_tokens",
        "/messages/count_tokens",
        "/v1/count-tokens",
        "/v1/chat/completions?api-version=2024-10-01",
    ] {
        assert_eq!(
            raw_request(&gateway, "POST", path, "", &request_body()).await,
            200,
            "{path}"
        );
    }
    let records = received.lock().unwrap().clone();
    assert_eq!(records.len(), 13);
    for record in records {
        assert_eq!(record.method, "POST");
        assert!(
            record.path.starts_with("/trusted-prefix/v1/"),
            "{}",
            record.path
        );
        assert_eq!(record.body["model"], MODEL);
        // Native Messages uses Anthropic headers; all other actions use Bearer.
        assert!(
            record
                .headers
                .get("authorization")
                .is_some_and(|value| value == format!("Bearer {KEY}").as_str())
                || record
                    .headers
                    .get("x-api-key")
                    .is_some_and(|value| value == KEY)
        );
    }
    gateway.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test]
async fn native_gemini_actions_bind_policy_and_upstream_to_the_uri_model() {
    let (mock, received) = mock_upstream().await;
    let (gateway, _storage) = gateway(&mock, true, true).await;
    for version in ["v1", "v1beta"] {
        for operation in ["generateContent", "streamGenerateContent", "countTokens"] {
            let path = format!("/{version}/models/{MODEL}:{operation}?alt=sse");
            let body = json!({"contents":[{"role":"user","parts":[{"text":"test"}]}]});
            assert_eq!(
                raw_request(&gateway, "POST", &path, "", &body).await,
                200,
                "{path}"
            );
            let records = received.lock().unwrap();
            let record = records.last().unwrap();
            assert_eq!(
                record.path,
                format!("/trusted-prefix/{version}/models/gemini-target:{operation}?alt=sse")
            );
            assert_eq!(record.headers["x-goog-api-key"], KEY);
            assert!(
                record.body.get("model").is_none(),
                "native Gemini does not add a body model"
            );
        }
    }
    let before = received.lock().unwrap().len();
    let path = "/v1beta/models/allowed-model:generateContent";
    assert_eq!(
        raw_request(
            &gateway,
            "POST",
            path,
            "",
            &json!({"model":"other-model", "contents":[]})
        )
        .await,
        403
    );
    assert_eq!(
        raw_request(
            &gateway,
            "POST",
            "/v1beta/models/other-model:generateContent",
            "",
            &request_body()
        )
        .await,
        403
    );
    assert_eq!(
        received.lock().unwrap().len(),
        before,
        "conflicting identities reached upstream"
    );
    gateway.shutdown().await;
    mock.shutdown().await;
}
