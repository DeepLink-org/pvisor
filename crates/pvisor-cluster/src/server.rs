use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::*;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct App {
    scheduler: Arc<Mutex<Scheduler>>,
}

struct ApiError(anyhow::Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = if self
            .0
            .downcast_ref::<crate::journal::JournalFailure>()
            .is_some()
            || self.0.downcast_ref::<std::io::Error>().is_some()
        {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::CONFLICT
        };
        (
            status,
            Json(serde_json::json!({"error": self.0.to_string()})),
        )
            .into_response()
    }
}

async fn run<T: Send + 'static>(
    app: App,
    f: impl FnOnce(&mut Scheduler) -> anyhow::Result<T> + Send + 'static,
) -> Result<Json<T>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let mut scheduler = app
            .scheduler
            .lock()
            .map_err(|_| anyhow::anyhow!("scheduler unavailable"))?;
        f(&mut scheduler).map(Json)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}

async fn auth(
    State(token): State<Arc<String>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let expected = format!("Bearer {}", token.as_str());
    let matches = request.headers().get("authorization").is_some_and(|h| {
        // Compare hashes to avoid early exit on a matching token prefix.
        blake3::hash(h.as_bytes()) == blake3::hash(expected.as_bytes())
    });
    if !matches {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

pub fn router(
    scheduler: Scheduler,
    admin_token: String,
    worker_token: String,
) -> anyhow::Result<Router> {
    anyhow::ensure!(
        admin_token.len() >= 16 && worker_token.len() >= 16 && admin_token != worker_token,
        "use distinct admin/worker tokens with at least 16 characters"
    );
    let app = App {
        scheduler: Arc::new(Mutex::new(scheduler)),
    };
    let weak = Arc::downgrade(&app.scheduler);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            let Some(scheduler) = weak.upgrade() else {
                break;
            };
            let result = tokio::task::spawn_blocking(move || {
                scheduler
                    .lock()
                    .map_err(|_| anyhow::anyhow!("scheduler unavailable"))?
                    .reap(pvisor_core::unix_now_ms())
            })
            .await;
            if !matches!(result, Ok(Ok(_))) {
                eprintln!("controller lease reaper failed: {result:?}");
            }
        }
    });
    let admin = Router::new()
        .route("/v1/tasks", post(submit))
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/tasks/{id}/cancel", post(cancel))
        .route("/v1/tasks/{id}/control", post(control))
        .route("/v1/workers", get(workers))
        .route("/v1/workers/{id}/drain", post(drain))
        .route("/v1/counts", get(counts))
        .route_layer(middleware::from_fn_with_state(Arc::new(admin_token), auth));
    let worker = Router::new()
        .route("/v1/workers/register", post(register))
        .route("/v1/workers/poll", post(poll))
        .route("/v1/workers/complete", post(complete))
        .route("/v1/workers/decline", post(decline))
        .route("/v1/workers/control-ack", post(control_ack))
        .route_layer(middleware::from_fn_with_state(Arc::new(worker_token), auth));
    Ok(admin
        .merge(worker)
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"version":CLUSTER_VERSION})) }),
        )
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(app))
}

async fn submit(
    State(app): State<App>,
    Json(spec): Json<TaskSpec>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| s.submit(spec, pvisor_core::unix_now_ms())).await
}
async fn task(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| {
        s.reap(pvisor_core::unix_now_ms())?;
        s.task(&id)
    })
    .await
}
async fn cancel(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| s.cancel(&id, pvisor_core::unix_now_ms())).await
}
async fn register(
    State(app): State<App>,
    Json(registration): Json<WorkerRegistration>,
) -> Result<Json<WorkerRecord>, ApiError> {
    run(app, move |s| {
        s.register(registration, pvisor_core::unix_now_ms())
    })
    .await
}
async fn control(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<ControlRequest>,
) -> Result<Json<ControlRecord>, ApiError> {
    run(app, move |s| {
        s.request_control(&id, request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn control_ack(
    State(app): State<App>,
    Json(acknowledgement): Json<ControlAcknowledgement>,
) -> Result<Json<ControlRecord>, ApiError> {
    run(app, move |s| {
        s.acknowledge_control(acknowledgement, pvisor_core::unix_now_ms())
    })
    .await
}
async fn poll(
    State(app): State<App>,
    Json(request): Json<PollRequest>,
) -> Result<Json<PollResponse>, ApiError> {
    run(app, move |s| s.poll(request, pvisor_core::unix_now_ms())).await
}
async fn complete(
    State(app): State<App>,
    Json(request): Json<Completion>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| {
        s.complete(request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn decline(
    State(app): State<App>,
    Json(rejection): Json<AdmissionRejection>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| {
        s.decline(rejection, pvisor_core::unix_now_ms())
    })
    .await
}
async fn workers(State(app): State<App>) -> Result<Json<Vec<WorkerRecord>>, ApiError> {
    run(app, |s| {
        s.reap(pvisor_core::unix_now_ms())?;
        Ok(s.workers())
    })
    .await
}
async fn counts(
    State(app): State<App>,
) -> Result<Json<std::collections::BTreeMap<String, usize>>, ApiError> {
    run(app, |s| {
        s.reap(pvisor_core::unix_now_ms())?;
        Ok(s.counts())
    })
    .await
}
#[derive(Deserialize)]
struct Drain {
    draining: bool,
}
async fn drain(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<Drain>,
) -> Result<Json<serde_json::Value>, ApiError> {
    run(app, move |s| {
        s.drain(&id, request.draining)?;
        Ok(serde_json::json!({"draining":request.draining}))
    })
    .await
}

pub fn open(
    path: &std::path::Path,
    config: SchedulerConfig,
    admin_token: String,
    worker_token: String,
) -> anyhow::Result<Router> {
    router(Scheduler::open(path, config)?, admin_token, worker_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_storage_commit_is_retryable_and_not_a_stale_result_ack() {
        assert_eq!(
            ApiError(crate::journal::JournalFailure.into())
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ApiError(anyhow::anyhow!("stale lease"))
                .into_response()
                .status(),
            StatusCode::CONFLICT
        );
    }
}
