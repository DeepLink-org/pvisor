//! Repeated bounded batches. Owns the request guard through EOF or stream drop.
//! No background reader: upstream polling stops while a batch is being delivered.

use super::{
    ModelByteStream, ModelResponseStream, ModelStreamEvent, PendingModelWait, stream_buffer,
    unless_stopped,
};
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::VecDeque;
use tokio::sync::{oneshot, watch};

struct Batches {
    wait: PendingModelWait,
    source: ModelByteStream,
    stop: watch::Receiver<()>,
    queued: VecDeque<Result<Bytes, reqwest::Error>>,
    ended: bool,
    passthrough: bool,
    ack: Option<oneshot::Receiver<()>>,
    failed: bool,
}

impl Batches {
    async fn collect(&mut self) -> anyhow::Result<()> {
        let policy = self.wait.inner.stream_buffer();
        let prefix = unless_stopped(self.stop.clone(), async {
            Ok(stream_buffer::read_prefix(
                &mut self.source,
                policy,
                self.wait.inner.wait_for_wake(),
            )
            .await)
        })
        .await?;
        let (queued, ended) = prefix.into_parts();

        // Neither chunks nor headers cross this barrier before native resume.
        unless_stopped(self.stop.clone(), self.wait.inner.before_batch()).await?;
        (self.queued, self.ended) = (queued, ended);
        self.passthrough = policy.is_none();
        Ok(())
    }

    async fn next(&mut self) -> anyhow::Result<Option<ModelStreamEvent>> {
        loop {
            if let Some(item) = self.queued.pop_front() {
                if item.is_err() {
                    self.failed = true;
                }
                return Ok(Some(ModelStreamEvent::Chunk(
                    item.map_err(|e| e.to_string()),
                )));
            }
            if self.ended {
                // Keep the request slot and delivery hold until its final batch
                // drains too. A subsequent request must not pause queued output.
                if let Some(ack) = self.ack.take() {
                    unless_stopped(self.stop.clone(), async {
                        ack.await
                            .map_err(|_| anyhow::anyhow!("model stream delivery dropped"))
                    })
                    .await?;

                    self.wait.before_delivery(self.stop.clone()).await?;
                    return Ok(None);
                }
                let (tx, rx) = oneshot::channel();
                self.ack = Some(rx);
                return Ok(Some(ModelStreamEvent::Flush(tx)));
            }
            if self.passthrough {
                // No configured buffering, or a manual/overlap fallback: retain
                // the delivery hold through EOF. Do not pause between raw chunks.
                match unless_stopped(self.stop.clone(), async { Ok(self.source.next().await) })
                    .await?
                {
                    Some(item) => {
                        if item.is_err() {
                            self.failed = true;
                        }
                        return Ok(Some(ModelStreamEvent::Chunk(
                            item.map_err(|e| e.to_string()),
                        )));
                    }
                    None => {
                        self.ended = true;
                        continue;
                    }
                }
            }
            if let Some(ack) = self.ack.take() {
                unless_stopped(self.stop.clone(), async {
                    ack.await
                        .map_err(|_| anyhow::anyhow!("model stream delivery dropped"))
                })
                .await?;

                unless_stopped(self.stop.clone(), self.wait.inner.after_batch()).await?;
                self.collect().await?;
            } else {
                let (tx, rx) = oneshot::channel();
                self.ack = Some(rx);
                return Ok(Some(ModelStreamEvent::Flush(tx)));
            }
        }
    }
}

