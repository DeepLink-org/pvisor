use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, lock_selected_job,
};
use crate::runtime::{ApplySelection, OverlayState, apply_overlay_selected, discard_overlay};
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};

pub struct ApplyRequest {
    pub job: JobSelection,
    pub target: Option<PathBuf>,
    pub selection: ApplySelection,
    pub all: bool,
}
pub struct DropRequest {
    pub job: JobSelection,
}
#[derive(Debug, serde::Serialize)]
pub struct MutationResponse {
    pub job_id: String,
    pub target: PathBuf,
    pub outcome: MutationOutcome,
}
#[derive(Debug, serde::Serialize)]
pub enum MutationOutcome {
    AlreadyApplied,
    AlreadyDropped,
    Dropped,
    Applied {
        applied: usize,
        apply_id: String,
        remaining: usize,
    },
}
impl RuntimeJobService {
    pub fn apply(
        context: &ServiceContext<'_>,
        request: ApplyRequest,
    ) -> anyhow::Result<MutationResponse> {
        if request.all
            && (!request.selection.paths.is_empty()
                || !request.selection.includes.is_empty()
                || !request.selection.excludes.is_empty())
        {
            bail!("--all cannot be combined with --path, --include, or --exclude");
        }
        mutate(
            context,
            request.job,
            true,
            request.target.as_deref(),
            Some(&request.selection),
        )
    }
    pub fn drop(
        context: &ServiceContext<'_>,
        request: DropRequest,
    ) -> anyhow::Result<MutationResponse> {
        mutate(context, request.job, false, None, None)
    }
}
fn mutate(
    context: &ServiceContext<'_>,
    job: JobSelection,
    apply: bool,
    target: Option<&Path>,
    selection: Option<&ApplySelection>,
) -> anyhow::Result<MutationResponse> {
    let selected = RuntimeJobService::resolve(context, &job)?;
    let _job = lock_selected_job(context, &selected)?;
    let (mut record, _lease) = selected.lock_current()?;
    check_selected_record(&selected, &record)?;
    context.check(&record)?;
    record.require_stopped()?;
    let next_generation = record
        .overlay
        .as_ref()
        .context("this Job has no OverlayFS workspace")?
        .generation
        .checked_add(1)
        .context("workspace generation exhausted")?;
    let mut overlay = record
        .overlay
        .take()
        .context("this Job has no OverlayFS workspace")?;
    // The final lower may be the Run-owned snapshot of the target. It is
    // still the base workspace, whereas any preceding lower is a composed
    // read-only layer whose changes cannot be applied to that workspace.
    let base_snapshot = record.storage.join(".overlay-lowers");
    if apply
        && (record.overlay_lowers.len() > 1
            || record.overlay_lowers.first().is_some_and(|lower| {
                lower != &overlay.target && !lower.starts_with(&base_snapshot)
            }))
    {
        bail!(
            "Job {} composes read-only layers above its base; apply is disabled until pVisor can materialize the complete merged diff",
            record.run_id
        );
    }
    match (apply, overlay.state) {
        (true, OverlayState::Applied) => {
            return Ok(MutationResponse {
                job_id: record.run_id,
                target: overlay.target,
                outcome: MutationOutcome::AlreadyApplied,
            });
        }
        (false, OverlayState::Discarded) => {
            return Ok(MutationResponse {
                job_id: record.run_id,
                target: overlay.target,
                outcome: MutationOutcome::AlreadyDropped,
            });
        }
        (false, OverlayState::Applied) => {
            bail!(
                "Job {} was already applied; drop cannot undo changes written to {}",
                record.run_id,
                overlay.target.display()
            );
        }
        (true, OverlayState::Discarded) => {
            bail!(
                "Job {} was already dropped; apply cannot recover discarded changes",
                record.run_id
            );
        }
        _ => {}
    }
    let outcome = if apply {
        if overlay.target == Path::new("/") {
            bail!(
                "Job {} is a full-root libkrun changeset; fork it or drop it instead of applying it to the host root",
                record.run_id
            );
        }
        if let Some(target) = target {
            let target = resolve_apply_target(target, &record.stage_dir())?;
            overlay.target = target.clone();
            overlay.baseline_lower = None;
            if let Some(primary_lower) = record.overlay_lowers.last_mut() {
                *primary_lower = target;
            } else {
                record.overlay_lowers.push(target);
            }
        }
        let lower_dirs = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let outcome = apply_overlay_selected(
            &mut overlay,
            &lower_dirs,
            selection.expect("apply always supplies a selection"),
        )?;
        MutationOutcome::Applied {
            applied: outcome.applied.len(),
            apply_id: outcome.apply_id,
            remaining: outcome.remaining.len(),
        }
    } else {
        discard_overlay(&mut overlay)?;
        MutationOutcome::Dropped
    };
    let target = overlay.target.clone();
    record.overlay = Some(overlay);
    let overlay = record.overlay.as_mut().expect("restored above");
    overlay.generation = next_generation;
    record.write()?;
    Ok(MutationResponse {
        job_id: record.run_id,
        target,
        outcome,
    })
}

fn resolve_apply_target(target: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("create apply target {}", target.display()))?;
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve apply target {}", target.display()))?;
    let stage = stage.canonicalize().unwrap_or_else(|_| stage.to_path_buf());
    if target.starts_with(&stage) || stage.starts_with(&target) {
        bail!(
            "apply target must not overlap the pVisor stage: target={}, stage={}",
            target.display(),
            stage.display()
        );
    }
    Ok(target)
}
