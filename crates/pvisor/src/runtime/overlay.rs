//! In-process FUSE overlay mount + staging apply/discard.
//!
//! ```text
//! target (RO lower / apply destination)
//!    +
//! staging/upper (writable deltas)
//!    →
//! staging/merged  (Agent cwd)
//!
//! After the Attempt: unmount, keep staging.
//! Review → apply_overlay (upper → target) | discard_overlay
//! ```
//!
use super::implant::OverlayHint;
use crate::util::create_dir_all_durable;
use pvisor_core::overlay::OverlayConfig;
pub use pvisor_core::overlay::{OverlayRecord, OverlayState, OverlayUpper};
pub use pvisor_overlay_core::apply::*;
use pvisor_overlayfs::{OverlayMountConfig, OverlaySession, mount as mount_embedded_overlay};
use std::fs;
use std::io;
#[cfg(not(target_os = "macos"))]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Live in-process FUSE mount; unmounted on [`Self::unmount`] / Drop.
/// Staging directories are **not** deleted on unmount.
pub struct OverlayMount {
    record: OverlayRecord,
    session: Option<OverlaySession>,
}

/// Independent kernel-enforced read-only view used by `pvisor inspect`.
pub struct ReadOnlyOverlayMount {
    session: Option<OverlaySession>,
    mountpoint: PathBuf,
}

impl ReadOnlyOverlayMount {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn unmount(mut self) -> anyhow::Result<()> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> anyhow::Result<()> {
        if let Some(session) = self.session.take() {
            session.unmount()?;
        }
        #[cfg(not(target_os = "macos"))]
        if self.mountpoint.is_dir() {
            let _ = fs::remove_dir(&self.mountpoint);
        }
        Ok(())
    }
}

impl Drop for ReadOnlyOverlayMount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

impl OverlayMount {
    pub fn record(&self) -> &OverlayRecord {
        &self.record
    }

    /// Unmount and mark staging as [`OverlayState::Staged`] (keep upper).
    pub fn unmount(mut self) -> anyhow::Result<OverlayRecord> {
        self.unmount_inner()?;
        self.record.state = OverlayState::Staged;
        write_overlay_record(&self.record)?;
        Ok(self.record.clone())
    }

    fn unmount_inner(&mut self) -> anyhow::Result<()> {
        if let Some(session) = self.session.take() {
            session.unmount()?;
        }
        // FSKit owns and removes its /Volumes directory. Do not stat a detached volume.
        #[cfg(not(target_os = "macos"))]
        if !self.record.merged_dir.starts_with(&self.record.stage_dir)
            && self.record.merged_dir.is_dir()
        {
            match fs::remove_dir(&self.record.merged_dir) {
                Ok(()) => {
                    if let Some(parent) = self.record.merged_dir.parent() {
                        let _ = fs::remove_dir(parent);
                    }
                }
                // A target mounted over an existing directory (for example
                // ~/.codex) reveals the original lower again after unmount.
                // It is expected to remain non-empty and must not be removed.
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

impl Drop for OverlayMount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
        if self.record.state == OverlayState::Active {
            self.record.state = OverlayState::Staged;
            let _ = write_overlay_record(&self.record);
        }
    }
}

/// Resolve config into concrete paths (target + staging layout).
pub fn resolve_overlay_workspace(
    cfg: &OverlayConfig,
    storage: &Path,
    session_id: &str,
) -> Result<Option<OverlayRecord>, OverlayError> {
    if !cfg.enabled && cfg.target.is_none() && cfg.lower_dirs.is_empty() {
        return Ok(None);
    }

    let resolve = |p: &str| -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            storage.join(path)
        }
    };

    let target = if let Some(t) = &cfg.target {
        resolve(t)
    } else if let Some(first) = cfg.lower_dirs.first() {
        resolve(first)
    } else {
        return Err(OverlayError::MissingTarget);
    };

    let stage_dir = cfg
        .stage_dir
        .as_deref()
        .map(resolve)
        .unwrap_or_else(|| storage.join(".overlay").join(session_id));

