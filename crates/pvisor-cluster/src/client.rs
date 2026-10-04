use crate::*;
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
        self.post("/v1/tasks", spec).await
    }
    pub async fn task(&self, id: &str) -> anyhow::Result<TaskRecord> {
        self.get(&format!("/v1/tasks/{id}")).await
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
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        static NEXT_DOWNLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let manifest = self.artifacts(id).await?;
        std::fs::create_dir_all(destination)?;
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
        Ok(manifest)
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
