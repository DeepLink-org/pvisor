//! Versioned CLI instance storage. Embedded Runs retain their existing layout.
use anyhow::{Context, ensure};
use pvisor_journal::api::DurableFiles;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Immutable instance identity stored at the instance root. Runtime state and
/// completion evidence live in Attempts, not in a second mutable projection here.
pub const MANIFEST: &str = "instance.json";
/// Run/Bundle binding from an Attempt to its owning instance directory.
pub const ROOT_KEY: &str = "pvisor.orchestration.instance_root";
const INITIAL: &str = "attempts/initial";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema: u16,
    id: String,
    kind: String,
}

fn read(root: &Path) -> anyhow::Result<Option<Identity>> {
    let path = root.join(MANIFEST);
    let bytes = match super::completion::read_file(&path) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let identity: Identity = serde_json::from_slice(&bytes)?;
    ensure!(
        identity.schema == 1
            && !identity.id.is_empty()
            && matches!(identity.kind.as_str(), "spawn" | "stage"),
        "invalid instance identity at {}",
        root.display()
    );
    Ok(Some(identity))
}

/// Initialize a new, absent or empty instance root and its initial Attempt.
/// Publishes only immutable identity; no running/success state is inferred.
/// Existing instance roots and nonempty directories are refused without deletion.
/// Failure can leave preparation evidence; callers retain ownership for cleanup.
pub fn prepare(root: &Path, id: &str, kind: &str) -> anyhow::Result<PathBuf> {
    ensure!(matches!(kind, "spawn" | "stage"), "invalid instance kind");
    fs::create_dir_all(root)?;
    ensure!(
        fs::read_dir(root)?.next().is_none(),
        "new instance storage must be empty: {}",
        root.display()
    );
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
        "instance root must be an owned directory"
    );
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    // Reserve identity exclusively. Atomic replacement would let two launches
    // overwrite each other's identity before either acquires its Attempt lease.
    let bytes = serde_json::to_vec_pretty(&Identity {
        schema: 1,
        id: id.into(),
        kind: kind.into(),
    })?;
    let mut manifest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(MANIFEST))?;
    manifest.write_all(&bytes)?;
    manifest.sync_all()?;
    let stage = root.join(INITIAL);
    pvisor_journal::api::Persistence::create_dir_all_durable(&stage)?;
    pvisor_journal::api::Persistence::sync_directory(root)?;
    Ok(stage)
}

/// Obtain the prepared initial Attempt for this identity before first startup,
/// such as after CoW fork preparation. A foreign identity or existing Run fails.
pub fn prepared_stage(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    validate_root(root, id)?;
    let stage = root.join(INITIAL);
    ensure!(
        !stage
            .join(super::registry::RUN_META_FILENAME)
            .try_exists()?,
        "instance already started; choose a new instance root"
    );
    Ok(stage)
}

/// Resolve a new instance root to its active Attempt. Legacy Attempt paths pass
/// through unchanged. Native execution Jobs own the active-Attempt pointer;
/// before Job publication, the initial Attempt is selected.
pub fn resolve_stage(root: &Path) -> anyhow::Result<PathBuf> {
    let Some(identity) = read(root)? else {
        return Ok(root.to_owned());
    };
    let root = root.canonicalize()?;
    if let Some(job) = super::job_execution::Job::read_stage(&root)? {
        ensure!(job.run_id == identity.id, "instance/Job identity mismatch");
        return Ok(
            if job.state == super::job_execution::JobState::Restoring
                && !job
                    .active_stage
                    .join(super::registry::RUN_META_FILENAME)
                    .exists()
            {
                job.previous_stage
            } else {
                job.active_stage
            },
        );
    }
    Ok(root.join(INITIAL))
}

pub(crate) fn validate_root(root: &Path, id: &str) -> anyhow::Result<()> {
    let identity = read(root)?.context("instance root has no identity manifest")?;
    ensure!(identity.id == id, "Attempt belongs to a different instance");
    Ok(())
}

pub(crate) fn owner_root(stage: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(attempts) = stage
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "attempts"))
    else {
        return Ok(None);
    };
    let root = attempts.parent().context("Attempt has no instance root")?;
    Ok(read(root)?.map(|_| root.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_exclusive_and_attempt_state_is_separate() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("instance");
        let stage = prepare(&root, "run-one", "stage").unwrap();
        assert_eq!(stage, root.join("attempts/initial"));
        assert_eq!(resolve_stage(&root).unwrap(), stage);
        assert_eq!(prepared_stage(&root, "run-one").unwrap(), stage);
        assert!(prepared_stage(&root, "run-other").is_err());
        let bytes = fs::read(root.join(MANIFEST)).unwrap();
        assert!(prepare(&root, "run-two", "stage").is_err());
        assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), bytes);
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(root.join(MANIFEST)).unwrap().mode() & 0o777,
            0o600
        );
        fs::write(stage.join("run.json"), b"started").unwrap();
        assert!(prepared_stage(&root, "run-one").is_err());
    }

    #[test]
    fn nonempty_roots_and_unknown_layouts_fail_without_migration() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("unrelated"), b"keep").unwrap();
        assert!(prepare(root.path(), "run-one", "stage").is_err());
        assert_eq!(fs::read(root.path().join("unrelated")).unwrap(), b"keep");
        assert_eq!(resolve_stage(root.path()).unwrap(), root.path());
        fs::write(
            root.path().join(MANIFEST),
            br#"{"schema":999,"id":"run-one","kind":"stage"}"#,
        )
        .unwrap();
        assert!(resolve_stage(root.path()).is_err());
        assert!(!root.path().join("attempts").exists());
    }
}
