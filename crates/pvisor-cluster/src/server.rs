use crate::scheduler::{Scheduler, SchedulerConfig};
use crate::*;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

mod dispatcher;

#[derive(Clone)]
struct App {
    scheduler: Arc<Mutex<Scheduler>>,
    dispatcher: Arc<dispatcher::Dispatcher>,
}

impl App {
    fn lock(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Scheduler>> {
        dispatcher::lock(&self.scheduler)
    }
}

#[derive(Debug)]
struct ArtifactsRetired;
impl std::fmt::Display for ArtifactsRetired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("task evidence has been retired")
    }
}
impl std::error::Error for ArtifactsRetired {}

struct ApiError(anyhow::Error);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = if self.0.downcast_ref::<ArtifactsRetired>().is_some() {
            StatusCode::GONE
        } else if self
            .0
            .downcast_ref::<crate::artifacts::QuotaExceeded>()
            .is_some()
        {
            StatusCode::INSUFFICIENT_STORAGE
        } else if self
            .0
            .downcast_ref::<crate::journal::JournalFailure>()
            .is_some()
            || self.0.downcast_ref::<dispatcher::Overloaded>().is_some()
            || self.0.downcast_ref::<std::io::Error>().is_some()
            || self
                .0
                .downcast_ref::<crate::artifacts::PublicationFailure>()
                .is_some_and(|error| error.retryable)
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
    app.dispatcher.call(f).await.map(Json).map_err(ApiError)
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
    let scheduler = Arc::new(Mutex::new(scheduler));
    let dispatcher = Arc::new(dispatcher::Dispatcher::new(scheduler.clone())?);
    let weak = Arc::downgrade(&dispatcher);
    let app = App {
        scheduler,
        dispatcher,
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            let Some(dispatcher) = weak.upgrade() else {
                break;
            };
            let result = dispatcher.reap().await;
            if result.is_err() {
                eprintln!("controller lease reaper failed: {result:?}");
                if result.as_ref().is_err_and(|error| {
                    error
                        .downcast_ref::<crate::journal::JournalFailure>()
                        .is_some()
                }) {
                    break;
                }
            }
        }
    });
    let admin = Router::new()
        .route("/v1/environments", post(publish_environment))
        .route("/v1/environments/{digest}", get(environment))
        .route("/v1/tasks", post(submit))
        .route("/v1/graphs", post(submit_graph))
        .route("/v1/graphs/{id}", get(task_graph))
        .route("/v1/graphs/{id}/cancel", post(cancel_graph))
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/tasks/{id}/cancel", post(cancel))
        .route("/v1/tasks/{id}/control", post(control))
        .route("/v1/tasks/{id}/forks", post(fork_execution))
        .route("/v1/tasks/{id}/forks/{request_id}", get(execution_fork))
        .route("/v1/tasks/{id}/live-forks", post(request_live_fork))
        .route("/v1/tasks/{id}/live-forks/{request_id}", get(live_fork))
        .route("/v1/tasks/{id}/artifacts", get(task_artifacts))
        .route(
            "/v1/tasks/{id}/artifact-downloads",
            post(begin_artifact_download),
        )
        .route(
            "/v1/artifact-downloads/{id}/renew",
            post(renew_artifact_download),
        )
        .route(
            "/v1/artifact-downloads/{id}/release",
            post(release_artifact_download),
        )
        .route("/v1/artifact-storage/gc/plan", post(artifact_gc_plan))
        .route("/v1/artifact-storage/gc/apply", post(artifact_gc_apply))
        .route("/v1/artifacts/{digest}", get(artifact_bytes))
        .route("/v1/workers", get(workers))
        .route("/v1/workers/{id}/drain", post(drain))
        .route("/v1/counts", get(counts))
        .route("/v1/artifact-storage", get(artifact_storage))
        .route("/v1/artifact-storage/limits", post(update_artifact_storage))
        .route_layer(middleware::from_fn_with_state(Arc::new(admin_token), auth));
    let worker = Router::new()
        .route("/v1/workers/register", post(register))
        .route("/v1/workers/poll", post(poll))
        .route("/v1/workers/memory", post(report_memory))
        .route("/v1/workers/cpu", post(report_cpu))
        .route("/v1/workers/node-memory", post(report_node_memory))
        .route("/v1/workers/recover", post(recover))
        .route("/v1/workers/complete", post(complete))
        .route("/v1/workers/native-done", post(native_done))
        .route("/v1/workers/decline", post(decline))
        .route("/v1/workers/control-ack", post(control_ack))
        .route(
            "/v1/workers/artifacts/{task_id}/{generation}/{worker_id}/{incarnation}/{digest}",
            post(upload_artifact).layer(DefaultBodyLimit::max(ARTIFACT_CHUNK_BYTES)),
        )
        .route_layer(middleware::from_fn_with_state(Arc::new(worker_token), auth));
    Ok(admin
        .merge(worker)
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(app))
}

async fn health(State(app): State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    app.dispatcher.ensure_available().map_err(ApiError)?;
    Ok(Json(serde_json::json!({"version":CLUSTER_VERSION})))
}

