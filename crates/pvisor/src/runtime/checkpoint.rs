//! Filesystem-backed logical checkpoints for Agent Runs.

use crate::runtime::{OverlayState, RunRecord, restore_overlay_upper, snapshot_overlay_upper};
use crate::unix_now_ms;
use crate::util::write_private_json;
use pvisor_journal::api::{DurableFiles, Persistence};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const CHECKPOINTS_DIR: &str = "checkpoints";
pub(crate) const SOURCE_CHECKPOINT_PIN: &str = "source-checkpoint.json";
const CHECKPOINT_FILENAME: &str = "checkpoint.json";
const CHECKPOINT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointConsistency {
    /// The process tree was no longer running when the filesystem was copied.
    Stopped,
    /// Captured at a live AgentCtl cooperative quiescence barrier.
    AgentQuiesced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalCheckpoint {
    pub schema_version: u32,
    pub kind: WorkspaceCheckpointKind,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    pub source_attempt_id: Option<String>,
    pub workspace_generation: u64,
    pub checkpoint_id: String,
    pub run_id: String,
    pub created_at_unix_ms: u64,
    pub consistency: CheckpointConsistency,
    pub source_stage: PathBuf,
    pub upper_snapshot: PathBuf,
    pub preimages_snapshot: PathBuf,
    pub target: PathBuf,
    pub lower_dirs: Vec<PathBuf>,
    pub protect_target: bool,
    pub access_policy: pvisor_core::overlay::FileAccessPolicy,
}

/// Explicit discriminator prevents execution manifests from being read as workspace checkpoints.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCheckpointKind {
    Workspace,
}

impl LogicalCheckpoint {
    pub fn manifest_path(&self) -> PathBuf {
        self.upper_snapshot
            .parent()
            .unwrap_or(&self.source_stage)
            .join(CHECKPOINT_FILENAME)
    }

    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let manifest = if path.is_dir() {
            path.join(CHECKPOINT_FILENAME)
        } else {
            path.to_path_buf()
        };
        let checkpoint: Self = serde_json::from_slice(&fs::read(&manifest)?)?;
        anyhow::ensure!(
            checkpoint.schema_version == CHECKPOINT_SCHEMA_VERSION,
            "unsupported logical checkpoint schema {}; expected {}",
            checkpoint.schema_version,
            CHECKPOINT_SCHEMA_VERSION
        );
        Ok(checkpoint)
    }
}

pub fn create_logical_checkpoint(
    record: &RunRecord,
    requested_id: Option<&str>,
) -> anyhow::Result<LogicalCheckpoint> {
    if let Some(id) = requested_id {
        validate_checkpoint_id(id)?;
    }
    let (current, _lease) = record.lock_current()?;
    create_stopped_checkpoint_locked(&current, requested_id)
}

/// Caller retains the Job lease through publication and any branch pin/copy.
pub fn create_stopped_checkpoint_locked(
    record: &RunRecord,
    requested_id: Option<&str>,
) -> anyhow::Result<LogicalCheckpoint> {
    record.require_stopped()?;
    create_checkpoint(record, requested_id, CheckpointConsistency::Stopped)
}

pub(crate) fn create_agent_quiesced_checkpoint(
    record: &RunRecord,
    checkpoint_id: &str,
) -> anyhow::Result<LogicalCheckpoint> {
    create_checkpoint(
        record,
        Some(checkpoint_id),
        CheckpointConsistency::AgentQuiesced,
    )
}