    let upper = OverlayUpper {
        upper_dir: cfg
            .upper_dir
            .as_deref()
            .map(resolve)
            .unwrap_or_else(|| stage_dir.join("upper")),
        work_dir: cfg
            .work_dir
            .as_deref()
            .map(resolve)
            .unwrap_or_else(|| stage_dir.join("work")),
    };
    let resolved_lowers = cfg
        .lower_dirs
        .iter()
        .map(|path| resolve(path))
        .collect::<Vec<_>>();
    let stage_is_nested = target != Path::new("/")
        && std::iter::once(&target)
            .chain(resolved_lowers.iter())
            .any(|lower| stage_dir.as_path() != lower.as_path() && stage_dir.starts_with(lower));
    let merged = cfg.merged_dir.as_deref().map(resolve).unwrap_or_else(|| {
        if stage_is_nested {
            storage
                .join(".overlay-mounts")
                .join(session_id)
                .join("merged")
        } else {
            stage_dir.join("merged")
        }
    });

    let mut backing_paths = vec![stage_dir.clone(), merged.clone()];
    backing_paths.push(upper.upper_dir.clone());
    backing_paths.push(upper.work_dir.clone());
    backing_paths.extend(resolved_lowers);
    let mut excluded_paths = backing_paths
        .into_iter()
        .filter_map(|path| {
            path.strip_prefix(&target)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .collect::<Vec<_>>();
    excluded_paths.sort_by_key(|path| path.components().count());
    let mut minimal_exclusions = Vec::<PathBuf>::new();
    for path in excluded_paths {
        if !minimal_exclusions
            .iter()
            .any(|parent| path.starts_with(parent))
        {
            minimal_exclusions.push(path);
        }
    }
    let excluded_paths = minimal_exclusions;

    Ok(Some(OverlayRecord {
        id: session_id.to_string(),
        generation: 0,
        target,
        baseline_lower: None,
        upper,
        merged_dir: merged,
        stage_dir,
        excluded_paths,
        access_policy: cfg.access_policy.clone(),
        auto_apply: cfg.auto_apply,
        auto_discard: cfg.auto_discard,
        protect_target: cfg.protect_target,
        state: OverlayState::Active,
    }))
}

/// Build an [`OverlayHint`] from a resolved record + full lower stack.
pub fn hint_from_record(record: &OverlayRecord, lower_dirs: Vec<PathBuf>) -> OverlayHint {
    OverlayHint {
        access_policy: record.access_policy.clone(),
        lower_dirs,
        stage_dir: Some(record.stage_dir.clone()),
        upper_dir: Some(record.upper.upper_dir.clone()),
        work_dir: Some(record.upper.work_dir.clone()),
        merged_dir: Some(record.merged_dir.clone()),
        auto_apply: record.auto_apply,
        auto_discard: record.auto_discard,
        protect_target: record.protect_target,
    }
}

/// Lower stack for mount: compose layers first (top), then the base target.
pub fn lower_stack_from_config(
    cfg: &OverlayConfig,
    storage: &Path,
    record: &mut OverlayRecord,
    snapshot_required: bool,
) -> io::Result<Vec<PathBuf>> {
    let target = &record.target;
    let resolve = |p: &str| -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            storage.join(path)
        }
    };
    let mut lowers: Vec<PathBuf> = cfg.lower_dirs.iter().map(|p| resolve(p)).collect();
    lowers.retain(|p| p != target);
    // Snapshot only when the mount or its backing paths overlap the source.
    // With an external stage (the normal --safe layout), the source is already
    // a valid read-only lower. Copying it into Run storage can be both slow and
    // larger than the available space there.
    let needs_snapshot = snapshot_required
        && (record.merged_dir == *target
            || !record.excluded_paths.is_empty()
            || storage.starts_with(target));
    let lower = if cfg.target.is_some() && target.is_dir() && needs_snapshot {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        target.to_string_lossy().hash(&mut hasher);
        let snapshot = storage
            .join(".overlay-lowers")
            .join(format!("{:016x}", hasher.finish()));
        if !snapshot.exists() {
            let pending = snapshot.with_extension(format!("pending-{}", uuid::Uuid::new_v4()));
            let copied = copy_tree(target, &pending, storage)
                .and_then(|()| std::fs::rename(&pending, &snapshot));
            if let Err(error) = copied {
                let _ = std::fs::remove_dir_all(&pending);
                return Err(error);
            }
        }
        snapshot
    } else {
        target.to_path_buf()
    };
    record.baseline_lower = (lower != *target).then(|| lower.clone());
    lowers.push(lower);
    Ok(lowers)
}