async fn publish_environment(
    State(app): State<App>,
    Json(template): Json<EnvironmentTemplate>,
) -> Result<Json<EnvironmentRecord>, ApiError> {
    run(app, move |s| s.publish_environment(template)).await
}
async fn environment(
    State(app): State<App>,
    Path(digest): Path<String>,
) -> Result<Json<EnvironmentRecord>, ApiError> {
    run(app, move |s| s.environment(&digest)).await
}
async fn submit(
    State(app): State<App>,
    Json(spec): Json<TaskSpec>,
) -> Result<Json<TaskRecord>, ApiError> {
    run(app, move |s| s.submit(spec, pvisor_core::unix_now_ms())).await
}
async fn submit_graph(
    State(app): State<App>,
    Json(spec): Json<TaskGraphSpec>,
) -> Result<Json<TaskGraphRecord>, ApiError> {
    run(app, move |s| {
        s.submit_graph(spec, pvisor_core::unix_now_ms())
    })
    .await
}
async fn task_graph(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<TaskGraphRecord>, ApiError> {
    run(app, move |s| {
        s.reap(pvisor_core::unix_now_ms())?;
        s.graph(&id)
    })
    .await
}
async fn cancel_graph(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<TaskGraphRecord>, ApiError> {
    run(app, move |s| {
        s.cancel_graph(&id, pvisor_core::unix_now_ms())
    })
    .await
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
async fn fork_execution(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<ExecutionForkRequest>,
) -> Result<Json<ExecutionForkRecord>, ApiError> {
    run(app, move |s| {
        s.fork_execution(&id, request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn execution_fork(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Result<Json<ExecutionForkRecord>, ApiError> {
    run(app, move |s| s.execution_fork(&id, &request_id)).await
}
async fn request_live_fork(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<ExecutionForkRequest>,
) -> Result<Json<LiveForkRecord>, ApiError> {
    run(app, move |s| {
        s.request_live_fork(&id, request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn live_fork(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Result<Json<LiveForkRecord>, ApiError> {
    run(app, move |s| {
        s.reap(pvisor_core::unix_now_ms())?;
        s.live_fork(&id, &request_id)
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
async fn report_cpu(
    State(app): State<App>,
    Json(request): Json<CpuReportRequest>,
) -> Result<Json<CpuReportReceipt>, ApiError> {
    run(app, move |scheduler| {
        scheduler.report_cpu(request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn report_memory(
    State(app): State<App>,
    Json(request): Json<MemoryReportRequest>,
) -> Result<Json<MemoryReportReceipt>, ApiError> {
    run(app, move |s| {
        s.report_memory(request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn report_node_memory(
    State(app): State<App>,
    Json(request): Json<NodeMemoryReportRequest>,
) -> Result<Json<NodeMemoryReportReceipt>, ApiError> {
    run(app, move |scheduler| {
        scheduler.report_node_memory(request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn native_done(
    State(app): State<App>,
    Json(request): Json<NativeDone>,
) -> Result<Json<NativeDoneReceipt>, ApiError> {
    run(app, move |s| {
        s.native_done(request, pvisor_core::unix_now_ms())
    })
    .await
}
async fn complete(
    State(app): State<App>,
    Json(request): Json<Completion>,
) -> Result<Json<TaskRecord>, ApiError> {
    let retry = request.clone();
    if let Some(receipt) = run(app.clone(), move |s| s.completion_receipt(&retry))
        .await?
        .0
    {
        return Ok(Json(receipt));
    }
    let reference = request.artifacts.clone();
    let key = request.key.clone();
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    let verified = tokio::task::spawn_blocking(move || {
        reference
            .as_ref()
            .map(|r| store.verify(r, &key))
            .transpose()
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)?;
    run(app, move |s| {
        s.complete_verified(request, verified, pvisor_core::unix_now_ms())
    })
    .await
}

async fn recover(
    State(app): State<App>,
    Json(request): Json<RecoveryRequest>,
) -> Result<Json<RecoveryResponse>, ApiError> {
    run(app, move |s| s.recover(request, pvisor_core::unix_now_ms())).await
}

async fn artifact_gc_plan(
    State(app): State<App>,
    Json(request): Json<ArtifactGcRequest>,
) -> Result<Json<ArtifactGcPlan>, ApiError> {
    request.validate().map_err(ApiError)?;
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || {
        let sequence = store.gc_sequence()?;
        let now = pvisor_core::unix_now_ms();
        let snapshot = app.lock()?.artifact_gc_snapshot(&request, now)?;
        store.gc_plan(request, snapshot, sequence, now).map(Json)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}
async fn artifact_gc_apply(
    State(app): State<App>,
    Json(request): Json<ArtifactGcApply>,
) -> Result<Json<ArtifactGcReport>, ApiError> {
    if request.version != CLUSTER_VERSION {
        return Err(ApiError(anyhow::anyhow!(
            "unsupported artifact GC protocol"
        )));
    }
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || {
        let now = pvisor_core::unix_now_ms();
        store
            .apply_gc(&request.plan_id, now, |entries| {
                app.lock()?.retire_artifacts(entries, now)
            })
            .map(Json)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}
async fn begin_artifact_download(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<ArtifactDownload>, ApiError> {
    let task_id = id.clone();
    let (store, reference) = run(app.clone(), move |scheduler| {
        let task = scheduler.task(&task_id)?;
        if task.artifact_retired_at_ms.is_some() {
            return Err(ArtifactsRetired.into());
        }
        Ok((
            scheduler.artifact_store(),
            task.artifacts
                .ok_or_else(|| anyhow::anyhow!("task has no retained artifacts"))?,
        ))
    })
    .await?
    .0;
    tokio::task::spawn_blocking(move || {
        let download = store.begin_download(&reference, pvisor_core::unix_now_ms())?;
        let eligible = {
            let scheduler = app.lock()?;
            let task = scheduler.task(&id)?;
            task.artifact_retired_at_ms.is_none() && task.artifacts.as_ref() == Some(&reference)
        };
        if !eligible {
            store.release_download(&download.id)?;
            return Err(ArtifactsRetired.into());
        }
        Ok(Json(download))
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}
async fn renew_artifact_download(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<ArtifactDownload>, ApiError> {
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || {
        store
            .renew_download(&id, pvisor_core::unix_now_ms())
            .map(Json)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}
async fn release_artifact_download(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || {
        store.release_download(&id)?;
        Ok(Json(serde_json::json!({"released": true})))
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}

async fn update_artifact_storage(
    State(app): State<App>,
    Json(limits): Json<ArtifactStorageLimits>,
) -> Result<Json<ArtifactStorageUsage>, ApiError> {
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || store.update_storage_limits(limits).map(Json))
        .await
        .map_err(|e| ApiError(e.into()))?
        .map_err(ApiError)
}
async fn artifact_storage(State(app): State<App>) -> Result<Json<ArtifactStorageUsage>, ApiError> {
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || store.storage_usage().map(Json))
        .await
        .map_err(|e| ApiError(e.into()))?
        .map_err(ApiError)
}

async fn task_artifacts(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<ArtifactManifest>, ApiError> {
    let (store, reference) = run(app, move |scheduler| {
        let task = scheduler.task(&id)?;
        if task.artifact_retired_at_ms.is_some() {
            return Err(ArtifactsRetired.into());
        }
        Ok((
            scheduler.artifact_store(),
            task.artifacts
                .ok_or_else(|| anyhow::anyhow!("task has no retained artifacts"))?,
        ))
    })
    .await?
    .0;
    tokio::task::spawn_blocking(move || store.read_manifest(&reference).map(Json))
        .await
        .map_err(|e| ApiError(e.into()))?
        .map_err(ApiError)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactSize {
    bytes: u64,
}
async fn artifact_bytes(
    State(app): State<App>,
    Path(digest): Path<String>,
    Query(size): Query<ArtifactSize>,
) -> Result<Bytes, ApiError> {
    let store = run(app.clone(), |s| Ok(s.artifact_store())).await?.0;
    tokio::task::spawn_blocking(move || {
        store
            .get(&BlobRef {
                digest,
                bytes: size.bytes,
            })
            .map(Bytes::from)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)
}
async fn upload_artifact(
    State(app): State<App>,
    Path((task_id, generation, worker_id, incarnation, digest)): Path<(
        String,
        u64,
        String,
        String,
        String,
    )>,
    bytes: Bytes,
) -> Result<Json<BlobRef>, ApiError> {
    let key = LeaseKey {
        task_id,
        generation,
        worker_id,
        incarnation,
    };
    let key_before = key.clone();
    let store = run(app.clone(), move |s| {
        s.authorize_artifact_upload(&key_before, pvisor_core::unix_now_ms())?;
        Ok(s.artifact_store())
    })
    .await?
    .0;
    let upload_key = key.clone();
    let reference = tokio::task::spawn_blocking(move || {
        let expected = BlobRef {
            digest,
            bytes: bytes.len() as u64,
        };
        expected.validate()?;
        anyhow::ensure!(
            blake3::hash(&bytes).to_hex().as_str() == expected.digest,
            "uploaded artifact hash mismatch"
        );
        store.put_for_lease(&upload_key, &bytes)
    })
    .await
    .map_err(|e| ApiError(e.into()))?
    .map_err(ApiError)?;
    run(app, move |s| {
        s.authorize_artifact_upload(&key, pvisor_core::unix_now_ms())?;
        Ok(reference)
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
    #[tokio::test]
    async fn dropping_router_releases_writer_and_reaper_journal_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wal");
        let scheduler = Scheduler::open(&path, SchedulerConfig::default()).unwrap();
        let app = router(
            scheduler,
            "admin-test-0123456789".into(),
            "worker-test-0123456789".into(),
        )
        .unwrap();
        tokio::task::yield_now().await;
        drop(app);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(reopened) = Scheduler::open(&path, SchedulerConfig::default()) {
                    break reopened;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn uncertain_storage_commit_is_retryable_and_not_a_stale_result_ack() {
        assert_eq!(
            ApiError(dispatcher::Overloaded.into())
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
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
