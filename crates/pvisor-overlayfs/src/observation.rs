//! Bounded, per-path observations from operations that reached the FUSE view.

use pvisor_control::ir::run::{FilesystemObservation, PathOperationCounters};
use pvisor_control::overlay::FileAccessDecision;
use std::path::Path;
use std::sync::{Arc, Mutex};

const MAX_PATHS: usize = 8192;

#[derive(Clone, Debug, Default)]
pub struct FsMetrics {
    state: Arc<Mutex<FilesystemObservation>>,
}

impl FsMetrics {
    pub fn snapshot(&self) -> FilesystemObservation {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

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