fn copy_tree(source: &Path, destination: &Path, excluded: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        if from.starts_with(excluded) {
            continue;
        }
        let to = destination.join(entry.file_name());
        let kind = std::fs::symlink_metadata(&from)?.file_type();
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
            copy_host_metadata(&from, &to)?;
        } else if kind.is_dir() {
            copy_tree(&from, &to, excluded)?;
        } else if kind.is_file() {
            std::fs::copy(&from, &to)?;
            copy_host_metadata(&from, &to)?;
        }
    }
    copy_host_metadata(source, destination)?;
    Ok(())
}

/// FSKit creates its own mount directory under /Volumes. Backing state stays in
/// the private stage; every mount (including inspect) receives a distinct name.
pub(crate) fn host_mountpoint(requested: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Volumes").join(format!("pvisor-{}", uuid::Uuid::new_v4()))
    } else {
        requested.to_path_buf()
    }
}

pub(crate) fn mount_overlay_record_observed(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    observation: Option<pvisor_overlayfs::FsMetrics>,
) -> Result<OverlayMount, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    let mut record = record.clone();
    record.merged_dir = host_mountpoint(&record.merged_dir);
    for lower in lower_dirs {
        if let Ok(relative) = record.merged_dir.strip_prefix(lower) {
            record.excluded_paths.push(relative.to_path_buf());
        }
    }
    for dir in lower_dirs.iter().chain([&record.stage_dir]) {
        create_dir_all_durable(dir)
            .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    create_dir_all_durable(&record.upper.upper_dir)
        .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    create_dir_all_durable(&record.upper.work_dir)
        .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;

    let mut config = OverlayMountConfig::new(
        lower_dirs.to_vec(),
        record.upper.upper_dir.clone(),
        Some(record.upper.work_dir.clone()),
        record.merged_dir.clone(),
    );
    config.fsname = format!("pvisor-{}", record.id);
    config.excluded_paths = record.excluded_paths.clone();
    config.access_policy = record.access_policy.clone();
    config.apply_target = Some(record.target.clone());
    config.baseline_lower = record.baseline_lower.clone();
    config.observation = observation;
    config.preimage_dir = Some(record.stage_dir.join("preimages"));
    let session = mount_embedded_overlay(config).map_err(embedded_mount_error)?;
    wait_merged_ready(&record.merged_dir, &session)
        .map_err(|error| embedded_mount_error(error.into()))?;

    record.state = OverlayState::Active;
    write_overlay_record(&record)?;

    Ok(OverlayMount {
        record,
        session: Some(session),
    })
}