fn create_checkpoint(
    record: &RunRecord,
    requested_id: Option<&str>,
    consistency: CheckpointConsistency,
) -> anyhow::Result<LogicalCheckpoint> {
    let overlay = record
        .overlay
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Run {} has no OverlayFS stage", record.run_id))?;
    let expected_state = match consistency {
        CheckpointConsistency::Stopped => OverlayState::Staged,
        CheckpointConsistency::AgentQuiesced => OverlayState::Active,
    };
    anyhow::ensure!(
        overlay.state == expected_state,
        "Run {} filesystem is {:?}; {:?} checkpoint requires {:?}",
        record.run_id,
        overlay.state,
        consistency,
        expected_state
    );
    let checkpoint_id = requested_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("checkpoint-{}", uuid::Uuid::new_v4().simple()));
    validate_checkpoint_id(&checkpoint_id)?;
    let journal = record.stage_dir().join("preimages");
    if consistency == CheckpointConsistency::Stopped {
        pvisor_overlay_core::stage::require_sealed(&journal)?;
    }
    let root = record
        .stage_dir()
        .join(CHECKPOINTS_DIR)
        .join(&checkpoint_id);
    let parent = root.parent().expect("checkpoint has a parent");
    Persistence::create_dir_all_durable(parent)?;
    anyhow::ensure!(!root.exists(), "checkpoint {checkpoint_id} already exists");
    let pending = parent.join(format!(".pending-{}", uuid::Uuid::new_v4().simple()));
    fs::DirBuilder::new().mode(0o700).create(&pending)?;
    let result = (|| -> anyhow::Result<LogicalCheckpoint> {
        Persistence::sync_directory(parent)?;
        let upper_snapshot = root.join("upper");
        snapshot_overlay_upper(overlay, &pending.join("upper"))?;
        let preimages_snapshot = root.join("preimages");
        let journal = record.stage_dir().join("preimages");
        if journal.is_dir() {
            pvisor_overlay_core::stage::sync_journal(&journal)?;
            restore_overlay_upper(&journal, &pending.join("preimages"))?;
        } else {
            Persistence::create_dir_all_durable(&pending.join("preimages"))?;
        }
        pvisor_overlay_core::stage::seal(&pending.join("upper"), &pending.join("preimages"))?;
        let checkpoint = LogicalCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            kind: WorkspaceCheckpointKind::Workspace,
            source_attempt_id: record.attempt_id.clone(),
            workspace_generation: overlay.generation,
            checkpoint_id,
            run_id: record.run_id.clone(),
            created_at_unix_ms: unix_now_ms(),
            consistency,
            source_stage: record.stage_dir(),
            upper_snapshot,
            preimages_snapshot,
            target: overlay.target.clone(),
            lower_dirs: if record.overlay_lowers.is_empty() {
                vec![overlay.target.clone()]
            } else {
                record.overlay_lowers.clone()
            },
            protect_target: overlay.protect_target,
            access_policy: overlay.access_policy.clone(),
        };
        write_private_json(&pending.join(CHECKPOINT_FILENAME), &checkpoint)?;
        fs::rename(&pending, &root)?;
        Persistence::sync_directory(parent)?;
        Ok(checkpoint)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    result
}

/// Committed workspace checkpoints only. Corrupt committed records are errors,
/// whereas unpublished transaction directories are never exposed as savepoints.
pub fn list_checkpoints(record: &RunRecord) -> anyhow::Result<Vec<LogicalCheckpoint>> {
    let root = record.stage_dir().join(CHECKPOINTS_DIR);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut checkpoints = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        anyhow::ensure!(
            entry.file_type()?.is_dir(),
            "invalid checkpoint entry {}",
            entry.path().display()
        );
        let checkpoint = LogicalCheckpoint::read(&entry.path())?;
        validate_owned_checkpoint(record, &checkpoint, &entry.path())?;
        checkpoints.push(checkpoint);
    }
    checkpoints.sort_by(|a, b| {
        a.created_at_unix_ms
            .cmp(&b.created_at_unix_ms)
            .then(a.checkpoint_id.cmp(&b.checkpoint_id))
    });
    Ok(checkpoints)
}

