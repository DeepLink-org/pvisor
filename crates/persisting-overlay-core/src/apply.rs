//! Filesystem-neutral review, apply, conflict detection, recovery and drop.
use crate::{fingerprint_at, load_preimages, preimage_journal_is_complete, remove_preimages};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
pub use persisting_control::overlay::{
    ApplyOutcome, ApplyRecord, ApplyRecordState, ApplySelection, ChangeEntry, ChangeEntryType,
    ChangeKind, OverlayRecord, OverlayState, OverlayStatus, OverlayUpper,
};
use persisting_control::overlay::{PathFingerprint, PathPreimage};
use persisting_journal::atomic_write;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::{CString, OsStr},
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

const META_FILENAME: &str = "overlay.json";
const APPLY_LEDGER_FILENAME: &str = "apply-ledger.json";
const APPLY_LEDGER_SCHEMA_VERSION: u32 = 1;
use crate::core::OPAQUE_XATTRS;
use crate::{OPAQUE_NAME as OPAQUE_WHITEOUT, WHITEOUT_PREFIX};

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("overlay enabled but no target / lower_dirs configured")]
    MissingTarget,
    #[error("invalid overlay upper configuration: {0}")]
    InvalidConfig(String),
    #[error("overlay meta missing or invalid at {0}")]
    Meta(String),
    #[error("failed to prepare overlay directories: {0}")]
    Prepare(#[source] std::io::Error),
    #[error("embedded FUSE mount failed: {0}")]
    Mount(String),
    #[error("merged mount point not ready: {0}")]
    NotReady(String),
    #[error("overlay apply failed: {0}")]
    Apply(String),
    #[error("{0}")]
    InvalidState(String),
    #[error("overlay metadata update failed: {0}")]
    Persist(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub selected: Vec<ChangeEntry>,
    selected_paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ApplyLedger {
    #[serde(default = "apply_ledger_schema_version")]
    schema_version: u32,
    #[serde(default)]
    records: Vec<ApplyRecord>,
}

fn apply_ledger_schema_version() -> u32 {
    APPLY_LEDGER_SCHEMA_VERSION
}

struct TargetApplyLock {
    file: File,
}

impl TargetApplyLock {
    fn acquire(target: &Path) -> Result<Self, OverlayError> {
        let directory = std::env::temp_dir()
            .join(format!("persisting-pvisor-apply-locks-{}", unsafe {
                libc::geteuid()
            }));
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let identity = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
        let path = directory.join(format!("{}.lock", path_digest(&identity)));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for TargetApplyLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn path_digest(path: &Path) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(path.as_os_str().as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

pub fn overlay_meta_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join(META_FILENAME)
}

pub fn write_overlay_record(record: &OverlayRecord) -> Result<(), OverlayError> {
    let path = overlay_meta_path(&record.stage_dir);
    let body = serde_json::to_string_pretty(record)
        .map_err(|e| OverlayError::Persist(format!("serialize meta: {e}")))?;
    atomic_write(&path, body.as_bytes(), 0o600)
        .map_err(|error| OverlayError::Persist(format!("{}: {error:#}", path.display())))?;
    Ok(())
}

pub fn load_overlay_record(stage_dir: &Path) -> Result<OverlayRecord, OverlayError> {
    let path = overlay_meta_path(stage_dir);
    let raw =
        fs::read_to_string(&path).map_err(|_| OverlayError::Meta(path.display().to_string()))?;
    serde_json::from_str(&raw).map_err(|e| OverlayError::Meta(format!("{}: {e}", path.display())))
}

pub fn overlay_status(record: &OverlayRecord) -> Result<OverlayStatus, OverlayError> {
    let upper_dir = record.upper.path();
    let mut changed = 0usize;
    let mut whiteouts = 0usize;
    let mut sample = Vec::new();
    if upper_dir.is_dir() {
        walk_upper(upper_dir, upper_dir, &mut |rel, is_wh| {
            if is_wh {
                whiteouts += 1;
            } else {
                changed += 1;
            }
            if sample.len() < 32 {
                sample.push(rel.display().to_string());
            }
            Ok(())
        })?;
    }
    Ok(OverlayStatus {
        changed_files: changed,
        whiteouts,
        sample_paths: sample,
    })
}

/// Build the complete classified upper-layer changeset without reading file
/// contents. `lower_dirs` use overlay priority order (highest first).
pub fn overlay_changes(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<Vec<ChangeEntry>, OverlayError> {
    let upper_dir = record.upper.path();
    let mut changes = Vec::new();
    if !upper_dir.is_dir() {
        return Ok(changes);
    }
    walk_upper(upper_dir, upper_dir, &mut |rel, is_whiteout| {
        let upper_path = upper_dir.join(&rel);
        if is_whiteout {
            let name = rel.file_name().unwrap_or_default();
            let parent = rel.parent().unwrap_or_else(|| Path::new(""));
            if name == OPAQUE_WHITEOUT {
                changes.push(ChangeEntry {
                    path: parent.display().to_string(),
                    kind: ChangeKind::Opaque,
                    old_type: Some(ChangeEntryType::Directory),
                    new_type: Some(ChangeEntryType::Directory),
                    size_bytes: None,
                    mode: None,
                });
            } else if let Some(victim) = whiteout_target(name) {
                let path = parent.join(victim);
                let old = lower_metadata(lower_dirs, &path);
                changes.push(ChangeEntry {
                    path: path.display().to_string(),
                    kind: ChangeKind::Deleted,
                    old_type: old.as_ref().map(metadata_type),
                    new_type: None,
                    size_bytes: None,
                    mode: None,
                });
            }
            return Ok(());
        }

        let new = fs::symlink_metadata(&upper_path)?;
        let old = lower_metadata(lower_dirs, &rel);
        let old_type = old.as_ref().map(metadata_type);
        let new_type = metadata_type(&new);
        let kind = match old_type {
            None => ChangeKind::Added,
            Some(old_type) if old_type != new_type => ChangeKind::TypeChanged,
            Some(_) => ChangeKind::Modified,
        };
        changes.push(ChangeEntry {
            path: rel.display().to_string(),
            kind,
            old_type,
            new_type: Some(new_type),
            size_bytes: new.is_file().then_some(new.len()),
            mode: Some(new.permissions().mode() & 0o7777),
        });
        Ok(())
    })?;
    changes.sort_by(|left, right| left.path.cmp(&right.path).then(left.kind.cmp(&right.kind)));
    Ok(changes)
}

fn lower_metadata(lower_dirs: &[PathBuf], relative: &Path) -> Option<fs::Metadata> {
    lower_dirs
        .iter()
        .find_map(|lower| fs::symlink_metadata(lower.join(relative)).ok())
}

fn metadata_type(metadata: &fs::Metadata) -> ChangeEntryType {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        ChangeEntryType::File
    } else if file_type.is_dir() {
        ChangeEntryType::Directory
    } else if file_type.is_symlink() {
        ChangeEntryType::Symlink
    } else {
        ChangeEntryType::Other
    }
}

/// Copy the raw upper tree without interpreting whiteouts or opaque markers.
/// The destination is replaced and can later seed another directory upper.
pub fn snapshot_overlay_upper(
    record: &OverlayRecord,
    destination: &Path,
) -> Result<(), OverlayError> {
    restore_overlay_upper(record.upper.path(), destination)
}

/// Restore a raw upper snapshot into a directory upper.
pub fn restore_overlay_upper(source: &Path, destination: &Path) -> Result<(), OverlayError> {
    if !source.is_dir() {
        return Err(OverlayError::InvalidConfig(format!(
            "snapshot source is not a directory: {}",
            source.display()
        )));
    }
    if path_exists(destination) {
        remove_path(destination)?;
    }
    fs::create_dir_all(destination)?;
    let mut hard_links = HashMap::new();
    snapshot_directory_raw(source, destination, &mut hard_links)?;
    if let Some(parent) = destination.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

struct CompiledApplySelection {
    paths: Vec<PathBuf>,
    includes: GlobSet,
    excludes: GlobSet,
    has_positive: bool,
}

impl CompiledApplySelection {
    fn compile(selection: &ApplySelection) -> Result<Self, OverlayError> {
        let paths = selection
            .paths
            .iter()
            .map(|path| normalize_selection_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            has_positive: !paths.is_empty() || !selection.includes.is_empty(),
            paths,
            includes: compile_globs(&selection.includes, "include")?,
            excludes: compile_globs(&selection.excludes, "exclude")?,
        })
    }

    fn requested(&self, path: &Path) -> bool {
        !self.has_positive
            || self
                .paths
                .iter()
                .any(|selected| path == selected || path.starts_with(selected))
            || self.includes.is_match(path)
    }

    fn excluded(&self, path: &Path) -> bool {
        let mut current = Some(path);
        while let Some(candidate) = current {
            if self.excludes.is_match(candidate) {
                return true;
            }
            current = candidate.parent();
        }
        false
    }
}

fn normalize_selection_path(path: &Path) -> Result<PathBuf, OverlayError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(OverlayError::InvalidConfig(format!(
                    "apply path must be relative and cannot contain `..`: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(normalized)
}

fn compile_globs(patterns: &[String], label: &str) -> Result<GlobSet, OverlayError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| {
                OverlayError::InvalidConfig(format!(
                    "invalid apply {label} glob `{pattern}`: {error}"
                ))
            })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| {
        OverlayError::InvalidConfig(format!("compile apply {label} globs: {error}"))
    })
}

fn at_or_below(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Resolve a filtered selection into a dependency-closed apply plan.
///
/// Directory ancestors and hard-link siblings are included automatically.
/// Opaque directories remain atomic: callers must select the opaque directory,
/// rather than only a child whose application would implicitly delete siblings.
pub fn plan_overlay_apply(
    record: &OverlayRecord,
    lower_dirs: &[PathBuf],
    selection: &ApplySelection,
) -> Result<ApplyPlan, OverlayError> {
    crate::OverlayLayout::new(lower_dirs.to_vec(), record.target.clone())?;
    let changes = overlay_changes(record, lower_dirs)?;
    let compiled = CompiledApplySelection::compile(selection)?;
    let mut selected_paths = changes
        .iter()
        .map(|change| PathBuf::from(&change.path))
        .filter(|path| compiled.requested(path) && !compiled.excluded(path))
        .collect::<BTreeSet<_>>();

    if !selection.is_all() && selected_paths.is_empty() {
        return Err(OverlayError::Apply(
            "no staged changes matched the apply selection".into(),
        ));
    }

    let opaque_dirs = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::Opaque)
        .map(|change| PathBuf::from(&change.path))
        .collect::<Vec<_>>();
    let hard_link_groups = upper_hard_link_groups(record.upper.path())?;

    loop {
        let before = selected_paths.len();

        for opaque in &opaque_dirs {
            let has_selected_child = selected_paths
                .iter()
                .any(|path| path != opaque && at_or_below(path, opaque));
            if has_selected_child && !selected_paths.contains(opaque) {
                return Err(OverlayError::Apply(format!(
                    "{} is inside opaque directory {}; select the directory as one atomic unit",
                    selected_paths
                        .iter()
                        .find(|path| *path != opaque && at_or_below(path, opaque))
                        .map_or_else(|| "selected path".into(), |path| path.display().to_string()),
                    if opaque.as_os_str().is_empty() {
                        ".".into()
                    } else {
                        opaque.display().to_string()
                    }
                )));
            }
            if selected_paths.contains(opaque) {
                for change in &changes {
                    let path = PathBuf::from(&change.path);
                    if at_or_below(&path, opaque) {
                        if compiled.excluded(&path) {
                            return Err(OverlayError::Apply(format!(
                                "cannot exclude {} from atomic opaque directory {}",
                                path.display(),
                                opaque.display()
                            )));
                        }
                        selected_paths.insert(path);
                    }
                }
            }
        }

        for group in &hard_link_groups {
            if group.iter().any(|path| selected_paths.contains(path)) {
                for path in group {
                    if compiled.excluded(path) {
                        return Err(OverlayError::Apply(format!(
                            "cannot exclude hard-link sibling {} from the selected apply batch",
                            path.display()
                        )));
                    }
                    selected_paths.insert(path.clone());
                }
            }
        }

        let selected_snapshot = selected_paths.iter().cloned().collect::<Vec<_>>();
        for path in selected_snapshot {
            let mut parent = path.parent();
            while let Some(ancestor) = parent {
                if changes.iter().any(|change| {
                    Path::new(&change.path) == ancestor
                        && change.new_type == Some(ChangeEntryType::Directory)
                }) {
                    if compiled.excluded(ancestor) {
                        return Err(OverlayError::Apply(format!(
                            "cannot exclude ancestor {} required by selected path {}",
                            ancestor.display(),
                            path.display()
                        )));
                    }
                    selected_paths.insert(ancestor.to_path_buf());
                }
                parent = ancestor.parent();
            }
        }

        if selected_paths.len() == before {
            break;
        }
    }

    let selected = changes
        .iter()
        .filter(|change| selected_paths.contains(Path::new(&change.path)))
        .cloned()
        .collect::<Vec<_>>();
    Ok(ApplyPlan {
        selected,
        selected_paths,
    })
}

fn upper_hard_link_groups(upper: &Path) -> Result<Vec<Vec<PathBuf>>, OverlayError> {
    let mut groups = HashMap::<(u64, u64), Vec<PathBuf>>::new();
    if !upper.is_dir() {
        return Ok(Vec::new());
    }
    walk_upper(upper, upper, &mut |rel, is_whiteout| {
        if !is_whiteout {
            let metadata = fs::symlink_metadata(upper.join(&rel))?;
            if metadata.is_file() && metadata.nlink() > 1 {
                groups
                    .entry((metadata.dev(), metadata.ino()))
                    .or_default()
                    .push(rel);
            }
        }
        Ok(())
    })?;
    Ok(groups
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect())
}

/// Apply one dependency-closed subset and retain all unselected changes for a
/// later apply or drop decision.
pub fn apply_overlay_selected(
    record: &mut OverlayRecord,
    lower_dirs: &[PathBuf],
    selection: &ApplySelection,
) -> Result<ApplyOutcome, OverlayError> {
    if record.protect_target {
        return Err(OverlayError::Apply(format!(
            "target is an immutable image rootfs: {}",
            record.target.display()
        )));
    }
    let _target_lock = TargetApplyLock::acquire(&record.target)?;
    recover_pending_applies_locked(record, lower_dirs)?;
    match record.state {
        OverlayState::Applied if selection.is_all() => {
            return Ok(ApplyOutcome {
                apply_id: String::new(),
                applied: Vec::new(),
                remaining: Vec::new(),
            });
        }
        OverlayState::Applied => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already fully applied",
                record.id
            )));
        }
        OverlayState::Discarded => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already dropped; apply cannot recover discarded changes",
                record.id
            )));
        }
        OverlayState::Active | OverlayState::Staged => {}
    }
    let plan = plan_overlay_apply(record, lower_dirs, selection)?;
    let preimages = prepare_apply_preimages(record, &plan.selected_paths, &plan.selected)?;
    validate_target_preimages(record, &preimages, &plan.selected, false)?;
    let apply_id = uuid::Uuid::new_v4().to_string();
    append_apply_record(
        record,
        ApplyRecord {
            schema_version: APPLY_LEDGER_SCHEMA_VERSION,
            apply_id: apply_id.clone(),
            created_at_unix_ms: persisting_control::unix_now_ms(),
            overlay_id: record.id.clone(),
            overlay_generation: record.generation,
            target: record.target.clone(),
            selection: selection.clone(),
            changes: plan.selected.clone(),
            planned_paths: plan.selected_paths.iter().cloned().collect(),
            preimages: preimages.clone(),
            state: ApplyRecordState::Prepared,
            remaining_changes: 0,
        },
    )?;
    apply_prepared_target(
        record,
        &plan.selected_paths,
        &preimages,
        &plan.selected,
        false,
    )?;
    mark_apply_target_applied(record, &apply_id)?;
    let remaining = complete_target_applied(record, lower_dirs, &plan.selected_paths)?;
    consume_applied_preimages(record, &plan.selected_paths)?;
    mark_apply_committed(record, &apply_id, remaining.len())?;
    Ok(ApplyOutcome {
        apply_id,
        applied: plan.selected,
        remaining,
    })
}

