//! Host-observed model calls and the response-delivery barrier.
//!
//! Installing an Attempt-bound lifecycle observes all authorized model calls.
//! No guest/client declaration is needed or trusted. A model request is only a
//! scheduling hint; the native owner separately requires guest-kernel idle
//! evidence. The lifecycle owns fencing and pause ownership; Gateway does not
//! decide whether a VM is idle or release resources itself.

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

mod round_stream;
mod stream_buffer;

pub(crate) type ModelByteStream =
    futures_util::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>;

/// Internal stream control markers never appear on the wire. The body writer
/// acknowledges a marker only after consuming all preceding output queue items.
pub(crate) enum ModelStreamEvent {
    Chunk(Result<bytes::Bytes, String>),
    Flush(tokio::sync::oneshot::Sender<()>),
}

pub(crate) type ModelResponseStream = futures_util::stream::BoxStream<'static, ModelStreamEvent>;

pub(crate) fn plain_stream(stream: ModelByteStream) -> ModelResponseStream {
    use futures_util::StreamExt;
    stream
        .map(|item| ModelStreamEvent::Chunk(item.map_err(|e| e.to_string())))
        .boxed()
}

/// Host-owned, opt-in buffering of model response batches. No client
/// header selects this policy. Buffers raw bytes, including reasoning, content,
/// heartbeats and tool calls; does not parse or suppress semantic events.
#[derive(Clone, Copy, Debug)]
pub struct ModelStreamBuffer {
    /// Maximum copied payload bytes retained before requesting delivery (1 byte
    /// to 16 MiB). An oversized transport chunk stops buffering immediately; its
    /// unconsumed suffix is retained for ordered delivery without another read.
    /// Transport/TLS buffers, that one transport chunk and capture storage are
    /// outside this limit. This is per request, not a node-wide memory budget.
    pub max_bytes: usize,
    /// Nonzero maximum buffering interval per batch, at most 60 seconds, starting
    /// when headers arrive for the first batch, then after the prior batch drains.
    /// Never resets on chunks or heartbeats. Host wake, EOF or error may end it
    /// earlier. Native resume/writeback may delay delivery beyond this interval;
    /// this does not extend any guest idle lease or request timeout.
    pub max_wait: Duration,
}

impl ModelStreamBuffer {
    /// Reject zero/excessive bounds without effects. Native Host configuration
    /// validates at startup; custom lifecycles are also checked before entry.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=16 * 1024 * 1024).contains(&self.max_bytes),
            "model stream buffer must be between 1 byte and 16 MiB"
        );
        anyhow::ensure!(
            !self.max_wait.is_zero() && self.max_wait <= Duration::from_secs(60),
            "model stream buffer wait must be >0 and <=60 seconds"
        );
        Ok(())
    }
}

use async_trait::async_trait;
use pvisor_core::ModelCallRequest;

/// Legacy declaration, stripped upstream. Only an explicitly configured legacy
/// lifecycle requires it; it cannot opt in or out of Host observation.
pub const INFERENCE_IDLE_HEADER: &str = "x-pvisor-inference-idle";

/// Attempt-local factory, called only after model/action authorization.
pub trait ModelWaitLifecycle: Send + Sync + Debug {
    /// Compatibility for the legacy native adapter only. New Host observers do
    /// not require or trust an Agent declaration.
    fn requires_idle_declaration(&self) -> bool {
        false
    }

    /// Allocate a cancellation-safe obligation before starting asynchronous
    /// work. Do not perform blocking I/O here. Derive authority from the bound
    /// Attempt, not from caller-supplied session headers.
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>>;
    /// Optional authorized HTTP GET/CONNECT lifecycle, bound to this same Attempt.
    /// Disabled by default. No client header enables it; CONNECT remains opaque.
    fn reserve_download(
        &self,
    ) -> anyhow::Result<Option<Box<dyn pvisor_overlaynet::server::DownloadWait>>> {
        Ok(None)
    }
}

// async_trait adds #[must_use] to already must-use boxed futures.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ModelWait: Send {
    /// Optional Host policy for stream batches. None keeps the first-nonempty-
    /// chunk delivery boundary. May become None to permanently stop buffering.
    fn stream_buffer(&self) -> Option<ModelStreamBuffer> {
        None
    }

    /// Opt in to keeping the obligation through stream EOF. Defaults to the
    /// original one-shot delivery contract for existing lifecycle adapters.
    fn repeat_stream_batches(&self) -> bool {
        false
    }

    /// Wake/resume the current round and hold off rearming while this batch is
    /// queued for delivery. Repeated lifecycles must implement this hold.
    async fn before_batch(&mut self) -> anyhow::Result<()> {
        self.before_delivery().await
    }

    /// Release the delivery hold after the Gateway body queue drains. This is
    /// not a guest application ACK; fresh idle evidence is still required.
    async fn after_batch(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Latched signal that buffering should stop (timer, external activity,
    /// cancellation or manual control). Must be cancellation-safe and must not
    /// itself resume the VM; before_batch/before_delivery are completion barriers.
    /// Repeated lifecycles consume each generation once, ignoring old wakes.
    /// Custom VM-owning lifecycles must override this to expose their wake source.
    /// The default has no external signal, leaving size/time/EOF/error bounds.
    async fn wait_for_wake(&mut self) {
        std::future::pending::<()>().await;
    }

    /// Register the Host-observed wait. Native coordination only arms timers;
    /// upstream dispatch must not wait for a future pause/offload to complete.
    /// Implementations coordinate parallel calls without relying on client hints.
    async fn enter(&mut self) -> anyhow::Result<()>;

    /// Reacquire admission and acknowledge native resume before returning.
    /// This must not resume a human-owned pause or an expired/stale lease.
    async fn before_delivery(&mut self) -> anyhow::Result<()>;

    /// Called once if entry/delivery fails or the handler is dropped, including
    /// while either future is pending. Enqueue bounded cleanup; do not block.
    /// Cleanup must preserve fencing and must never override another pause.
    fn cancel(&mut self);
}

pub(crate) fn cooperative_idle(headers: &axum::http::HeaderMap) -> anyhow::Result<bool> {
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
        request: ModelCallRequest,
    ) -> anyhow::Result<Option<Self>> {
        lifecycle
            .map(|lifecycle| {
                let pending = Self {
                    inner: lifecycle.reserve(request)?,
                    delivered: false,
                };
                if let Some(policy) = pending.inner.stream_buffer() {
                    policy.validate()?;
                }
                Ok(pending)
            })
            .transpose()
    }

    pub(crate) async fn enter(
        &mut self,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> anyhow::Result<()> {
        unless_stopped(stop, self.inner.enter()).await
    }

    /// Consume a bounded prefix without exposing guest response headers. Even
    /// a wake/error must cross native cleanup/publication before returning.
    pub(crate) async fn prepare_stream(
        mut self,
        mut stream: ModelByteStream,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> anyhow::Result<ModelResponseStream> {
        if self.inner.repeat_stream_batches() {
            return round_stream::prepare(self, stream, stop).await;
        }
        let policy = self.inner.stream_buffer();
        let prefix = unless_stopped(stop.clone(), async {
            Ok(stream_buffer::read_prefix(&mut stream, policy, self.inner.wait_for_wake()).await)
        })
        .await;
        self.before_delivery(stop).await?;
        let mut prefix = prefix?;
        // Preserve pre-body HTTP failure handling. Errors after buffered data
        // are replayed only after that data through the normal stream path.
        if let Some(error) = prefix.take_initial_error() {
            return Err(anyhow::Error::new(error).context("read model stream prefix"));
        }
        Ok(plain_stream(prefix.replay(stream)))
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
