//! Filesystem-backed logical checkpoints for Agent Runs.

use crate::runtime::{
    OverlayState, RunRecord, is_live, restore_overlay_upper, snapshot_overlay_upper,
};
use crate::unix_now_ms;
use crate::util::{create_dir_all_durable, sync_directory, write_private_json};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub const CHECKPOINTS_DIR: &str = "checkpoints";
const CHECKPOINT_FILENAME: &str = "checkpoint.json";
const CHECKPOINT_SCHEMA_VERSION: u32 = 2;

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
    pub checkpoint_id: String,
    pub run_id: String,
    pub created_at_unix_ms: u64,
    pub consistency: CheckpointConsistency,
    pub source_stage: PathBuf,
    pub upper_snapshot: PathBuf,
    pub preimages_snapshot: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub lower_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub protect_target: bool,
    #[serde(default)]
    pub access_policy: persisting_control::overlay::FileAccessPolicy,
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
    anyhow::ensure!(
        !is_live(&record.stage_dir())?,
        "Run {} is live; forking from its current staged files requires a stopped Run",
        record.run_id
    );
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
    let root = record
        .stage_dir()
        .join(CHECKPOINTS_DIR)
        .join(&checkpoint_id);
    let parent = root.parent().expect("checkpoint has a parent");
    create_dir_all_durable(parent)?;
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let result = (|| -> anyhow::Result<LogicalCheckpoint> {
        sync_directory(parent)?;
        let upper_snapshot = root.join("upper");
        snapshot_overlay_upper(overlay, &upper_snapshot)?;
        let preimages_snapshot = root.join("preimages");
        let journal = record.stage_dir().join("preimages");
        if journal.is_dir() {
            restore_overlay_upper(&journal, &preimages_snapshot)?;
        } else {
            create_dir_all_durable(&preimages_snapshot)?;
        }
        let checkpoint = LogicalCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
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
        write_private_json(&root.join(CHECKPOINT_FILENAME), &checkpoint)?;
        Ok(checkpoint)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&root);
    }
    result
}

pub fn latest_logical_checkpoint(record: &RunRecord) -> anyhow::Result<LogicalCheckpoint> {
    let root = record.stage_dir().join(CHECKPOINTS_DIR);
    let checkpoints = if root.is_dir() {
        fs::read_dir(root)?
            .filter_map(Result::ok)
            .filter_map(|entry| LogicalCheckpoint::read(&entry.path()).ok())
            .filter(|checkpoint| checkpoint.run_id == record.run_id)
            .max_by_key(|checkpoint| checkpoint.created_at_unix_ms)
    } else {
        None
    };
    checkpoints.ok_or_else(|| anyhow::anyhow!("Run {} has no logical checkpoints", record.run_id))
}

pub fn restore_logical_checkpoint(
    checkpoint: &LogicalCheckpoint,
    destination_upper: &Path,
    destination_preimages: &Path,
) -> anyhow::Result<()> {
    let sources = [&checkpoint.upper_snapshot, &checkpoint.preimages_snapshot];
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
    anyhow::ensure!(!trimmed.is_empty(), "checkpoint id cannot be empty");
    anyhow::ensure!(
        trimmed != "."
            && trimmed != ".."
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
    use crate::runtime::{OverlayRecord, OverlayUpper, RunLineage};
    use std::os::unix::fs::MetadataExt;

    fn stopped_record(root: &Path) -> RunRecord {
        let target = root.join("target");
        let upper = root.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        RunRecord {
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
            state: "completed".into(),
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
            run_plan: None,
        }
    }

    #[test]
    fn stopped_checkpoint_preserves_links_and_can_seed_a_fork() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = stopped_record(temp.path());
        record.overlay.as_mut().unwrap().protect_target = true;
        record.overlay.as_mut().unwrap().access_policy =
            persisting_control::FileAccessPolicy::new(vec!["**/.ssh".into()], vec![]).unwrap();
        let upper = record.overlay.as_ref().unwrap().upper.path();
        fs::write(upper.join("one"), b"value").unwrap();
        fs::hard_link(upper.join("one"), upper.join("two")).unwrap();
        std::os::unix::fs::symlink("one", upper.join("link")).unwrap();

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
    }

    #[test]
    fn fork_preserves_conflict_baselines_and_validates_before_replacing() {
        use persisting_overlay_core::{OverlayCore, load_preimages, preimage_journal_is_complete};
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
