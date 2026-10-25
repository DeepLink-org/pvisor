//! Guest-only startup intervals. Never compare this epoch with host timestamps.
use std::{io::Write, sync::OnceLock, time::Instant};

static ORIGIN: OnceLock<Instant> = OnceLock::new();

pub(super) struct Span {
    stage: &'static str,
    start: Option<Instant>,
    end: Option<Instant>,
}

impl Span {
    pub(super) fn start(stage: &'static str, enabled: bool) -> Self {
        Self::since(stage, enabled.then(Instant::now))
    }

    pub(super) fn since(stage: &'static str, start: Option<Instant>) -> Self {
        if let Some(start) = start {
            ORIGIN.get_or_init(|| start);
        }
        Self {
            stage,
            start,
            end: None,
        }
    }

    pub(super) fn complete(&mut self) {
        self.end = self.start.map(|_| Instant::now());
    }

    fn record(&self) -> Option<serde_json::Value> {
        let start = self.start?;
        let origin = *ORIGIN.get()?;
        let start_ns = start
            .saturating_duration_since(origin)
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        let duration_ns = self
            .end
            .unwrap_or_else(Instant::now)
            .duration_since(start)
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        Some(serde_json::json!({
            "schema": 1, "pid": std::process::id(), "role": "guest",
            "clock": "guest_monotonic_process_relative", "epoch": "guest-supervisor",
            "stage": self.stage, "start_ns": start_ns,
            "end_ns": start_ns.saturating_add(duration_ns), "duration_ns": duration_ns,
            "outcome": if self.end.is_some() { "ok" } else { "unknown" },
        }))
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(record) = self.record() {
            // Submit one complete line so concurrent host/runner diagnostics
            // cannot interleave individual JSON formatting fragments.
            let line = format!("pvisor-startup-profile {record}\n");
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_interval_stays_frozen_until_stdio_is_ready() {
        let mut span = Span::start("guest.config", true);
        span.complete();
        let first = span.record().unwrap();
        let second = span.record().unwrap();
        assert_eq!(first, second);
        span.start = None;
    }

    #[test]
    fn disabled_has_no_record_and_guest_epoch_is_explicit() {
        assert!(Span::start("disabled", false).record().is_none());
        let mut span = Span::start("guest.test", true);
        assert_eq!(span.record().unwrap()["outcome"], "unknown");
        span.complete();
        let record = span.record().unwrap();
        assert_eq!(record["outcome"], "ok");
        assert_eq!(record["clock"], "guest_monotonic_process_relative");
        assert_eq!(record["epoch"], "guest-supervisor");
        assert_eq!(
            record["end_ns"].as_u64().unwrap() - record["start_ns"].as_u64().unwrap(),
            record["duration_ns"].as_u64().unwrap()
        );
        span.start = None;
    }
}
