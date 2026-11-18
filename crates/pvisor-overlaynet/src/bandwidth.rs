//! Shared aggregate bandwidth scheduling for intercepted streams.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use futures_util::StreamExt;
use tokio::sync::Mutex;
use tokio::time::Instant;

use pvisor_core::network::NetworkBandwidthLimit;

/// Shared aggregate bandwidth owner. Default is empty; clones share scheduling buckets.
#[derive(Debug, Clone, Default)]
pub struct BandwidthRegistry {
    buckets: Arc<Mutex<HashMap<NetworkBandwidthLimit, Arc<BandwidthBucket>>>>,
}

impl BandwidthRegistry {
    pub(crate) async fn session(&self, limits: Vec<NetworkBandwidthLimit>) -> BandwidthSession {
        let mut registry = self.buckets.lock().await;
        let mut buckets = Vec::with_capacity(limits.len());
        for limit in limits {
            let rate = limit.bytes_per_second;
            let bucket = registry
                .entry(limit)
                .or_insert_with(|| Arc::new(BandwidthBucket::new(rate)))
                .clone();
            if !buckets.iter().any(|current| Arc::ptr_eq(current, &bucket)) {
                buckets.push(bucket);
            }
        }
        BandwidthSession { buckets }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BandwidthSession {
    buckets: Vec<Arc<BandwidthBucket>>,
}

impl BandwidthSession {
    pub(crate) async fn throttle(&self, bytes: usize) {
        if bytes == 0 || self.buckets.is_empty() {
            return;
        }
        let mut ready_at = Instant::now();
        for bucket in &self.buckets {
            ready_at = ready_at.max(bucket.reserve(bytes).await);
        }
        tokio::time::sleep_until(ready_at).await;
    }

    pub(crate) fn is_limited(&self) -> bool {
        !self.buckets.is_empty()
    }
}

#[derive(Debug)]
struct BandwidthBucket {
    bytes_per_second: u64,
    next_available: Mutex<Instant>,
}

impl BandwidthBucket {
    fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second,
            next_available: Mutex::new(Instant::now()),
        }
    }

    async fn reserve(&self, bytes: usize) -> Instant {
        let now = Instant::now();
        let mut next_available = self.next_available.lock().await;
        let starts_at = (*next_available).max(now);
        let nanos = (bytes as u128)
            .saturating_mul(1_000_000_000)
            .div_ceil(self.bytes_per_second as u128)
            .min(u64::MAX as u128) as u64;
        let ready_at = starts_at + Duration::from_nanos(nanos);
        *next_available = ready_at;
        ready_at
    }
}

