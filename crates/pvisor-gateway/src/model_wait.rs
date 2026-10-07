//! Cooperatively idle model calls and the response-delivery barrier.
//!
//! A model call alone does not prove that tools or other guest work are idle.
//! Embedders may install a lifecycle and agents may declare a quiescent call
//! with `x-pvisor-inference-idle: true`. The lifecycle owns admission, lease
//! fencing and pause ownership; Gateway never releases resources itself.

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;
use pvisor_core::ModelCallRequest;

/// Local cooperation signal. Never forwarded to the model supplier.
pub const INFERENCE_IDLE_HEADER: &str = "x-pvisor-inference-idle";

/// Attempt-local factory, called only after model/action authorization.
pub trait ModelWaitLifecycle: Send + Sync + Debug {
    /// Allocate a cancellation-safe obligation before starting asynchronous
    /// work. Do not perform blocking I/O here. Derive authority from the bound
    /// Attempt, not from caller-supplied session headers.
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>>;
}

// async_trait adds #[must_use] to already must-use boxed futures.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ModelWait: Send {
    /// Publish the wait and confirm that any owned pause has completed before
    /// returning. Parallel calls must be coordinated by the implementation.
    async fn enter(&mut self) -> anyhow::Result<()>;

    /// Reacquire admission and acknowledge native resume before returning.
    /// This must not resume a human-owned pause or an expired/stale lease.
    async fn before_delivery(&mut self) -> anyhow::Result<()>;

    /// Called once if entry/delivery fails or the handler is dropped, including
    /// while either future is pending. Enqueue bounded cleanup; do not block.
    /// Cleanup must preserve fencing and must never override another pause.
    fn cancel(&mut self);
}

pub(crate) fn cooperative_idle(headers: &HeaderMap) -> anyhow::Result<bool> {
    let mut values = headers.get_all(INFERENCE_IDLE_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(false);
    };
    anyhow::ensure!(
        values.next().is_none(),
        "duplicate inference-idle declaration"
    );
    match value.as_bytes() {
        b"true" => Ok(true),
        b"false" => Ok(false),
        _ => anyhow::bail!("inference-idle declaration must be true or false"),
    }
}

pub(crate) struct PendingModelWait {
    inner: Box<dyn ModelWait>,
    delivered: bool,
}

impl PendingModelWait {
    pub(crate) fn reserve(
        lifecycle: Option<&Arc<dyn ModelWaitLifecycle>>,
        cooperative: bool,
        request: ModelCallRequest,
    ) -> anyhow::Result<Option<Self>> {
        if !cooperative {
            return Ok(None);
        }
        lifecycle
            .map(|lifecycle| {
                Ok(Self {
                    inner: lifecycle.reserve(request)?,
                    delivered: false,
                })
            })
            .transpose()
    }

    pub(crate) async fn enter(
        &mut self,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> anyhow::Result<()> {
        unless_stopped(stop, self.inner.enter()).await
    }

    pub(crate) async fn before_delivery(
        &mut self,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> anyhow::Result<()> {
        unless_stopped(stop, self.inner.before_delivery()).await?;
        self.delivered = true;
        Ok(())
    }
}

impl Drop for PendingModelWait {
    fn drop(&mut self) {
        if !self.delivered {
            self.inner.cancel();
        }
    }
}

pub(crate) async fn unless_stopped<T>(
    mut stop: tokio::sync::watch::Receiver<()>,
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    // A shutdown observed before entry must not be lost by changed().
    anyhow::ensure!(
        !stop.has_changed().unwrap_or(true),
        "Gateway stopped during model wait"
    );
    tokio::select! {
        biased;
        _ = stop.changed() => anyhow::bail!("Gateway stopped during model wait"),
        result = future => result,
    }
}
