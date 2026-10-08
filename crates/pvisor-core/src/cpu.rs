//! Native process CPU observations. These never reduce admission reservations.
use crate::{AttemptId, RunId};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

/// v1 supports live reports; v2 additionally carries terminal result counters.
pub const CPU_OBSERVATION_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessCpuUsage {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub clock_ticks_per_second: u64,
    pub sampled_monotonic_ns: u64,
    pub threads: u32,
    /// Includes guest CPU time. The guest counter must not be added again.
    pub user_time_ticks: u64,
    pub system_time_ticks: u64,
    pub guest_time_ticks: u64,
}
impl ProcessCpuUsage {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.pid > 0
                && self.start_time_ticks > 0
                && self.clock_ticks_per_second > 0
                && self.sampled_monotonic_ns > 0
                && self.threads > 0,
            "invalid CPU observation identity/clock"
        );
        self.total_ticks()?;
        Ok(())
    }
    pub fn total_ticks(&self) -> anyhow::Result<u64> {
        self.user_time_ticks
            .checked_add(self.system_time_ticks)
            .context("CPU counter overflow")
    }
    pub fn interval_since(&self, previous: &Self) -> anyhow::Result<CpuIntervalUsage> {
        self.validate()?;
        previous.validate()?;
        ensure!(
            self.pid == previous.pid
                && self.start_time_ticks == previous.start_time_ticks
                && self.clock_ticks_per_second == previous.clock_ticks_per_second,
            "CPU process identity or clock changed within Attempt"
        );
        ensure!(
            self.sampled_monotonic_ns > previous.sampled_monotonic_ns
                && self.user_time_ticks >= previous.user_time_ticks
                && self.system_time_ticks >= previous.system_time_ticks
                && self.guest_time_ticks >= previous.guest_time_ticks,
            "CPU observation clock/counters regressed"
        );
        let elapsed_ns = self.sampled_monotonic_ns - previous.sampled_monotonic_ns;
        let ticks = u128::from(self.total_ticks()? - previous.total_ticks()?);
        let hz = u128::from(self.clock_ticks_per_second);
        Ok(CpuIntervalUsage {
            elapsed_monotonic_ns: elapsed_ns,
            total_cpu_time_ns: u64::try_from(ticks * 1_000_000_000 / hz)?,
            cpu_millis: u64::try_from(ticks * 1_000_000_000_000 / (hz * u128::from(elapsed_ns)))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuIntervalUsage {
    pub elapsed_monotonic_ns: u64,
    pub total_cpu_time_ns: u64,
    /// Average consumed CPU over this interval: 1000 means one full logical CPU.
    pub cpu_millis: u64,
}

/// Final native process counters collected after thread-group exit, before reap.
/// An unavailable reading is explicit; it must never become a zero CPU charge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalCpuUsage {
    Measured { usage: ProcessCpuUsage },
    Unavailable { error: String },
}
impl TerminalCpuUsage {
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Measured { usage } => usage.validate(),
            Self::Unavailable { error } => {
                ensure!(
                    !error.is_empty() && error.len() <= 1024,
                    "invalid final CPU probe error"
                );
                Ok(())
            }
        }
    }
    pub fn unavailable(error: impl std::fmt::Display) -> Self {
        let mut error = error.to_string();
        let mut end = error.len().min(1024);
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        if error.is_empty() {
            error = "final CPU observation unavailable".into();
        }
        Self::Unavailable { error }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCpuSample {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub sampled_at_unix_ms: u64,
    pub usage: Option<ProcessCpuUsage>,
    pub error: Option<String>,
}
impl RunCpuSample {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.run_id.is_empty()
                && !self.attempt_id.is_empty()
                && self.run_id.as_str().len() <= 256
                && self.attempt_id.as_str().len() <= 256
                && self.sampled_at_unix_ms > 0,
            "invalid CPU sample identity"
        );
        match (&self.usage, &self.error) {
            (Some(usage), None) => usage.validate()?,
            (None, Some(error)) => ensure!(
                !error.is_empty() && error.len() <= 1024,
                "invalid CPU probe error"
            ),
            _ => anyhow::bail!("CPU sample requires exactly one usage or error"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn usage() -> ProcessCpuUsage {
        ProcessCpuUsage {
            pid: 7,
            start_time_ticks: 22,
            clock_ticks_per_second: 100,
            sampled_monotonic_ns: 1_000_000_000,
            threads: 4,
            user_time_ticks: 300,
            system_time_ticks: 50,
            guest_time_ticks: 200,
        }
    }
    #[test]
    fn intervals_include_guest_once_use_monotonic_time_and_support_multiple_cpus() {
        let previous = usage();
        assert_eq!(previous.total_ticks().unwrap(), 350);
        let mut next = previous.clone();
        next.user_time_ticks += 400;
        next.system_time_ticks += 200;
        next.guest_time_ticks += 300;
        next.sampled_monotonic_ns += 2_000_000_000;
        next.threads = 1; // Exited threads' cumulative time stays in the process counters.
        assert_eq!(
            next.interval_since(&previous).unwrap(),
            CpuIntervalUsage {
                elapsed_monotonic_ns: 2_000_000_000,
                total_cpu_time_ns: 6_000_000_000,
                cpu_millis: 3000,
            }
        );
    }
    #[test]
    fn changed_identity_clocks_regressing_counters_and_arithmetic_overflow_are_rejected() {
        let previous = usage();
        for case in 0..7 {
            let mut next = previous.clone();
            next.sampled_monotonic_ns += 1_000_000_000;
            match case {
                0 => next.pid += 1,
                1 => next.start_time_ticks += 1,
                2 => next.clock_ticks_per_second += 1,
                3 => next.sampled_monotonic_ns = previous.sampled_monotonic_ns,
                4 => next.user_time_ticks -= 1,
                5 => next.system_time_ticks -= 1,
                6 => next.guest_time_ticks -= 1,
                _ => unreachable!(),
            }
            assert!(next.interval_since(&previous).is_err(), "case {case}");
        }
        let mut next = previous.clone();
        next.user_time_ticks = u64::MAX;
        assert!(next.validate().is_err());
        next.system_time_ticks = 0;
        next.clock_ticks_per_second = 1;
        next.sampled_monotonic_ns += 1;
        let mut previous = previous;
        previous.clock_ticks_per_second = 1;
        assert!(next.interval_since(&previous).is_err());
    }
    #[test]
    fn cpu_samples_keep_errors_explicit_and_reject_unknown_fields() {
        let mut sample = RunCpuSample {
            run_id: "run".into(),
            attempt_id: "attempt".into(),
            sampled_at_unix_ms: 1,
            usage: Some(usage()),
            error: None,
        };
        sample.validate().unwrap();
        sample.error = Some("unavailable".into());
        assert!(sample.validate().is_err());
        sample.usage = None;
        sample.validate().unwrap();
        sample.error = Some("é".repeat(513));
        assert!(sample.validate().is_err());
        let mut json = serde_json::to_value(&sample).unwrap();
        json["extra"] = true.into();
        assert!(serde_json::from_value::<RunCpuSample>(json).is_err());
    }

    #[test]
    fn final_cpu_errors_are_bounded_and_wire_outcomes_are_strict() {
        let measured = TerminalCpuUsage::Measured { usage: usage() };
        measured.validate().unwrap();
        let encoded = serde_json::to_value(&measured).unwrap();
        assert_eq!(
            serde_json::from_value::<TerminalCpuUsage>(encoded.clone()).unwrap(),
            measured
        );
        let mut invalid = encoded;
        invalid["error"] = "synthetic".into();
        assert!(serde_json::from_value::<TerminalCpuUsage>(invalid).is_err());
        TerminalCpuUsage::unavailable("é".repeat(1000))
            .validate()
            .unwrap();
        TerminalCpuUsage::unavailable("").validate().unwrap();
        assert!(
            TerminalCpuUsage::Unavailable { error: "".into() }
                .validate()
                .is_err()
        );
    }
}
