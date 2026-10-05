use crate::*;
use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}
impl Client {
    pub fn new(base: &str, token: String) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(base)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "controller URL must be HTTP(S)"
        );
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: base.trim_end_matches('/').to_owned(),
            token,
        })
    }
    async fn post<T: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> anyhow::Result<R> {
        Ok(self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    async fn get<R: DeserializeOwned>(&self, path: &str) -> anyhow::Result<R> {
        Ok(self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn publish_environment(
        &self,
        template: &EnvironmentTemplate,
    ) -> anyhow::Result<EnvironmentRecord> {
        let record: EnvironmentRecord = self.post("/v1/environments", template).await?;
        crate::environment::validate(&record)?;
        anyhow::ensure!(
            record.template == *template,
            "published environment differs from request"
        );
        Ok(record)
    }
    pub async fn environment(&self, digest: &str) -> anyhow::Result<EnvironmentRecord> {
        BlobRef {
            digest: digest.into(),
            bytes: 0,
        }
        .validate()?;
        let record: EnvironmentRecord = self.get(&format!("/v1/environments/{digest}")).await?;
        crate::environment::validate(&record)?;
        anyhow::ensure!(
            record.digest == digest,
            "environment response differs from request"
        );
        Ok(record)
    }
    pub async fn submit(&self, spec: &TaskSpec) -> anyhow::Result<TaskRecord> {
        spec.validate_cpu_qos()?;
        spec.validate_gateway()?;
        spec.validate_artifacts()?;
        self.post("/v1/tasks", spec).await
    }
    pub async fn task(&self, id: &str) -> anyhow::Result<TaskRecord> {
        self.get(&format!("/v1/tasks/{id}")).await
    }
    pub async fn submit_graph(&self, spec: &TaskGraphSpec) -> anyhow::Result<TaskGraphRecord> {
        for node in &spec.nodes {
            node.task.validate_cpu_qos()?;
            node.task.validate_gateway()?;
            node.task.validate_artifacts()?;
        }
        let record: TaskGraphRecord = self.post("/v1/graphs", spec).await?;
        Self::validate_graph(&record, &spec.id)?;
        anyhow::ensure!(
            serde_json::to_value(&record.spec)? == serde_json::to_value(spec)?,
            "graph receipt differs from request"
        );
        Ok(record)
    }
    pub async fn graph(&self, id: &str) -> anyhow::Result<TaskGraphRecord> {
        let record = self.get(&format!("/v1/graphs/{id}")).await?;
        Self::validate_graph(&record, id)?;
        Ok(record)
    }
    pub async fn cancel_graph(&self, id: &str) -> anyhow::Result<TaskGraphRecord> {
        let record = self
            .post(&format!("/v1/graphs/{id}/cancel"), &serde_json::json!({}))
            .await?;
        Self::validate_graph(&record, id)?;
        Ok(record)
    }
    fn validate_graph(record: &TaskGraphRecord, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            record.spec.version == CLUSTER_VERSION
                && record.spec.id == id
                && !record.nodes.is_empty()
                && record.nodes.len() == record.spec.nodes.len()
                && record
                    .nodes
                    .iter()
                    .zip(&record.spec.nodes)
                    .all(|(state, node)| state.task_id == node.task.id),
            "graph response differs from request"
        );
        Ok(())
    }
    pub async fn fork_execution(
        &self,
        source: &str,
        request: &ExecutionForkRequest,
    ) -> anyhow::Result<ExecutionForkRecord> {
        let record: ExecutionForkRecord = self
            .post(&format!("/v1/tasks/{source}/forks"), request)
            .await?;
        anyhow::ensure!(
            record.version == CLUSTER_VERSION
                && record.source_task_id == source
                && record.source_key.task_id == source
                && record.request == *request,
            "fork receipt differs from request"
        );
        record.checkpoint.validate()?;
        Ok(record)
    }
    pub async fn execution_fork(
        &self,
        source: &str,
        request_id: &str,
    ) -> anyhow::Result<ExecutionForkRecord> {
        let record: ExecutionForkRecord = self
            .get(&format!("/v1/tasks/{source}/forks/{request_id}"))
            .await?;
        anyhow::ensure!(
            record.version == CLUSTER_VERSION
                && record.source_task_id == source
                && record.source_key.task_id == source
                && record.request.request_id == request_id,
            "fork receipt differs from query"
        );
        record.checkpoint.validate()?;
        Ok(record)
    }
    pub async fn request_live_fork(
        &self,
        source: &str,
        request: &ExecutionForkRequest,
    ) -> anyhow::Result<LiveForkRecord> {
        let record: LiveForkRecord = self
            .post(&format!("/v1/tasks/{source}/live-forks"), request)
            .await?;
        Self::validate_live_fork(&record, source, &request.request_id)?;
        anyhow::ensure!(record.request == *request, "live fork differs from request");
        Ok(record)
    }
    pub async fn live_fork(
        &self,
        source: &str,
        request_id: &str,
    ) -> anyhow::Result<LiveForkRecord> {
        let record: LiveForkRecord = self
            .get(&format!("/v1/tasks/{source}/live-forks/{request_id}"))
            .await?;
        Self::validate_live_fork(&record, source, request_id)?;
        Ok(record)
    }
    fn validate_live_fork(
        record: &LiveForkRecord,
        source: &str,
        request_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            record.version == CLUSTER_VERSION
                && record.request.version == CLUSTER_VERSION
                && record.source_task_id == source
                && record.source_key.task_id == source
                && record.request.request_id == request_id,
            "live fork differs from query"
        );
        anyhow::ensure!(
            match record.phase {
                LiveForkPhase::Capturing =>
                    record.fork.is_none()
                        && record.error.is_none()
                        && record.completed_at_ms.is_none(),
                LiveForkPhase::Ready =>
                    record.fork.is_some()
                        && record.error.is_none()
                        && record.completed_at_ms.is_some(),
                LiveForkPhase::Failed =>
                    record.fork.is_none()
                        && record.error.is_some()
                        && record.completed_at_ms.is_some(),
            },
            "inconsistent live fork phase"
        );
        if let Some(fork) = &record.fork {
            anyhow::ensure!(
                fork.version == CLUSTER_VERSION
                    && fork.source_task_id == source
                    && fork.source_key == record.source_key
                    && fork.request == record.request
                    && Some(fork.created_at_ms) == record.completed_at_ms,
                "live fork receipt differs from capture"
            );
            fork.checkpoint.validate()?;
        }
        Ok(())
    }
    pub async fn cancel(&self, id: &str) -> anyhow::Result<TaskRecord> {
        self.post(&format!("/v1/tasks/{id}/cancel"), &()).await
    }
    pub async fn workers(&self) -> anyhow::Result<Vec<WorkerRecord>> {
        self.get("/v1/workers").await
    }
    pub async fn register(&self, spec: &WorkerRegistration) -> anyhow::Result<WorkerRecord> {
        self.post("/v1/workers/register", spec).await
    }
    pub async fn poll(&self, request: &PollRequest) -> anyhow::Result<PollResponse> {
        self.post("/v1/workers/poll", request).await
    }
    pub async fn report_cpu(&self, request: &CpuReportRequest) -> anyhow::Result<CpuReportReceipt> {
        let receipt: CpuReportReceipt = self.post("/v1/workers/cpu", request).await?;
        let mut expected: Vec<_> = request.samples.iter().map(|s| &s.key).collect();
        let mut actual: Vec<_> = receipt.accepted.iter().chain(&receipt.ignored).collect();
        let order = |a: &&LeaseKey, b: &&LeaseKey| {
            (&a.task_id, &a.worker_id, &a.incarnation, a.generation).cmp(&(
                &b.task_id,
                &b.worker_id,
                &b.incarnation,
                b.generation,
            ))
        };
        expected.sort_by(order);
        actual.sort_by(order);
        anyhow::ensure!(
            expected == actual,
            "CPU receipt does not match reported leases"
        );
        Ok(receipt)
    }
    pub async fn report_memory(
        &self,
        request: &MemoryReportRequest,
    ) -> anyhow::Result<MemoryReportReceipt> {
        let receipt: MemoryReportReceipt = self.post("/v1/workers/memory", request).await?;
        let mut expected: Vec<_> = request.samples.iter().map(|s| &s.key).collect();
        let mut actual: Vec<_> = receipt.accepted.iter().chain(&receipt.ignored).collect();
        let order = |a: &&LeaseKey, b: &&LeaseKey| {
            (&a.task_id, &a.worker_id, &a.incarnation, a.generation).cmp(&(
                &b.task_id,
                &b.worker_id,
                &b.incarnation,
                b.generation,
            ))
        };
        expected.sort_by(order);
        actual.sort_by(order);
        anyhow::ensure!(
            expected == actual,
            "memory receipt does not match reported leases"
        );
        Ok(receipt)
    }
    pub async fn report_node_memory(
        &self,
        request: &NodeMemoryReportRequest,
    ) -> anyhow::Result<NodeMemoryReportReceipt> {
        let receipt: NodeMemoryReportReceipt =
            self.post("/v1/workers/node-memory", request).await?;
        anyhow::ensure!(
            receipt.worker_id == request.worker_id
                && receipt.incarnation == request.incarnation
                && receipt.sequence == request.sequence,
            "node memory receipt differs from request"
        );
        Ok(receipt)
    }
    pub async fn plan_artifact_gc(
        &self,
        request: &ArtifactGcRequest,
    ) -> anyhow::Result<ArtifactGcPlan> {
        request.validate()?;
        let plan: ArtifactGcPlan = self.post("/v1/artifact-storage/gc/plan", request).await?;
        anyhow::ensure!(
            plan.version == CLUSTER_VERSION
                && plan.retire.len() <= 256
                && plan.objects.len() <= request.max_objects as usize,
            "invalid artifact GC plan"
        );
        BlobRef {
            digest: plan.id.clone(),
            bytes: 0,
        }
        .validate()?;
        for object in &plan.objects {
            object.validate()?;
        }
        anyhow::ensure!(
            plan.bytes == plan.objects.iter().map(|o| o.bytes).sum::<u64>(),
            "invalid artifact GC byte count"
        );
        Ok(plan)
    }
    pub async fn apply_artifact_gc(&self, plan_id: &str) -> anyhow::Result<ArtifactGcReport> {
        BlobRef {
            digest: plan_id.into(),
            bytes: 0,
        }
        .validate()?;
        let report: ArtifactGcReport = self
            .post(
                "/v1/artifact-storage/gc/apply",
                &ArtifactGcApply {
                    version: CLUSTER_VERSION,
                    plan_id: plan_id.into(),
                },
            )
            .await?;
        anyhow::ensure!(
            report.version == CLUSTER_VERSION && report.plan_id == plan_id,
            "invalid artifact GC receipt"
        );
        Ok(report)
    }
    pub async fn begin_artifact_download(&self, id: &str) -> anyhow::Result<ArtifactDownload> {
        let download: ArtifactDownload = self
            .post(
                &format!("/v1/tasks/{}/artifact-downloads", id),
                &serde_json::json!({}),
            )
            .await?;
        Self::validate_download(&download, id)?;
        Ok(download)
    }
    pub async fn renew_artifact_download(
        &self,
        download: &ArtifactDownload,
    ) -> anyhow::Result<ArtifactDownload> {
        let next: ArtifactDownload = self
            .post(
                &format!("/v1/artifact-downloads/{}/renew", download.id),
                &serde_json::json!({}),
            )
            .await?;
        Self::validate_download(&next, &download.manifest.key.task_id)?;
        anyhow::ensure!(
            next.id == download.id
                && next.reference == download.reference
                && next.manifest == download.manifest,
            "artifact download changed during renewal"
        );
        Ok(next)
    }
    pub async fn release_artifact_download(&self, id: &str) -> anyhow::Result<()> {
        BlobRef {
            digest: id.into(),
            bytes: 0,
        }
        .validate()?;
        let _: serde_json::Value = self
            .post(
                &format!("/v1/artifact-downloads/{id}/release"),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }
    fn validate_download(download: &ArtifactDownload, task_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            download.version == CLUSTER_VERSION && download.manifest.key.task_id == task_id,
            "invalid artifact download receipt"
        );
        BlobRef {
            digest: download.id.clone(),
            bytes: 0,
        }
        .validate()?;
        download.reference.validate()?;
        download.manifest.validate()?;
        Ok(())
    }
    pub async fn update_artifact_storage(
        &self,
        limits: &ArtifactStorageLimits,
    ) -> anyhow::Result<ArtifactStorageUsage> {
        limits.validate()?;
        let usage: ArtifactStorageUsage = self.post("/v1/artifact-storage/limits", limits).await?;
        anyhow::ensure!(
            usage.version == CLUSTER_VERSION && usage.limits == *limits,
            "artifact storage policy acknowledgement differs from request"
        );
        Ok(usage)
    }
    pub async fn artifact_storage(&self) -> anyhow::Result<ArtifactStorageUsage> {
        let usage: ArtifactStorageUsage = self.get("/v1/artifact-storage").await?;
        anyhow::ensure!(
            usage.version == CLUSTER_VERSION,
            "unsupported artifact storage protocol"
        );
        usage.limits.validate()?;
        Ok(usage)
    }
    pub async fn native_done(&self, request: &NativeDone) -> anyhow::Result<NativeDoneReceipt> {
        let receipt: NativeDoneReceipt = self.post("/v1/workers/native-done", request).await?;
        anyhow::ensure!(
            receipt.version == ARTIFACT_DELIVERY_VERSION
                && receipt.key == request.key
                && receipt.reserved
                    == artifact_delivery_reservation(receipt.reserved)
                        .context("invalid artifact delivery reservation")?,
            "native handoff acknowledgement differs from request"
        );
        Ok(receipt)
    }
    pub async fn complete(&self, completion: &Completion) -> anyhow::Result<TaskRecord> {
        let task: TaskRecord = self.post("/v1/workers/complete", completion).await?;
        anyhow::ensure!(
            task.phase.terminal()
                && task.phase != TaskPhase::Lost
                && task.spec.id == completion.key.task_id
                && task
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.key == completion.key)
                && serde_json::to_value(&task.result)? == serde_json::to_value(&completion.result)?
                && task.error == completion.error
                && task.artifacts == completion.artifacts
                && task.artifact_error == completion.artifact_error,
            "completion acknowledgement does not match delivered evidence"
        );
        Ok(task)
    }
    pub async fn recover(&self, request: &RecoveryRequest) -> anyhow::Result<RecoveryResponse> {
        self.post("/v1/workers/recover", request).await
    }
    pub async fn upload_artifact(&self, key: &LeaseKey, bytes: Vec<u8>) -> anyhow::Result<BlobRef> {
        anyhow::ensure!(
            bytes.len() <= ARTIFACT_CHUNK_BYTES,
            "artifact chunk too large"
        );
        let expected = BlobRef {
            digest: blake3::hash(&bytes).to_hex().to_string(),
            bytes: bytes.len() as u64,
        };
        let path = format!(
            "/v1/workers/artifacts/{}/{}/{}/{}/{}",
            key.task_id, key.generation, key.worker_id, key.incarnation, expected.digest
        );
        let received: BlobRef = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .body(bytes)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(
            received == expected,
            "artifact acknowledgement does not match upload"
        );
        Ok(received)
    }
    pub async fn artifacts(&self, id: &str) -> anyhow::Result<ArtifactManifest> {
        let manifest: ArtifactManifest = self.get(&format!("/v1/tasks/{id}/artifacts")).await?;
        manifest.validate()?;
        anyhow::ensure!(
            manifest.key.task_id == id,
            "artifact manifest belongs to another task"
        );
        Ok(manifest)
    }
    pub async fn download_artifacts(
        &self,
        id: &str,
        destination: &std::path::Path,
    ) -> anyhow::Result<ArtifactManifest> {
        let mut lease = match self.begin_artifact_download(id).await {
            Ok(download) => Some(download),
            Err(error)
                if error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    == Some(reqwest::StatusCode::NOT_FOUND) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let manifest = match &lease {
            Some(download) => download.manifest.clone(),
            None => self.artifacts(id).await?,
        };
        let result = self
            .download_pinned(&manifest, destination, &mut lease)
            .await;
        if let Some(download) = &lease {
            let _ = self.release_artifact_download(&download.id).await;
        }
        result?;
        Ok(manifest)
    }
    async fn download_pinned(
        &self,
        manifest: &ArtifactManifest,
        destination: &std::path::Path,
        lease: &mut Option<ArtifactDownload>,
    ) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        static NEXT_DOWNLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut renewed = std::time::Instant::now();
        std::fs::create_dir_all(destination)?;
        for artifact in &manifest.files {
            anyhow::ensure!(
                std::fs::symlink_metadata(destination.join(&artifact.name))
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "artifact destination already exists or is inaccessible"
            );
        }
        let temporary = destination.join(format!(
            ".pvisor-download-{}-{}",
            std::process::id(),
            NEXT_DOWNLOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&temporary)?;
        let downloaded = async {
            for artifact in &manifest.files {
                let path = temporary.join(&artifact.name);
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)?;
                let mut hash = blake3::Hasher::new();
                for chunk in &artifact.chunks {
                    if renewed.elapsed() >= Duration::from_secs(30) {
                        if let Some(download) = lease.as_ref() {
                            *lease = Some(self.renew_artifact_download(download).await?);
                        }
                        renewed = std::time::Instant::now();
                    }
                    let bytes = self.artifact_bytes(chunk).await?;
                    hash.update(&bytes);
                    file.write_all(&bytes)?;
                }
                anyhow::ensure!(
                    hash.finalize().to_hex().as_str() == artifact.digest,
                    "whole file download integrity check failed"
                );
                file.sync_all()?;
            }
            for artifact in &manifest.files {
                // Publish verified files atomically without replacing existing user data.
                std::fs::hard_link(
                    temporary.join(&artifact.name),
                    destination.join(&artifact.name),
                )?;
            }
            std::fs::File::open(destination)?.sync_all()?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        for artifact in &manifest.files {
            let _ = std::fs::remove_file(temporary.join(&artifact.name));
        }
        let _ = std::fs::remove_dir(&temporary);
        downloaded?;
        Ok(())
    }
    pub async fn artifact_bytes(&self, reference: &BlobRef) -> anyhow::Result<Vec<u8>> {
        reference.validate()?;
        let response = self
            .http
            .get(format!("{}/v1/artifacts/{}", self.base, reference.digest))
            .bearer_auth(&self.token)
            .query(&[("bytes", reference.bytes)])
            .send()
            .await?
            .error_for_status()?;
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= ARTIFACT_CHUNK_BYTES as u64),
            "artifact response exceeds chunk limit"
        );
        let mut response = response;
        let mut bytes = Vec::with_capacity(reference.bytes as usize);
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                bytes.len() + chunk.len() <= ARTIFACT_CHUNK_BYTES,
                "artifact response exceeds chunk limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        anyhow::ensure!(
            bytes.len() as u64 == reference.bytes
                && blake3::hash(&bytes).to_hex().as_str() == reference.digest,
            "artifact download integrity check failed"
        );
        Ok(bytes)
    }
    pub async fn decline(&self, rejection: &AdmissionRejection) -> anyhow::Result<TaskRecord> {
        self.post("/v1/workers/decline", rejection).await
    }
    pub async fn drain(&self, id: &str, draining: bool) -> anyhow::Result<serde_json::Value> {
        self.post(
            &format!("/v1/workers/{id}/drain"),
            &serde_json::json!({"draining":draining}),
        )
        .await
    }
    pub async fn control(
        &self,
        id: &str,
        request: &ControlRequest,
    ) -> anyhow::Result<ControlRecord> {
        self.post(&format!("/v1/tasks/{id}/control"), request).await
    }
    pub async fn acknowledge_control(
        &self,
        acknowledgement: &ControlAcknowledgement,
    ) -> anyhow::Result<ControlRecord> {
        self.post("/v1/workers/control-ack", acknowledgement).await
    }
}
