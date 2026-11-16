//! B-STARTUP: prepared-environment Ready and normal Exit, observed by Rust.
//!
//! The two clocks belong to the same child invocation. Ready and Exit Criterion
//! streams each execute fresh invocations and retain both clocks; no stream
//! replays cached observations measured by another stream.
//! Preparation, validation, cleanup and evidence I/O are never timed.

mod evidence;
mod images;
mod observer;
mod prepared;
mod subjects;

use anyhow::{Context, Result, bail};
use criterion::{Criterion, SamplingMode};
use std::{env, path::PathBuf, time::Duration};

/// Run a private image-worker reexecution before the shared CLI parses args or
/// overwrites entry environment variables. `None` means an ordinary CLI turn;
/// `Some(code)` means the owned worker finished and the caller should exit.
pub fn worker_dispatch() -> Option<i32> {
    images::worker_dispatch()
}

/// Register the selected startup table with Criterion.
///
/// Configuration is read only when this family is selected by the parent
/// dispatcher. Invalid configuration or a failed correctness gate aborts the
/// cohort and retains its evidence; it never becomes a fast Criterion sample.
pub fn register(criterion: &mut Criterion) {
    if let Err(error) = register_checked(criterion) {
        panic!("startup benchmark failed: {error:#}");
    }
}

fn register_checked(criterion: &mut Criterion) -> Result<()> {
    let entry = env::var("PVISOR_BENCH_ENTRY").unwrap_or_else(|_| "startup/timing.json".into());
    let name = entry.strip_prefix("startup/").unwrap_or(&entry);
    if matches!(
        name,
        "image_compare.json" | "image_lazy_compare.json" | "stage_lazy_compare.json"
    ) {
        return images::register(criterion);
    }
    let backends = if name == "timing.json" {
        subjects::BACKENDS.to_vec()
    } else if let Some(backend) = subjects::BACKENDS
        .iter()
        .find(|backend| backend.entry() == name)
    {
        vec![*backend]
    } else {
        bail!("startup entry {entry:?} is not implemented by the prepared Ready/Exit adapter");
    };
    let output = PathBuf::from(
        env::var_os("PVISOR_BENCH_OUTPUT")
            .context("PVISOR_BENCH_OUTPUT must name a new evidence directory under .data")?,
    );
    let mut cohort = prepared::Cohort::new(output, &backends)?;
    // These are correctness warmups, not a replacement for Criterion's adaptive
    // warmup/calibration. Every invocation during Criterion warmup is also checked.
    for backend in &backends {
        cohort.trial(*backend, "preflight")?;
        for _ in 0..3 {
            cohort.trial(*backend, "protocol-warmup")?;
        }
    }
    let mut group = criterion.benchmark_group("startup");
    group.sample_size(60);
    group.sampling_mode(SamplingMode::Flat);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    for backend in backends {
        for metric in ["ready", "exit"] {
            let mut callbacks = Vec::new();
            group.bench_function(format!("{}/{metric}",backend.name()), |bencher| {
                bencher.iter_custom(|iterations| {
                    let first=cohort.trial_index();
                    let mut measured = Duration::ZERO;
                    for _ in 0..iterations {
                        // A sample is one fresh execution, not an entire fixed-count
                        // campaign. Criterion owns iteration selection.
                        match cohort.trial(backend, &format!("criterion-{metric}")) {
                            Ok(observation) => measured += Duration::from_nanos(
                                if metric=="ready" {observation.ready_ns} else {observation.exit_ns}),
                            Err(error) => {
                                let _ = cohort.fail(&format!("{error:#}"));
                                panic!("{} correctness gate failed: {error:#}", backend.name());
                            }
                        }
                    }
                    callbacks.push(first);
                    cohort.record(&format!("criterion-{}-{metric}-{:06}.json",backend.name(),callbacks.len()),
                        &serde_json::json!({"first_trial_index":first,"last_trial_exclusive":cohort.trial_index(),
                            "iterations_requested":iterations,"returned_ns":measured.as_nanos().to_string(),
                            "metric":metric,"both_raw_clocks_retained":true,
                            "includes_framework_calibration":true})).expect("retain Criterion callback mapping");
                    measured
                });
            });
            cohort.record(&format!("criterion-{}-{metric}-scope.json",backend.name()),
                &serde_json::json!({"callback_count":callbacks.len(),
                    "sampling":"Criterion controls warmup, calibration and statistical samples",
                    "phase_mapping":"not exposed by Criterion callback API; no inferred formal subset",
                    "statistics":"Criterion output only; raw callbacks include framework calibration"}))?;
        }
    }
    group.finish();
    cohort.finish()
}
