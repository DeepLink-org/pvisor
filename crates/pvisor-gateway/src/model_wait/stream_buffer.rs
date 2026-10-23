//! Bounded buffering of a single stream batch. No detached reader or unbounded chunk queue.
use super::ModelStreamBuffer;
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt, stream::BoxStream};
use std::{future::Future, time::Duration};

pub(super) struct Prefix<E> {
    bytes: Bytes,
    // Already received transport data exceeding the copy budget. Never discard,
    // read another chunk, or duplicate this suffix while waiting for resume.
    tail: Option<Bytes>,
    error: Option<E>,
    ended: bool,
}

impl<E: Send + 'static> Prefix<E> {
    pub(super) fn take_initial_error(&mut self) -> Option<E> {
        if self.bytes.is_empty() && self.tail.is_none() {
            self.error.take()
        } else {
            None
        }
    }
    pub(super) fn into_parts(self) -> (std::collections::VecDeque<Result<Bytes, E>>, bool) {
        let mut items = std::collections::VecDeque::with_capacity(3);
        if !self.bytes.is_empty() {
            items.push_back(Ok(self.bytes));
        }
        if let Some(tail) = self.tail {
            items.push_back(Ok(tail));
        }
        if let Some(error) = self.error {
            items.push_back(Err(error));
        }
        (items, self.ended)
    }
    pub(super) fn replay<S>(self, stream: S) -> BoxStream<'static, Result<Bytes, E>>
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    {
        let (prefix, ended) = self.into_parts();
        // EOF/errors are terminal: do not poll the underlying stream again.
        futures_util::stream::iter(prefix)
            .chain(stream.take(if ended { 0 } else { usize::MAX }))
            .boxed()
    }
}

