//! Opt-in startup spans, independent of performance samples and guest clocks.
use std::{io::Write, sync::LazyLock, time::Instant};

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("PVISOR_STARTUP_PROFILE").as_deref() == Ok("1"));
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

pub(crate) struct Span {
    stage: &'static str,
    role: &'static str,
    started: Option<(Instant, u64)>,
    bytes: Option<usize>,
    outcome: &'static str,
}

impl Span {
    pub(crate) fn new(stage: &'static str, role: &'static str) -> Self {
        Self::configured(stage, role, *ENABLED)
    }

    fn configured(stage: &'static str, role: &'static str, enabled: bool) -> Self {
        let started = enabled.then(|| {
            let epoch = *EPOCH;
            let now = Instant::now();
            (now, nanoseconds(now.duration_since(epoch).as_nanos()))
        });
        Self {
            stage,
            role,
            started,
            bytes: None,
            outcome: "unknown",
        }
    }

    pub(crate) fn complete(&mut self, bytes: Option<usize>) {
        self.bytes = bytes;
        self.outcome = "ok";
    }

    fn record(&self) -> Option<serde_json::Value> {
        let (started, start_ns) = self.started?;
        let duration_ns = nanoseconds(started.elapsed().as_nanos());
        Some(serde_json::json!({
            "schema": 1,
            "pid": std::process::id(),
            "role": self.role,
            "clock": "host_monotonic_process_relative",
            "stage": self.stage,
            "start_ns": start_ns,
            "end_ns": start_ns.saturating_add(duration_ns),
            "duration_ns": duration_ns,
            "outcome": self.outcome,
            "bytes": self.bytes,
            "epoch": "vm-runtime",
        }))
    }
}

fn nanoseconds(value: u128) -> u64 {
    value.min(u64::MAX as u128) as u64
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(record) = self.record() {
            // Diagnostics must not turn a full/closed stderr pipe into a panic.
            let _ = writeln!(std::io::stderr().lock(), "pvisor-startup-profile {record}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_span_has_no_timestamp_or_record() {
        let mut span = Span::configured("test", "runtime", false);
        assert!(span.started.is_none());
        span.complete(Some(12));
        assert!(span.record().is_none());
    }

    #[test]
    fn closed_span_has_consistent_clock_identity_and_outcome() {
        let mut span = Span::configured("test", "runtime", true);
        assert_eq!(span.record().unwrap()["outcome"], "unknown");
        span.complete(Some(123));
        let record = span.record().unwrap();
        assert_eq!(record["pid"], std::process::id());
        assert_eq!(record["bytes"], 123);
        assert_eq!(record["outcome"], "ok");
        assert_eq!(record["epoch"], "vm-runtime");
        assert_eq!(
            record["end_ns"].as_u64().unwrap() - record["start_ns"].as_u64().unwrap(),
            record["duration_ns"].as_u64().unwrap()
        );
        // Keep the test quiet; production emits the completed record on drop.
        span.started = None;
    }
}
