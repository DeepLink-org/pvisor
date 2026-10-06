//! Single-node ownership: durable sandbox intentions, bounded admission and
//! runtime reconciliation. No Worker placement, DAG or distributed lease.
pub mod api;
mod models;
mod store;
#[cfg(test)]
mod tests;

use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
pub use models::{ApiError, CreateRequest, RenewRequest, Sandbox, SandboxStatus};
use models::{Record, Registry};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::Mutex;

pub const OPENSANDBOX_VERSION: &str = "1.1.0";
pub const OPENSANDBOX_COMMIT: &str = "b1a29cf93a823a95913f7943010febb3f29de05c";

pub struct Config {
    pub state_dir: PathBuf,
    pub api_key: String,
    /// Externally reachable authority (host:port), without a scheme or path.
    pub public_endpoint: String,
    pub max_sandboxes: usize,
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub max_timeout_seconds: u64,
}

pub struct Daemon {
    config: Config,
    store: Arc<store::Store>,
    registry: Mutex<Registry>,
    commits: Mutex<()>,
    operations: Mutex<BTreeMap<String, Weak<Mutex<()>>>>,
    runtime: Arc<dyn Runtime>,
    proxy: reqwest::Client,
    storage_failed: std::sync::atomic::AtomicBool,
}

impl Daemon {
    /// Factory receives the durable node owner. Native runtimes must fence every
    /// operation against this owner, including after a daemon restart.
    pub async fn open(
        config: Config,
        factory: impl FnOnce(String) -> anyhow::Result<Arc<dyn Runtime>>,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.api_key.len() >= 32,
            "API key must contain at least 32 bytes"
        );
        anyhow::ensure!(
            config.max_sandboxes > 0
                && config.cpu_millis > 0
                && config.memory_bytes > 0
                && (60..=365 * 24 * 60 * 60).contains(&config.max_timeout_seconds),
            "invalid node capacity or timeout limit"
        );
        let authority = url::Url::parse(&format!("http://{}", config.public_endpoint))?;
        anyhow::ensure!(
            authority.host_str().is_some()
                && authority.username().is_empty()
                && authority.password().is_none()
                && authority.path() == "/"
                && authority.query().is_none()
                && authority.fragment().is_none(),
            "public endpoint must be a host[:port] authority"
        );
        let (store, mut registry) = store::Store::open(&config.state_dir)?;
        let runtime = factory(registry.owner.clone())?;
        // The native factory must accept the legacy owner before migration can
        // replace its occupied snapshot with an incremental-store header.
        store.initialize(&mut registry)?;
        runtime.preflight().await?;
        let daemon = Arc::new(Self {
            config,
            store: Arc::new(store),
            registry: Mutex::new(registry),
            commits: Mutex::new(()),
            operations: Mutex::new(BTreeMap::new()),
            storage_failed: std::sync::atomic::AtomicBool::new(false),
            runtime,
            proxy: reqwest::Client::builder()
                .no_proxy()
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()?,
        });
        // Never assume a stored Running state proves a native execution survived.
        for sandbox in daemon.list().await? {
            let outcome = if sandbox.status.state == "Stopping" {
                daemon.delete(&sandbox.id).await.map(|_| ())
            } else {
                daemon.get(&sandbox.id).await.map(|_| ())
            };
            if let Err(error) = outcome {
                eprintln!(
                    "sandbox {} reconciliation deferred: {}",
                    sandbox.id, error.message
                );
            }
        }
        Ok(daemon)
    }

    pub fn authorize(&self, headers: &axum::http::HeaderMap) -> bool {
        if headers.get_all("OPEN-SANDBOX-API-KEY").iter().count() != 1 {
            return false;
        }
        headers
            .get("OPEN-SANDBOX-API-KEY")
            .is_some_and(|key| secret_eq(key.as_bytes(), self.config.api_key.as_bytes()))
    }

    pub async fn authorize_proxy(&self, id: &str, headers: &axum::http::HeaderMap) -> bool {
        if headers.get_all("OPEN-SANDBOX-API-KEY").iter().count() > 1
            || headers.get_all("X-PVISOR-SANDBOX-TOKEN").iter().count() > 1
        {
            return false;
        }
        if self.authorize(headers) {
            return true;
        }
        let registry = self.registry.lock().await;
        registry.sandboxes.get(id).is_some_and(|record| {
            headers
                .get("X-PVISOR-SANDBOX-TOKEN")
                .is_some_and(|token| secret_eq(token.as_bytes(), record.endpoint_token.as_bytes()))
        })
    }

    pub fn proxy_client(&self) -> &reqwest::Client {
        &self.proxy
    }

    async fn update<T>(
        &self,
        id: &str,
        change: impl FnOnce(&Registry) -> Result<(Option<Record>, T), ApiError>,
    ) -> Result<T, ApiError> {
        // Serialize admission and durable mutations, but let readers observe
        // the last committed inventory while disk I/O is in flight.
        let _commit = self.commits.lock().await;
        self.ensure_storage()?;
        let (record, result) = {
            let registry = self.registry.lock().await;
            change(&registry)?
        };
        let store = self.store.clone();
        let key = id.to_owned();
        let outcome = tokio::task::spawn_blocking(move || {
            store.commit_record(&key, record.as_ref())?;
            Ok::<_, anyhow::Error>(record)
        })
        .await;
        let record = match outcome {
            Ok(Ok(record)) => record,
            _ => {
                // Rename/unlink may have committed before directory sync failed.
                // Never overwrite that unknown state from an older memory view.
                self.storage_failed
                    .store(true, std::sync::atomic::Ordering::Release);
                return Err(ApiError::new(
                    503,
                    "STORAGE_UNAVAILABLE",
                    "registry commit uncertain; restart and reconcile required",
                ));
            }
        };
        let mut registry = self.registry.lock().await;
        if let Some(record) = record {
            registry.sandboxes.insert(id.to_owned(), record);
        } else {
            registry.sandboxes.remove(id);
        }
        Ok(result)
    }

    async fn operation(&self, id: &str) -> Result<Arc<Mutex<()>>, ApiError> {
        self.record(id).await?;
        let mut operations = self.operations.lock().await;
        operations.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = operations.get(id).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        operations.insert(id.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }

    pub async fn proxy_guard(
        &self,
        id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, ApiError> {
        Ok(self.operation(id).await?.lock_owned().await)
    }

    fn ensure_storage(&self) -> Result<(), ApiError> {
        if self
            .storage_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Err(ApiError::new(
                503,
                "STORAGE_UNAVAILABLE",
                "registry commit uncertain; restart required",
            ))
        } else {
            Ok(())
        }
    }

    async fn record(&self, id: &str) -> Result<Record, ApiError> {
        self.ensure_storage()?;
        self.registry
            .lock()
            .await
            .sandboxes
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::new(404, "SANDBOX_NOT_FOUND", "sandbox does not exist"))
    }

    async fn transition(
        &self,
        id: &str,
        state: &str,
        message: Option<String>,
    ) -> Result<Sandbox, ApiError> {
        self.update(id, |registry| {
            let mut record =
                registry.sandboxes.get(id).cloned().ok_or_else(|| {
                    ApiError::new(404, "SANDBOX_NOT_FOUND", "sandbox does not exist")
                })?;
            record.sandbox.status = SandboxStatus {
                state: state.to_owned(),
                message,
                last_transition_at: Some(Utc::now()),
            };
            let sandbox = record.sandbox.clone();
            Ok((Some(record), sandbox))
        })
        .await
    }

    /// A disconnected HTTP client must not cancel an accepted native operation.
    pub async fn create(self: &Arc<Self>, request: CreateRequest) -> Result<Sandbox, ApiError> {
        let this = self.clone();
        tokio::spawn(async move { this.create_owned(request).await })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
    }

    async fn create_owned(&self, request: CreateRequest) -> Result<Sandbox, ApiError> {
        let validated = request.validate(self.config.max_timeout_seconds)?;
        let id = format!("sb-{}", uuid::Uuid::new_v4());
        let now = Utc::now();
        let sandbox = Sandbox {
            id: id.clone(),
            status: SandboxStatus {
                state: "Pending".into(),
                message: None,
                last_transition_at: Some(now),
            },
            created_at: now,
            expires_at: validated
                .timeout
                .map(|seconds| now + ChronoDuration::seconds(seconds as i64)),
            image: validated.image.clone(),
            entrypoint: validated.entrypoint.clone(),
            metadata: validated.metadata.clone(),
        };
        let record = Record {
            sandbox,
            env: validated.env,
            cpu_millis: validated.cpu_millis,
            memory_bytes: validated.memory_bytes,
            endpoint_token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        };
        let spec = RuntimeSpec {
            id: id.clone(),
            image: record.sandbox.image.uri.clone(),
            entrypoint: record.sandbox.entrypoint.clone(),
            env: record.env.clone(),
            cpu_millis: record.cpu_millis,
            memory_bytes: record.memory_bytes,
        };
        crate::runtime::validate_spec(&spec).map_err(|_| {
            ApiError::bad_request("invalid runtime image, argv, environment or resource limits")
        })?;
        // Hold the lifecycle gate before exposing the Pending intention, so a
        // concurrent GET/DELETE cannot observe or remove a half-created runtime.
        let gate = Arc::new(Mutex::new(()));
        let _operation = gate.lock().await;
        {
            let mut operations = self.operations.lock().await;
            operations.retain(|_, lock| lock.strong_count() > 0);
            operations.insert(id.clone(), Arc::downgrade(&gate));
        }
        self.update(&id, |registry| {
            let cpu = registry
                .sandboxes
                .values()
                .try_fold(0u64, |n, r| n.checked_add(r.cpu_millis));
            let memory = registry
                .sandboxes
                .values()
                .try_fold(0u64, |n, r| n.checked_add(r.memory_bytes));
            if registry.sandboxes.len() >= self.config.max_sandboxes
                || cpu
                    .and_then(|n| n.checked_add(record.cpu_millis))
                    .is_none_or(|n| n > self.config.cpu_millis)
                || memory
                    .and_then(|n| n.checked_add(record.memory_bytes))
                    .is_none_or(|n| n > self.config.memory_bytes)
            {
                return Err(ApiError::new(
                    429,
                    "CAPACITY_EXCEEDED",
                    "node capacity exhausted",
                ));
            }
            Ok((Some(record), ()))
        })
        .await?;
        if let Err(error) = self.runtime.create(&spec).await {
            if error
                .downcast_ref::<crate::runtime::CreateError>()
                .is_some_and(|error| !error.requires_reconciliation())
            {
                self.update(&id, |_| Ok((None, ()))).await?;
                return Err(ApiError::new(
                    500,
                    "SANDBOX_CREATE_FAILED",
                    "runtime creation failed; cleanup confirmed",
                ));
            }
            // Preserve the intention and reservation even if cleanup was uncertain.
            // The ID is surfaced so clients can query/delete rather than resubmit blindly.
            self.transition(
                &id,
                "Failed",
                Some("runtime creation failed; cleanup required".into()),
            )
            .await?;
            eprintln!("sandbox {id} creation failed: {error:#}");
            return Err(ApiError::new(
                500,
                "SANDBOX_CREATE_FAILED",
                format!("sandbox {id} failed; query or delete this ID"),
            ));
        }
        self.transition(&id, "Running", None).await
    }

    pub async fn list(&self) -> Result<Vec<Sandbox>, ApiError> {
        self.ensure_storage()?;
        Ok(self
            .registry
            .lock()
            .await
            .sandboxes
            .values()
            .map(|record| record.sandbox.clone())
            .collect())
    }

    pub async fn get(self: &Arc<Self>, id: &str) -> Result<Sandbox, ApiError> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move { this.get_owned(&id).await })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
    }

    async fn get_owned(&self, id: &str) -> Result<Sandbox, ApiError> {
        let gate = self.operation(id).await?;
        let _operation = gate.lock().await;
        let record = self.record(id).await?;
        if record.sandbox.status.state == "Stopping" {
            return Ok(record.sandbox);
        }
        let state = self.runtime.inspect(id).await.map_err(|e| {
            eprintln!("sandbox {id} inspection failed: {e:#}");
            ApiError::new(
                503,
                "RUNTIME_UNAVAILABLE",
                "cannot establish native sandbox state",
            )
        })?;
        let state = match state {
            RuntimeState::Running => "Running",
            RuntimeState::Paused => "Paused",
            RuntimeState::Stopped => "Terminated",
            RuntimeState::Missing => "Failed",
        };
        if record.sandbox.status.state != state {
            self.transition(
                id,
                state,
                (state == "Failed").then(|| {
                    "native sandbox missing; explicit deletion releases reservation".into()
                }),
            )
            .await
        } else {
            Ok(record.sandbox)
        }
    }

    pub async fn pause(self: &Arc<Self>, id: &str) -> Result<(), ApiError> {
        self.control(id, true).await
    }
    pub async fn resume(self: &Arc<Self>, id: &str) -> Result<(), ApiError> {
        self.control(id, false).await
    }

    async fn control(self: &Arc<Self>, id: &str, pause: bool) -> Result<(), ApiError> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let gate = this.operation(&id).await?;
            let _operation = gate.lock().await;
            let record = this.record(&id).await?;
            if !matches!(
                record.sandbox.status.state.as_str(),
                "Running" | "Paused" | "Pausing" | "Resuming"
            ) {
                return Err(ApiError::new(
                    409,
                    "INVALID_SANDBOX_STATE",
                    "sandbox lifecycle does not permit control",
                ));
            }
            check_expiration(record.sandbox.expires_at)?;
            let current = this
                .runtime
                .inspect(&id)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
            let desired = if pause {
                RuntimeState::Paused
            } else {
                RuntimeState::Running
            };
            if current == desired {
                return Ok(());
            }
            let allowed = if pause {
                RuntimeState::Running
            } else {
                RuntimeState::Paused
            };
            if current != allowed {
                return Err(ApiError::new(
                    409,
                    "INVALID_SANDBOX_STATE",
                    "native state does not permit this operation",
                ));
            }
            this.transition(&id, if pause { "Pausing" } else { "Resuming" }, None)
                .await?;
            let outcome = if pause {
                this.runtime.pause(&id).await
            } else {
                this.runtime.resume(&id).await
            };
            outcome.map_err(|e| ApiError::new(503, "RUNTIME_UNAVAILABLE", e.to_string()))?;
            let observed = this
                .runtime
                .inspect(&id)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
            if observed != desired {
                return Err(ApiError::new(
                    503,
                    "RUNTIME_UNAVAILABLE",
                    "native control not confirmed",
                ));
            }
            this.transition(&id, if pause { "Paused" } else { "Running" }, None)
                .await?;
            Ok(())
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }

    pub async fn delete(self: &Arc<Self>, id: &str) -> Result<(), ApiError> {
        self.delete_conditionally(id, false).await
    }

    async fn delete_conditionally(
        self: &Arc<Self>,
        id: &str,
        expired_only: bool,
    ) -> Result<(), ApiError> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let gate = this.operation(&id).await?;
            let _operation = gate.lock().await;
            let record = this.record(&id).await?;
            // Recheck under the same lock used by renew, not from a stale scan.
            if expired_only
                && record.sandbox.status.state != "Stopping"
                && !record
                    .sandbox
                    .expires_at
                    .is_some_and(|deadline| deadline <= Utc::now())
            {
                return Ok(());
            }
            this.transition(&id, "Stopping", None).await?;
            this.runtime.delete(&id).await.map_err(|_| {
                ApiError::new(
                    503,
                    "RUNTIME_UNAVAILABLE",
                    "native deletion failed; cleanup remains pending",
                )
            })?;
            if this.runtime.inspect(&id).await.map_err(|_| {
                ApiError::new(503, "RUNTIME_UNAVAILABLE", "native deletion state unknown")
            })? != RuntimeState::Missing
            {
                return Err(ApiError::new(
                    503,
                    "RUNTIME_UNAVAILABLE",
                    "native deletion not confirmed",
                ));
            }
            this.update(&id, |_| Ok((None, ()))).await?;
            Ok(())
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }

    pub async fn renew(
        self: &Arc<Self>,
        id: &str,
        request: RenewRequest,
    ) -> Result<RenewRequest, ApiError> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move { this.renew_owned(&id, request).await })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
    }

    async fn renew_owned(&self, id: &str, request: RenewRequest) -> Result<RenewRequest, ApiError> {
        let gate = self.operation(id).await?;
        let _operation = gate.lock().await;
        let now = Utc::now();
        if request.expires_at <= now
            || request.expires_at
                > now + ChronoDuration::seconds(self.config.max_timeout_seconds as i64)
        {
            return Err(ApiError::bad_request(
                "expiresAt must be future and within the configured timeout limit",
            ));
        }
        self.update(id, |registry| {
            let mut record =
                registry.sandboxes.get(id).cloned().ok_or_else(|| {
                    ApiError::new(404, "SANDBOX_NOT_FOUND", "sandbox does not exist")
                })?;
            check_expiration(record.sandbox.expires_at)?;
            if matches!(
                record.sandbox.status.state.as_str(),
                "Stopping" | "Terminated" | "Failed"
            ) {
                return Err(ApiError::new(
                    409,
                    "INVALID_SANDBOX_STATE",
                    "sandbox cannot be renewed",
                ));
            }
            if record
                .sandbox
                .expires_at
                .is_some_and(|old| request.expires_at <= old)
            {
                return Err(ApiError::bad_request(
                    "expiresAt must extend the current expiration",
                ));
            }
            record.sandbox.expires_at = Some(request.expires_at);
            Ok((Some(record), request))
        })
        .await
    }

    pub async fn endpoint(
        &self,
        id: &str,
        port: u16,
        server_proxy: bool,
    ) -> Result<serde_json::Value, ApiError> {
        // Verify a real publication exists, but don't expose native loopback ports.
        self.upstream(id, port).await?;
        let record = self.record(id).await?;
        let address = format!(
            "{}/v1/sandboxes/{id}/proxy/{port}",
            self.config.public_endpoint
        );
        if server_proxy {
            Ok(serde_json::json!({"endpoint": address}))
        } else {
            Ok(serde_json::json!({"endpoint": address,
            "headers": {"X-PVISOR-SANDBOX-TOKEN": record.endpoint_token}}))
        }
    }

    pub async fn upstream(&self, id: &str, port: u16) -> Result<String, ApiError> {
        if !matches!(port, 44772 | 18080) {
            return Err(ApiError::new(
                501,
                "NOT_SUPPORTED",
                "only execd and egress endpoints are supported",
            ));
        }
        let record = self.record(id).await?;
        check_expiration(record.sandbox.expires_at)?;
        if record.sandbox.status.state != "Running" {
            return Err(ApiError::new(
                409,
                "INVALID_SANDBOX_STATE",
                "sandbox is not running",
            ));
        }
        self.runtime
            .endpoint(id, port)
            .await
            .map_err(|e| ApiError::new(503, "ENDPOINT_UNAVAILABLE", e.to_string()))
    }

    /// Bounded node inventory, not a distributed scheduling or lease loop.
    pub fn start_maintenance(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut jobs = tokio::task::JoinSet::new();
            let mut in_flight = std::collections::BTreeSet::new();
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let Some(this) = weak.upgrade() else { return; };
                        let sandboxes = match this.list().await { Ok(items) => items, Err(_) => continue };
                        for sandbox in sandboxes {
                            if jobs.len() >= 8 { break; }
                            if !in_flight.contains(&sandbox.id)
                                && (sandbox.expires_at.is_some_and(|deadline| deadline <= Utc::now())
                                    || sandbox.status.state == "Stopping") {
                                let id = sandbox.id;
                                in_flight.insert(id.clone());
                                let this = this.clone();
                                jobs.spawn(async move {
                                    let outcome = this.delete_conditionally(&id, true).await;
                                    (id, outcome)
                                });
                            }
                        }
                    }
                    Some(completed) = jobs.join_next(), if !jobs.is_empty() => {
                        match completed {
                            Ok((id, outcome)) => {
                                in_flight.remove(&id);
                                if let Err(error) = outcome { eprintln!("sandbox {id} cleanup deferred: {}", error.message); }
                            }
                            Err(error) => { eprintln!("cleanup task failed: {error}"); in_flight.clear(); }
                        }
                    }
                }
            }
        })
    }
}

fn check_expiration(deadline: Option<DateTime<Utc>>) -> Result<(), ApiError> {
    if deadline.is_some_and(|deadline| deadline <= Utc::now()) {
        Err(ApiError::new(409, "SANDBOX_EXPIRED", "sandbox has expired"))
    } else {
        Ok(())
    }
}

fn secret_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
