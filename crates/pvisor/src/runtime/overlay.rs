//! In-process FUSE overlay mount + staging apply/discard.
//!
//! ```text
//! target (RO lower / apply destination)
//!    +
//! staging/upper (writable deltas)
//!    →
//! staging/merged  (Agent cwd)
//!
//! After the Execution: unmount, keep staging.
//! Review → apply_overlay (upper → target) | discard_overlay
//! ```
//!
use super::implant::OverlayHint;
use super::registry::{LEASE_FILENAME, LEASE_IDENTITY};
use pvisor_core::overlay::OverlayConfig;
pub use pvisor_core::overlay::{OverlayRecord, OverlayState, OverlayUpper};
use pvisor_journal::api::{DurableFiles, Persistence};
pub use pvisor_overlay_core::apply::*;
use pvisor_overlayfs::api::{
    FsMetrics, OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting,
    OverlaySession, OverlaySessionControl,
};
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
        complete_overlay_record(&self.record)?;
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
        if self.unmount_inner().is_err() {
            return;
        }
        if self.record.state == OverlayState::Active
            && complete_overlay_record(&self.record).is_ok()
        {
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
        durability: None,
        recording: None,
        execution_snapshot: None,
        access_policy: record.access_policy.clone(),
        lower_dirs,
        lower_mutability: Vec::new(),
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

// A filename alone cannot distinguish project data from runtime-owned storage.
fn is_managed_stage(dir: &Path) -> io::Result<bool> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(LEASE_FILENAME))
    {
        Ok(file) => file,
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ELOOP)) => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut identity = Vec::new();
    file.take(LEASE_IDENTITY.len() as u64 + 1)
        .read_to_end(&mut identity)?;
    if identity == LEASE_IDENTITY {
        return Ok(metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o777 == 0o600);
    }
    if !LEASE_IDENTITY.starts_with(&identity) {
        return Ok(false);
    }
    let Ok(bytes) = super::completion::read_file(&dir.join(super::registry::RUN_META_FILENAME))
    else {
        return Ok(false);
    };
    Ok(serde_json::from_slice::<super::registry::RunRecord>(&bytes)
        .is_ok_and(|record| record.validate_schema().is_ok() && record.stage_dir() == dir))
}

fn copy_tree(source: &Path, destination: &Path, excluded: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    std::fs::create_dir_all(destination)?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let from = entry.path();
        if from.starts_with(excluded) {
            continue;
        }
        let to = destination.join(entry.file_name());
        let kind = match std::fs::symlink_metadata(&from) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if kind.is_symlink() {
            match std::fs::read_link(&from)
                .and_then(|target| std::os::unix::fs::symlink(&target, &to))
            {
                Ok(()) => match copy_host_metadata(&from, &to) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        } else if kind.is_dir() {
            if is_managed_stage(&from)? {
                continue;
            }
            copy_tree(&from, &to, excluded)?;
            if is_managed_stage(&from)? {
                fs::remove_dir_all(&to)?;
            }
        } else if kind.is_file() {
            match std::fs::copy(&from, &to) {
                Ok(_) => match copy_host_metadata(&from, &to) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        }
    }
    copy_host_metadata(source, destination)?;
    Ok(())
}

/// Directories the rootless sandbox copy-on-writes. Keep this list aligned with
/// `projected_state_roots`.
const HOST_STATE_ROOT_KEYS: [&str; 6] = [
    "HOME",
    "CODEX_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

fn session_mount_component(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains(['/', '\\', '\0'])
        && session_id != "."
        && session_id != ".."
}

fn host_state_roots_from_env() -> Vec<PathBuf> {
    HOST_STATE_ROOT_KEYS
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(PathBuf::from))
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir() && path != Path::new("/"))
        .collect()
}

