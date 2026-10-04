//! Shared helpers for LLM gateway capture handlers.

use std::sync::Arc;

use axum::http::HeaderMap;
use bytes::Bytes;
use pvisor_core::ModelAccessPolicy;
use serde_json::Value;

use super::state::GatewayState;
use crate::Call;
use crate::config::ProxyConfig;
use crate::engine::CallContext;
use crate::engine::headers_to_vec;
use crate::runtime::run_config::load_session_proxy_config;
use crate::session::storage::{CaptureRoute, route_config_key};

pub(crate) fn effective_config(state: &GatewayState, route: &CaptureRoute) -> Arc<ProxyConfig> {
    load_session_proxy_config(state.storage.as_path(), route_config_key(route))
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(&state.config))
}

pub(crate) fn model_access_policy(config: &ProxyConfig) -> ModelAccessPolicy {
    let allowed_models = config
        .models
        .iter()
        .map(|route| route.name.clone())
        .collect();
    let providers: Vec<String> = config
        .models
        .iter()
        .filter_map(|route| route.provider.clone())
        .collect();
    // An inferred/custom provider must remain representable during migration.
    // An empty provider list means model identity is enforced but provider is open.
    let allowed_providers = if providers.len() == config.models.len() {
        providers
    } else {
        Vec::new()
    };
    ModelAccessPolicy {
        allowed_models,
        allowed_providers,
    }
}

pub(crate) fn call_context(
    route: &CaptureRoute,
    agent_id: &str,
    call: &Call,
    headers: &HeaderMap,
    capture: crate::engine::CallCaptureConfig,
) -> CallContext {
    CallContext::new(
        crate::engine::StoryContext::from_route(route.clone(), agent_id),
        call.clone(),
        headers_to_vec(headers),
        capture,
    )
}

pub(crate) fn extract_model(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

pub(crate) fn attach_capture_headers(
    builder: axum::http::response::Builder,
    call: &Call,
) -> axum::http::response::Builder {
    builder
        .header("x-pvisor-call-id", call.call_id.as_str())
        .header("x-pvisor-trace-id", call.trace_id.as_str())
}
