//! Gateway admin HTTP API: status + session list.

use pvisor_overlaynet::api::{InterceptionCoverage, InterceptionObservation};
use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::session::index::{SessionIndex, SessionIndexHandle};
use pvisor_overlaynet::api::{InterceptionMetrics, InterceptionProfile, InterceptionSnapshot};

#[derive(Clone)]
pub struct AdminState {
    pub index: SessionIndexHandle,
    pub listen: String,
    pub admin_listen: String,
    pub started_at: String,
    pub active_requests: Arc<std::sync::atomic::AtomicUsize>,
    pub interception_metrics: InterceptionMetrics,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub listen: String,
    pub admin: String,
    pub started_at: String,
    pub active_requests: usize,
    pub interception: InterceptionProfile,
    pub interception_metrics: InterceptionSnapshot,
    pub sessions: Vec<crate::session::index::SessionSummary>,
}

pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/status", get(status_handler))
        .route("/admin/sessions", get(sessions_handler))
        .with_state(state)
}

async fn status_handler(
    State(st): State<AdminState>,
) -> Result<Json<StatusResponse>, (axum::http::StatusCode, &'static str)> {
    let index = st.index.snapshot().map_err(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "session index unavailable",
        )
    })?;
    Ok(Json(StatusResponse {
        listen: st.listen.clone(),
        admin: st.admin_listen.clone(),
        started_at: st.started_at.clone(),
        active_requests: st
            .active_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        interception: InterceptionProfile::explicit_proxy(),
        interception_metrics: st.interception_metrics.snapshot(),
        sessions: index.sessions,
    }))
}

async fn sessions_handler(
    State(st): State<AdminState>,
) -> Result<Json<SessionIndex>, (axum::http::StatusCode, &'static str)> {
    st.index.snapshot().map(Json).map_err(|_| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "session index unavailable",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::index::SessionIndexStore;

    #[tokio::test]
    async fn status_reports_both_listen_addresses() {
        let storage = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(storage.path())
            .unwrap()
            .clone_handle();
        let response = status_handler(State(AdminState {
            index,
            listen: "127.0.0.1:9875".into(),
            admin_listen: "127.0.0.1:9876".into(),
            started_at: "2026-07-31T00:00:00Z".into(),
            active_requests: Arc::new(std::sync::atomic::AtomicUsize::new(3)),
            interception_metrics: InterceptionMetrics::default(),
        }))
        .await
        .unwrap();

        assert_eq!(response.0.listen, "127.0.0.1:9875");
        assert_eq!(response.0.admin, "127.0.0.1:9876");
        assert_eq!(response.0.active_requests, 3);
        assert!(
            response.0.interception.strength
                != pvisor_overlaynet::api::InterceptionStrength::NonBypassable
        );
        assert_eq!(
            response.0.interception_metrics,
            InterceptionSnapshot::default()
        );
    }
}
