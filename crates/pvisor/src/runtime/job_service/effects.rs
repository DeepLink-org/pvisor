use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, lock_selected_job,
};
use crate::runtime::{ApplySelection, OverlayState, apply_overlay_selected, discard_overlay};
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};

pub struct ApplyRequest {
    pub job: JobSelection,
    pub target: Option<PathBuf>,
    /// Explicitly authorize applying a recorded `/` target as effective UID 0.
    /// Target overrides (even `/`) are forbidden in this mode; non-root targets
    /// reject it. Use a root-created stage in root-controlled storage: existing
    /// same-UID Job locks are not bypassed for sudo or `SUDO_UID`.
    /// This is not an atomic system upgrade. Filtering package files separately
    /// from package metadata can leave the system/package database inconsistent.
    pub rootfs: bool,
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
        if !request.all && request.selection.is_all() {
            bail!("apply requires --all or an explicit --path, --include, or --exclude selection");
        }
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
            request.rootfs,
            Some(&request.selection),
        )
    }
    pub fn drop(
        context: &ServiceContext<'_>,
        request: DropRequest,
    ) -> anyhow::Result<MutationResponse> {
        mutate(context, request.job, false, None, false, None)
    }
}
fn mutate(
    context: &ServiceContext<'_>,
    job: JobSelection,
    apply: bool,
    target: Option<&Path>,
    rootfs: bool,
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
    let stage = record.stage_dir();
    let mut overlay = record
        .overlay
        .take()
        .context("this Job has no OverlayFS workspace")?;
    // Authorization must precede pending-apply recovery, which can write the
    // recorded target even when the workspace already reports Applied.
    if apply {
        validate_apply_target(&overlay.target, target, rootfs, unsafe { libc::geteuid() })?;
        if rootfs {
            validate_rootfs_storage(&stage)?;
        }
    }
    let _workspace_owner = if apply {
        super::super::owned_workspace::apply_owner(&record, &overlay)?
    } else {
        None
    };
    if _workspace_owner.is_some() && target.is_some() {
        bail!("owned workspace apply does not support overriding its live target");
    }
    // The final lower may be the Run-owned snapshot of the target. It is
    // still the base workspace, whereas any preceding lower is a composed
    // read-only layer whose changes cannot be applied to that workspace.
    let base_snapshot = record.storage.join(".overlay-lowers");
    if apply
        && _workspace_owner.is_none()
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
        if let Some(target) = target {
            let target = resolve_apply_target(target, &stage)?;
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

fn is_system_root(target: &Path) -> anyhow::Result<bool> {
    match target.canonicalize() {
        Ok(path) => Ok(path == Path::new("/")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Do not create `missing/..` while discovering that it denotes `/`.
            anyhow::ensure!(
                !target
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
                "apply target containing '..' must already exist; use an existing canonical target"
            );
            Ok(false)
        }
        Err(error) => {
            Err(error).with_context(|| format!("resolve apply target {}", target.display()))
        }
    }
}

pub(super) fn validate_apply_target(
    recorded: &Path,
    target: Option<&Path>,
    rootfs: bool,
    effective_uid: libc::uid_t,
) -> anyhow::Result<()> {
    if is_system_root(recorded)? {
        anyhow::ensure!(
            rootfs,
            "applying the system root '/' requires explicit --rootfs; review the stage first, or drop it"
        );
        anyhow::ensure!(
            target.is_none(),
            "--rootfs does not support --target overrides (including --target /); use the recorded '/' target"
        );
        anyhow::ensure!(
            effective_uid == 0,
            "--rootfs requires effective UID 0 and a root-created stage; sudo apply of another user's stage is unsupported by same-UID Job locks; recreate and review the stage as root in root-controlled storage"
        );
    } else {
        anyhow::ensure!(
            !rootfs,
            "--rootfs is only valid for a Job whose recorded target is '/'; it cannot turn a workspace stage into a system-root stage"
        );
        if let Some(target) = target {
            anyhow::ensure!(
                !is_system_root(target)?,
                "--target / (including aliases) is forbidden for ordinary apply; create and review a rootfs stage as root and apply it with --rootfs without --target"
            );
        }
    }
    Ok(())
}

pub(super) fn validate_rootfs_storage(stage: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let stage = stage
        .canonicalize()
        .context("resolve rootfs stage storage")?;
    for directory in stage.ancestors() {
        let metadata = std::fs::metadata(directory)?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "--rootfs requires a root-created stage in root-controlled storage: {} must be root-owned and not group/world writable; sudo apply of a user stage is unsupported; recreate and review the stage as root in private storage",
            directory.display()
        );
    }
    Ok(())
}

fn resolve_apply_target(target: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !is_system_root(target)?,
        "--target / (including aliases) is forbidden; rootfs apply must use its recorded target without --target"
    );
    std::fs::create_dir_all(target)
        .with_context(|| format!("create apply target {}", target.display()))?;
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve apply target {}", target.display()))?;
    anyhow::ensure!(
        target != Path::new("/"),
        "apply target override resolved to system root; refusing apply"
    );
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
    fn system_root_requires_opt_in_and_effective_root_without_overrides() {
        // Only exercise the authorization gate: no apply ever targets real `/`.
        for uid in [0, 1000] {
            let error = validate_apply_target(Path::new("/"), None, false, uid).unwrap_err();
            assert!(error.to_string().contains("explicit --rootfs"));
        }
        let error = validate_apply_target(Path::new("/"), None, true, 1000).unwrap_err();
        assert!(error.to_string().contains("effective UID 0"));
        assert!(error.to_string().contains("same-UID Job locks"));
        validate_apply_target(Path::new("/"), None, true, 0).unwrap();
        for target in ["/", "/tmp/elsewhere"] {
            let error = validate_apply_target(Path::new("/"), Some(Path::new(target)), true, 0)
                .unwrap_err();
            assert!(error.to_string().contains("does not support --target"));
        }
    }

    #[test]
    fn ordinary_apply_rejects_rootfs_and_root_target_aliases_without_creating_paths() {
        let root = tempfile::tempdir().unwrap();
        let recorded = root.path().join("target");
        std::fs::create_dir(&recorded).unwrap();
        let alias = root.path().join("root-alias");
        std::os::unix::fs::symlink("/", &alias).unwrap();
        let error = validate_apply_target(&recorded, None, true, 0).unwrap_err();
        assert!(error.to_string().contains("only valid"));
        for target in [Path::new("/"), Path::new("/.."), alias.as_path()] {
            let error = validate_apply_target(&recorded, Some(target), false, 0).unwrap_err();
            assert!(error.to_string().contains("forbidden"));
            assert!(resolve_apply_target(target, root.path()).is_err());
        }
        let missing_parent = root.path().join("missing/..");
        assert!(validate_apply_target(&recorded, Some(&missing_parent), false, 0).is_err());
        assert!(resolve_apply_target(&missing_parent, root.path()).is_err());
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn unauthorized_apply_leaves_stage_and_targets_unchanged_even_before_recovery() {
        for (recorded_root, rootfs, override_target, terminal) in [
            (true, false, None, false),
            (true, false, None, true),
            (true, true, Some("/"), false),
            (true, true, Some("missing-target"), false),
            (false, false, Some("/"), false),
            (false, true, Some("missing-target"), false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut record = fixture(root.path());
            let overlay = record.overlay.as_mut().unwrap();
            if recorded_root {
                overlay.target = PathBuf::from("/");
            }
            if terminal {
                overlay.state = OverlayState::Applied;
            }
            record.write().unwrap();
            std::fs::write(root.path().join("upper/change"), b"staged").unwrap();
            let before = std::fs::read(root.path().join("run.json")).unwrap();
            let target = override_target.map(|target| {
                if target == "/" {
                    PathBuf::from(target)
                } else {
                    root.path().join(target)
                }
            });
            let error = RuntimeJobService::apply(
                &ServiceContext::default(),
                ApplyRequest {
                    job: JobSelection {
                        selector: Some(root.path().to_path_buf()),
                        storage: root.path().to_path_buf(),
                    },
                    target,
                    rootfs,
                    all: true,
                    selection: ApplySelection::default(),
                },
            )
            .unwrap_err();
            assert!(
                error.to_string().contains(if recorded_root && !rootfs {
                    "explicit --rootfs"
                } else if recorded_root {
                    "does not support --target"
                } else if rootfs {
                    "only valid"
                } else {
                    "forbidden"
                }),
                "{error:#}"
            );
            assert_eq!(std::fs::read(root.path().join("run.json")).unwrap(), before);
            assert_eq!(
                std::fs::read(root.path().join("upper/change")).unwrap(),
                b"staged"
            );
            assert!(!root.path().join("missing-target").exists());
            assert!(!root.path().join("target/change").exists());
            assert!(!root.path().join("apply-ledger.json").exists());
        }
    }

    #[test]
    fn rootfs_storage_rejects_user_or_shared_ancestors() {
        let root = tempfile::tempdir().unwrap();
        // Even in a root-run test, /tmp is not private root-controlled storage.
        assert!(validate_rootfs_storage(root.path()).is_err());
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
        // Model terminal publication preceding ledger commit: the target is
        // already applied, but the ledger still requires recovery.
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
            rootfs: false,
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
                rootfs: false,
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
