//! Adapter from the existing Gateway delivery barrier to Attempt VM ownership.

use super::wait_coordinator::{WaitCoordinator, WaitEntry};
use async_trait::async_trait;
use pvisor_core::{AttemptId, ModelCallRequest};
use pvisor_gateway::model_wait::{ModelWait, ModelWaitLifecycle};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct NativeModelWait {
    pub coordinator: Arc<WaitCoordinator>,
    pub attempt_id: AttemptId,
}

impl ModelWaitLifecycle for NativeModelWait {
    fn reserve_download(
        &self,
    ) -> anyhow::Result<Option<Box<dyn pvisor_overlaynet::server::DownloadWait>>> {
        if !self.coordinator.downloads_enabled() {
            return Ok(None);
        }
        Ok(Some(Box::new(NativeDownload {
            owner: self.coordinator.clone(),
            entry: None,
            passive: false,
        })))
    }
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>> {
        // The coordinator was bound by Session, never selected by session headers.
        anyhow::ensure!(
            request.attempt_id.as_ref() == Some(&self.attempt_id),
            "model wait Attempt mismatch"
        );
        match self.coordinator.observe()? {
            Some(entry) => Ok(Box::new(NativeWait(entry, 0))),
            None => Ok(Box::new(PassiveWait(self.coordinator.clone()))),
        }
    }
}

struct NativeWait(Arc<WaitEntry>, u64);

#[async_trait]
impl ModelWait for NativeWait {
    fn stream_buffer(&self) -> Option<pvisor_gateway::model_wait::ModelStreamBuffer> {
        self.0.stream_buffer()
    }
    fn repeat_stream_batches(&self) -> bool {
        self.0.repeat_stream_batches()
    }
    async fn wait_for_wake(&mut self) {
        self.0.wait_for_round_wake(&mut self.1).await;
    }
    async fn before_batch(&mut self) -> anyhow::Result<()> {
        self.1 = self.0.before_batch().await?;
        Ok(())
    }
    async fn after_batch(&mut self) -> anyhow::Result<()> {
        self.0.after_batch();
        Ok(())
    }
    async fn enter(&mut self) -> anyhow::Result<()> {
        self.0.enter();
        Ok(())
    }
    async fn before_delivery(&mut self) -> anyhow::Result<()> {
        self.0.before_delivery().await
    }
    fn cancel(&mut self) {
        self.0.cancel();
    }
}

// After overlap, forwarding stays available but cannot authorize another pause.
// Every response still waits for an already-owned transition and manual resume.
struct PassiveWait(Arc<WaitCoordinator>);

#[async_trait]
impl ModelWait for PassiveWait {
    fn repeat_stream_batches(&self) -> bool {
        true
    }
    async fn before_batch(&mut self) -> anyhow::Result<()> {
        self.0.passive_delivery().await
    }
    async fn enter(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn before_delivery(&mut self) -> anyhow::Result<()> {
        self.0.passive_delivery().await
    }
    fn cancel(&mut self) {}
}

impl Drop for PassiveWait {
    fn drop(&mut self) {
        self.0.finish_passive();
    }
}

struct NativeDownload {
    owner: Arc<WaitCoordinator>,
    entry: Option<Arc<WaitEntry>>,
    passive: bool,
}
#[async_trait]
impl pvisor_overlaynet::server::DownloadWait for NativeDownload {
    async fn waiting(&mut self) -> anyhow::Result<()> {
        if self.entry.is_none() && !self.passive {
            self.entry = self.owner.observe()?;
            if let Some(wait) = &self.entry {
                wait.enter();
            } else {
                self.passive = true;
            }
        }
        Ok(())
    }
    async fn ready(&mut self) -> anyhow::Result<()> {
        if let Some(wait) = &self.entry {
            wait.before_delivery().await?;
        }
        self.entry = None;
        if self.passive {
            self.owner.passive_delivery().await?;
            self.owner.finish_passive();
            self.passive = false;
        }
        Ok(())
    }
    fn cancel(&mut self) {
        if let Some(wait) = self.entry.take() {
            wait.cancel();
        }
        if self.passive {
            self.owner.finish_passive();
            self.passive = false;
        }
    }
}
impl Drop for NativeDownload {
    fn drop(&mut self) {
        pvisor_overlaynet::server::DownloadWait::cancel(self);
    }
}