/// Choose a merged mountpoint that is not inside a rootless state root.
///
/// The FUSE server is this process, and the sandbox mounts OverlayFS over
/// those roots from `mount(2)`. Walking into the live mount returns `EINVAL`;
/// a mount that succeeded would hide it. An absolute base that is not itself
/// inside a state root is usable even when a state root lives under that base.
pub(crate) fn merged_mount_outside_state_roots(
    merged: &Path,
    session_id: &str,
    state_roots: &[PathBuf],
    runtime_dir: Option<&Path>,
    temp_dir: &Path,
) -> PathBuf {
    let covered = state_roots
        .iter()
        .any(|root| root.is_absolute() && root != Path::new("/") && merged.starts_with(root));
    if !covered || !session_mount_component(session_id) {
        return merged.to_path_buf();
    }
    for candidate in runtime_dir.into_iter().chain(std::iter::once(temp_dir)) {
        if !candidate.is_absolute() || candidate == Path::new("/") {
            continue;
        }
        if state_roots.iter().any(|root| candidate.starts_with(root)) {
            continue;
        }
        let mounted = candidate
            .join("pvisor-mounts")
            .join(session_id)
            .join("merged");
        if state_roots.iter().any(|root| mounted.starts_with(root)) {
            continue;
        }
        return mounted;
    }
    merged.to_path_buf()
}

/// Move the live workspace mount outside directories the rootless sandbox will
/// copy-on-write. See [`merged_mount_outside_state_roots`].
#[cfg(target_os = "linux")]
pub(crate) fn relocate_merged_for_host_sandbox(record: &mut OverlayRecord, session_id: &str) {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let relocated = merged_mount_outside_state_roots(
        &record.merged_dir,
        session_id,
        &host_state_roots_from_env(),
        runtime.as_deref(),
        &std::env::temp_dir(),
    );
    if relocated == record.merged_dir {
        return;
    }
    record.merged_dir = relocated;
    if let Some(parent) = record.merged_dir.parent()
        && fs::create_dir_all(parent).is_ok()
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
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
    observation: Option<FsMetrics>,
    lower_mutability: &[pvisor_overlay_core::LayerMutability],
    durability: pvisor_core::overlay::StageDurability,
    recording: pvisor_core::overlay::StageRecording,
) -> Result<OverlayMount, OverlayError> {
    recording
        .validate(durability)
        .map_err(|e| OverlayError::Prepare(io::Error::new(io::ErrorKind::InvalidInput, e)))?;
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    if record.auto_apply && durability == pvisor_core::overlay::StageDurability::Ephemeral {
        return Err(OverlayError::Prepare(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ephemeral stages cannot auto-apply",
        )));
    }
    let mut record = record.clone();
    record.merged_dir = host_mountpoint(&record.merged_dir);
    for lower in lower_dirs {
        if let Ok(relative) = record.merged_dir.strip_prefix(lower) {
            record.excluded_paths.push(relative.to_path_buf());
        }
    }
    for dir in lower_dirs.iter().chain([&record.stage_dir]) {
        Persistence::create_dir_all_durable(dir)
            .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    Persistence::create_dir_all_durable(&record.upper.upper_dir)
        .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    Persistence::create_dir_all_durable(&record.upper.work_dir)
        .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    pvisor_overlay_core::stage::begin_with_recording(
        &record.stage_dir.join("preimages"),
        durability,
        recording,
    )
    .map_err(OverlayError::Prepare)?;

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
    config.lower_mutability = if lower_mutability.is_empty() {
        vec![pvisor_overlay_core::LayerMutability::Mutable; lower_dirs.len()]
    } else {
        lower_mutability.to_vec()
    };
    config.preimage_dir = Some(record.stage_dir.join("preimages"));
    // Reopened journals and nonempty uppers retain their existing format.
    config.compact_preimages = true;
    select_embedded_backend(&mut config)?;
    let session = OverlayFs::mount(config).map_err(embedded_mount_error)?;
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
    run_id: &str,
    durability: pvisor_core::overlay::StageDurability,
    recording: pvisor_core::overlay::StageRecording,
) -> Result<OverlayRecord, OverlayError> {
    prepare_overlay_record_mountless_inner(record, lower_dirs, run_id, false, durability, recording)
}

pub(crate) fn prepare_execution_overlay_record(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    run_id: &str,
    durability: pvisor_core::overlay::StageDurability,
    recording: pvisor_core::overlay::StageRecording,
) -> Result<OverlayRecord, OverlayError> {
    prepare_overlay_record_mountless_inner(record, lower_dirs, run_id, true, durability, recording)
}