pub(crate) fn throttle_body(body: Body, bandwidth: BandwidthSession) -> Body {
    if !bandwidth.is_limited() {
        return body;
    }
    Body::from_stream(body.into_data_stream().then(move |result| {
        let bandwidth = bandwidth.clone();
        async move {
            if let Ok(bytes) = &result {
                bandwidth.throttle(bytes.len()).await;
            }
            result
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[rstest::fixture]
    fn registry() -> BandwidthRegistry {
        BandwidthRegistry::default()
    }

    #[rstest::rstest]
    #[case::empty(1, 0, 0)]
    #[case::one_second(1_000, 1_000, 1_000_000_000)]
    #[case::one_nanosecond(1_000_000_000, 1, 1)]
    #[case::round_up(3, 1, 333_333_334)]
    #[case::large_payload(1, 100_000, 100_000_000_000_000)]
    #[case::large_rate(1_000_000_000, 100_000, 100_000)]
    #[tokio::test(start_paused = true)]
    async fn reservation_serializes_aggregate_bytes(
        #[case] rate: u64,
        #[case] bytes: usize,
        #[case] nanos: u64,
    ) {
        let bucket = BandwidthBucket::new(rate);
        let start = Instant::now();
        let first = bucket.reserve(bytes).await;
        let second = bucket.reserve(bytes).await;
        let expected = Duration::from_nanos(nanos);
        assert_eq!(first.duration_since(start), expected);
        assert_eq!(second.duration_since(first), expected);
    }

    #[rstest::rstest]
    #[case::empty(&[], 0)]
    #[case::one(&[(None, None, 1)], 1)]
    #[case::duplicates(&[(Some("api.example.com"), Some(443), 1_000); 15], 1)]
    #[case::different_hosts(&[(Some("one.example.com"), None, 1), (Some("two.example.com"), None, 1)], 2)]
    #[case::different_ports(&[(None, Some(1), 1), (None, Some(65_535), 1)], 2)]
    #[case::different_rates(&[(None, None, 1), (None, None, 1_000_000_000)], 2)]
    #[case::mixed_duplicates(&[(None, None, 1), (None, Some(443), 1), (None, None, 1), (Some("api.example.com"), None, 1)], 3)]
    #[tokio::test]
    async fn registry_session_contains_one_bucket_per_distinct_limit(
        registry: BandwidthRegistry,
        #[case] limits: &[(Option<&str>, Option<u16>, u64)],
        #[case] count: usize,
    ) {
        let limits = limits
            .iter()
            .map(|(host, port, bytes_per_second)| NetworkBandwidthLimit {
                host: host.map(str::to_owned),
                port: *port,
                bytes_per_second: *bytes_per_second,
            })
            .collect();
        let session = registry.session(limits).await;
        assert_eq!(session.buckets.len(), count);
        assert_eq!(session.is_limited(), count != 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn registry_shares_identical_limits_between_sessions(
        registry: BandwidthRegistry,
        #[values(1, 1_000, 1_000_000)] rate: u64,
    ) {
        let limit = NetworkBandwidthLimit {
            host: None,
            port: None,
            bytes_per_second: rate,
        };
        let first = registry.session(vec![limit.clone()]).await;
        let second = registry.session(vec![limit]).await;
        assert!(Arc::ptr_eq(&first.buckets[0], &second.buckets[0]));
    }

    #[rstest::rstest]
    #[case::one_second(1_000, 1_000, 1_000)]
    #[case::one_millisecond(1_000, 1, 1)]
    #[case::one_byte_per_second(1, 1, 1_000)]
    #[tokio::test(start_paused = true)]
    async fn throttle_waits_for_reserved_capacity(
        registry: BandwidthRegistry,
        #[case] rate: u64,
        #[case] bytes: usize,
        #[case] delay_ms: u64,
    ) {
        let session = registry
            .session(vec![NetworkBandwidthLimit {
                host: None,
                port: None,
                bytes_per_second: rate,
            }])
            .await;
        let task = tokio::spawn(async move { session.throttle(bytes).await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(delay_ms - 1)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        task.await.unwrap();
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn identical_limits_aggregate_concurrent_sessions(registry: BandwidthRegistry) {
        let limit = NetworkBandwidthLimit {
            host: None,
            port: None,
            bytes_per_second: 1_000,
        };
        let first = registry.session(vec![limit.clone()]).await;
        let second = registry.session(vec![limit]).await;
        let first = tokio::spawn(async move { first.throttle(1_000).await });
        let second = tokio::spawn(async move { second.throttle(1_000).await });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_ne!(first.is_finished(), second.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        first.await.unwrap();
        second.await.unwrap();
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn stacked_limits_use_the_strictest_schedule(registry: BandwidthRegistry) {
        let session = registry
            .session(vec![
                NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 1_000,
                },
                NetworkBandwidthLimit {
                    host: Some("api.example.com".into()),
                    port: None,
                    bytes_per_second: 500,
                },
            ])
            .await;
        let task = tokio::spawn(async move { session.throttle(1_000).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1_999)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        task.await.unwrap();
    }

    #[rstest::rstest]
    #[tokio::test(start_paused = true)]
    async fn throttled_body_preserves_bytes_and_obeys_schedule(registry: BandwidthRegistry) {
        let session = registry
            .session(vec![NetworkBandwidthLimit {
                host: None,
                port: None,
                bytes_per_second: 1_000,
            }])
            .await;
        let body = throttle_body(Body::from(vec![b'x'; 1_000]), session);
        let task = tokio::spawn(async move { body.collect().await.unwrap().to_bytes() });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        let bytes = task.await.unwrap();
        assert_eq!(bytes.len(), 1_000);
        assert!(bytes.iter().all(|byte| *byte == b'x'));
    }
}