pub(super) async fn read_prefix<S, E>(
    stream: &mut S,
    policy: Option<ModelStreamBuffer>,
    wake: impl Future<Output = ()>,
) -> Prefix<E>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    let mut buffered = BytesMut::with_capacity(policy.map_or(0, |p| p.max_bytes));
    let deadline = tokio::time::Instant::now() + policy.map_or(Duration::ZERO, |p| p.max_wait);
    tokio::pin!(wake);
    let mut result = Prefix {
        bytes: Bytes::new(),
        tail: None,
        error: None,
        ended: false,
    };
    loop {
        let item = tokio::select! {
            biased;
            _ = &mut wake, if policy.is_some() => break,
            _ = tokio::time::sleep_until(deadline), if policy.is_some() => break,
            item = stream.next() => item,
        };
        match item {
            None => {
                result.ended = true;
                break;
            }
            Some(Err(error)) => {
                result.error = Some(error);
                result.ended = true;
                break;
            }
            Some(Ok(chunk)) if chunk.is_empty() => continue,
            Some(Ok(chunk)) => {
                let Some(policy) = policy else {
                    // Preserve the default first-chunk path without a copy.
                    result.tail = Some(chunk);
                    break;
                };
                let count = chunk.len().min(policy.max_bytes - buffered.len());
                buffered.extend_from_slice(&chunk[..count]);
                if count < chunk.len() {
                    result.tail = Some(chunk.slice(count..));
                }
                if buffered.len() == policy.max_bytes {
                    break;
                }
            }
        }
    }
    result.bytes = buffered.freeze();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    fn policy(max_bytes: usize) -> ModelStreamBuffer {
        ModelStreamBuffer {
            max_bytes,
            max_wait: Duration::from_millis(100),
        }
    }
    fn bytes(value: &'static [u8]) -> Result<Bytes, std::io::Error> {
        Ok(Bytes::from_static(value))
    }
    async fn collect(stream: BoxStream<'static, Result<Bytes, std::io::Error>>) -> Vec<u8> {
        stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flat_map(|item| item.unwrap().to_vec())
            .collect()
    }

    #[tokio::test]
    async fn oversized_chunk_stops_at_budget_and_replays_suffix_exactly_once() {
        let mut source = stream::iter([bytes(b"ab"), bytes(b"cdefgh"), bytes(b"ij")]);
        let prefix = read_prefix(&mut source, Some(policy(4)), std::future::pending()).await;
        assert_eq!(&prefix.bytes[..], b"abcd");
        assert_eq!(prefix.tail.as_deref(), Some(&b"efgh"[..]));
        assert_eq!(collect(prefix.replay(source)).await, b"abcdefghij");
    }

    #[tokio::test]
    async fn eof_and_partial_errors_preserve_data_without_polling_past_terminal() {
        let mut source = stream::iter([bytes(b"first"), bytes(b"last")]);
        let prefix = read_prefix(&mut source, Some(policy(128)), std::future::pending()).await;
        assert!(prefix.ended);
        assert_eq!(collect(prefix.replay(source)).await, b"firstlast");
        let mut source = stream::iter([
            bytes(b"first"),
            Err(std::io::Error::other("upstream failed")),
            bytes(b"must not leak"),
        ]);
        let mut prefix = read_prefix(&mut source, Some(policy(128)), std::future::pending()).await;
        assert!(prefix.take_initial_error().is_none());
        let items = prefix.replay(source).collect::<Vec<_>>().await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_ref().unwrap().as_ref(), b"first");
        assert!(items[1].is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_includes_first_byte_wait_and_heartbeats_do_not_reset_it() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut source = UnboundedReceiverStream::new(rx);
        let started = tokio::time::Instant::now();
        let producer = tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                tx.send(bytes(b": heartbeat\n\n")).unwrap();
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let prefix = read_prefix(&mut source, Some(policy(4096)), std::future::pending()).await;
        assert_eq!(started.elapsed(), Duration::from_millis(100));
        assert_eq!(prefix.bytes.len(), 3 * b": heartbeat\n\n".len());
        assert!(!prefix.ended);
        producer.abort();

        let mut source = stream::pending::<Result<Bytes, std::io::Error>>();
        let started = tokio::time::Instant::now();
        let prefix = read_prefix(&mut source, Some(policy(4096)), std::future::pending()).await;
        assert_eq!(started.elapsed(), Duration::from_millis(100));
        assert!(prefix.bytes.is_empty());
        assert!(!prefix.ended);
    }

    #[tokio::test(start_paused = true)]
    async fn latched_and_midstream_wakes_end_buffering_before_deadline() {
        let mut source = stream::iter([bytes(b"unread")]);
        let prefix = read_prefix(&mut source, Some(policy(128)), async {}).await;
        assert!(prefix.bytes.is_empty());
        assert_eq!(collect(prefix.replay(source)).await, b"unread");

        let mut source = stream::iter([bytes(b"partial")]).chain(stream::pending());
        let started = tokio::time::Instant::now();
        let prefix = read_prefix(
            &mut source,
            Some(policy(128)),
            tokio::time::sleep(Duration::from_millis(10)),
        )
        .await;
        assert_eq!(started.elapsed(), Duration::from_millis(10));
        assert_eq!(&prefix.bytes[..], b"partial");
    }

    #[tokio::test]
    async fn disabled_buffer_preserves_first_nonempty_chunk_boundary() {
        let mut source = stream::iter([bytes(b""), bytes(b"first"), bytes(b"second")]);
        let prefix = read_prefix(&mut source, None, async {}).await;
        assert!(prefix.bytes.is_empty());
        assert_eq!(prefix.tail.as_deref(), Some(&b"first"[..]));
        assert_eq!(collect(prefix.replay(source)).await, b"firstsecond");
    }

    #[test]
    fn policy_bounds_reject_zero_and_excessive_limits() {
        policy(1).validate().unwrap();
        policy(16 * 1024 * 1024).validate().unwrap();
        assert!(policy(0).validate().is_err());
        assert!(policy(16 * 1024 * 1024 + 1).validate().is_err());
        for max_wait in [Duration::ZERO, Duration::from_secs(61)] {
            assert!(
                ModelStreamBuffer {
                    max_bytes: 1,
                    max_wait
                }
                .validate()
                .is_err()
            );
        }
    }
}