fn validate_owned_checkpoint(
    record: &RunRecord,
    checkpoint: &LogicalCheckpoint,
    root: &Path,
) -> anyhow::Result<()> {
    validate_checkpoint_id(&checkpoint.checkpoint_id)?;
    anyhow::ensure!(
        checkpoint.run_id == record.run_id
            && root.file_name().and_then(|s| s.to_str()) == Some(&checkpoint.checkpoint_id),
        "checkpoint identity or Job ownership mismatch"
    );
    let root = root.canonicalize()?;
    anyhow::ensure!(
        checkpoint.upper_snapshot.is_dir()
            && checkpoint.preimages_snapshot.is_dir()
            && checkpoint
                .upper_snapshot
                .file_name()
                .and_then(|s| s.to_str())
                == Some("upper")
            && checkpoint
                .preimages_snapshot
                .file_name()
                .and_then(|s| s.to_str())
                == Some("preimages")
            && checkpoint
                .upper_snapshot
                .parent()
                .expect("upper parent")
                .canonicalize()?
                == root
            && checkpoint
                .preimages_snapshot
                .parent()
                .expect("preimages parent")
                .canonicalize()?
                == root
            && checkpoint.upper_snapshot.canonicalize()? == root.join("upper")
            && checkpoint.preimages_snapshot.canonicalize()? == root.join("preimages"),
        "checkpoint files are not contained in their published directory"
    );
    Ok(())
}

pub fn resolve_checkpoint(record: &RunRecord, id: &str) -> anyhow::Result<LogicalCheckpoint> {
    validate_checkpoint_id(id)?;
    let checkpoints = list_checkpoints(record)?;
    if let Some(exact) = checkpoints.iter().find(|cp| cp.checkpoint_id == id) {
        return Ok(exact.clone());
    }
    let mut candidates = checkpoints.into_iter().filter(|cp| {
        cp.checkpoint_id.starts_with(id)
            || cp
                .checkpoint_id
                .strip_prefix("checkpoint-")
                .is_some_and(|suffix| suffix.starts_with(id))
    });
    let checkpoint = candidates
        .next()
        .ok_or_else(|| anyhow::anyhow!("Job {} has no checkpoint matching {id}", record.run_id))?;
    anyhow::ensure!(
        candidates.next().is_none(),
        "ambiguous checkpoint prefix {id}"
    );
    Ok(checkpoint)
}

/// A hard link is a durable branch retention reference, including when the
/// child fails before its runner starts. Job lease serializes pin vs delete.
pub fn pin_checkpoint(checkpoint: &LogicalCheckpoint, child_stage: &Path) -> anyhow::Result<()> {
    fs::hard_link(checkpoint.manifest_path(), child_stage.join(SOURCE_CHECKPOINT_PIN))
        .map_err(|e| anyhow::anyhow!("retain source checkpoint: {e}; workspace branches currently require a stage on the same filesystem"))?;
    Persistence::sync_directory(child_stage)?;
    Ok(())
}

pub fn checkpoint_branch_refs(checkpoint: &LogicalCheckpoint) -> anyhow::Result<u64> {
    Ok(fs::metadata(checkpoint.manifest_path())?
        .nlink()
        .saturating_sub(1))
}

/// In-memory read-only projection, never a replacement Job/Attempt record.
pub fn workspace_view(
    record: &RunRecord,
    checkpoint: &LogicalCheckpoint,
) -> anyhow::Result<RunRecord> {
    let mut view = record.clone();
    let overlay = view
        .overlay
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("Job has no staged filesystem"))?;
    overlay.upper.upper_dir = checkpoint.upper_snapshot.clone();
    overlay.target = checkpoint.target.clone();
    overlay.access_policy = checkpoint.access_policy.clone();
    overlay.state = OverlayState::Staged;
    overlay.generation = checkpoint.workspace_generation;
    view.overlay_lowers = checkpoint.lower_dirs.clone();
    Ok(view)
}

pub fn latest_logical_checkpoint(record: &RunRecord) -> anyhow::Result<LogicalCheckpoint> {
    list_checkpoints(record)?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("Run {} has no logical checkpoints", record.run_id))
}