/// Complete any transaction whose durable intent was written before a crash.
/// `TargetApplied` is persisted before pruning starts, so recovery never tries
/// to reinterpret a partially-pruned opaque upper as a fresh target mutation.
#[cfg(test)]
fn recover_pending_applies(
    record: &mut OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<Vec<String>, OverlayError> {
    let _target_lock = TargetApplyLock::acquire(&record.target)?;
    recover_pending_applies_locked(record, lower_dirs)
}

fn recover_pending_applies_locked(
    record: &mut OverlayRecord,
    lower_dirs: &[PathBuf],
) -> Result<Vec<String>, OverlayError> {
    let pending = load_apply_records(&record.stage_dir)?
        .into_iter()
        .filter(|apply| apply.state != ApplyRecordState::Committed)
        .collect::<Vec<_>>();
    let mut recovered = Vec::with_capacity(pending.len());
    for apply in pending {
        if apply.overlay_id != record.id
            || apply.overlay_generation != record.generation
            || apply.target != record.target
        {
            return Err(OverlayError::InvalidState(format!(
                "prepared apply {} belongs to overlay {} generation {} target {}, not overlay {} generation {} target {}",
                apply.apply_id,
                apply.overlay_id,
                apply.overlay_generation,
                apply.target.display(),
                record.id,
                record.generation,
                record.target.display()
            )));
        }
        let selected_paths = apply
            .planned_paths
            .iter()
            .map(|path| normalize_selection_path(path))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if selected_paths.is_empty() && !apply.changes.is_empty() {
            return Err(OverlayError::InvalidState(format!(
                "prepared apply {} has changes but no recovery paths",
                apply.apply_id
            )));
        }
        if apply.state == ApplyRecordState::Prepared {
            apply_prepared_target(
                record,
                &selected_paths,
                &apply.preimages,
                &apply.changes,
                true,
            )?;
            mark_apply_target_applied(record, &apply.apply_id)?;
        }
        let remaining = complete_target_applied(record, lower_dirs, &selected_paths)?;
        consume_applied_preimages(record, &selected_paths)?;
        mark_apply_committed(record, &apply.apply_id, remaining.len())?;
        recovered.push(apply.apply_id);
    }
    Ok(recovered)
}

fn consume_applied_preimages(
    record: &OverlayRecord,
    selected_paths: &BTreeSet<PathBuf>,
) -> Result<(), OverlayError> {
    // A partially applied directory remains in upper for its pending children.
    // Keep the post-apply baseline written before TargetApplied was committed.
    let paths = selected_paths
        .iter()
        .filter(|path| {
            !fs::symlink_metadata(record.upper.path().join(path)).is_ok_and(|m| m.is_dir())
        })
        .cloned()
        .collect::<Vec<_>>();
    remove_preimages(&record.stage_dir.join("preimages"), &paths).map_err(OverlayError::Io)
}

fn apply_prepared_target(
    record: &OverlayRecord,
    selected_paths: &BTreeSet<PathBuf>,
    preimages: &[PathPreimage],
    changes: &[ChangeEntry],
    recovering: bool,
) -> Result<(), OverlayError> {
    validate_target_preimages(record, preimages, changes, recovering)?;
    let upper_dir = record.upper.path();
    if upper_dir.is_dir() && !selected_paths.is_empty() {
        apply_selected_upper(upper_dir, &record.target, selected_paths)?;
        // Persist this before TargetApplied/pruning: recovery must not adopt
        // arbitrary target edits made after a crash as a new baseline.
        for path in selected_paths {
            let state = fingerprint_at(upper_dir, path)?;
            if matches!(state, PathFingerprint::Directory { .. }) {
                let preimage = PathPreimage {
                    path: path.as_os_str().as_bytes().to_vec(),
                    state,
                };
                atomic_write(
                    &record
                        .stage_dir
                        .join("preimages/entries")
                        .join(format!("{}.json", path_digest(path))),
                    &serde_json::to_vec(&preimage)
                        .map_err(|error| OverlayError::Persist(error.to_string()))?,
                    0o600,
                )
                .map_err(|error| OverlayError::Persist(error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn prepare_apply_preimages(
    record: &OverlayRecord,
    selected_paths: &BTreeSet<PathBuf>,
    changes: &[ChangeEntry],
) -> Result<Vec<PathPreimage>, OverlayError> {
    let journal_directory = record.stage_dir.join("preimages");
    let complete = preimage_journal_is_complete(&journal_directory);
    let mut journal = load_preimages(&journal_directory)?
        .into_iter()
        .map(|preimage| (preimage.relative_path(), preimage))
        .collect::<HashMap<_, _>>();
    let mut preimages = Vec::with_capacity(selected_paths.len());
    for path in selected_paths {
        if let Some(preimage) = journal.remove(path) {
            preimages.push(preimage);
        } else if complete {
            return Err(OverlayError::InvalidState(format!(
                "complete preimage journal is missing selected path {}; refusing an unverified apply",
                path.display()
            )));
        } else {
            // Backward compatibility for stages produced before first-touch
            // journaling existed. These paths get an apply-time preimage, but
            // only complete-v1 stages claim run-time conflict detection.
            preimages.push(PathPreimage {
                path: path.as_os_str().as_bytes().to_vec(),
                state: fingerprint_at(&record.target, path)?,
            });
        }
    }
    // Whiteouts collapse a removed tree to one change, but every recorded
    // descendant still needs validation before recursive deletion/replacement.
    preimages.extend(journal.into_values().filter(|preimage| {
        changes.iter().any(|change| {
            replaces_directory(change) && preimage.relative_path().starts_with(&change.path)
        })
    }));
    preimages.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(preimages)
}

fn replaces_directory(change: &ChangeEntry) -> bool {
    change.kind == ChangeKind::Opaque
        || (change.old_type == Some(ChangeEntryType::Directory)
            && matches!(change.kind, ChangeKind::Deleted | ChangeKind::TypeChanged))
}

fn recovery_fingerprint_matches(current: &PathFingerprint, expected: &PathFingerprint) -> bool {
    if current == expected {
        return true;
    }
    matches!(
        (current, expected),
        (
            PathFingerprint::Directory {
                mode: current_mode,
                uid: current_uid,
                gid: current_gid,
                ..
            },
            PathFingerprint::Directory {
                mode: expected_mode,
                uid: expected_uid,
                gid: expected_gid,
                ..
            }
        ) if current_mode == expected_mode
            && current_uid == expected_uid
            && current_gid == expected_gid
    )
}

fn desired_fingerprint(
    record: &OverlayRecord,
    path: &Path,
    changes: &[ChangeEntry],
) -> Result<Option<PathFingerprint>, OverlayError> {
    let Some(change) = changes.iter().find(|change| {
        Path::new(&change.path) == path
            || (replaces_directory(change) && path.starts_with(&change.path))
    }) else {
        return Ok(None);
    };
    if change.kind == ChangeKind::Deleted
        || (change.kind == ChangeKind::TypeChanged && Path::new(&change.path) != path)
    {
        return Ok(Some(PathFingerprint::Absent));
    }
    Ok(Some(fingerprint_at(record.upper.path(), path)?))
}

fn validate_target_preimages(
    record: &OverlayRecord,
    preimages: &[PathPreimage],
    changes: &[ChangeEntry],
    recovering: bool,
) -> Result<(), OverlayError> {
    for preimage in preimages {
        let path = preimage.relative_path();
        let current = fingerprint_at(&record.target, &path)?;
        if current == preimage.state {
            continue;
        }
        if recovering
            && desired_fingerprint(record, &path, changes)?
                .is_some_and(|desired| recovery_fingerprint_matches(&current, &desired))
        {
            continue;
        }
        return Err(OverlayError::Apply(format!(
            "target changed after staging at {}; refusing to overwrite concurrent changes",
            record.target.join(&path).display()
        )));
    }
    Ok(())
}

fn complete_target_applied(
    record: &mut OverlayRecord,
    lower_dirs: &[PathBuf],
    selected_paths: &BTreeSet<PathBuf>,
) -> Result<Vec<ChangeEntry>, OverlayError> {
    let upper_dir = record.upper.path().to_path_buf();
    if upper_dir.is_dir() && !selected_paths.is_empty() {
        prune_selected_upper(&upper_dir, selected_paths)?;
    }

    let remaining = overlay_changes(record, lower_dirs)?;
    if remaining.is_empty() {
        cleanup_terminal_overlay_data(record)?;
        record.state = OverlayState::Applied;
    } else {
        record.state = OverlayState::Staged;
    }
    write_overlay_record(record)?;
    Ok(remaining)
}

/// Merge the complete staging upper onto `target`.
pub fn apply_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    let lower_dirs = vec![record.target.clone()];
    apply_overlay_selected(record, &lower_dirs, &ApplySelection::default()).map(|_| ())
}

/// Drop staging upper (and optionally the whole stage dir contents except meta).
pub fn discard_overlay(record: &mut OverlayRecord) -> Result<(), OverlayError> {
    match record.state {
        OverlayState::Discarded => return Ok(()),
        OverlayState::Applied => {
            return Err(OverlayError::InvalidState(format!(
                "overlay {} was already applied; drop cannot undo applied changes",
                record.id
            )));
        }
        OverlayState::Active | OverlayState::Staged => {}
    }
    cleanup_terminal_overlay_data(record)?;
    record.state = OverlayState::Discarded;
    write_overlay_record(record)?;
    Ok(())
}

pub fn cleanup_terminal_overlay_data(record: &OverlayRecord) -> Result<(), OverlayError> {
    clear_path(&record.upper.upper_dir)?;
    clear_path(&record.upper.work_dir)?;

    // Never recursively remove a mountpoint: after a clean teardown this is
    // either absent or an empty placeholder. A non-empty directory is retained
    // for inspection instead of risking traversal into a stale mount.
    if record.merged_dir.starts_with(&record.stage_dir) {
        match fs::remove_dir(&record.merged_dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn clear_path(path: &Path) -> Result<(), OverlayError> {
    if path_exists(path) {
        remove_path(path)?;
    }
    Ok(())
}

#[cfg(test)]
fn apply_upper_onto_target(upper: &Path, target: &Path) -> Result<(), OverlayError> {
    ensure_directory(target)?;
    let mut hard_links = HashMap::new();
    apply_directory(upper, target, &mut hard_links, false)
}

fn apply_selected_upper(
    upper: &Path,
    target: &Path,
    selected: &BTreeSet<PathBuf>,
) -> Result<(), OverlayError> {
    ensure_directory(target)?;
    let mut hard_links = HashMap::new();
    apply_selected_directory(upper, target, Path::new(""), selected, &mut hard_links)
}

fn apply_selected_directory(
    source: &Path,
    destination: &Path,
    relative: &Path,
    selected: &BTreeSet<PathBuf>,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    ensure_directory(destination)?;
    let opaque = path_exists(&source.join(OPAQUE_WHITEOUT)) || has_opaque_xattr(source);
    if opaque && selected.contains(relative) {
        for entry in fs::read_dir(destination)? {
            remove_path(&entry?.path())?;
        }
    }

    let entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    for entry in &entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            let logical = relative.join(victim);
            if selected.contains(&logical) {
                remove_path(&destination.join(victim))?;
            }
        }
    }

    for entry in entries {
        let name = entry.file_name();
        if name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()) {
            continue;
        }
        let logical = relative.join(&name);
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            let has_selected_descendant = selected
                .iter()
                .any(|path| path != &logical && path.starts_with(&logical));
            if selected.contains(&logical) || has_selected_descendant {
                let target_path = destination.join(&name);
                ensure_directory(&target_path)?;
                apply_selected_directory(
                    &source_path,
                    &target_path,
                    &logical,
                    selected,
                    hard_links,
                )?;
                if selected.contains(&logical) {
                    copy_host_metadata(&source_path, &target_path)?;
                }
            }
        } else if selected.contains(&logical) {
            copy_upper_entry(&source_path, &destination.join(name), hard_links)?;
        }
    }
    Ok(())
}

fn prune_selected_upper(upper: &Path, selected: &BTreeSet<PathBuf>) -> Result<(), OverlayError> {
    prune_selected_directory(upper, Path::new(""), selected)
}

fn prune_selected_directory(
    directory: &Path,
    relative: &Path,
    selected: &BTreeSet<PathBuf>,
) -> Result<(), OverlayError> {
    if selected.contains(relative) {
        clear_opaque_xattrs(directory)?;
    }
    let entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            if selected.contains(relative) {
                remove_path(&entry.path())?;
            }
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            if selected.contains(&relative.join(victim)) {
                remove_path(&entry.path())?;
            }
            continue;
        }

        let path = entry.path();
        let logical = relative.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            let has_selected_descendant = selected.iter().any(|selected_path| {
                selected_path != &logical && selected_path.starts_with(&logical)
            });
            if selected.contains(&logical) || has_selected_descendant {
                prune_selected_directory(&path, &logical, selected)?;
            }
            if selected.contains(&logical) && fs::read_dir(&path)?.next().is_none() {
                fs::remove_dir(&path)?;
            }
        } else if selected.contains(&logical) {
            remove_path(&path)?;
        }
    }
    Ok(())
}

fn apply_ledger_path(stage_dir: &Path) -> PathBuf {
    stage_dir.join(APPLY_LEDGER_FILENAME)
}

pub fn load_apply_records(stage_dir: &Path) -> Result<Vec<ApplyRecord>, OverlayError> {
    let path = apply_ledger_path(stage_dir);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let ledger = serde_json::from_slice::<ApplyLedger>(&raw)
        .map_err(|error| OverlayError::Meta(format!("{}: {error}", path.display())))?;
    if ledger.schema_version != APPLY_LEDGER_SCHEMA_VERSION {
        return Err(OverlayError::Meta(format!(
            "{}: unsupported apply ledger schema {}",
            path.display(),
            ledger.schema_version
        )));
    }
    Ok(ledger.records)
}

fn append_apply_record(
    overlay: &OverlayRecord,
    apply_record: ApplyRecord,
) -> Result<(), OverlayError> {
    let mut ledger = ApplyLedger {
        schema_version: APPLY_LEDGER_SCHEMA_VERSION,
        records: load_apply_records(&overlay.stage_dir)?,
    };
    ledger.records.push(apply_record);
    let path = apply_ledger_path(&overlay.stage_dir);
    let body = serde_json::to_vec_pretty(&ledger)
        .map_err(|error| OverlayError::Persist(format!("serialize apply ledger: {error}")))?;
    atomic_write(&path, &body, 0o600)
        .map_err(|error| OverlayError::Persist(format!("{}: {error:#}", path.display())))
}

fn mark_apply_committed(
    overlay: &OverlayRecord,
    apply_id: &str,
    remaining_changes: usize,
) -> Result<(), OverlayError> {
    update_apply_state(
        overlay,
        apply_id,
        ApplyRecordState::Committed,
        Some(remaining_changes),
    )
}

fn mark_apply_target_applied(overlay: &OverlayRecord, apply_id: &str) -> Result<(), OverlayError> {
    update_apply_state(overlay, apply_id, ApplyRecordState::TargetApplied, None)
}

fn update_apply_state(
    overlay: &OverlayRecord,
    apply_id: &str,
    state: ApplyRecordState,
    remaining_changes: Option<usize>,
) -> Result<(), OverlayError> {
    let mut ledger = ApplyLedger {
        schema_version: APPLY_LEDGER_SCHEMA_VERSION,
        records: load_apply_records(&overlay.stage_dir)?,
    };
    let record = ledger
        .records
        .iter_mut()
        .find(|record| record.apply_id == apply_id)
        .ok_or_else(|| {
            OverlayError::Persist(format!("prepared apply {apply_id} disappeared from ledger"))
        })?;
    record.state = state;
    if let Some(remaining_changes) = remaining_changes {
        record.remaining_changes = remaining_changes;
    }
    let path = apply_ledger_path(&overlay.stage_dir);
    let body = serde_json::to_vec_pretty(&ledger)
        .map_err(|error| OverlayError::Persist(format!("serialize apply ledger: {error}")))?;
    atomic_write(&path, &body, 0o600)
        .map_err(|error| OverlayError::Persist(format!("{}: {error:#}", path.display())))
}

pub fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => {
            remove_path(path)?;
            fs::create_dir(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path),
        Err(error) => Err(error),
    }
}

fn whiteout_target(name: &OsStr) -> Option<&OsStr> {
    let bytes = name.as_bytes();
    bytes
        .strip_prefix(WHITEOUT_PREFIX.as_bytes())
        .filter(|stripped| !stripped.is_empty())
        .map(OsStr::from_bytes)
}

fn apply_directory(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
    preserve_metadata: bool,
) -> Result<(), OverlayError> {
    ensure_directory(destination)?;
    let opaque = path_exists(&source.join(OPAQUE_WHITEOUT)) || has_opaque_xattr(source);
    if opaque {
        for entry in fs::read_dir(destination)? {
            remove_path(&entry?.path())?;
        }
    }

    let entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    // Whiteouts are processed first, independent of host readdir order.
    for entry in &entries {
        let name = entry.file_name();
        if name == OPAQUE_WHITEOUT {
            continue;
        }
        if let Some(victim) = whiteout_target(&name) {
            remove_path(&destination.join(victim))?;
        }
    }
    for entry in entries {
        let name = entry.file_name();
        if name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()) {
            continue;
        }
        copy_upper_entry(&entry.path(), &destination.join(name), hard_links)?;
    }
    if preserve_metadata {
        copy_host_metadata(source, destination)?;
    }
    Ok(())
}

