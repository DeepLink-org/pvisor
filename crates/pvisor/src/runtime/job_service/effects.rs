//! Drop orchestration and recorded-target authorization shared with apply_command.
use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, lock_selected_job,
};
use crate::runtime::{OverlayState, discard_overlay};
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};

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
    AlreadyDropped,
    Dropped,
}
impl RuntimeJobService {
    pub fn drop(
        context: &ServiceContext<'_>,
        request: DropRequest,
    ) -> anyhow::Result<MutationResponse> {
        let selected = Self::resolve(context, &request.job)?;
        let _job = lock_selected_job(context, &selected)?;
        let (mut record, _lease) = selected.lock_current()?;
        check_selected_record(&selected, &record)?;
        context.check(&record)?;
        record.require_stopped()?;
        let mut overlay = record
            .overlay
            .take()
            .context("this Job has no OverlayFS workspace")?;
        if pvisor_overlay_core::apply::has_pending_applies(&overlay)? {
            bail!(
                "Job {} has a pending apply; explicit reconciliation is required before drop",
                record.run_id
            );
        }
        match overlay.state {
            OverlayState::Discarded => {
                return Ok(MutationResponse {
                    job_id: record.run_id,
                    target: overlay.target,
                    outcome: MutationOutcome::AlreadyDropped,
                });
            }
            OverlayState::Applied => bail!(
                "Job {} was already applied; drop cannot undo changes written to {}",
                record.run_id,
                overlay.target.display()
            ),
            _ => {}
        }
        let generation = overlay
            .generation
            .checked_add(1)
            .context("workspace generation exhausted")?;
        discard_overlay(&mut overlay)?;
        overlay.generation = generation;
        let target = overlay.target.clone();
        record.overlay = Some(overlay);
        record.write()?;
        Ok(MutationResponse {
            job_id: record.run_id,
            target,
            outcome: MutationOutcome::Dropped,
        })
    }
}

fn is_system_root(target: &Path) -> anyhow::Result<bool> {
    match target.canonicalize() {
        Ok(path) => Ok(path == Path::new("/")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::ensure!(
                !target
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
                "recorded apply target containing '..' must already exist"
            );
            Ok(false)
        }
        Err(error) => Err(error)
            .with_context(|| format!("resolve recorded apply target {}", target.display())),
    }
}

