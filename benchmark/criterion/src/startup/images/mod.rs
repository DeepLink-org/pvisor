//! B-LAZY-STARTUP native randomized paired image experiments.
mod audit;
mod image_compare;
mod image_lazy;
pub(super) mod lab;
pub(super) mod process;
pub(super) mod proxy;
pub(super) mod source;
mod stage_lazy;
mod vm;

use anyhow::{Context, Result, bail};
use criterion::Criterion;
use std::{env, path::PathBuf};

/// Dispatch an image entry. Private worker reexecution exits before Criterion.
pub(super) fn register(criterion: &mut Criterion) -> Result<()> {
    if let Some(code) = worker_dispatch() {
        std::process::exit(code);
    }
    let entry = env::var("PVISOR_BENCH_ENTRY").context("PVISOR_BENCH_ENTRY is required")?;
    match entry.strip_prefix("startup/").unwrap_or(&entry) {
        "image_compare.json" => image_compare::run(criterion),
        "image_lazy_compare.json" => image_lazy::run(criterion),
        "stage_lazy_compare.json" => stage_lazy::run(criterion),
        _ => bail!("not an image startup entry: {entry:?}"),
    }
}

/// Run the private native owner before top-level CLI parsing overwrites entry
/// defaults. `None` means this is an ordinary benchmark invocation.
pub(super) fn worker_dispatch() -> Option<i32> {
    if let Some(path) = env::var_os("PVISOR_BENCH_IMAGE_WORKER") {
        let result = vm::worker(&PathBuf::from(path));
        if let Err(error) = &result {
            eprintln!("image worker: {error:#}");
        }
        return Some(if result.is_ok() { 0 } else { 1 });
    }
    None
}
