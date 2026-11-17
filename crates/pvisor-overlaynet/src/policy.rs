//! HTTP adapter for the shared Control network policy.
use axum::http::StatusCode;
use pvisor_core::network::DenyReason;

pub(crate) fn forbidden_response(host: &str, reason: &DenyReason) -> (StatusCode, String) {
    (
        StatusCode::FORBIDDEN,
        format!(
            "pvisor-overlaynet: egress to `{host}` denied ({})",
            reason.as_str()
        ),
    )
}