pub(super) fn validate_recorded_apply_target(
    recorded: &Path,
    rootfs: bool,
    effective_uid: libc::uid_t,
) -> anyhow::Result<()> {
    if is_system_root(recorded)? {
        anyhow::ensure!(
            rootfs,
            "applying the system root '/' requires explicit --rootfs; review the stage first, or drop it"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::job_service::{ApplyCommandRequest, ApplyCommandResponse};
    use crate::runtime::{ApplySelection, RunRecord};
    use pvisor_overlay_core::apply::apply_overlay_selected;

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
                "merged_dir":root.join("merged"),"stage_dir":root,"auto_apply":false,"state":"staged"}
        })).unwrap();
        record.write().unwrap();
        record
    }
    fn job(record: &RunRecord) -> JobSelection {
        JobSelection {
            selector: Some(record.stage_dir()),
            storage: record.storage.clone(),
        }
    }
    fn apply(record: &RunRecord, rootfs: bool) -> ApplyCommandResponse {
        RuntimeJobService::apply_command(
            &ServiceContext::default(),
            ApplyCommandRequest {
                job: job(record),
                rootfs,
                all: true,
                selection: Default::default(),
                dry_run: false,
                expected_plan: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn system_root_requires_opt_in_and_effective_root() {
        // Authorization only: no test writes to real '/'.
        for uid in [0, 1000] {
            assert!(
                validate_recorded_apply_target(Path::new("/"), false, uid)
                    .unwrap_err()
                    .to_string()
                    .contains("explicit --rootfs")
            );
        }
        let error = validate_recorded_apply_target(Path::new("/"), true, 1000).unwrap_err();
        assert!(error.to_string().contains("effective UID 0"));
        assert!(error.to_string().contains("same-UID Job locks"));
        validate_recorded_apply_target(Path::new("/"), true, 0).unwrap();
    }

    #[test]
    fn recorded_root_aliases_require_rootfs_and_missing_targets_are_never_created() {
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("root-alias");
        std::os::unix::fs::symlink("/", &alias).unwrap();
        for path in [Path::new("/"), Path::new("/.."), alias.as_path()] {
            assert!(
                validate_recorded_apply_target(path, false, 0)
                    .unwrap_err()
                    .to_string()
                    .contains("explicit --rootfs")
            );
        }
        let target = root.path().join("missing/..");
        assert!(validate_recorded_apply_target(&target, false, 0).is_err());
        assert!(!root.path().join("missing").exists());
        assert!(
            validate_recorded_apply_target(root.path(), true, 0)
                .unwrap_err()
                .to_string()
                .contains("only valid")
        );
    }

    #[test]
    fn unauthorized_apply_leaves_stage_and_targets_unchanged() {
        for (recorded_root, rootfs, terminal) in [
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (false, true, false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut record = fixture(root.path());
            let overlay = record.overlay.as_mut().unwrap();
            if recorded_root {
                overlay.target = "/".into();
            }
            if terminal {
                overlay.state = OverlayState::Applied;
            }
            record.write().unwrap();
            std::fs::write(root.path().join("upper/change"), b"staged").unwrap();
            let before = std::fs::read(root.path().join("run.json")).unwrap();
            let response = apply(&record, rootfs);
            assert_eq!(response.status, "blocked", "{:?}", response.blockers);
            assert!(response.execution.is_none());
            let reason = response.blockers.join(" ");
            assert!(
                reason.contains(if recorded_root && !rootfs {
                    "completion-bound launch contract"
                } else if recorded_root && unsafe { libc::geteuid() } != 0 {
                    "effective UID 0"
                } else if recorded_root {
                    "root-controlled storage"
                } else {
                    "only valid"
                }),
                "{reason}"
            );
            assert_eq!(std::fs::read(root.path().join("run.json")).unwrap(), before);
            assert_eq!(
                std::fs::read(root.path().join("upper/change")).unwrap(),
                b"staged"
            );
            assert!(!root.path().join("target/change").exists());
            assert!(!root.path().join("apply-ledger.json").exists());
        }
    }

    #[test]
    fn rootfs_storage_rejects_user_or_shared_ancestors() {
        let root = tempfile::tempdir().unwrap();
        assert!(validate_rootfs_storage(root.path()).is_err());
    }

    #[test]
    fn pending_terminal_apply_blocks_runtime_and_core_recovery_keeps_original_fence() {
        use pvisor_core::overlay::ApplyRecordState;
        use pvisor_overlay_core::apply::{
            has_pending_applies, load_apply_records, recover_pending_applies,
        };
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        std::fs::write(root.path().join("upper/change"), b"staged").unwrap();
        let mut overlay = record.overlay.clone().unwrap();
        let lowers = vec![overlay.target.clone()];
        apply_overlay_selected(&mut overlay, &lowers, &ApplySelection::default()).unwrap();
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
        let error = RuntimeJobService::drop(
            &ServiceContext::default(),
            DropRequest { job: job(&record) },
        )
        .unwrap_err();
        assert!(error.to_string().contains("pending apply"));
        let entries = root.path().join("preimages/entries");
        std::fs::create_dir_all(&entries).unwrap();
        let corrupt = entries.join("invalid.json");
        std::fs::write(&corrupt, b"invalid").unwrap();
        let before = std::fs::read(&ledger_path).unwrap();
        let blocked = apply(&record, false);
        assert_eq!(blocked.status, "blocked");
        assert!(blocked.blockers.join(" ").contains("pending apply"));
        assert_eq!(before, std::fs::read(&ledger_path).unwrap());
        assert!(recover_pending_applies(&mut overlay, &lowers).is_err());
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
        // Removing corruption is not authorization for runtime to recover implicitly.
        assert_eq!(apply(&record, false).status, "blocked");
        recover_pending_applies(&mut overlay, &lowers).unwrap();
        assert!(!has_pending_applies(&overlay).unwrap());
        let committed = load_apply_records(root.path()).unwrap();
        assert_eq!(committed[0].state, ApplyRecordState::Committed);
        assert_eq!(committed[0].overlay_generation, 7);
        assert_eq!(
            RunRecord::read(root.path())
                .unwrap()
                .overlay
                .unwrap()
                .generation,
            8
        );
        for _ in 0..2 {
            assert_eq!(apply(&record, false).status, "no_changes");
        }
        assert_eq!(
            RunRecord::read(root.path())
                .unwrap()
                .overlay
                .unwrap()
                .generation,
            8
        );
        assert_eq!(
            std::fs::read(root.path().join("target/change")).unwrap(),
            b"staged"
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
        let current = RunRecord::read(root.path()).unwrap();
        assert_eq!(
            current.overlay.as_ref().unwrap().state,
            OverlayState::Applied
        );
        assert_eq!(current.overlay.as_ref().unwrap().generation, 8);
        let error = RuntimeJobService::drop(
            &ServiceContext::default(),
            DropRequest { job: job(&record) },
        )
        .unwrap_err();
        assert!(error.to_string().contains("already applied"));
        let response = apply(&record, false);
        assert_eq!(response.status, "no_changes");
        assert!(response.execution.is_none());
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