fn prepare_overlay_record_mountless_inner(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    run_id: &str,
    preserve_existing: bool,
    durability: pvisor_core::overlay::StageDurability,
    recording: pvisor_core::overlay::StageRecording,
) -> Result<OverlayRecord, OverlayError> {
    recording
        .validate(durability)
        .map_err(|e| OverlayError::Prepare(io::Error::new(io::ErrorKind::InvalidInput, e)))?;
    if lower_dirs.is_empty() {
        return Err(OverlayError::MissingTarget);
    }
    if record.auto_apply && durability == pvisor_core::overlay::StageDurability::Ephemeral {
        return Err(OverlayError::Prepare(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ephemeral stages cannot auto-apply",
        )));
    }
    for dir in lower_dirs.iter().chain([&record.stage_dir]) {
        crate::util::persistence_step(run_id, "overlay", "directory_prepare", || {
            Persistence::create_dir_all_durable(dir)
        })
        .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
    }
    let layout = pvisor_overlay_core::OverlayLayout::with_baseline(
        lower_dirs.to_vec(),
        record.target.clone(),
        record.baseline_lower.as_deref(),
    )
    .map_err(OverlayError::Prepare)?;
    // Mountless host-root stages are served by the native VM, whose VFS owns
    // guest DAC. Opt in before Core imports a potentially read-only host root.
    #[cfg(target_os = "linux")]
    let layout = if record.target == Path::new("/") && lower_dirs == [PathBuf::from("/")] {
        layout.with_private_backing()
    } else {
        layout
    };
    pvisor_overlay_core::stage::begin_with_recording(
        &record.stage_dir.join("preimages"),
        durability,
        recording,
    )
    .map_err(OverlayError::Prepare)?;
    // Share backing validation and journal initialization with the host adapter.
    crate::util::persistence_step(run_id, "overlay", "backing_and_journal", || {
        let open = if preserve_existing {
            pvisor_overlay_core::OverlayCore::open_existing_for_layout
        } else {
            pvisor_overlay_core::OverlayCore::new_for_layout_with_compact_preimages
        };
        open(
            layout,
            record.upper.upper_dir.clone(),
            Some(record.upper.work_dir.clone()),
            record.excluded_paths.clone(),
            Some(record.stage_dir.join("preimages")),
        )
    })
    .map_err(OverlayError::Prepare)?;

    let mut record = record.clone();
    record.state = OverlayState::Active;
    write_overlay_record_observed(&record, |phase, elapsed, success| {
        crate::util::persistence_log(run_id, "overlay_record", phase, elapsed, success);
    })?;
    Ok(record)
}

pub(crate) fn stage_overlay_record(record: &mut OverlayRecord) -> anyhow::Result<()> {
    if record.state == OverlayState::Active {
        complete_overlay_record(record)?;
        record.state = OverlayState::Staged;
        write_overlay_record(record)?;
    }
    Ok(())
}

fn complete_overlay_record(record: &OverlayRecord) -> anyhow::Result<()> {
    pvisor_overlay_core::stage::complete(record.upper.path(), &record.stage_dir.join("preimages"))?;
    Ok(())
}

const LOWER_DECLARATION: &str = "lower-mutability.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct LowerDeclaration {
    lowers: Vec<PathBuf>,
    mutability: Vec<pvisor_overlay_core::LayerMutability>,
}

pub(crate) fn save_lower_mutability(
    stage: &Path,
    lowers: &[PathBuf],
    mutability: &[pvisor_overlay_core::LayerMutability],
) -> anyhow::Result<()> {
    Persistence::atomic_write(
        &stage.join(LOWER_DECLARATION),
        &serde_json::to_vec(&LowerDeclaration {
            lowers: lowers.to_vec(),
            mutability: mutability.to_vec(),
        })?,
        0o600,
    )?;
    Ok(())
}

