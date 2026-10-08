//! Bounded, per-path observations from operations that reached the FUSE view.

use pvisor_core::operation::{FilesystemObservation, PathOperationCounters};
use pvisor_core::overlay::FileAccessDecision;
use std::path::Path;
use std::sync::{Arc, Mutex};

const MAX_PATHS: usize = 8192;

/// Opaque, cloneable metrics sink. Clones share mutex-protected counters;
/// default construction creates an independent empty sink.
#[derive(Clone, Debug, Default)]
pub struct FsMetrics {
    state: Arc<Mutex<FilesystemObservation>>,
}

impl crate::api::FilesystemMetrics for FsMetrics {
    fn snapshot(&self) -> FilesystemObservation {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl FsMetrics {
    pub(crate) fn observe(
        &self,
        path: &Path,
        operation: &str,
        outcome: Result<u64, i32>,
        mutating: bool,
        decision: FileAccessDecision,
        matched_rules: &[String],
    ) {
        let path = path.to_string_lossy().into_owned();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.paths.contains_key(&path) || state.paths.len() < MAX_PATHS {
            let counters = state
                .paths
                .entry(path)
                .or_default()
                .entry(operation.into())
                .or_default();
            update(counters, operation, &outcome, mutating, decision);
        } else {
            state.overflow_hits = state.overflow_hits.saturating_add(1);
        }
        for rule in matched_rules {
            update(
                state.rules.entry(rule.clone()).or_default(),
                operation,
                &outcome,
                mutating,
                decision,
            );
        }
        if mutating {
            update(
                state.rules.entry("fs.stage".into()).or_default(),
                operation,
                &outcome,
                true,
                decision,
            );
        }
    }
}

fn update(
    counters: &mut PathOperationCounters,
    operation: &str,
    outcome: &Result<u64, i32>,
    mutating: bool,
    decision: FileAccessDecision,
) {
    counters.hits = counters.hits.saturating_add(1);
    if matches!(outcome, Err(libc::EACCES | libc::EPERM))
        && matches!(decision, FileAccessDecision::Deny | FileAccessDecision::Ask)
    {
        counters.denied = counters.denied.saturating_add(1);
        return;
    }
    counters.allowed = counters.allowed.saturating_add(1);
    match outcome {
        Ok(bytes) => {
            counters.succeeded = counters.succeeded.saturating_add(1);
            if mutating {
                counters.effects = counters.effects.saturating_add(1);
            }
            if operation == "read" {
                counters.bytes_read = counters.bytes_read.saturating_add(*bytes);
            } else if operation == "write" {
                counters.bytes_written = counters.bytes_written.saturating_add(*bytes);
            }
        }
        Err(_) => {
            counters.failed = counters.failed.saturating_add(1);
            if mutating {
                counters.uncertain_effects = counters.uncertain_effects.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::FilesystemMetrics;

    #[test]
    fn clones_share_updates_but_snapshots_and_defaults_are_independent() {
        let metrics = FsMetrics::default();
        let shared = metrics.clone();
        std::thread::spawn(move || {
            shared.observe(
                Path::new("value"),
                "write",
                Ok(7),
                true,
                FileAccessDecision::Allow,
                &["rule".into()],
            );
        })
        .join()
        .unwrap();
        let snapshot = metrics.snapshot();
        let counters = &snapshot.paths["value"]["write"];
        assert_eq!(
            (
                counters.hits,
                counters.succeeded,
                counters.effects,
                counters.bytes_written
            ),
            (1, 1, 1, 7)
        );
        assert_eq!(snapshot.rules["rule"].hits, 1);
        assert_eq!(snapshot.rules["fs.stage"].effects, 1);
        metrics.observe(
            Path::new("value"),
            "read",
            Ok(3),
            false,
            FileAccessDecision::Allow,
            &[],
        );
        assert!(!snapshot.paths["value"].contains_key("read"));
        assert_eq!(metrics.snapshot().paths["value"]["read"].bytes_read, 3);
        assert!(FsMetrics::default().snapshot().paths.is_empty());
    }

    #[test]
    fn path_limit_retains_existing_paths_and_counts_rules_for_overflow() {
        let metrics = FsMetrics::default();
        for index in 0..MAX_PATHS {
            metrics.observe(
                Path::new(&format!("path-{index}")),
                "read",
                Ok(1),
                false,
                FileAccessDecision::Allow,
                &[],
            );
        }
        metrics.observe(
            Path::new("overflow"),
            "read",
            Ok(2),
            false,
            FileAccessDecision::Allow,
            &["overflow-rule".into()],
        );
        metrics.observe(
            Path::new("path-0"),
            "read",
            Ok(3),
            false,
            FileAccessDecision::Allow,
            &[],
        );
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.paths.len(), MAX_PATHS);
        assert!(!snapshot.paths.contains_key("overflow"));
        assert_eq!(snapshot.overflow_hits, 1);
        assert_eq!(snapshot.paths["path-0"]["read"].bytes_read, 4);
        assert_eq!(snapshot.rules["overflow-rule"].bytes_read, 2);
    }

    #[test]
    fn snapshot_recovers_a_poisoned_sink() {
        let metrics = FsMetrics::default();
        let shared = metrics.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = shared.state.lock().unwrap();
                panic!("poison for recovery test");
            })
            .join()
            .is_err()
        );
        assert!(metrics.snapshot().paths.is_empty());
        metrics.observe(
            Path::new("after"),
            "read",
            Ok(1),
            false,
            FileAccessDecision::Allow,
            &[],
        );
        assert_eq!(metrics.snapshot().paths["after"]["read"].hits, 1);
    }
}