/// Prepare durable overlay backing directories for a consumer that serves the
/// union itself (currently libkrun virtio-fs), without creating a host mount.
pub(crate) fn prepare_overlay_record_mountless(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<OverlayRecord, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    for dir in lower_dirs.iter().chain([&record.stage_dir]) {
        create_dir_all_durable(dir)
            .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    let layout = pvisor_overlay_core::OverlayLayout::with_baseline(
        lower_dirs.to_vec(),
        record.target.clone(),
        record.baseline_lower.as_deref(),
    )
    .map_err(OverlayError::Prepare)?;
    // Share backing validation and journal initialization with the host adapter.
    pvisor_overlay_core::OverlayCore::new_for_layout(
        layout,
        record.upper.upper_dir.clone(),
        Some(record.upper.work_dir.clone()),
        record.excluded_paths.clone(),
        Some(record.stage_dir.join("preimages")),
    )
    .map_err(OverlayError::Prepare)?;

    let mut record = record.clone();
    record.state = OverlayState::Active;
    write_overlay_record(&record)?;
    Ok(record)
}

pub(crate) fn stage_overlay_record(record: &mut OverlayRecord) -> anyhow::Result<()> {
    if record.state == OverlayState::Active {
        record.state = OverlayState::Staged;
        write_overlay_record(record)?;
    }
    Ok(())
}

/// Mount the same lower/upper projection without permitting any mutation.
/// The kernel's read-only FUSE mount rejects writes before they reach the
/// writable overlay implementation.
pub fn mount_overlay_record_read_only(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    mountpoint: &Path,
) -> Result<ReadOnlyOverlayMount, OverlayError> {
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    let mountpoint = host_mountpoint(mountpoint);
    let mut config = OverlayMountConfig::new(
        lower_dirs.to_vec(),
        record.upper.upper_dir.clone(),
        Some(record.upper.work_dir.clone()),
        mountpoint.to_path_buf(),
    );
    config.fsname = format!("pvisor-inspect-{}", record.id);
    config.excluded_paths = record.excluded_paths.clone();
    for lower in lower_dirs {
        if let Ok(relative) = mountpoint.strip_prefix(lower)
            && !relative.as_os_str().is_empty()
        {
            config.excluded_paths.push(relative.to_path_buf());
        }
    }
    config.access_policy = record.access_policy.clone();
    config.apply_target = Some(record.target.clone());
    config.baseline_lower = record.baseline_lower.clone();
    config.read_only = true;
    let session = mount_embedded_overlay(config).map_err(embedded_mount_error)?;
    wait_merged_ready(&mountpoint, &session).map_err(|error| embedded_mount_error(error.into()))?;
    Ok(ReadOnlyOverlayMount {
        session: Some(session),
        mountpoint: mountpoint.to_path_buf(),
    })
}

fn embedded_mount_error(error: anyhow::Error) -> OverlayError {
    #[cfg(target_os = "macos")]
    {
        OverlayError::Mount(format!(
            "{error:#}; macOS mounts require macFUSE >= 5.4.0 with its FSKit extension enabled in System Settings > General > Login Items & Extensions > File System Extensions (brew install --cask macfuse); no kernel extension is used. If already enabled, inspect fskit_agent/fskitd logs for extension startup failures"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        OverlayError::Mount(error.to_string())
    }
}

fn wait_merged_ready(merged: &Path, session: &OverlaySession) -> Result<(), OverlayError> {
    // FSKit activates volumes asynchronously; concurrent mounts can exceed 2.5s.
    let attempts = if cfg!(target_os = "macos") { 600 } else { 50 };
    for _ in 0..attempts {
        if session.has_exited() {
            return Err(OverlayError::Mount(
                "embedded FUSE request loop exited before mount became ready".into(),
            ));
        }
        if merged_root_is_ready(merged) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if merged_root_is_ready(merged) {
        return Ok(());
    }
    Err(OverlayError::NotReady(merged.display().to_string()))
}

fn merged_root_is_ready(path: &Path) -> bool {
    if !is_mountpoint(path) {
        return false;
    }
    // macFUSE may publish the mountpoint before its request loop can serve the
    // root directory. Probe opendir/readdir so the Agent never races the mount.
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none_or(|entry| entry.is_ok()),
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn is_mountpoint(path: &Path) -> bool {
    pvisor_overlayfs::is_mountpoint(path)
}

#[cfg(not(target_os = "macos"))]
fn is_mountpoint(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return true;
    };
    let Ok(parent_metadata) = fs::metadata(parent) else {
        return false;
    };
    // `dev`/`ino` differ (or `ino` matches for hardlinks) across the parent
    // boundary means this path is a distinct mount. On Linux there is an
    // additional `/proc/self/mounts` probe that the early return cannot fold
    // into, so the platforms are branched explicitly to stay clippy-clean.
    #[cfg(not(target_os = "linux"))]
    {
        metadata.dev() != parent_metadata.dev()
            || (metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino())
    }

    #[cfg(target_os = "linux")]
    {
        if metadata.dev() != parent_metadata.dev()
            || (metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino())
        {
            return true;
        }
        let target = path.display().to_string();
        if let Ok(mounts) = fs::read_to_string("/proc/self/mounts") {
            return mounts.lines().any(|line| {
                line.split_whitespace()
                    .nth(1)
                    .is_some_and(|mount| mount == target)
            });
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    #[test]
    fn resolve_uses_target_and_default_stage() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/proj".into()),
            ..OverlayConfig::default()
        };
        let rec = resolve_overlay_workspace(&cfg, Path::new("/tmp/store"), "run-1")
            .unwrap()
            .unwrap();
        assert_eq!(rec.target, PathBuf::from("/proj"));
        assert_eq!(rec.stage_dir, PathBuf::from("/tmp/store/.overlay/run-1"));
        assert_eq!(
            rec.upper,
            OverlayUpper {
                upper_dir: PathBuf::from("/tmp/store/.overlay/run-1/upper"),
                work_dir: PathBuf::from("/tmp/store/.overlay/run-1/work")
            }
        );
    }

    #[test]
    fn root_overlay_hides_its_stage_and_compose_backing_paths() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/".into()),
            stage_dir: Some("/tmp/pvisor-runs/run-one".into()),
            lower_dirs: vec!["/var/lib/pvisor/layers/base".into()],
            ..OverlayConfig::default()
        };
        let record = resolve_overlay_workspace(&cfg, Path::new("/unused"), "run-one")
            .unwrap()
            .unwrap();
        assert_eq!(record.target, Path::new("/"));
        assert_eq!(
            record.excluded_paths,
            [
                PathBuf::from("tmp/pvisor-runs/run-one"),
                PathBuf::from("var/lib/pvisor/layers/base"),
            ]
        );
    }

    #[test]
    fn nested_stage_is_hidden_from_a_non_root_overlay() {
        let cfg = OverlayConfig {
            enabled: true,
            target: Some("/Users/example/workspace".into()),
            stage_dir: Some("/Users/example/workspace/project/tmp".into()),
            ..OverlayConfig::default()
        };
        let record = resolve_overlay_workspace(&cfg, Path::new("/unused"), "run-one")
            .unwrap()
            .unwrap();
        assert_eq!(record.excluded_paths, [PathBuf::from("project/tmp")]);
        assert_eq!(
            record.merged_dir,
            PathBuf::from("/unused/.overlay-mounts/run-one/merged")
        );
        assert!(!record.merged_dir.starts_with(&record.target));
    }

    #[test]
    fn mountless_preparation_creates_backing_state_without_a_merged_mount() {
        let tmp = tempdir().unwrap();
        let lower = tmp.path().join("lower");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&lower).unwrap();
        let record = OverlayRecord {
            id: "mountless".into(),
            generation: 0,
            target: lower.clone(),
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Active,
        };
        let prepared = prepare_overlay_record_mountless(&record, &[lower]).unwrap();
        assert!(prepared.upper.path().is_dir());
        assert!(stage.join("work").is_dir());
        assert!(!prepared.merged_dir.exists());
        assert_eq!(
            load_overlay_record(&stage).unwrap().state,
            OverlayState::Active
        );
    }

    #[test]
    fn lower_stack_keeps_target_as_bottom_base_layer() {
        let cfg = OverlayConfig {
            target: Some("/target".into()),
            lower_dirs: vec!["extra-a".into(), "extra-b".into()],
            ..OverlayConfig::default()
        };
        let mut record = resolve_overlay_workspace(&cfg, Path::new("/store"), "test")
            .unwrap()
            .unwrap();
        assert_eq!(
            lower_stack_from_config(&cfg, Path::new("/store"), &mut record, true).unwrap(),
            vec![
                PathBuf::from("/store/extra-a"),
                PathBuf::from("/store/extra-b"),
                PathBuf::from("/target"),
            ]
        );
    }

    #[test]
    fn external_stage_uses_workspace_directly_without_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let storage = temporary.path().join("stage");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("file"), b"content").unwrap();
        let cfg = OverlayConfig {
            target: Some(workspace.display().to_string()),
            ..OverlayConfig::default()
        };
        let mut record = resolve_overlay_workspace(&cfg, &storage, "test")
            .unwrap()
            .unwrap();
        assert_eq!(
            lower_stack_from_config(&cfg, &storage, &mut record, true).unwrap(),
            vec![workspace]
        );
        assert!(!storage.join(".overlay-lowers").exists());
    }

    #[cfg(unix)]
    #[test]
    fn staged_lower_preserves_external_symlinks_without_copying_their_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        let storage = temporary.path().join("stage");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("escape")).unwrap();
        let cfg = OverlayConfig {
            target: Some(workspace.display().to_string()),
            merged_dir: Some(workspace.display().to_string()),
            ..OverlayConfig::default()
        };
        let mut record = resolve_overlay_workspace(&cfg, &storage, "test")
            .unwrap()
            .unwrap();
        let lowers = lower_stack_from_config(&cfg, &storage, &mut record, true).unwrap();
        assert_ne!(lowers[0], workspace);
        let staged_link = lowers[0].join("escape");
        assert!(
            std::fs::symlink_metadata(&staged_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(staged_link).unwrap(), outside);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an enabled macFUSE kernel extension"]
    fn embedded_mount_roundtrip() {
        let tmp = tempdir().unwrap();
        let lower = tmp.path().join("lower");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        let work = stage.join("work");
        let merged = stage.join("merged");
        fs::create_dir_all(&lower).unwrap();
        fs::write(lower.join("lower-file"), b"lower").unwrap();
        fs::write(lower.join("deleted-file"), b"delete me").unwrap();
        let mut record = OverlayRecord {
            id: "embedded-e2e".into(),
            generation: 0,
            target: lower.clone(),
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: work,
            },
            merged_dir: merged.clone(),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let mount =
            mount_overlay_record_observed(&record, std::slice::from_ref(&lower), None).unwrap();
        assert_eq!(fs::read(merged.join("lower-file")).unwrap(), b"lower");
        fs::write(merged.join("lower-file"), b"copied-up").unwrap();
        fs::remove_file(merged.join("deleted-file")).unwrap();
        fs::write(merged.join("created"), b"upper").unwrap();
        fs::hard_link(merged.join("created"), merged.join("created-link")).unwrap();
        fs::create_dir(merged.join("new-dir")).unwrap();
        fs::write(merged.join("new-dir/before-rename"), b"nested").unwrap();
        fs::rename(
            merged.join("new-dir/before-rename"),
            merged.join("new-dir/after-rename"),
        )
        .unwrap();
        std::os::unix::fs::symlink("created", merged.join("created-symlink")).unwrap();
        assert!(upper.is_dir());
        let mut record = mount.unmount().unwrap();
        assert_eq!(record.state, OverlayState::Staged);
        assert!(!is_mountpoint(&merged));
        let status = overlay_status(&record).unwrap();
        assert!(status.changed_files >= 5);
        assert_eq!(status.whiteouts, 1);
        apply_overlay(&mut record).unwrap();
        assert_eq!(record.state, OverlayState::Applied);
        assert_eq!(fs::read(lower.join("lower-file")).unwrap(), b"copied-up");
        assert!(!lower.join("deleted-file").exists());
        assert_eq!(fs::read(lower.join("created")).unwrap(), b"upper");
        assert_eq!(
            fs::read(lower.join("new-dir/after-rename")).unwrap(),
            b"nested"
        );
        assert_eq!(
            fs::read_link(lower.join("created-symlink")).unwrap(),
            PathBuf::from("created")
        );
        assert_eq!(
            fs::metadata(lower.join("created")).unwrap().ino(),
            fs::metadata(lower.join("created-link")).unwrap().ino()
        );
        assert!(!upper.exists());
        assert!(!stage.join("work").exists());
        assert!(overlay_meta_path(&stage).is_file());
    }
}