fn inspect_lower_mutability(
    stage: &Path,
    lowers: &[PathBuf],
) -> Result<Vec<pvisor_overlay_core::LayerMutability>, OverlayError> {
    match fs::read(stage.join(LOWER_DECLARATION)) {
        Ok(bytes) => {
            let saved: LowerDeclaration = serde_json::from_slice(&bytes)
                .map_err(|error| OverlayError::Prepare(io::Error::other(error)))?;
            if saved.lowers == lowers && saved.mutability.len() == lowers.len() {
                return Ok(saved.mutability);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(OverlayError::Prepare(error)),
    }
    Ok(vec![
        pvisor_overlay_core::LayerMutability::Mutable;
        lowers.len()
    ])
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
    config.lower_mutability = inspect_lower_mutability(&record.stage_dir, lower_dirs)?;
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
    select_embedded_backend(&mut config)?;
    let session = OverlayFs::mount(config).map_err(embedded_mount_error)?;
    wait_merged_ready(&mountpoint, &session).map_err(|error| embedded_mount_error(error.into()))?;
    Ok(ReadOnlyOverlayMount {
        session: Some(session),
        mountpoint: mountpoint.to_path_buf(),
    })
}

fn select_embedded_backend(config: &mut OverlayMountConfig) -> Result<(), OverlayError> {
    #[cfg(target_os = "macos")]
    if let Some(value) = std::env::var_os("PVISOR_OVERLAY_BACKEND") {
        let value = value.into_string().map_err(|_| {
            OverlayError::Prepare(io::Error::other(
                "PVISOR_OVERLAY_BACKEND must be kernel or fskit",
            ))
        })?;
        if !matches!(value.as_str(), "kernel" | "fskit") {
            return Err(OverlayError::Prepare(io::Error::other(
                "PVISOR_OVERLAY_BACKEND must be kernel or fskit",
            )));
        }
        config.backend = Some(value);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = config;
    Ok(())
}

fn embedded_mount_error(error: anyhow::Error) -> OverlayError {
    #[cfg(target_os = "macos")]
    {
        OverlayError::Mount(format!(
            "{error:#}; macOS defaults to macFUSE >= 5.4.0 with its FSKit extension enabled in System Settings > General > Login Items & Extensions > File System Extensions; inspect fskit_agent/fskitd logs if mount readiness fails. PVISOR_OVERLAY_BACKEND=kernel explicitly selects an already-installed and enabled macFUSE kernel backend; no automatic fallback or system extension change is performed"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        OverlayError::Mount(error.to_string())
    }
}

fn wait_merged_ready(merged: &Path, session: &OverlaySession) -> Result<(), OverlayError> {
    // FSKit activates volumes asynchronously; concurrent mounts can exceed 2.5s.
    let executions = if cfg!(target_os = "macos") { 600 } else { 50 };
    for _ in 0..executions {
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
    OverlayFs::is_mountpoint(path)
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
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    #[test]
    fn workspace_snapshot_excludes_managed_stages_and_own_storage() {
        let workspace = tempdir().unwrap();
        std::fs::write(workspace.path().join("user.txt"), b"data").unwrap();
        let sibling = workspace.path().join("sibling");
        std::fs::create_dir_all(&sibling).unwrap();
        let lease = super::super::registry::RunLease::acquire(&sibling).unwrap();
        std::fs::write(sibling.join("payload"), b"internal").unwrap();
        let own = workspace.path().join("own");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("run.json"), b"{}").unwrap();
        std::fs::write(workspace.path().join(LEASE_FILENAME), b"user lock file").unwrap();
        let output = tempdir().unwrap();
        let destination = output.path().join("snapshot");
        copy_tree(workspace.path(), &destination, &own).unwrap();
        assert!(destination.join("user.txt").is_file());
        assert!(destination.join(LEASE_FILENAME).is_file());
        assert!(!destination.join("sibling").exists());
        assert!(!destination.join("own").exists());
        drop(lease);
        assert!(is_managed_stage(&sibling).unwrap());
    }

    #[test]
    fn workspace_snapshot_preserves_project_locks_and_symbolic_links() {
        let workspace = tempdir().unwrap();
        let output = tempdir().unwrap();
        for (name, bytes) in [
            ("empty", b"".as_slice()),
            ("ordinary", b"project lock".as_slice()),
        ] {
            let dir = workspace.path().join(name);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join(LEASE_FILENAME), bytes).unwrap();
            fs::write(dir.join("keep"), b"user data").unwrap();
            std::os::unix::fs::symlink(name, workspace.path().join(format!("{name}-link")))
                .unwrap();
        }
        let stage = workspace.path().join("managed");
        std::os::unix::fs::symlink("keep", workspace.path().join("empty/run.json")).unwrap();
        fs::write(workspace.path().join("ordinary/run.json"), b"{}").unwrap();
        let checked_out = workspace.path().join("checked-out-marker");
        fs::create_dir(&checked_out).unwrap();
        fs::write(checked_out.join(LEASE_FILENAME), LEASE_IDENTITY).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            checked_out.join(LEASE_FILENAME),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let _lease = super::super::registry::RunLease::acquire(&stage).unwrap();
        std::os::unix::fs::symlink("managed", workspace.path().join("stage-link")).unwrap();
        let destination = output.path().join("snapshot");
        copy_tree(
            workspace.path(),
            &destination,
            &output.path().join("excluded"),
        )
        .unwrap();
        for name in ["empty", "ordinary"] {
            assert_eq!(
                fs::read(destination.join(name).join("keep")).unwrap(),
                b"user data"
            );
            assert_eq!(
                fs::read_link(destination.join(format!("{name}-link"))).unwrap(),
                Path::new(name)
            );
        }
        assert!(!destination.join("managed").exists());
        assert_eq!(
            fs::read(destination.join("checked-out-marker").join(LEASE_FILENAME)).unwrap(),
            LEASE_IDENTITY
        );
        assert_eq!(
            fs::read_link(destination.join("empty/run.json")).unwrap(),
            Path::new("keep")
        );
        assert_eq!(
            fs::read(destination.join("ordinary/run.json")).unwrap(),
            b"{}"
        );
        assert_eq!(
            fs::read_link(destination.join("stage-link")).unwrap(),
            Path::new("managed")
        );
    }

    #[test]
    fn workspace_snapshot_excludes_a_sibling_during_prepare_and_after_release() {
        let workspace = tempdir().unwrap();
        let output = tempdir().unwrap();
        let sibling = workspace.path().join("sibling");
        let (ready, readiness) = std::sync::mpsc::sync_channel(1);
        let (copied, completion) = std::sync::mpsc::sync_channel(1);
        let wait = Duration::from_secs(5);
        std::thread::scope(|scope| {
            let sibling = &sibling;
            scope.spawn(move || {
                let _lease = super::super::registry::RunLease::acquire(sibling).unwrap();
                ready.send(()).unwrap();
                for index in 0..64 {
                    let payload = sibling.join(format!("payload-{index}"));
                    fs::write(&payload, b"internal").unwrap();
                    fs::remove_file(payload).unwrap();
                }
                completion.recv_timeout(wait).unwrap();
            });
            readiness.recv_timeout(wait).unwrap();
            let result = copy_tree(
                workspace.path(),
                &output.path().join("live"),
                &output.path().join("excluded"),
            );
            copied.send(()).unwrap();
            result.unwrap();
        });
        copy_tree(
            workspace.path(),
            &output.path().join("stopped"),
            &output.path().join("excluded"),
        )
        .unwrap();
        assert!(!output.path().join("live/sibling").exists());
        assert!(!output.path().join("stopped/sibling").exists());
    }

    proptest::proptest! {
        #[test]
        fn workspace_snapshot_preserves_generated_project_lock_content(
            name in "[a-z]{1,16}", bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
        ) {
            let workspace = tempdir().unwrap();
            let output = tempdir().unwrap();
            let directory = workspace.path().join(&name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join(LEASE_FILENAME), &bytes).unwrap();
            fs::write(directory.join("keep"), b"user data").unwrap();
            let destination = output.path().join("snapshot");
            copy_tree(workspace.path(), &destination, &output.path().join("excluded")).unwrap();
            proptest::prop_assert_eq!(fs::read(destination.join(&name).join(LEASE_FILENAME)).unwrap(), bytes);
            proptest::prop_assert_eq!(fs::read(destination.join(&name).join("keep")).unwrap(), b"user data");
        }
    }

    #[test]
    fn workspace_snapshot_skips_concurrently_deleted_entries() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(workspace.path().join("file"), b"x").unwrap();
        let gone = workspace.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        std::fs::write(gone.join("inner"), b"y").unwrap();
        let destination = outside.path().join("snapshot");
        std::fs::remove_dir_all(&gone).unwrap();
        let unexcluded = outside.path().join("no-storage");
        copy_tree(&gone, &destination.join("gone"), &unexcluded).unwrap();
        copy_tree(workspace.path(), &destination, &unexcluded).unwrap();
        assert!(destination.join("file").is_file());
        assert!(!destination.join("gone").exists());
    }

    #[test]
    fn readonly_inspect_uses_ordered_declarations_and_mutable_legacy_fallback() {
        use pvisor_overlay_core::LayerMutability::{Immutable, Mutable};
        let stage = tempdir().unwrap();
        let lowers = vec![stage.path().join("a"), stage.path().join("b")];
        assert_eq!(
            inspect_lower_mutability(stage.path(), &lowers).unwrap(),
            [Mutable, Mutable]
        );
        save_lower_mutability(stage.path(), &lowers, &[Immutable, Mutable]).unwrap();
        assert_eq!(
            inspect_lower_mutability(stage.path(), &lowers).unwrap(),
            [Immutable, Mutable]
        );
        assert_eq!(
            inspect_lower_mutability(stage.path(), &[lowers[1].clone(), lowers[0].clone()])
                .unwrap(),
            [Mutable, Mutable]
        );
        assert_eq!(
            inspect_lower_mutability(stage.path(), &lowers[..1]).unwrap(),
            [Mutable]
        );
    }

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
    fn merged_mount_outside_state_roots_prefers_runtime_then_temp() {
        let merged = Path::new("/home/user/.pvisor/runs/run-1/merged");
        let roots = [PathBuf::from("/home/user")];
        assert_eq!(
            merged_mount_outside_state_roots(
                merged,
                "run-1",
                &roots,
                Some(Path::new("/run/user/1000")),
                Path::new("/tmp"),
            ),
            PathBuf::from("/run/user/1000/pvisor-mounts/run-1/merged")
        );
        assert_eq!(
            merged_mount_outside_state_roots(
                merged,
                "run-1",
                &roots,
                Some(Path::new("/home/user/runtime")),
                Path::new("/tmp"),
            ),
            PathBuf::from("/tmp/pvisor-mounts/run-1/merged")
        );
        assert_eq!(
            merged_mount_outside_state_roots(
                merged,
                "run-1",
                &roots,
                Some(Path::new("/home/user/runtime")),
                Path::new("/home/user/tmp"),
            ),
            merged
        );
        let nested_home = PathBuf::from("/tmp/job/home");
        assert_eq!(
            merged_mount_outside_state_roots(
                Path::new("/tmp/job/home/.pvisor/runs/run-1/merged"),
                "run-1",
                &[nested_home],
                Some(Path::new("/tmp/job/home/runtime")),
                Path::new("/tmp/job"),
            ),
            PathBuf::from("/tmp/job/pvisor-mounts/run-1/merged")
        );
        assert_eq!(
            merged_mount_outside_state_roots(
                Path::new("/var/stage/merged"),
                "run-1",
                &roots,
                Some(Path::new("/run/user/1000")),
                Path::new("/tmp"),
            ),
            PathBuf::from("/var/stage/merged")
        );
        assert_eq!(
            merged_mount_outside_state_roots(
                merged,
                "../run",
                &roots,
                Some(Path::new("/run/user/1000")),
                Path::new("/tmp"),
            ),
            merged
        );
    }

    #[test]
    fn ephemeral_mountless_completion_and_auto_apply_admission() {
        use pvisor_core::overlay::StageDurability;
        for recording in [
            pvisor_core::overlay::StageRecording::On,
            pvisor_core::overlay::StageRecording::Off,
        ] {
            let tmp = tempdir().unwrap();
            let lower = tmp.path().join("lower");
            fs::create_dir(&lower).unwrap();
            let stage = tmp.path().join("stage");
            let config = OverlayConfig {
                enabled: true,
                target: Some(lower.to_string_lossy().into_owned()),
                stage_dir: Some(stage.to_string_lossy().into_owned()),
                ..OverlayConfig::default()
            };
            let mut record = resolve_overlay_workspace(&config, tmp.path(), "ephemeral")
                .unwrap()
                .unwrap();
            record.auto_apply = true;
            assert!(
                prepare_overlay_record_mountless(
                    &record,
                    std::slice::from_ref(&lower),
                    "ephemeral",
                    StageDurability::Ephemeral,
                    recording,
                )
                .is_err()
            );
            assert!(!stage.exists());
            record.auto_apply = false;
            let mut prepared = prepare_overlay_record_mountless(
                &record,
                &[lower],
                "ephemeral",
                StageDurability::Ephemeral,
                recording,
            )
            .unwrap();
            fs::write(prepared.upper.path().join("result"), b"workload result").unwrap();
            stage_overlay_record(&mut prepared).unwrap();
            assert_eq!(prepared.state, OverlayState::Staged);
            assert_eq!(
                load_overlay_record(&stage).unwrap().state,
                OverlayState::Staged
            );
            assert_eq!(
                fs::read(prepared.upper.path().join("result")).unwrap(),
                b"workload result"
            );
            assert!(stage.join("preimages/completed-volatile-v1").is_file());
            assert!(!stage.join("preimages/sealed-v1").exists());
            assert!(pvisor_overlay_core::stage::require_sealed(&stage.join("preimages")).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountless_host_root_opts_in_before_metadata_import_and_reopens_without_reset() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let stage = tmp.path().join("stage");
        let record = OverlayRecord {
            id: "private-root".into(),
            generation: 0,
            target: PathBuf::from("/"),
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: vec![tmp.path().strip_prefix("/").unwrap().to_owned()],
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Active,
        };
        let host = fs::metadata("/").unwrap();
        let prepared = prepare_overlay_record_mountless(
            &record,
            &[PathBuf::from("/")],
            "private-root-test",
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let upper = prepared.upper.path();
        let native = fs::metadata(upper).unwrap();
        assert_eq!(native.permissions().mode() & 0o7777, 0o700);
        let logical = pvisor_overlay_core::logical_identity(upper, &native).unwrap();
        assert_eq!(
            (logical.uid, logical.gid, logical.mode),
            (host.uid(), host.gid(), host.mode())
        );
        pvisor_overlay_core::sys::set_xattr(
            upper,
            std::ffi::OsStr::new(pvisor_overlay_core::OVERRIDE_STAT_XATTR),
            b"123:456:40555",
            0,
        )
        .unwrap();
        complete_overlay_record(&prepared).unwrap();
        prepare_overlay_record_mountless(
            &prepared,
            &[PathBuf::from("/")],
            "private-root-reopen",
            Default::default(),
            Default::default(),
        )
        .unwrap();
        let logical =
            pvisor_overlay_core::logical_identity(upper, &fs::metadata(upper).unwrap()).unwrap();
        assert_eq!(
            (logical.uid, logical.gid, logical.mode),
            (123, 456, 0o40555)
        );
        fs::create_dir(upper.join("scratch")).unwrap();
        assert!(!prepared.merged_dir.exists());
        let current = fs::metadata("/").unwrap();
        assert_eq!(
            (current.uid(), current.gid(), current.mode()),
            (host.uid(), host.gid(), host.mode())
        );
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
        let prepared = prepare_overlay_record_mountless(
            &record,
            &[lower],
            "test-run",
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert!(prepared.upper.path().is_dir());
        assert!(stage.join("work").is_dir());
        assert!(stage.join("preimages/entries/format-v2.json").is_file());
        assert!(
            prepare_overlay_record_mountless(
                &prepared,
                std::slice::from_ref(&prepared.target),
                "interrupted-reopen",
                Default::default(),
                Default::default(),
            )
            .is_err()
        );
        complete_overlay_record(&prepared).unwrap();
        prepare_overlay_record_mountless(
            &prepared,
            std::slice::from_ref(&prepared.target),
            "test-reopen",
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert!(stage.join("preimages/entries/format-v2.json").is_file());
        assert!(!prepared.merged_dir.exists());
        assert_eq!(
            load_overlay_record(&stage).unwrap().state,
            OverlayState::Active
        );
        let legacy_stage = tmp.path().join("legacy-stage");
        let legacy = OverlayRecord {
            upper: OverlayUpper {
                upper_dir: legacy_stage.join("upper"),
                work_dir: legacy_stage.join("work"),
            },
            stage_dir: legacy_stage.clone(),
            merged_dir: legacy_stage.join("merged"),
            ..record
        };
        pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
            vec![legacy.target.clone()],
            legacy.upper.upper_dir.clone(),
            Some(legacy.upper.work_dir.clone()),
            Vec::new(),
            Some(legacy_stage.join("preimages")),
        )
        .unwrap();
        prepare_overlay_record_mountless(
            &legacy,
            std::slice::from_ref(&legacy.target),
            "legacy-reopen",
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert!(
            !legacy_stage
                .join("preimages/entries/format-v2.json")
                .exists()
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
        let record = OverlayRecord {
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

        let mount = mount_overlay_record_observed(
            &record,
            std::slice::from_ref(&lower),
            None,
            &[],
            Default::default(),
            Default::default(),
        )
        .unwrap();
        assert!(stage.join("preimages/entries/format-v2.json").is_file());
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
