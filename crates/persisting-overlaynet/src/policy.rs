//! HTTP adapter for the shared Control network policy.
use axum::http::StatusCode;
pub use persisting_control::network::*;

pub fn forbidden_response(host: &str, reason: &DenyReason) -> (StatusCode, String) {
    (
        StatusCode::FORBIDDEN,
        format!(
            "persisting-overlaynet: egress to `{host}` denied ({})",
            reason.as_str()
        ),
    )
}