fn copy_upper_entry(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    let metadata = fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        ensure_directory(destination)?;
        return apply_directory(source, destination, hard_links, true);
    }
    let parent = destination.parent().ok_or_else(|| {
        OverlayError::Apply(format!(
            "apply destination has no parent: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    // The deterministic reserved name lets a Prepared transaction clean up
    // its own interrupted copy before retrying. TargetApplyLock serializes all
    // pVisor writers for this target while the entry exists.
    let temporary = parent.join(format!(".pvisor-apply-{}", path_digest(destination)));
    remove_path(&temporary)?;
    let identity = (metadata.dev(), metadata.ino());
    let result = (|| {
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(source)?, &temporary)?;
        } else if kind.is_file() {
            if metadata.nlink() > 1 {
                if let Some(existing) = hard_links.get(&identity) {
                    fs::hard_link(existing, &temporary)?;
                } else {
                    fs::copy(source, &temporary)?;
                }
            } else {
                fs::copy(source, &temporary)?;
            }
        } else {
            let path = c_path(&temporary)?;
            // SAFETY: path is NUL terminated and points to valid storage for this call.
            let rc = unsafe {
                libc::mknod(
                    path.as_ptr(),
                    metadata.mode() as libc::mode_t,
                    metadata.rdev() as libc::dev_t,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        copy_host_metadata(source, &temporary)?;
        if kind.is_file() {
            File::open(&temporary)?.sync_all()?;
        }
        if fs::symlink_metadata(destination).is_ok_and(|metadata| metadata.is_dir()) {
            remove_path(destination)?;
        }
        fs::rename(&temporary, destination)?;
        File::open(parent)?.sync_all()?;
        if kind.is_file() && metadata.nlink() > 1 {
            hard_links.insert(identity, destination.to_path_buf());
        }
        Ok::<_, OverlayError>(())
    })();
    if result.is_err() {
        let _ = remove_path(&temporary);
    }
    result
}

fn snapshot_directory_raw(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    ensure_directory(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        snapshot_entry_raw(
            &entry.path(),
            &destination.join(entry.file_name()),
            hard_links,
        )?;
    }
    copy_snapshot_metadata(source, destination)?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

fn snapshot_entry_raw(
    source: &Path,
    destination: &Path,
    hard_links: &mut HashMap<(u64, u64), PathBuf>,
) -> Result<(), OverlayError> {
    let metadata = fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        return snapshot_directory_raw(source, destination, hard_links);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_path(destination)?;
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if kind.is_file() {
        let identity = (metadata.dev(), metadata.ino());
        if metadata.nlink() > 1
            && let Some(existing) = hard_links.get(&identity)
        {
            fs::hard_link(existing, destination)?;
            return Ok(());
        }
        fs::copy(source, destination)?;
        if metadata.nlink() > 1 {
            hard_links.insert(identity, destination.to_path_buf());
        }
    } else {
        let path = c_path(destination)?;
        // SAFETY: the C path and metadata remain valid for this call.
        let rc = unsafe {
            libc::mknod(
                path.as_ptr(),
                metadata.mode() as libc::mode_t,
                metadata.rdev() as libc::dev_t,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    copy_snapshot_metadata(source, destination)?;
    if kind.is_file() {
        File::open(destination)?.sync_all()?;
    }
    Ok(())
}

pub fn copy_snapshot_metadata(source: &Path, destination: &Path) -> io::Result<()> {
    copy_host_metadata(source, destination)?;
    let source_c = c_path(source)?;
    let destination_c = c_path(destination)?;
    for name in OPAQUE_XATTRS {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if let Ok(value) = get_host_xattr(&source_c, &name)
            && let Err(error) = set_host_xattr(&destination_c, &name, &value)
            && !matches!(
                error.raw_os_error(),
                Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
            )
        {
            return Err(error);
        }
    }
    Ok(())
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

pub fn copy_host_metadata(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    let nofollow = metadata.file_type().is_symlink();
    let source = c_path(source)?;
    let destination_c = c_path(destination)?;

    // Preserve ownership where permitted. An unprivileged apply still preserves
    // all metadata it is allowed to own instead of failing the whole transaction.
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: both C strings and syscall arguments remain valid for each call.
    let chown_rc = unsafe {
        libc::fchownat(
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            metadata.uid(),
            metadata.gid(),
            flags,
        )
    };
    if chown_rc != 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
            return Err(error);
        }
    }
    if !nofollow {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
    }
    copy_host_xattrs(&source, &destination_c)?;

    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    // SAFETY: destination and times are valid for the duration of the call.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            times.as_ptr(),
            flags,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if nofollow
            && matches!(
                error.raw_os_error(),
                Some(libc::ENOTSUP) | Some(libc::EPERM)
            )
        {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn copy_host_xattrs(source: &CString, destination: &CString) -> io::Result<()> {
    let names = list_host_xattrs(source)?;
    let destination_names = list_host_xattrs(destination)?;
    for name in destination_names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        if OPAQUE_XATTRS.iter().any(|marker| marker.as_bytes() == name)
            || !names
                .split(|byte| *byte == 0)
                .any(|source_name| source_name == name)
        {
            let name =
                CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            if let Err(error) = remove_host_xattr(destination, &name)
                && !matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
                )
            {
                return Err(error);
            }
        }
    }
    for name in names.split(|byte| *byte == 0).filter(|name| {
        !name.is_empty()
            && !OPAQUE_XATTRS
                .iter()
                .any(|marker| marker.as_bytes() == *name)
    }) {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let value = get_host_xattr(source, &name)?;
        if let Err(error) = set_host_xattr(destination, &name, &value)
            && !matches!(
                error.raw_os_error(),
                Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
            )
        {
            return Err(error);
        }
    }
    Ok(())
}

fn has_opaque_xattr(path: &Path) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    OPAQUE_XATTRS.iter().any(|name| {
        let Ok(name) = CString::new(*name) else {
            return false;
        };
        get_host_xattr(&path, &name).is_ok_and(|value| value == b"y")
    })
}

fn clear_opaque_xattrs(path: &Path) -> Result<(), OverlayError> {
    let path = c_path(path)?;
    for name in OPAQUE_XATTRS {
        let name = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        if get_host_xattr(&path, &name).is_ok_and(|value| value == b"y") {
            remove_host_xattr(&path, &name)?;
        }
    }
    Ok(())
}

fn remove_host_xattr(path: &CString, name: &CString) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    // SAFETY: both strings are NUL terminated and valid for this call.
    let rc = unsafe { libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
    #[cfg(not(target_os = "macos"))]
    // SAFETY: both strings are NUL terminated and valid for this call.
    let rc = unsafe { libc::lremovexattr(path.as_ptr(), name.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn list_host_xattrs(path: &CString) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let list = |buffer: *mut libc::c_char, size| unsafe {
        libc::listxattr(path.as_ptr(), buffer, size, libc::XATTR_NOFOLLOW)
    };
    #[cfg(not(target_os = "macos"))]
    let list =
        |buffer: *mut libc::c_char, size| unsafe { libc::llistxattr(path.as_ptr(), buffer, size) };
    let needed = list(std::ptr::null_mut(), 0);
    if needed < 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENOTSUP)) {
            return Ok(Vec::new());
        }
        return Err(error);
    }
    let mut names = vec![0; needed as usize];
    if !names.is_empty() {
        let actual = list(names.as_mut_ptr().cast(), names.len());
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        names.truncate(actual as usize);
    }
    Ok(names)
}

fn get_host_xattr(path: &CString, name: &CString) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let get = |buffer: *mut libc::c_void, size| unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer,
            size,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let get = |buffer: *mut libc::c_void, size| unsafe {
        libc::lgetxattr(path.as_ptr(), name.as_ptr(), buffer, size)
    };
    let needed = get(std::ptr::null_mut(), 0);
    if needed < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut value = vec![0; needed as usize];
    if !value.is_empty() {
        let actual = get(value.as_mut_ptr().cast(), value.len());
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        value.truncate(actual as usize);
    }
    Ok(value)
}

fn set_host_xattr(path: &CString, name: &CString, value: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn walk_upper(
    root: &Path,
    dir: &Path,
    visit: &mut dyn FnMut(PathBuf, bool) -> Result<(), OverlayError>,
) -> Result<(), OverlayError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| OverlayError::Apply(e.to_string()))?
            .to_path_buf();
        let is_wh = path
            .file_name()
            .is_some_and(|name| name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes()));
        if fs::symlink_metadata(&path)?.is_dir() && !is_wh {
            visit(rel.clone(), false)?;
            walk_upper(root, &path, visit)?;
        } else {
            visit(rel, is_wh)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn apply_copies_and_honors_whiteout() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("keep")).unwrap();
        fs::write(target.join("keep/a.txt"), b"old").unwrap();
        fs::write(target.join("gone.txt"), b"x").unwrap();
        fs::create_dir_all(upper.join("keep")).unwrap();
        fs::write(upper.join("keep/a.txt"), b"new").unwrap();
        fs::write(upper.join("keep/b.txt"), b"added").unwrap();
        fs::write(upper.join(".wh.gone.txt"), b"").unwrap();

        let work = tmp.path().join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("scratch"), b"temporary").unwrap();
        let mut rec = OverlayRecord {
            id: "t".into(),
            generation: 0,
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: work.clone(),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        apply_overlay(&mut rec).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("keep/a.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(target.join("keep/b.txt")).unwrap(),
            "added"
        );
        assert!(!target.join("gone.txt").exists());
        assert_eq!(rec.state, OverlayState::Applied);
        assert!(!upper.exists());
        assert!(!work.exists());
        assert!(tmp.path().join(META_FILENAME).is_file());
    }

    #[test]
    fn selective_apply_retains_pending_changes_and_can_repeat() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(target.join("src")).unwrap();
        fs::write(target.join("src/a.txt"), b"old-a").unwrap();
        fs::write(target.join("src/b.txt"), b"old-b").unwrap();
        fs::write(target.join("gone.txt"), b"old-gone").unwrap();
        let core = crate::OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(stage.join("work")),
            Vec::new(),
            Some(stage.join("preimages")),
        )
        .unwrap();
        fs::write(core.copy_up(Path::new("src/a.txt")).unwrap(), b"new-a").unwrap();
        fs::write(core.copy_up(Path::new("src/b.txt")).unwrap(), b"new-b").unwrap();
        core.remove(Path::new("gone.txt"), false).unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "selective".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let first = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["src/a.txt".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/a.txt")).unwrap(), b"new-a");
        assert_eq!(fs::read(target.join("src/b.txt")).unwrap(), b"old-b");
        assert!(target.join("gone.txt").exists());
        assert_eq!(record.state, OverlayState::Staged);
        assert!(
            first
                .remaining
                .iter()
                .any(|change| change.path == "src/b.txt")
        );
        assert!(upper.join("src/b.txt").is_file());
        assert!(upper.join(".wh.gone.txt").is_file());

        // Re-enter the durable crash state after pruning, before the final
        // ledger commit. Recovery must retain, not rebaseline, a shared parent.
        mark_apply_target_applied(&record, &first.apply_id).unwrap();
        let permissions = fs::metadata(target.join("src")).unwrap().permissions();
        fs::set_permissions(
            target.join("src"),
            fs::Permissions::from_mode(permissions.mode() ^ 0o020),
        )
        .unwrap();
        recover_pending_applies(&mut record, std::slice::from_ref(&target)).unwrap();
        let error = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["src/b.txt".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("target changed after staging"));
        fs::set_permissions(target.join("src"), permissions).unwrap();

        apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["gone.txt".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(!target.join("gone.txt").exists());
        assert_eq!(record.state, OverlayState::Staged);

        let final_apply = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["src".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/b.txt")).unwrap(), b"new-b");
        assert!(final_apply.remaining.is_empty());
        assert_eq!(record.state, OverlayState::Applied);
        assert!(!upper.exists());

        let ledger = load_apply_records(&stage).unwrap();
        assert_eq!(ledger.len(), 3);
        assert!(
            ledger
                .iter()
                .all(|record| record.state == ApplyRecordState::Committed)
        );
        assert_eq!(ledger[0].remaining_changes, first.remaining.len());
        assert_eq!(ledger[2].remaining_changes, 0);
    }

    #[test]
    fn directory_replacement_checks_descendants_and_recovers_after_mutation() {
        for operation in ["delete", "rename", "replace", "opaque"] {
            let tmp = tempdir().unwrap();
            let target = tmp.path().join("target");
            let stage = tmp.path().join("stage");
            let upper = stage.join("upper");
            fs::create_dir_all(target.join("dir/nested")).unwrap();
            fs::write(target.join("dir/nested/file"), b"original").unwrap();
            let core = crate::OverlayCore::new_with_exclusions_and_preimages(
                vec![target.clone()],
                upper.clone(),
                Some(stage.join("work")),
                Vec::new(),
                Some(stage.join("preimages")),
            )
            .unwrap();
            if operation == "rename" {
                core.rename(Path::new("dir"), Path::new("moved"), false)
                    .unwrap();
            } else {
                core.remove(Path::new("dir/nested/file"), false).unwrap();
                core.remove(Path::new("dir/nested"), true).unwrap();
                core.remove(Path::new("dir"), true).unwrap();
                if operation == "replace" {
                    core.create_file(Path::new("dir"), 0o600, libc::O_WRONLY)
                        .unwrap();
                } else if operation == "opaque" {
                    core.create_dir(Path::new("dir"), 0o700).unwrap();
                }
            }
            let mut record = OverlayRecord {
                id: operation.into(),
                generation: 0,
                target: target.clone(),
                upper: OverlayUpper {
                    upper_dir: upper.clone(),
                    work_dir: stage.join("work"),
                },
                merged_dir: stage.join("merged"),
                stage_dir: stage.clone(),
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            };
            fs::write(target.join("dir/nested/file"), b"concurrent").unwrap();
            let error = apply_overlay(&mut record).unwrap_err();
            assert!(
                error.to_string().contains("target changed after staging"),
                "{operation}: {error}"
            );
            assert_eq!(
                fs::read(target.join("dir/nested/file")).unwrap(),
                b"concurrent"
            );
            assert!(load_apply_records(&stage).unwrap().is_empty());

            fs::write(target.join("dir/nested/file"), b"original").unwrap();
            let plan = plan_overlay_apply(
                &record,
                std::slice::from_ref(&target),
                &ApplySelection::default(),
            )
            .unwrap();
            let preimages =
                prepare_apply_preimages(&record, &plan.selected_paths, &plan.selected).unwrap();
            append_apply_record(
                &record,
                ApplyRecord {
                    schema_version: APPLY_LEDGER_SCHEMA_VERSION,
                    apply_id: "recovery".into(),
                    created_at_unix_ms: 0,
                    overlay_id: record.id.clone(),
                    overlay_generation: 0,
                    target: target.clone(),
                    selection: ApplySelection::default(),
                    changes: plan.selected,
                    planned_paths: plan.selected_paths.iter().cloned().collect(),
                    preimages,
                    state: ApplyRecordState::Prepared,
                    remaining_changes: 0,
                },
            )
            .unwrap();
            // Crash after writing the target, before recording TargetApplied.
            apply_selected_upper(&upper, &target, &plan.selected_paths).unwrap();
            recover_pending_applies(&mut record, std::slice::from_ref(&target)).unwrap();
            assert_eq!(record.state, OverlayState::Applied, "{operation}");
            assert!(!target.join("dir/nested/file").exists());
            if operation == "rename" {
                assert_eq!(
                    fs::read(target.join("moved/nested/file")).unwrap(),
                    b"original"
                );
            }
        }
    }

    #[test]
    fn prepared_apply_recovers_before_or_after_target_mutation() {
        for target_already_mutated in [false, true] {
            let tmp = tempdir().unwrap();
            let target = tmp.path().join("target");
            let stage = tmp.path().join("stage");
            let upper = stage.join("upper");
            fs::create_dir_all(&target).unwrap();
            fs::create_dir_all(&upper).unwrap();
            fs::write(target.join("value.txt"), b"old").unwrap();
            fs::write(upper.join("value.txt"), b"new").unwrap();
            let mut record = OverlayRecord {
                generation: 0,
                id: format!("recover-{target_already_mutated}"),
                target: target.clone(),
                upper: OverlayUpper {
                    upper_dir: upper.clone(),
                    work_dir: stage.join("work"),
                },
                merged_dir: stage.join("merged"),
                stage_dir: stage.clone(),
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            };
            let selection = ApplySelection::default();
            let plan =
                plan_overlay_apply(&record, std::slice::from_ref(&target), &selection).unwrap();
            let preimages =
                prepare_apply_preimages(&record, &plan.selected_paths, &plan.selected).unwrap();
            let apply_id = format!("prepared-{target_already_mutated}");
            append_apply_record(
                &record,
                ApplyRecord {
                    schema_version: APPLY_LEDGER_SCHEMA_VERSION,
                    apply_id: apply_id.clone(),
                    created_at_unix_ms: persisting_control::unix_now_ms(),
                    overlay_id: record.id.clone(),
                    overlay_generation: record.generation,
                    target: target.clone(),
                    selection,
                    changes: plan.selected.clone(),
                    planned_paths: plan.selected_paths.iter().cloned().collect(),
                    preimages,
                    state: ApplyRecordState::Prepared,
                    remaining_changes: 0,
                },
            )
            .unwrap();

            let mut next_generation = record.clone();
            next_generation.generation += 1;
            let stale =
                recover_pending_applies(&mut next_generation, std::slice::from_ref(&target))
                    .unwrap_err();
            assert!(stale.to_string().contains("generation"));

            if target_already_mutated {
                apply_selected_upper(&upper, &target, &plan.selected_paths).unwrap();
                assert_eq!(fs::read(target.join("value.txt")).unwrap(), b"new");
                assert!(upper.join("value.txt").is_file());
            }

            assert_eq!(
                recover_pending_applies(&mut record, std::slice::from_ref(&target)).unwrap(),
                vec![apply_id.clone()]
            );
            assert_eq!(fs::read(target.join("value.txt")).unwrap(), b"new");
            assert_eq!(record.state, OverlayState::Applied);
            assert!(!upper.exists());
            let ledger = load_apply_records(&stage).unwrap();
            assert_eq!(ledger.len(), 1);
            assert_eq!(ledger[0].apply_id, apply_id);
            assert_eq!(ledger[0].state, ApplyRecordState::Committed);
            assert_eq!(ledger[0].remaining_changes, 0);
        }
    }

    #[test]
    fn apply_rejects_a_target_changed_after_first_touch() {
        let temporary = tempdir().unwrap();
        let target = temporary.path().join("target");
        let stage = temporary.path().join("stage");
        let upper = stage.join("upper");
        let work = stage.join("work");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("value.txt"), b"original").unwrap();
        let core = crate::OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(work.clone()),
            Vec::new(),
            Some(stage.join("preimages")),
        )
        .unwrap();
        core.copy_up(Path::new("value.txt")).unwrap();
        fs::write(upper.join("value.txt"), b"staged").unwrap();
        fs::write(target.join("value.txt"), b"concurrent").unwrap();

        let mut record = OverlayRecord {
            generation: 0,
            id: "conflicting-apply".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: work,
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let error = apply_overlay(&mut record).unwrap_err();
        assert!(error.to_string().contains("target changed after staging"));
        assert_eq!(fs::read(target.join("value.txt")).unwrap(), b"concurrent");
        assert_eq!(fs::read(upper.join("value.txt")).unwrap(), b"staged");
        assert!(load_apply_records(&stage).unwrap().is_empty());
    }

    #[test]
    fn target_applied_recovery_only_finishes_partially_pruned_opaque_upper() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(target.join("replaced")).unwrap();
        fs::create_dir_all(upper.join("replaced")).unwrap();
        fs::write(target.join("replaced/old"), b"old").unwrap();
        fs::write(upper.join("replaced").join(OPAQUE_WHITEOUT), b"").unwrap();
        fs::write(upper.join("replaced/new"), b"new").unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "opaque-recovery".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage.clone(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        let selection = ApplySelection {
            paths: vec!["replaced".into()],
            ..ApplySelection::default()
        };
        let plan = plan_overlay_apply(&record, std::slice::from_ref(&target), &selection).unwrap();
        let apply_id = "opaque-partial-prune".to_string();
        append_apply_record(
            &record,
            ApplyRecord {
                schema_version: APPLY_LEDGER_SCHEMA_VERSION,
                apply_id: apply_id.clone(),
                created_at_unix_ms: persisting_control::unix_now_ms(),
                overlay_id: record.id.clone(),
                overlay_generation: record.generation,
                target: target.clone(),
                selection,
                changes: plan.selected.clone(),
                planned_paths: plan.selected_paths.iter().cloned().collect(),
                preimages: Vec::new(),
                state: ApplyRecordState::Prepared,
                remaining_changes: 0,
            },
        )
        .unwrap();

        apply_prepared_target(&record, &plan.selected_paths, &[], &plan.selected, false).unwrap();
        mark_apply_target_applied(&record, &apply_id).unwrap();
        assert!(!target.join("replaced/old").exists());
        assert_eq!(fs::read(target.join("replaced/new")).unwrap(), b"new");

        // Simulate a crash after opaque metadata was pruned but before the
        // selected upper entries and ledger were finalized.
        fs::remove_file(upper.join("replaced").join(OPAQUE_WHITEOUT)).unwrap();
        assert_eq!(
            load_apply_records(&stage).unwrap()[0].state,
            ApplyRecordState::TargetApplied
        );

        assert_eq!(
            recover_pending_applies(&mut record, std::slice::from_ref(&target)).unwrap(),
            vec![apply_id]
        );
        assert_eq!(record.state, OverlayState::Applied);
        assert!(!upper.exists());
        assert_eq!(
            load_apply_records(&stage).unwrap()[0].state,
            ApplyRecordState::Committed
        );
    }

    #[test]
    fn glob_excludes_leave_matching_subtrees_staged() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(upper.join("src/generated")).unwrap();
        fs::write(upper.join("src/lib.rs"), b"accepted").unwrap();
        fs::write(upper.join("src/generated/code.rs"), b"pending").unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "glob".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let outcome = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                includes: vec!["src/**".into()],
                excludes: vec!["src/generated/**".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert_eq!(fs::read(target.join("src/lib.rs")).unwrap(), b"accepted");
        assert!(!target.join("src/generated/code.rs").exists());
        assert!(upper.join("src/generated/code.rs").is_file());
        assert!(
            outcome
                .remaining
                .iter()
                .any(|change| change.path == "src/generated/code.rs")
        );
        assert_eq!(record.state, OverlayState::Staged);
    }

    #[test]
    fn opaque_directory_requires_atomic_selection() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(target.join("replaced")).unwrap();
        fs::create_dir_all(upper.join("replaced")).unwrap();
        fs::write(target.join("replaced/old"), b"old").unwrap();
        fs::write(upper.join("replaced").join(OPAQUE_WHITEOUT), b"").unwrap();
        fs::write(upper.join("replaced/new"), b"new").unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "opaque-select".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let error = plan_overlay_apply(
            &record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["replaced/new".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("opaque directory replaced"));

        apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["replaced".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(!target.join("replaced/old").exists());
        assert_eq!(fs::read(target.join("replaced/new")).unwrap(), b"new");
    }

    #[test]
    fn selective_apply_expands_hard_link_groups() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let stage = tmp.path().join("stage");
        let upper = stage.join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("first"), b"linked").unwrap();
        fs::hard_link(upper.join("first"), upper.join("second")).unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "hard-links".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let outcome = apply_overlay_selected(
            &mut record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["first".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap();
        assert!(outcome.applied.iter().any(|change| change.path == "first"));
        assert!(outcome.applied.iter().any(|change| change.path == "second"));
        assert_eq!(
            fs::metadata(target.join("first")).unwrap().ino(),
            fs::metadata(target.join("second")).unwrap().ino()
        );
        assert_eq!(record.state, OverlayState::Applied);
    }

    #[test]
    fn apply_selection_rejects_parent_traversal() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("value"), b"value").unwrap();
        let record = OverlayRecord {
            generation: 0,
            id: "invalid-selection".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        let error = plan_overlay_apply(
            &record,
            std::slice::from_ref(&target),
            &ApplySelection {
                paths: vec!["../outside".into()],
                ..ApplySelection::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot contain `..`"));
    }

    #[test]
    fn changeset_classifies_added_modified_deleted_and_opaque_entries() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("dir")).unwrap();
        fs::write(target.join("modified.txt"), b"old").unwrap();
        fs::write(target.join("deleted.txt"), b"gone").unwrap();
        fs::write(target.join("dir/lower.txt"), b"lower").unwrap();
        fs::create_dir_all(upper.join("dir")).unwrap();
        fs::write(upper.join("modified.txt"), b"new").unwrap();
        fs::write(upper.join("added.txt"), b"added").unwrap();
        fs::write(upper.join(".wh.deleted.txt"), b"").unwrap();
        fs::write(upper.join("dir/.wh..wh..opq"), b"").unwrap();
        let record = OverlayRecord {
            generation: 0,
            id: "changes".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };

        let changes = overlay_changes(&record, std::slice::from_ref(&target)).unwrap();
        assert!(
            changes
                .iter()
                .any(|change| change.path == "added.txt" && change.kind == ChangeKind::Added)
        );
        assert!(changes.iter().any(|change| {
            change.path == "modified.txt" && change.kind == ChangeKind::Modified
        }));
        assert!(
            changes.iter().any(|change| {
                change.path == "deleted.txt" && change.kind == ChangeKind::Deleted
            })
        );
        assert!(
            changes
                .iter()
                .any(|change| change.path == "dir" && change.kind == ChangeKind::Opaque)
        );
    }

    #[test]
    fn apply_rejects_an_immutable_image_target() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&upper).unwrap();
        fs::write(target.join("system"), b"cached").unwrap();
        fs::write(upper.join("system"), b"changed").unwrap();
        let mut record = OverlayRecord {
            generation: 0,
            id: "immutable".into(),
            target: target.clone(),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: true,
            state: OverlayState::Staged,
        };
        let error = apply_overlay(&mut record).unwrap_err();
        assert!(error.to_string().contains("immutable image rootfs"));
        assert_eq!(fs::read(target.join("system")).unwrap(), b"cached");
        assert_eq!(record.state, OverlayState::Staged);
    }

    #[test]
    fn apply_honors_opaque_before_children_and_preserves_posix_types() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("target");
        let upper = tmp.path().join("upper");
        fs::create_dir_all(target.join("replaced")).unwrap();
        fs::write(target.join("replaced/old"), b"old").unwrap();
        fs::create_dir_all(upper.join("replaced")).unwrap();
        fs::write(upper.join("replaced").join(OPAQUE_WHITEOUT), b"").unwrap();
        fs::write(upper.join("replaced/new"), b"new").unwrap();
        fs::set_permissions(
            upper.join("replaced/new"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
        std::os::unix::fs::symlink("new", upper.join("replaced/link")).unwrap();
        fs::hard_link(upper.join("replaced/new"), upper.join("replaced/hard-link")).unwrap();

        apply_upper_onto_target(&upper, &target).unwrap();

        assert!(!target.join("replaced/old").exists());
        assert_eq!(fs::read(target.join("replaced/new")).unwrap(), b"new");
        assert_eq!(
            fs::read_link(target.join("replaced/link")).unwrap(),
            PathBuf::from("new")
        );
        let original = fs::metadata(target.join("replaced/new")).unwrap();
        let linked = fs::metadata(target.join("replaced/hard-link")).unwrap();
        assert_eq!(original.ino(), linked.ino());
        assert_eq!(original.mode() & 0o777, 0o751);
    }

    #[test]
    fn status_does_not_follow_symlinked_directories() {
        let tmp = tempdir().unwrap();
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&upper).unwrap();
        std::os::unix::fs::symlink(tmp.path(), upper.join("loop")).unwrap();
        let record = OverlayRecord {
            generation: 0,
            id: "t".into(),
            target: tmp.path().join("target"),
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        let status = overlay_status(&record).unwrap();
        assert_eq!(status.changed_files, 1);
    }

    #[test]
    fn discard_clears_upper() {
        let tmp = tempdir().unwrap();
        let upper = tmp.path().join("upper");
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("x"), b"1").unwrap();
        let mut rec = OverlayRecord {
            id: "t".into(),
            generation: 0,
            target: tmp.path().join("target"),
            upper: OverlayUpper {
                upper_dir: upper.clone(),
                work_dir: tmp.path().join("work"),
            },
            merged_dir: tmp.path().join("merged"),
            stage_dir: tmp.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        discard_overlay(&mut rec).unwrap();
        assert!(!upper.exists());
        assert_eq!(rec.state, OverlayState::Discarded);
    }

    #[test]
    fn terminal_decisions_are_idempotent_but_cannot_be_reversed() {
        let applied_root = tempdir().unwrap();
        let applied_target = applied_root.path().join("target");
        let applied_upper = applied_root.path().join("upper");
        fs::create_dir_all(&applied_target).unwrap();
        fs::create_dir_all(&applied_upper).unwrap();
        fs::write(applied_upper.join("value"), b"applied").unwrap();
        let mut applied = OverlayRecord {
            id: "applied-run".into(),
            generation: 0,
            target: applied_target,
            upper: OverlayUpper {
                upper_dir: applied_upper,
                work_dir: applied_root.path().join("work"),
            },
            merged_dir: applied_root.path().join("merged"),
            stage_dir: applied_root.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        apply_overlay(&mut applied).unwrap();
        apply_overlay(&mut applied).unwrap();
        let error = discard_overlay(&mut applied).unwrap_err();
        assert_eq!(
            error.to_string(),
            "overlay applied-run was already applied; drop cannot undo applied changes"
        );
        assert_eq!(applied.state, OverlayState::Applied);

        let dropped_root = tempdir().unwrap();
        let dropped_upper = dropped_root.path().join("upper");
        fs::create_dir_all(&dropped_upper).unwrap();
        fs::write(dropped_upper.join("value"), b"discarded").unwrap();
        let mut dropped = OverlayRecord {
            id: "dropped-run".into(),
            generation: 0,
            target: dropped_root.path().join("target"),
            upper: OverlayUpper {
                upper_dir: dropped_upper,
                work_dir: dropped_root.path().join("work"),
            },
            merged_dir: dropped_root.path().join("merged"),
            stage_dir: dropped_root.path().to_path_buf(),
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        discard_overlay(&mut dropped).unwrap();
        discard_overlay(&mut dropped).unwrap();
        let error = apply_overlay(&mut dropped).unwrap_err();
        assert_eq!(
            error.to_string(),
            "overlay dropped-run was already dropped; apply cannot recover discarded changes"
        );
        assert_eq!(dropped.state, OverlayState::Discarded);
    }
}