pub(super) async fn prepare(
    wait: PendingModelWait,
    source: ModelByteStream,
    stop: watch::Receiver<()>,
) -> anyhow::Result<ModelResponseStream> {
    let mut batches = Batches {
        wait,
        source,
        stop,
        queued: VecDeque::new(),
        ended: false,
        passthrough: false,
        ack: None,
        failed: false,
    };
    batches.collect().await?;
    // Retain the existing pre-header failure response when no bytes arrived.
    if batches.queued.front().is_some_and(Result::is_err)
        && let Some(Err(error)) = batches.queued.pop_front()
    {
        batches.wait.before_delivery(batches.stop.clone()).await?;
        return Err(anyhow::Error::new(error).context("read model stream prefix"));
    }
    Ok(
        futures_util::stream::unfold(batches, |mut batches| async move {
            if batches.failed {
                return None;
            }
            match batches.next().await {
                Ok(Some(event)) => Some((event, batches)),
                Ok(None) => None,
                Err(error) => {
                    batches.failed = true;
                    Some((ModelStreamEvent::Chunk(Err(error.to_string())), batches))
                }
            }
        })
        .boxed(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{ModelStreamBuffer, ModelWait};
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct State {
        calls: Mutex<Vec<&'static str>>,
        held: Mutex<bool>,
    }
    struct Wait(Arc<State>);
    #[async_trait]
    impl ModelWait for Wait {
        fn stream_buffer(&self) -> Option<ModelStreamBuffer> {
            Some(ModelStreamBuffer {
                max_bytes: 2,
                max_wait: Duration::from_secs(1),
            })
        }
        fn repeat_stream_batches(&self) -> bool {
            true
        }
        async fn enter(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn before_batch(&mut self) -> anyhow::Result<()> {
            assert!(!*self.0.held.lock().unwrap());
            *self.0.held.lock().unwrap() = true;
            self.0.calls.lock().unwrap().push("batch");
            Ok(())
        }
        async fn after_batch(&mut self) -> anyhow::Result<()> {
            assert!(*self.0.held.lock().unwrap());
            *self.0.held.lock().unwrap() = false;
            self.0.calls.lock().unwrap().push("drained");
            Ok(())
        }
        async fn before_delivery(&mut self) -> anyhow::Result<()> {
            assert!(*self.0.held.lock().unwrap());
            self.0.calls.lock().unwrap().push("done");
            Ok(())
        }
        fn cancel(&mut self) {
            self.0.calls.lock().unwrap().push("cancel");
        }
    }

    fn pending(state: &Arc<State>) -> PendingModelWait {
        PendingModelWait {
            inner: Box::new(Wait(state.clone())),
            delivered: false,
        }
    }

    #[tokio::test]
    async fn repeated_batches_keep_exact_bytes_and_finalize_only_after_last_drain() {
        let state = Arc::new(State::default());
        let (_stop_tx, stop) = watch::channel(());
        let source = futures_util::stream::iter([
            Ok(Bytes::from_static(b"abcdef")),
            Ok(Bytes::from_static(b"gh")),
        ])
        .boxed();
        let mut stream = prepare(pending(&state), source, stop).await.unwrap();
        let mut bytes = Vec::new();
        let mut flushes = 0;
        while let Some(event) = stream.next().await {
            match event {
                ModelStreamEvent::Chunk(item) => bytes.extend(item.unwrap()),
                ModelStreamEvent::Flush(ack) => {
                    flushes += 1;
                    assert!(*state.held.lock().unwrap());
                    assert!(!state.calls.lock().unwrap().contains(&"done"));
                    ack.send(()).unwrap();
                }
            }
        }
        assert_eq!(bytes, b"abcdefgh");
        assert_eq!(flushes, 3); // oversized first chunk, second chunk, EOF
        assert_eq!(
            *state.calls.lock().unwrap(),
            ["batch", "drained", "batch", "drained", "batch", "done"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn queued_delivery_prevents_rearm_and_drop_cancels_once() {
        let state = Arc::new(State::default());
        let (_stop_tx, stop) = watch::channel(());
        let source = futures_util::stream::iter([
            Ok(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cd")),
        ])
        .boxed();
        let mut stream = prepare(pending(&state), source, stop).await.unwrap();
        assert!(matches!(
            stream.next().await,
            Some(ModelStreamEvent::Chunk(Ok(_)))
        ));
        let Some(ModelStreamEvent::Flush(_ack)) = stream.next().await else {
            panic!("missing drain marker");
        };
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .is_err()
        );
        assert_eq!(*state.calls.lock().unwrap(), ["batch"]);
        drop(stream);
        assert_eq!(*state.calls.lock().unwrap(), ["batch", "cancel"]);
    }

    #[tokio::test]
    async fn shutdown_while_awaiting_drain_cancels_the_owned_request() {
        let state = Arc::new(State::default());
        let (stop_tx, stop) = watch::channel(());
        let source = futures_util::stream::iter([Ok(Bytes::from_static(b"ab"))]).boxed();
        let mut stream = prepare(pending(&state), source, stop).await.unwrap();
        let _ = stream.next().await;
        let Some(ModelStreamEvent::Flush(_ack)) = stream.next().await else {
            panic!("missing drain marker");
        };
        stop_tx.send_replace(());
        assert!(matches!(
            stream.next().await,
            Some(ModelStreamEvent::Chunk(Err(_)))
        ));
        drop(stream);
        assert_eq!(*state.calls.lock().unwrap(), ["batch", "cancel"]);
    }
}