pub fn restore_logical_checkpoint(
    checkpoint: &LogicalCheckpoint,
    destination_upper: &Path,
    destination_preimages: &Path,
) -> anyhow::Result<()> {
    let sources = [&checkpoint.upper_snapshot, &checkpoint.preimages_snapshot];
    pvisor_overlay_core::stage::require_sealed(&checkpoint.preimages_snapshot)?;
    let sources = sources
        .map(|source| source.canonicalize())
        .into_iter()
        .collect::<std::io::Result<Vec<_>>>()?;
    anyhow::ensure!(
        sources.iter().all(|source| source.is_dir()),
        "checkpoint snapshots must be directories"
    );
    let destinations = [
        absolute_candidate(destination_upper)?,
        absolute_candidate(destination_preimages)?,
    ];
    let overlaps = |left: &Path, right: &Path| left.starts_with(right) || right.starts_with(left);
    anyhow::ensure!(
        !overlaps(&destinations[0], &destinations[1])
            && sources.iter().all(|source| destinations
                .iter()
                .all(|destination| !overlaps(source, destination))),
        "checkpoint sources and restore destinations must not overlap"
    );
    restore_overlay_upper(&sources[0], &destinations[0])?;
    restore_overlay_upper(&sources[1], &destinations[1])?;
    Ok(())
}

fn absolute_candidate(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow::anyhow!("destination upper has no parent"))?
        .canonicalize()?;
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination upper has no final component"))?;
    Ok(parent.join(name))
}

