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
    if overlay.state == OverlayState::Applied
        && pvisor_overlay_core::apply::has_pending_applies(&overlay)?
    {
        if !apply {
            bail!(
                "Job {} has a pending apply; retry apply to reconcile it before any drop",
                record.run_id
            );
        }
        let lower_dirs = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        pvisor_overlay_core::apply::recover_pending_applies(&mut overlay, &lower_dirs)
            .context("reconcile pending apply before reporting AlreadyApplied")?;
        anyhow::ensure!(
            overlay.state == OverlayState::Applied,
            "pending apply reconciliation retained staged changes; review and retry apply"
        );
        // The Job mutation lock and Run lease are held. Only publish a new
        // runtime fence after target-locked recovery has committed the ledger.
        overlay.generation = next_generation;
        record.overlay = Some(overlay.clone());
        record.write()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RunRecord;

    fn fixture(root: &Path) -> RunRecord {
        for dir in ["target", "upper", "work", "merged"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"effects-gap","session_id":"session",
            "attempt_id":"attempt","agent":"sh","pid":0,"command":["/bin/sh"],
            "state":"completed","started_at_unix_ms":1,"finished_at_unix_ms":2,
            "storage":root,"network":{},"gateway_listen":null,
            "overlay":{"id":"effects-gap","generation":7,"target":root.join("target"),
                "upper":{"upper_dir":root.join("upper"),"work_dir":root.join("work")},
                "merged_dir":root.join("merged"),"stage_dir":root,
                "auto_apply":false,"state":"staged"}
        }))
        .unwrap();
        record.write().unwrap();
        record
    }

    #[test]
    fn pending_terminal_apply_is_recovered_before_success_and_new_fence() {
        use pvisor_core::overlay::ApplyRecordState;
        use pvisor_overlay_core::apply::{has_pending_applies, load_apply_records};
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        std::fs::write(root.path().join("upper/change"), b"staged").unwrap();
        let mut overlay = record.overlay.clone().unwrap();
        let lowers = vec![overlay.target.clone()];
        apply_overlay_selected(&mut overlay, &lowers, &ApplySelection::default()).unwrap();
        // Reconstruct the ledger side of the terminal-publication interruption.
        // The core regression stops at the actual consume/commit boundary.
        let ledger_path = root.path().join("apply-ledger.json");
        let mut ledger: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&ledger_path).unwrap()).unwrap();
        ledger["records"][0]["state"] = serde_json::json!("target_applied");
        std::fs::write(&ledger_path, serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert!(has_pending_applies(&overlay).unwrap());
        let current = RunRecord::read(root.path()).unwrap();
        assert_eq!(
            current.overlay.as_ref().unwrap().state,
            OverlayState::Applied
        );
        assert_eq!(current.overlay.as_ref().unwrap().generation, 7);
        let job = || JobSelection {
            selector: Some(root.path().to_path_buf()),
            storage: root.path().to_path_buf(),
        };
        let context = ServiceContext::default();
        let error = RuntimeJobService::drop(&context, DropRequest { job: job() }).unwrap_err();
        assert!(error.to_string().contains("pending apply"));
        assert!(has_pending_applies(&overlay).unwrap());
        assert_eq!(
            RunRecord::read(root.path())
                .unwrap()
                .overlay
                .unwrap()
                .generation,
            7
        );
        // Recovery failure must not return AlreadyApplied or advance the fence.
        let entries = root.path().join("preimages/entries");
        std::fs::create_dir_all(&entries).unwrap();
        let corrupt = entries.join("invalid.json");
        std::fs::write(&corrupt, b"invalid").unwrap();
        let request = || ApplyRequest {
            job: job(),
            target: None,
            selection: ApplySelection::default(),
            all: true,
        };
        let error = RuntimeJobService::apply(&context, request()).unwrap_err();
        assert!(error.to_string().contains("reconcile pending apply"));
        assert!(has_pending_applies(&overlay).unwrap());
        assert_eq!(
            RunRecord::read(root.path())
                .unwrap()
                .overlay
                .unwrap()
                .generation,
            7
        );
        std::fs::remove_file(corrupt).unwrap();
        let response = RuntimeJobService::apply(&context, request()).unwrap();
        assert!(matches!(response.outcome, MutationOutcome::AlreadyApplied));
        assert!(!has_pending_applies(&overlay).unwrap());
        let committed = load_apply_records(root.path()).unwrap();
        assert_eq!(committed[0].state, ApplyRecordState::Committed);
        assert_eq!(committed[0].overlay_generation, 7);
        // Read raw run.json too: mutation, not read projection, publishes g+1.
        let raw: RunRecord =
            serde_json::from_slice(&std::fs::read(root.path().join("run.json")).unwrap()).unwrap();
        assert_eq!(raw.overlay.as_ref().unwrap().state, OverlayState::Applied);
        assert_eq!(raw.overlay.as_ref().unwrap().generation, 8);
        let repeated = RuntimeJobService::apply(&context, request()).unwrap();
        assert!(matches!(repeated.outcome, MutationOutcome::AlreadyApplied));
        assert_eq!(
            RunRecord::read(root.path())
                .unwrap()
                .overlay
                .unwrap()
                .generation,
            8
        );
    }

    #[test]
    fn core_apply_survives_missing_run_publication_and_rejects_drop() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        std::fs::write(root.path().join("upper/change"), b"staged").unwrap();
        let mut overlay = record.overlay.clone().unwrap();
        let lowers = vec![overlay.target.clone()];
        apply_overlay_selected(&mut overlay, &lowers, &ApplySelection::default()).unwrap();
        // Deliberately leave run.json unchanged, as on failure/exit after core commit.
        let current = RunRecord::read(root.path()).unwrap();
        assert_eq!(
            current.overlay.as_ref().unwrap().state,
            OverlayState::Applied
        );
        assert_eq!(current.overlay.as_ref().unwrap().generation, 8);
        let job = || JobSelection {
            selector: Some(root.path().to_path_buf()),
            storage: root.path().to_path_buf(),
        };
        let error = RuntimeJobService::drop(&ServiceContext::default(), DropRequest { job: job() })
            .unwrap_err();
        assert!(error.to_string().contains("already applied"));
        let response = RuntimeJobService::apply(
            &ServiceContext::default(),
            ApplyRequest {
                job: job(),
                target: None,
                selection: ApplySelection::default(),
                all: true,
            },
        )
        .unwrap();
        assert!(matches!(response.outcome, MutationOutcome::AlreadyApplied));
        assert_eq!(
            std::fs::read(root.path().join("target/change")).unwrap(),
            b"staged"
        );
        assert_eq!(
            RunRecord::read(root.path()).unwrap().overlay.unwrap().state,
            OverlayState::Applied
        );
    }
}
