//! Local protocol fixture. Replies are deterministic; clients execute real tools.
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use std::sync::{Arc, Mutex};

pub(crate) const KEY: &str = "model-upstream-test-secret-must-stay-in-worker";
pub(crate) struct ModelService {
    pub base_url: String,
    pub calls: Arc<Mutex<Vec<serde_json::Value>>>,
    server: tokio::task::JoinHandle<()>,
    gate: tokio::sync::watch::Sender<bool>,
}
#[derive(Clone)]
struct ModelState {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
    gate: tokio::sync::watch::Receiver<bool>,
}
impl ModelService {
    pub async fn start_held() -> Self {
        Self::spawn(false).await
    }
    pub fn release(&self) {
        self.gate.send_replace(true);
    }
    async fn spawn(released: bool) -> Self {
        async fn model(
            State(state): State<ModelState>,
            headers: HeaderMap,
            Json(request): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            assert_eq!(
                headers.get("authorization").unwrap(),
                format!("Bearer {KEY}").as_str()
            );
            assert_eq!(request["model"], "test-model");
            let message = if request["messages"].as_array().unwrap().len() == 1 {
                serde_json::json!({"role": "assistant", "content": null, "tool_calls": [{"id": "write-tool", "type": "function",
                    "function": {"name": "write_and_test", "arguments": "{\"path\":\"answer.py\",\"contents\":\"def multiply(a, b):\\n    return a * b\\n\"}"}}]})
            } else {
                assert_eq!(request["messages"][2]["role"], "tool");
                assert_eq!(request["messages"][2]["content"], "3 tests passed\n");
                serde_json::json!({"role": "assistant", "content": "3 tests passed"})
            };
            state.calls.lock().unwrap().push(request);
            let mut gate = state.gate.clone();
            gate.wait_for(|released| *released).await.unwrap();
            Json(
                serde_json::json!({"id": "cluster-model-reply", "object": "chat.completion", "created": 0,
                "model": "test-model", "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}),
            )
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (gate, gate_rx) = tokio::sync::watch::channel(released);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v1/chat/completions", post(model))
            .with_state(ModelState {
                calls: calls.clone(),
                gate: gate_rx,
            });
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base_url,
            calls,
            server,
            gate,
        }
    }
}
impl Drop for ModelService {
    fn drop(&mut self) {
        self.server.abort();
    }
}