pub(crate) fn validate_checkpoint_id(id: &str) -> anyhow::Result<()> {
    let trimmed = id.trim();
    anyhow::ensure!(
        !trimmed.is_empty() && trimmed == id && id.len() <= 256,
        "checkpoint id must contain 1..256 bytes without surrounding whitespace"
    );
    anyhow::ensure!(
        !trimmed.starts_with('.')
            && !trimmed.chars().any(char::is_control)
            && !trimmed.contains('/')
            && !trimmed.contains('\\')
            && !id.contains('\0'),
        "checkpoint id must be one path-safe segment"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::job_service::{RuntimeJobService, ServiceContext};
    use crate::runtime::{OverlayRecord, OverlayUpper, RunLineage};
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn live_checkpoint_seals_its_copy_and_never_seals_the_running_source() {
        use pvisor_overlay_core::{OverlayCore, OverlayLayout, stage};
        let temp = tempfile::tempdir().unwrap();
        let mut record = stopped_record(temp.path());
        let overlay = record.overlay.as_mut().unwrap();
        overlay.state = OverlayState::Active;
        let target = overlay.target.clone();
        fs::write(target.join("value"), b"original").unwrap();
        let journal = temp.path().join("preimages");
        stage::begin(&journal, Default::default()).unwrap();
        let core = OverlayCore::new_for_layout_with_compact_preimages(
            OverlayLayout::new(vec![target.clone()], target.clone()).unwrap(),
            overlay.upper.upper_dir.clone(),
            Some(overlay.upper.work_dir.clone()),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        fs::write(
            core.copy_up(Path::new("value")).unwrap(),
            b"checkpoint version",
        )
        .unwrap();
        let cp = create_agent_quiesced_checkpoint(&record, "live-boundary").unwrap();
        stage::require_sealed(&cp.preimages_snapshot).unwrap();
        assert!(stage::require_sealed(&journal).is_err());
        fs::write(
            core.copy_up(Path::new("value")).unwrap(),
            b"continued version",
        )
        .unwrap();
        assert_eq!(
            fs::read(cp.upper_snapshot.join("value")).unwrap(),
            b"checkpoint version"
        );
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        restore_logical_checkpoint(&cp, &child.join("upper"), &child.join("preimages")).unwrap();
        stage::require_sealed(&child.join("preimages")).unwrap();
        stage::begin(&child.join("preimages"), Default::default()).unwrap();
        assert!(stage::require_sealed(&child.join("preimages")).is_err());
        record.overlay.as_mut().unwrap().state = OverlayState::Staged;
        assert!(
            create_checkpoint(&record, Some("unconfirmed"), CheckpointConsistency::Stopped)
                .is_err()
        );
    }

    fn stopped_record(root: &Path) -> RunRecord {
        let target = root.join("target");
        let upper = root.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        RunRecord {
            attempt_id: None,
            schema_version: 1,
            run_id: "run-source".into(),
            parent_run_id: None,
            task_id: None,
            session_id: "run-source".into(),
            agent: "codex".into(),
            pid: 0,
            command: vec!["codex".into()],
            executor: None,
            executor_plan: None,
            state: crate::RunRecordState::Completed,
            started_at_unix_ms: 1,
            finished_at_unix_ms: Some(2),
            storage: root.to_path_buf(),
            workspace: None,
            overlaynet_listen: None,
            network_interception: None,
            network_interception_metrics: None,
            filesystem_observation: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            network_policy: None,
            environment: Default::default(),
            resource_limits: Default::default(),
            overlay: Some(OverlayRecord {
                id: "run-source".into(),
                generation: 0,
                target: target.clone(),
                baseline_lower: None,
                upper: OverlayUpper {
                    upper_dir: upper,
                    work_dir: root.join("work"),
                },
                merged_dir: root.join("merged"),
                stage_dir: root.to_path_buf(),
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            }),
            overlay_lowers: vec![target],
            lineage: Some(RunLineage {
                parent_run_id: "parent".into(),
                checkpoint_id: "parent-cp".into(),
            }),
            orchestration: Default::default(),
            operation: None,
        }
    }

    #[test]
    fn stopped_checkpoint_preserves_links_and_can_seed_a_fork() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = stopped_record(temp.path());
        record.overlay.as_mut().unwrap().protect_target = true;
        record.overlay.as_mut().unwrap().access_policy =
            pvisor_core::FileAccessPolicy::new(vec!["**/.ssh".into()], vec![]).unwrap();
        let upper = record.overlay.as_ref().unwrap().upper.path();
        fs::write(upper.join("one"), b"value").unwrap();
        fs::hard_link(upper.join("one"), upper.join("two")).unwrap();
        std::os::unix::fs::symlink("one", upper.join("link")).unwrap();

        record.write().unwrap();
        let checkpoint = create_logical_checkpoint(&record, Some("before-refactor")).unwrap();
        assert!(checkpoint.protect_target);
        assert_eq!(checkpoint.access_policy.deny(), ["**/.ssh"]);
        assert_eq!(
            LogicalCheckpoint::read(&checkpoint.manifest_path())
                .unwrap()
                .access_policy,
            checkpoint.access_policy
        );
        let restored = temp.path().join("restored");
        restore_logical_checkpoint(
            &checkpoint,
            &restored,
            &temp.path().join("restored-preimages"),
        )
        .unwrap();

        assert_eq!(fs::read(restored.join("one")).unwrap(), b"value");
        assert_eq!(
            fs::read_link(restored.join("link")).unwrap(),
            PathBuf::from("one")
        );
        assert_eq!(
            fs::metadata(restored.join("one")).unwrap().ino(),
            fs::metadata(restored.join("two")).unwrap().ino()
        );
        assert_eq!(
            latest_logical_checkpoint(&record).unwrap().checkpoint_id,
            "before-refactor"
        );
    }

    #[test]
    fn checkpoint_ids_cannot_escape_the_stage() {
        let temp = tempfile::tempdir().unwrap();
        let record = stopped_record(temp.path());
        assert!(create_logical_checkpoint(&record, Some("../escape")).is_err());
        assert!(create_logical_checkpoint(&record, Some(".pending-user")).is_err());
    }

    #[test]
    fn missing_lease_does_not_make_an_unconfirmed_attempt_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = stopped_record(temp.path());
        record.state = crate::RunRecordState::Running;
        record.write().unwrap();
        assert!(
            create_logical_checkpoint(&record, None)
                .unwrap_err()
                .to_string()
                .contains("EXECUTION_UNKNOWN")
        );
        assert!(!record.stage_dir().join(CHECKPOINTS_DIR).exists());
    }

    #[test]
    fn branch_pin_survives_reopen_and_prevents_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let record = stopped_record(temp.path());
        record.write().unwrap();
        fs::write(
            record.overlay.as_ref().unwrap().upper.path().join("file"),
            b"original",
        )
        .unwrap();
        let cp = create_logical_checkpoint(&record, Some("branch-point")).unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        pin_checkpoint(&cp, &child).unwrap();
        restore_logical_checkpoint(&cp, &child.join("upper"), &child.join("preimages")).unwrap();
        fs::write(child.join("upper/file"), b"child").unwrap();
        assert_eq!(
            fs::read(cp.upper_snapshot.join("file")).unwrap(),
            b"original"
        );
        let reopened = RunRecord::read(temp.path()).unwrap();
        assert_eq!(checkpoint_branch_refs(&cp).unwrap(), 1);
        let context = ServiceContext::default();
        assert!(
            RuntimeJobService::delete_selected_workspace_checkpoint(
                &context,
                &reopened,
                "branch-point",
            )
            .unwrap_err()
            .to_string()
            .contains("CHECKPOINT_REFERENCED")
        );
        fs::remove_file(child.join(SOURCE_CHECKPOINT_PIN)).unwrap();
        RuntimeJobService::delete_selected_workspace_checkpoint(
            &context,
            &reopened,
            "branch-point",
        )
        .unwrap();
        assert_eq!(fs::read(child.join("upper/file")).unwrap(), b"child");
    }

    #[test]
    fn committed_corruption_and_ambiguous_prefix_are_not_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let record = stopped_record(temp.path());
        record.write().unwrap();
        create_logical_checkpoint(&record, Some("checkpoint-abcdef1")).unwrap();
        create_logical_checkpoint(&record, Some("checkpoint-abcdef2")).unwrap();
        assert!(
            resolve_checkpoint(&record, "abcdef")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert_eq!(
            resolve_checkpoint(&record, "abcdef1")
                .unwrap()
                .checkpoint_id,
            "checkpoint-abcdef1"
        );
        fs::create_dir(
            record
                .stage_dir()
                .join(CHECKPOINTS_DIR)
                .join(".pending-abandoned"),
        )
        .unwrap();
        assert_eq!(list_checkpoints(&record).unwrap().len(), 2);
        fs::write(
            record
                .stage_dir()
                .join(CHECKPOINTS_DIR)
                .join("checkpoint-abcdef1/checkpoint.json"),
            b"broken",
        )
        .unwrap();
        assert!(list_checkpoints(&record).is_err());
    }

    #[test]
    fn workspace_manifests_require_current_schema_and_explicit_fields() {
        let temp = tempfile::tempdir().unwrap();
        let record = stopped_record(temp.path());
        record.write().unwrap();
        let cp = create_logical_checkpoint(&record, None).unwrap();
        for field in [
            "kind",
            "source_attempt_id",
            "workspace_generation",
            "lower_dirs",
            "protect_target",
            "access_policy",
        ] {
            let mut json = serde_json::to_value(&cp).unwrap();
            json.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<LogicalCheckpoint>(json).is_err(),
                "{field}"
            );
        }
        let mut json = serde_json::to_value(&cp).unwrap();
        json["kind"] = serde_json::json!("execution");
        assert!(serde_json::from_value::<LogicalCheckpoint>(json).is_err());
        let mut json = serde_json::to_value(&cp).unwrap();
        json["schema_version"] = serde_json::json!(2);
        fs::write(cp.manifest_path(), serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(LogicalCheckpoint::read(&cp.manifest_path()).is_err());
    }

    #[test]
    fn fork_preserves_read_observation_before_any_upper_mutation() {
        use pvisor_overlay_core::{OverlayCore, load_preimages};
        let temp = tempfile::tempdir().unwrap();
        let parent = stopped_record(temp.path());
        let overlay = parent.overlay.as_ref().unwrap();
        let target = overlay.target.clone();
        fs::write(target.join("value"), b"original").unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            overlay.upper.path().to_path_buf(),
            Some(temp.path().join("work")),
            vec![],
            Some(temp.path().join("preimages")),
        )
        .unwrap();
        core.observe_read(Path::new("value")).unwrap();
        assert!(!overlay.upper.path().join("value").exists());
        drop(core);
        parent.write().unwrap();
        let checkpoint = create_logical_checkpoint(&parent, Some("read-baseline")).unwrap();
        fs::write(target.join("value"), b"host edit").unwrap();
        let stage = temp.path().join("child");
        let upper = stage.join("upper");
        let journal = stage.join("preimages");
        fs::create_dir(&stage).unwrap();
        restore_logical_checkpoint(&checkpoint, &upper, &journal).unwrap();
        assert_eq!(
            load_preimages(&journal).unwrap(),
            load_preimages(&checkpoint.preimages_snapshot).unwrap()
        );
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(stage.join("work")),
            vec![],
            Some(journal),
        )
        .unwrap();
        fs::write(
            core.copy_up(Path::new("value")).unwrap(),
            b"agent edit based on original",
        )
        .unwrap();
        drop(core);
        let mut child = parent.overlay.unwrap();
        child.id = "child".into();
        child.stage_dir = stage.clone();
        child.upper = OverlayUpper {
            upper_dir: upper,
            work_dir: stage.join("work"),
        };
        assert!(crate::runtime::overlay::apply_overlay(&mut child).is_err());
        assert_eq!(fs::read(target.join("value")).unwrap(), b"host edit");
    }

    #[test]
    fn fork_preserves_conflict_baselines_and_validates_before_replacing() {
        use pvisor_overlay_core::{OverlayCore, load_preimages, preimage_journal_is_complete};
        let temp = tempfile::tempdir().unwrap();
        let mut parent = stopped_record(temp.path());
        let overlay = parent.overlay.as_ref().unwrap();
        let target = overlay.target.clone();
        let upper = overlay.upper.path().to_path_buf();
        fs::write(target.join("value"), b"original").unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(temp.path().join("work")),
            vec![],
            Some(temp.path().join("preimages")),
        )
        .unwrap();
        core.copy_up(Path::new("value")).unwrap();
        fs::write(upper.join("value"), b"staged").unwrap();
        parent.write().unwrap();
        let checkpoint = create_logical_checkpoint(&parent, Some("baseline")).unwrap();
        assert!(create_logical_checkpoint(&parent, Some("baseline")).is_err());
        fs::write(target.join("value"), b"external-edit").unwrap();
        assert!(crate::runtime::overlay::apply_overlay(parent.overlay.as_mut().unwrap()).is_err());
        let stage = temp.path().join("child");
        fs::create_dir(&stage).unwrap();
        let upper = stage.join("upper");
        let journal = stage.join("preimages");
        restore_logical_checkpoint(&checkpoint, &upper, &journal).unwrap();
        assert!(preimage_journal_is_complete(&journal));
        assert_eq!(
            load_preimages(&journal).unwrap(),
            load_preimages(&checkpoint.preimages_snapshot).unwrap()
        );
        let mut child = parent.overlay.unwrap();
        child.id = "child".into();
        child.stage_dir = stage.clone();
        child.upper = OverlayUpper {
            upper_dir: upper.clone(),
            work_dir: stage.join("work"),
        };
        assert!(crate::runtime::overlay::apply_overlay(&mut child).is_err());
        assert_eq!(fs::read(target.join("value")).unwrap(), b"external-edit");
        assert!(restore_logical_checkpoint(&checkpoint, &upper, &upper.join("nested")).is_err());
        assert_eq!(fs::read(upper.join("value")).unwrap(), b"staged");
        fs::remove_dir_all(&checkpoint.preimages_snapshot).unwrap();
        assert!(restore_logical_checkpoint(&checkpoint, &upper, &journal).is_err());
        assert_eq!(fs::read(upper.join("value")).unwrap(), b"staged");
    }
}
