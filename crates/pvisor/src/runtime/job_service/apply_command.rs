//! Read-only planning and explicitly scoped apply orchestration. No terminal policy.
use super::{JobSelection, RuntimeJobService, ServiceContext, check_selected_record};
use crate::runtime::{ApplySelection, ChangeEntry, OverlayState, RunRecord};
use anyhow::{Context, ensure};
use pvisor_overlay_core::apply as core;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::unix::{
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::{Component, Path, PathBuf},
};

/// Explicit RunSpec metadata opt-in. Ordinary spawn does not set this key.
pub const PROJECT_APPLY_SCOPE_KEY: &str = "pvisor.project_apply_scope";

/// Versioned project authority captured before launching a root-overlay Attempt.
/// Deserialization alone grants no authority: consumers revalidate the directory
/// identity and completion binding. Symlink components and filesystem root fail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectApplyScope {
    /// Contract version; only 1 is supported.
    pub version: u32,
    /// Existing canonical absolute project directory, never `/`.
    pub root: PathBuf,
    /// Device captured from the opened, no-follow directory.
    pub device: u64,
    /// Inode captured from the opened, no-follow directory.
    pub inode: u64,
}
impl ProjectApplyScope {
    /// Capture an existing canonical absolute directory without following symlinks.
    /// Does not create storage or change normal spawn behavior.
    pub fn capture(root: &Path) -> anyhow::Result<Self> {
        let file = open_project(root)?;
        let metadata = file.metadata()?;
        Ok(Self {
            version: 1,
            root: root.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    /// Check version, canonical spelling, no-follow traversal and current identity.
    /// Rebinding fails closed; this does not reserve the path against other writers.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.version == 1, "unsupported project apply scope version");
        let metadata = open_project(&self.root)?.metadata()?;
        ensure!(
            metadata.dev() == self.device && metadata.ino() == self.inode,
            "project apply root identity changed"
        );
        Ok(())
    }
    fn boundary(&self) -> anyhow::Result<core::ProjectApplyBoundary> {
        self.validate()?;
        Ok(core::ProjectApplyBoundary {
            target: self.root.clone(),
            overlay_prefix: self.root.strip_prefix("/")?.to_path_buf(),
            device: self.device,
            inode: self.inode,
        })
    }
}
fn open_project(root: &Path) -> anyhow::Result<File> {
    ensure!(
        root.is_absolute() && root != Path::new("/"),
        "project root must be absolute and not '/'"
    );
    ensure!(
        !root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir)),
        "project root must be canonical"
    );
    ensure!(
        root.canonicalize()? == root,
        "project root must use canonical spelling without symlinks"
    );
    let mut directory = File::open("/")?;
    for part in root.components() {
        if let Component::Normal(name) = part {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(name.as_bytes())?;
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("open project root without following symlinks");
            }
            directory = unsafe { File::from_raw_fd(fd) };
        }
    }
    let metadata = directory.metadata()?;
    let root_metadata = File::open("/")?.metadata()?;
    ensure!(
        metadata.dev() != root_metadata.dev() || metadata.ino() != root_metadata.ino(),
        "project root cannot have filesystem root '/' identity"
    );
    Ok(directory)
}

/// One frontend-independent preview or execution request. Empty positive
/// selection is only permitted for preview; interactive selection belongs to CLI.
pub struct ApplyCommandRequest {
    /// Selector resolved once, then pinned under all mutation locks.
    pub job: JobSelection,

    /// Explicit privileged system-root mode with unchanged UID/storage checks.
    pub rootfs: bool,
    /// Strict scope-relative paths and existing git-style include/exclude globs.
    pub selection: ApplySelection,
    /// Select the complete scope; may combine with exclusions, not paths/includes.
    pub all: bool,
    /// Plan without recovery, ledger publication or stage consumption.
    pub dry_run: bool,
    /// Confirmation token from preview. A changed source/scope returns blocked.
    pub expected_plan: Option<String>,
}
/// Pinned source identity; stage is never re-resolved from `last` during execution.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyCommandSource {
    pub job_id: String,
    pub stage: PathBuf,
    pub attempt_id: Option<String>,
    pub generation: Option<u64>,
}
/// Resolved authority; kind is `project` or `rootfs`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyCommandScope {
    pub kind: String,
    pub target: Option<PathBuf>,
}
/// Remaining changes. Unknown observations are null, never fabricated zeroes.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ApplyCommandRemaining {
    pub in_scope: Option<usize>,
    pub outside_scope: Option<usize>,
}
/// Execution evidence. Counts are reported only from confirmed committed intent.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyCommandExecution {
    pub apply_id: Option<String>,
    pub applied: Option<usize>,
    pub recovery_required: bool,
    pub ledger_confirmed: bool,
    pub error: Option<String>,
}
/// JSON schema 1 response. `mode` is preview/execute and `status` is
/// ready/applied/no_changes/blocked/error;
/// selected and required are separate, scope-relative change records. Candidates
/// remain available to frontend pickers even when planning detects a conflict.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyCommandResponse {
    pub schema_version: u32,
    pub operation: String,
    pub mode: String,
    pub status: String,
    pub source: ApplyCommandSource,
    pub scope: ApplyCommandScope,
    pub selected: Vec<ChangeEntry>,
    pub required: Vec<ChangeEntry>,
    pub blockers: Vec<String>,
    pub remaining: ApplyCommandRemaining,
    pub execution: Option<ApplyCommandExecution>,
    pub candidates: Vec<ChangeEntry>,
    pub plan: Option<String>,
}
impl RuntimeJobService {
    /// Plan and optionally execute under Job directory, ExecutionJob and Attempt
    /// leases. Admission failures after resolution are structured blocked results.
    /// No interaction, implicit recovery or scope expansion is performed.
    pub fn apply_command(
        context: &ServiceContext<'_>,
        request: ApplyCommandRequest,
    ) -> anyhow::Result<ApplyCommandResponse> {
        let selected = Self::resolve(&ServiceContext::default(), &request.job)?;
        let mut response = ApplyCommandResponse {
            schema_version: 1,
            operation: "apply".into(),
            mode: if request.dry_run {
                "preview"
            } else {
                "execute"
            }
            .into(),
            status: "blocked".into(),
            source: ApplyCommandSource {
                job_id: selected.run_id.clone(),
                stage: selected.stage_dir(),
                attempt_id: selected.attempt_id.clone(),
                generation: selected.overlay.as_ref().map(|o| o.generation),
            },
            scope: ApplyCommandScope {
                kind: if request.rootfs { "rootfs" } else { "project" }.into(),
                target: None,
            },
            selected: vec![],
            required: vec![],
            blockers: vec![],
            remaining: Default::default(),
            execution: None,
            candidates: vec![],
            plan: None,
        };
        if let Err(error) = command(context, &selected, &request, &mut response) {
            let error = format!("{error:#}");
            response.blockers.push(error.clone());
            if let Some(execution) = &mut response.execution {
                execution.error = Some(error);
                response.status = "error".into();
            } else {
                response.status = "blocked".into();
            }
        }
        Ok(response)
    }
}
fn command(
    context: &ServiceContext<'_>,
    selected: &RunRecord,
    request: &ApplyCommandRequest,
    response: &mut ApplyCommandResponse,
) -> anyhow::Result<()> {
    validate_apply_source(selected, &request.job)?;
    context.check(selected)?;
    let _directory = if request.dry_run {
        crate::runtime::JobDirectoryLock::observe_existing(selected)?
    } else {
        crate::runtime::JobDirectoryLock::acquire(selected, "apply-command")?
    };
    let _job = super::lock_selected_job_mode(context, selected, request.dry_run)?;
    let (mut record, _lease) = if request.dry_run {
        selected.observe_current()?
    } else {
        selected.lock_current()?
    };
    check_selected_record(selected, &record)?;
    ensure!(
        record.stage_dir() == selected.stage_dir(),
        "selected stage changed"
    );
    context.check(&record)?;
    record.require_stopped()?;
    validate_selection(request)?;
    let mut overlay = record
        .overlay
        .clone()
        .context("this Job has no OverlayFS workspace")?;
    if let Some(scope) = &record.project_apply_scope {
        scope.validate()?;
    }
    let rootstage = overlay.target == Path::new("/");
    let boundary = if rootstage && !request.rootfs {
        ensure!(
            record.completion_protocol.is_some(),
            "rootstage project apply requires a completion-bound launch contract"
        );
        let scope = record.project_apply_scope.as_ref().context("rootstage has no launch-time project apply contract; use explicit authorized --rootfs or recreate stage")?;
        ensure!(
            record.workspace.as_deref() == Some(scope.root.as_path()),
            "project apply scope must match recorded workspace"
        );
        Some(scope.boundary()?)
    } else {
        super::effects::validate_recorded_apply_target(&overlay.target, request.rootfs, unsafe {
            libc::geteuid()
        })?;
        if request.rootfs {
            super::effects::validate_rootfs_storage(&record.stage_dir())?;
        }
        None
    };
    let _owner = crate::runtime::owned_workspace::apply_owner(&record, &overlay)?;

    let base = record.storage.join(".overlay-lowers");
    ensure!(
        _owner.is_some()
            || (record.overlay_lowers.len() <= 1
                && record
                    .overlay_lowers
                    .first()
                    .is_none_or(|p| p == &overlay.target || p.starts_with(&base))),
        "composed read-only layers cannot be applied"
    );
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };

    let prefix = boundary.as_ref().map(|b| b.overlay_prefix.as_path());
    response.scope.target = Some(
        boundary
            .as_ref()
            .map_or_else(|| overlay.target.clone(), |b| b.target.clone()),
    );
    let changes = core::overlay_changes(&overlay, &lowers)?;
    response.candidates = changes
        .iter()
        .filter(|c| in_scope(&c.relative_path(), prefix))
        .map(|c| relative_change(c, prefix))
        .collect();
    let mut selection = request.selection.clone();
    if !request.all && selection.paths.is_empty() && selection.includes.is_empty() {
        // An initial picker request observes candidates, not an implicit all plan.
        // '/' cannot match any strict relative staged path.
        selection.includes.push("/".into());
    }
    let mapped = map_selection(&selection, &changes, prefix)?;
    response.plan = Some(plan_token(
        &record,
        &overlay,
        &changes,
        response.scope.target.as_deref(),
        &boundary,
    )?);
    ensure!(
        !core::has_pending_applies(&overlay)?,
        "pending apply requires explicit reconciliation; preview never recovers or consumes intent"
    );
    ensure!(
        overlay.state != OverlayState::Discarded,
        "stage was dropped; changes cannot be recovered"
    );
    if overlay.state == OverlayState::Applied {
        if let Some(expected) = &request.expected_plan {
            ensure!(
                Some(expected) == response.plan.as_ref(),
                "apply plan changed; inspect the new preview and confirm again"
            );
        }
        set_remaining(response, &changes, prefix);
        response.status = if request.dry_run {
            "ready"
        } else {
            "no_changes"
        }
        .into();
        return Ok(());
    }
    let compiled = CompiledApplySelection::compile(&mapped)?;
    let has_matches = changes.iter().any(|change| {
        let path = change.relative_path();
        in_scope(&path, prefix) && compiled.requested(&path) && !compiled.excluded(&path)
    });
    if !has_matches {
        // Core treats an empty match as an error. Here it is a confirmed empty
        // command plan (also the picker candidate-only case), never an all plan.
        if let Some(expected) = &request.expected_plan {
            ensure!(
                Some(expected) == response.plan.as_ref(),
                "apply plan changed; inspect the new preview and confirm again"
            );
        }
        set_remaining(response, &changes, prefix);
        response.status = if request.dry_run {
            "ready"
        } else {
            "no_changes"
        }
        .into();
        return Ok(());
    }
    let preview = if let Some(boundary) = &boundary {
        core::preview_overlay_apply_scoped(&overlay, &lowers, &mapped, boundary)?
    } else {
        core::preview_overlay_apply(&overlay, &lowers, &mapped)?
    };
    let applied_paths: std::collections::HashSet<_> = preview
        .selected
        .iter()
        .chain(&preview.required)
        .map(ChangeEntry::relative_path)
        .collect();
    let remaining: Vec<_> = changes
        .iter()
        .filter(|change| !applied_paths.contains(&change.relative_path()))
        .cloned()
        .collect();
    set_remaining(response, &remaining, prefix);
    response.selected = preview
        .selected
        .iter()
        .map(|c| relative_change(c, prefix))
        .collect();
    response.required = preview
        .required
        .iter()
        .map(|c| relative_change(c, prefix))
        .collect();
    if let Some(expected) = &request.expected_plan {
        ensure!(
            Some(expected) == response.plan.as_ref(),
            "apply plan changed; inspect the new preview and confirm again"
        );
    }
    if request.dry_run {
        response.status = "ready".into();
        return Ok(());
    }
    if preview.selected.is_empty() && preview.required.is_empty() {
        response.status = "no_changes".into();
        return Ok(());
    }
    context.check(&record)?;
    if let Some(scope) = &record.project_apply_scope {
        scope.validate()?;
    }
    let current = RunRecord::read(&record.stage_dir())?;
    check_selected_record(&record, &current)?;
    ensure!(
        current.project_apply_scope == record.project_apply_scope
            && current.workspace == record.workspace
            && current.stage_dir() == record.stage_dir()
            && current.overlay.as_ref().map(|o| &o.target)
                == record.overlay.as_ref().map(|o| &o.target),
        "stage or target contract changed before execution"
    );
    let generation = overlay
        .generation
        .checked_add(1)
        .context("workspace generation exhausted")?;
    let before = core::load_apply_records(&overlay.stage_dir)?;
    response.remaining = ApplyCommandRemaining::default();
    response.execution = Some(ApplyCommandExecution {
        apply_id: None,
        applied: None,
        recovery_required: true,
        ledger_confirmed: false,
        error: None,
    });
    let result = if let Some(boundary) = &boundary {
        core::apply_overlay_selected_scoped(&mut overlay, &lowers, &mapped, boundary)
    } else {
        core::apply_overlay_selected(&mut overlay, &lowers, &mapped)
    };
    // Observe durable evidence even when publication/pruning failed partway through.
    let ledger = core::load_apply_records(&overlay.stage_dir);
    if let Ok(ledger) = &ledger {
        let execution = response.execution.as_mut().unwrap();
        if let Some(intent) = ledger
            .iter()
            .rev()
            .find(|intent| !before.iter().any(|old| old.apply_id == intent.apply_id))
        {
            execution.apply_id = Some(intent.apply_id.clone());
            execution.ledger_confirmed = true;
            execution.recovery_required =
                intent.state != pvisor_core::overlay::ApplyRecordState::Committed;
            if !execution.recovery_required {
                execution.applied = Some(intent.changes.len());
            }
        } else {
            execution.recovery_required = false;
        }
    }
    let outcome = result?;
    ledger.context("cannot confirm apply ledger after execution")?;
    set_remaining(response, &outcome.remaining, prefix);
    overlay.generation = generation;
    record.overlay = Some(overlay);
    record.overlay_lowers = lowers;
    record
        .write()
        .context("apply committed but Run projection publication failed; reconcile before retry")?;
    response.status = "applied".into();
    Ok(())
}
fn validate_apply_source(record: &RunRecord, selection: &JobSelection) -> anyhow::Result<()> {
    let selector = selection.selector.as_deref();
    let implicit = selector.is_none() || selector == Some(Path::new("last"));
    if selector == Some(Path::new(&record.run_id)) && !selector.unwrap().exists() {
        return Ok(());
    }
    let candidate = if implicit {
        std::env::current_dir()?
    } else {
        selector.unwrap().to_path_buf()
    };
    let candidate = super::paths::fork_stage_candidate(&candidate)?;
    if let Some(overlay) = &record.overlay {
        let upper = super::paths::fork_stage_candidate(overlay.upper.path())?;
        ensure!(
            !candidate.starts_with(&upper),
            "apply source cannot be an upper directory or a path inside it; select the Job id, stage directory or workspace"
        );
    }
    if !implicit {
        let mut allowed = vec![record.stage_dir()];
        if let Some(workspace) = &record.workspace {
            allowed.push(workspace.clone());
        }
        if let Some(overlay) = &record.overlay
            && overlay.target != Path::new("/")
        {
            allowed.push(overlay.target.clone());
        }
        if let Some(job) = crate::runtime::job_execution::Job::read(record)? {
            allowed.push(job.root);
        }
        ensure!(
            allowed
                .iter()
                .any(|path| super::paths::fork_stage_candidate(path)
                    .is_ok_and(|path| path == candidate)),
            "apply source must be a Job id, stage directory or workspace, not an internal record or workspace child path"
        );
    }
    Ok(())
}

fn in_scope(path: &Path, prefix: Option<&Path>) -> bool {
    prefix.is_none_or(|p| path != p && path.starts_with(p))
}
fn set_remaining(
    response: &mut ApplyCommandResponse,
    changes: &[ChangeEntry],
    prefix: Option<&Path>,
) {
    let inside = changes
        .iter()
        .filter(|c| in_scope(&c.relative_path(), prefix))
        .count();
    response.remaining = ApplyCommandRemaining {
        in_scope: Some(inside),
        outside_scope: Some(changes.len() - inside),
    };
}
fn relative_change(change: &ChangeEntry, prefix: Option<&Path>) -> ChangeEntry {
    use std::os::unix::ffi::OsStrExt;
    let mut change = change.clone();
    if let Some(prefix) = prefix {
        let path = change.relative_path();
        if let Ok(relative) = path.strip_prefix(prefix) {
            change.path = relative.to_string_lossy().into_owned();
            change.path_bytes = relative
                .to_str()
                .is_none()
                .then(|| relative.as_os_str().as_bytes().to_vec());
        }
    }
    change
}
fn strict_relative(path: &Path) -> anyhow::Result<()> {
    ensure!(
        !path.is_absolute()
            && !path.as_os_str().is_empty()
            && !path.components().any(|c| matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )),
        "apply selection must be relative without '..': {}",
        path.display()
    );
    Ok(())
}
fn validate_selection(request: &ApplyCommandRequest) -> anyhow::Result<()> {
    ensure!(
        !request.all
            || (request.selection.paths.is_empty() && request.selection.includes.is_empty()),
        "all allows exclusions, not paths or includes"
    );
    ensure!(
        request.dry_run
            || request.all
            || !request.selection.paths.is_empty()
            || !request.selection.includes.is_empty(),
        "execution requires all or explicit positive selection; interactive selection belongs to CLI"
    );
    for path in &request.selection.paths {
        strict_relative(path)?;
    }
    for pattern in request
        .selection
        .includes
        .iter()
        .chain(&request.selection.excludes)
    {
        strict_relative(Path::new(pattern))?;
    }
    Ok(())
}
// Same GlobBuilder options and ancestor-exclusion semantics as the core's private
// CompiledApplySelection. Match relative to the scope before mapping to root paths.
struct CompiledApplySelection {
    paths: Vec<PathBuf>,
    includes: globset::GlobSet,
    excludes: globset::GlobSet,
    positive: bool,
}
impl CompiledApplySelection {
    fn compile(selection: &ApplySelection) -> anyhow::Result<Self> {
        fn globs(patterns: &[String]) -> anyhow::Result<globset::GlobSet> {
            let mut builder = globset::GlobSetBuilder::new();
            for pattern in patterns {
                builder.add(
                    globset::GlobBuilder::new(pattern)
                        .literal_separator(true)
                        .build()?,
                );
            }
            Ok(builder.build()?)
        }
        Ok(Self {
            paths: selection
                .paths
                .iter()
                .map(|path| {
                    path.components()
                        .filter(|component| !matches!(component, Component::CurDir))
                        .collect()
                })
                .collect(),
            includes: globs(&selection.includes)?,
            excludes: globs(&selection.excludes)?,
            positive: !selection.paths.is_empty() || !selection.includes.is_empty(),
        })
    }
    fn requested(&self, path: &Path) -> bool {
        !self.positive
            || self.paths.iter().any(|p| path.starts_with(p))
            || self.includes.is_match(path)
    }
    fn excluded(&self, path: &Path) -> bool {
        path.ancestors().any(|p| self.excludes.is_match(p))
    }
}
fn map_selection(
    selection: &ApplySelection,
    changes: &[ChangeEntry],
    prefix: Option<&Path>,
) -> anyhow::Result<ApplySelection> {
    let Some(prefix) = prefix else {
        CompiledApplySelection::compile(selection)?;
        return Ok(selection.clone());
    };
    let compiled = CompiledApplySelection::compile(selection)?;
    let mut paths = vec![];
    let mut excludes = vec![];
    for change in changes {
        let path = change.relative_path();
        if !in_scope(&path, Some(prefix)) {
            continue;
        }
        let relative = path.strip_prefix(prefix)?;
        if compiled.excluded(relative) {
            // Literal escaped full path prevents dependency closure from reviving
            // excluded hardlinks, ancestors or opaque-directory descendants.
            let text = path.to_str().context(
                "excluded non-UTF8 path cannot be represented safely in core glob selection",
            )?;
            excludes.push(globset::escape(text));
        } else if compiled.requested(relative) {
            paths.push(path);
        }
    }
    // Empty core selection means all: use an impossible positive include instead.
    let includes = if paths.is_empty() {
        vec!["/".into()]
    } else {
        vec![]
    };
    Ok(ApplySelection {
        paths,
        includes,
        excludes,
    })
}
fn plan_token(
    record: &RunRecord,
    overlay: &pvisor_core::overlay::OverlayRecord,
    changes: &[ChangeEntry],
    target: Option<&Path>,
    boundary: &Option<core::ProjectApplyBoundary>,
) -> anyhow::Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(&serde_json::to_vec(&(
        record.run_id.clone(),
        record.attempt_id.clone(),
        record.stage_dir(),
        overlay,
        target,
        boundary,
        changes,
    ))?);
    if let Some(target) = target {
        let metadata = std::fs::symlink_metadata(target)?;
        hash.update(&serde_json::to_vec(&(
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
        ))?);
    }
    for change in changes {
        let path = overlay.upper.path().join(change.relative_path());
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                hash.update(&serde_json::to_vec(&(
                    metadata.dev(),
                    metadata.ino(),
                    metadata.mode(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                ))?);
                if metadata.is_file() {
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut file = std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&path)?;
                    std::io::copy(&mut file, &mut hash)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use pvisor_core::overlay::{ApplyRecord, ApplyRecordState};
    use std::fs;

    fn fixture(root: &Path) -> RunRecord {
        fixture_layout(root, false)
    }

    fn fixture_layout(root: &Path, nested_stage: bool) -> RunRecord {
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let stage = if nested_stage {
            project.join("experiment")
        } else {
            root.join("stage")
        };
        for directory in [
            &project,
            &stage,
            &stage.join("upper"),
            &stage.join("work"),
            &stage.join("merged"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        for name in ["a", "b"] {
            fs::write(project.join(name), b"old").unwrap();
        }
        fs::write(root.join("environment"), b"old env").unwrap();
        let mut record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":2,"completion_protocol":1,"run_id":"apply-command",
            "attempt_id":"attempt-1","session_id":"apply-command","agent":"test",
            "pid":0,"command":["true"],"state":"running","started_at_unix_ms":1,
            "finished_at_unix_ms":null,"storage":stage,"workspace":project,"gateway_listen":null,"network":{},
            "overlay":{"id":"apply-command","generation":7,"target":"/",
                "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"},
            "overlay_lowers":["/"]
        })).unwrap();
        record.project_apply_scope = Some(ProjectApplyScope::capture(&project).unwrap());
        // Match launch ordering: prepare_overlay holds the new Attempt lease;
        // finish_preparation initializes observation locks before record publication.
        let launch_lease = crate::runtime::registry::RunLease::acquire_new(&stage).unwrap();
        record.initialize_apply_observation_locks().unwrap();
        let overlay = record.overlay.as_ref().unwrap();
        let journal = stage.join("preimages");
        pvisor_overlay_core::stage::begin(&journal, pvisor_core::overlay::StageDurability::Strict)
            .unwrap();
        let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
            vec!["/".into()],
            overlay.upper.path().to_path_buf(),
            Some(overlay.upper.work_dir.clone()),
            vec![stage.strip_prefix("/").unwrap().to_path_buf()],
            Some(journal.clone()),
        )
        .unwrap();
        // Some hosts mount / with mode 0555; only our fixture upper needs write access.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(overlay.upper.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for path in [
            project.join("a"),
            project.join("b"),
            root.join("environment"),
        ] {
            core.observe_read(path.strip_prefix("/").unwrap())
                .expect("record preimage");
            // Fixture root coordinates include root-owned /tmp ancestors. Build
            // only temporary upper scaffolding; do not import host ancestor metadata.
            fs::create_dir_all(
                overlay
                    .upper
                    .path()
                    .join(path.strip_prefix("/").unwrap())
                    .parent()
                    .unwrap(),
            )
            .unwrap();
            fs::write(
                core.copy_up(path.strip_prefix("/").unwrap())
                    .unwrap_or_else(|e| panic!("copy up {}: {e}", path.display())),
                b"new",
            )
            .unwrap();
        }
        drop(core);
        pvisor_overlay_core::stage::seal(overlay.upper.path(), &journal).unwrap();
        record.write().unwrap();
        let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"attempt_id":record.attempt_id,"state":"completed",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
        }))
        .unwrap();
        let agentctl = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"attempt_id":record.attempt_id,
            "directive":crate::AgentDirective::Continue,"clients":[]
        }))
        .unwrap();
        crate::RunBundle::capture(&record, &result, agentctl, false)
            .unwrap()
            .write(&stage)
            .unwrap();
        crate::runtime::completion::Completion::commit(&record, &result).unwrap();
        drop(launch_lease);
        RunRecord::read(&stage).unwrap()
    }
    fn request(record: &RunRecord, dry_run: bool) -> ApplyCommandRequest {
        ApplyCommandRequest {
            job: JobSelection {
                selector: Some(record.stage_dir()),
                storage: record.storage.clone(),
            },
            rootfs: false,
            selection: Default::default(),
            all: true,
            dry_run,
            expected_plan: None,
        }
    }
    fn run(request: ApplyCommandRequest) -> ApplyCommandResponse {
        RuntimeJobService::apply_command(&ServiceContext::default(), request).unwrap()
    }
    fn project(record: &RunRecord) -> &Path {
        &record.project_apply_scope.as_ref().unwrap().root
    }

    // Include every file, directory and symlink, including lock bytes and all
    // stage metadata. Reads may update atime; no other timestamp is ignored.
    fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, Vec<i128>)> {
        fn visit(root: &Path, path: &Path, result: &mut Vec<(PathBuf, Vec<u8>, Vec<i128>)>) {
            use std::os::unix::ffi::OsStrExt;
            let m = fs::symlink_metadata(path).unwrap();
            let bytes = if m.is_file() {
                fs::read(path).unwrap()
            } else if m.is_symlink() {
                fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec()
            } else {
                vec![]
            };
            result.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                bytes,
                vec![
                    m.dev() as i128,
                    m.ino() as i128,
                    m.mode() as i128,
                    m.uid() as i128,
                    m.gid() as i128,
                    m.nlink() as i128,
                    m.len() as i128,
                    m.mtime() as i128,
                    m.mtime_nsec() as i128,
                    m.ctime() as i128,
                    m.ctime_nsec() as i128,
                ],
            ));
            if m.is_dir() {
                let mut paths: Vec<_> = fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                paths.sort();
                for path in paths {
                    visit(root, &path, result);
                }
            }
        }
        let mut result = vec![];
        visit(root, root, &mut result);
        result
    }

    fn seed_execution_job(record: &RunRecord) {
        let job: crate::runtime::job_execution::Job = serde_json::from_value(serde_json::json!({
            "version":crate::runtime::job_execution::JOB_SCHEMA_VERSION,
            "run_id":record.run_id,"root":record.stage_dir(),"active_stage":record.stage_dir(),
            "previous_stage":record.stage_dir(),"active_attempt":record.attempt_id,
            "config":crate::config::RunConfig::default(),
            "spec":pvisor_core::RunSpec::process(record.run_id.clone(), "test", "true"),
            "state":"terminal","head":null,"checkpoints":{},"requests":{},"resumes":{},"forks":{},"stores":[]
        })).unwrap();
        job.write().unwrap();
    }

    #[test]
    fn apply_sources_reject_upper_subpaths_and_aliases_without_lock_or_target_writes() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture_layout(root.path(), true);
        let upper = record.overlay.as_ref().unwrap().upper.path();
        let child = upper.join("selector-child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("file"), b"internal stage bytes").unwrap();
        let alias = root.path().join("upper-alias");
        std::os::unix::fs::symlink(upper, &alias).unwrap();
        let child_alias = root.path().join("upper-file-alias");
        std::os::unix::fs::symlink(child.join("file"), &child_alias).unwrap();
        let before = tree_snapshot(root.path());
        for selector in [
            upper.to_path_buf(),
            child.clone(),
            child.join("file"),
            child.join("not-yet-created/deep"),
            alias.clone(),
            alias.join("selector-child"),
            child_alias,
        ] {
            // Shared resolution stays compatible for review/status; only apply rejects.
            assert_eq!(
                RuntimeJobService::resolve(
                    &ServiceContext::default(),
                    &JobSelection {
                        selector: Some(selector.clone()),
                        storage: record.storage.clone(),
                    }
                )
                .unwrap()
                .run_id,
                record.run_id
            );
            for dry_run in [true, false] {
                let mut req = request(&record, dry_run);
                req.job.selector = Some(selector.clone());
                let response = run(req);
                assert_eq!(
                    response.status,
                    "blocked",
                    "{}: {:?}",
                    selector.display(),
                    response.blockers
                );
                assert!(
                    response
                        .blockers
                        .join(" ")
                        .contains("upper directory or a path inside it")
                );
                assert!(response.execution.is_none());
                assert_eq!(before, tree_snapshot(root.path()));
            }
        }
    }

    #[test]
    fn apply_source_contract_accepts_job_stage_workspace_but_not_internal_record_paths() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture_layout(root.path(), true);
        // Register the workspace selector exactly as a persisted Job index does.
        let indexes = project(&record).join(".pvisor/runs");
        fs::create_dir_all(&indexes).unwrap();
        let index = fs::read_dir(record.storage.join(".pvisor/runs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::copy(&index, indexes.join(index.file_name().unwrap())).unwrap();
        let before = tree_snapshot(root.path());
        for selector in [
            PathBuf::from(&record.run_id),
            record.stage_dir(),
            project(&record).to_path_buf(),
        ] {
            let mut req = request(&record, true);
            req.job.selector = Some(selector);
            let response = run(req);
            assert_eq!(response.status, "ready", "{:?}", response.blockers);
            assert_eq!(before, tree_snapshot(root.path()));
        }
        for selector in [
            record.stage_dir().join("run.json"),
            record.stage_dir().join("merged"),
        ] {
            let mut req = request(&record, false);
            req.job.selector = Some(selector);
            let response = run(req);
            assert_eq!(response.status, "blocked");
            assert!(
                response
                    .blockers
                    .join(" ")
                    .contains("source must be a Job id, stage directory or workspace")
            );
            assert_eq!(before, tree_snapshot(root.path()));
        }
    }

    #[test]
    fn native_stage_preparation_initializes_locks_before_first_preview() {
        use crate::runtime::attempt::{OverlayAttemptPrepareOpts, prepare_overlay_attempt};
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let stage = project.join("experiment");
        let mut spec = pvisor_core::RunSpec::process("native-stage-preview", "test", "true");
        spec.metadata.insert(
            "pvisor.workspace".into(),
            serde_json::to_value(&project).unwrap(),
        );
        spec.metadata.insert(
            PROJECT_APPLY_SCOPE_KEY.into(),
            serde_json::to_value(ProjectApplyScope::capture(&project).unwrap()).unwrap(),
        );
        let mut preparation = crate::PVisor::new()
            .resolve_run(spec.clone())
            .unwrap()
            .preparation;
        // Exercise the actual native mountless preparation and completion path,
        // without entering a VM or granting writes to the real host root.
        preparation.guest_workspace_overlay = true;
        let attempt = pvisor_core::AttemptId::new("native-preview-attempt");
        let session = prepare_overlay_attempt(
            &mut spec,
            &preparation,
            OverlayAttemptPrepareOpts {
                storage: &stage,
                overlay: crate::runtime::OverlayHint {
                    lower_dirs: vec!["/".into()],
                    stage_dir: Some(stage.clone()),
                    ..Default::default()
                },
                vm_network: None,
                attempt_id: &attempt,
            },
        )
        .unwrap();
        for path in [
            stage.join("job.lock"),
            stage.join(crate::runtime::registry::LEASE_FILENAME),
            stage
                .join("execution-operation")
                .join(crate::runtime::registry::LEASE_FILENAME),
        ] {
            assert!(
                path.is_file(),
                "launch did not initialize {}",
                path.display()
            );
        }
        let mut teardown = session.teardown(Some(0), true, false);
        assert!(
            teardown.error_message().is_none(),
            "{:?}",
            teardown.error_message()
        );
        let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
            "run_id":spec.run_id,"attempt_id":attempt,"state":"completed",
            "started_at_unix_ms":teardown.run_record().started_at_unix_ms,
            "finished_at_unix_ms":pvisor_core::unix_now_ms(),"exit_code":0
        }))
        .unwrap();
        teardown
            .persist(
                &result,
                crate::AgentCtlSnapshot {
                    run_id: spec.run_id.to_string(),
                    attempt_id: attempt.to_string(),
                    directive: crate::AgentDirective::Continue,
                    clients: vec![],
                },
                false,
            )
            .unwrap();
        teardown.complete(&result).unwrap();
        drop(teardown);
        let record = RunRecord::read(&stage).unwrap();
        seed_execution_job(&record);
        let before = tree_snapshot(root.path());
        let preview = run(request(&record, true));
        assert_eq!(preview.status, "ready", "{:?}", preview.blockers);
        assert_eq!(before, tree_snapshot(root.path()));
    }

    #[test]
    fn nested_stage_first_preview_picker_conflict_pending_and_invalid_root_never_write() {
        for scenario in ["ready", "picker", "conflict", "pending", "invalid-root"] {
            for execution_job in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let record = fixture_layout(root.path(), true);
                if execution_job {
                    seed_execution_job(&record);
                }
                match scenario {
                    "conflict" => fs::write(project(&record).join("a"), b"concurrent").unwrap(),
                    "pending" => {
                        let overlay = record.overlay.as_ref().unwrap();
                        let intent = ApplyRecord {
                            schema_version: 1,
                            apply_id: "nested-pending".into(),
                            created_at_unix_ms: 1,
                            overlay_id: overlay.id.clone(),
                            overlay_generation: overlay.generation,
                            target: "/".into(),
                            selection: Default::default(),
                            changes: vec![],
                            planned_paths: vec![],
                            preimages: vec![],
                            state: ApplyRecordState::Prepared,
                            remaining_changes: 0,
                        };
                        fs::write(
                            record.stage_dir().join("apply-ledger.json"),
                            serde_json::to_vec(
                                &serde_json::json!({"schema_version":1,"records":[intent]}),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                    }
                    "invalid-root" => {
                        let moved = root.path().join("rebound-project");
                        fs::rename(project(&record), &moved).unwrap();
                        std::os::unix::fs::symlink(moved, project(&record)).unwrap();
                    }
                    _ => {}
                }
                let before = tree_snapshot(root.path());
                let mut req = request(&record, true);
                if scenario == "picker" {
                    req.all = false;
                }
                let response = run(req);
                let expected = if matches!(scenario, "ready" | "picker") {
                    "ready"
                } else {
                    "blocked"
                };
                assert_eq!(
                    response.status, expected,
                    "{scenario}, execution_job={execution_job}: {:?}",
                    response.blockers
                );
                if expected == "ready" {
                    assert!(!response.candidates.is_empty());
                    assert!(response.plan.is_some());
                }
                assert_eq!(
                    before,
                    tree_snapshot(root.path()),
                    "preview wrote: {scenario}, execution_job={execution_job}"
                );
            }
        }
    }

    #[test]
    fn missing_observation_locks_fail_closed_without_initializing_any_path() {
        for missing in ["job", "attempt", "execution"] {
            let root = tempfile::tempdir().unwrap();
            let record = fixture_layout(root.path(), true);
            seed_execution_job(&record);
            let path = match missing {
                "job" => record.stage_dir().join("job.lock"),
                "attempt" => record
                    .stage_dir()
                    .join(crate::runtime::registry::LEASE_FILENAME),
                _ => record
                    .stage_dir()
                    .join("execution-operation")
                    .join(crate::runtime::registry::LEASE_FILENAME),
            };
            fs::remove_file(&path).unwrap();
            if missing == "execution" {
                fs::remove_dir(path.parent().unwrap()).unwrap();
            }
            let before = tree_snapshot(root.path());
            let response = run(request(&record, true));
            assert_eq!(
                response.status, "blocked",
                "{missing}: {:?}",
                response.blockers
            );
            assert!(
                response
                    .blockers
                    .join(" ")
                    .contains("preview never initializes locks")
            );
            assert_eq!(before, tree_snapshot(root.path()));
            assert!(!path.exists());
        }
    }

    #[test]
    fn missing_scope_target_never_gets_created_by_preview() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        fs::rename(project(&record), root.path().join("old-project")).unwrap();
        let before = tree_snapshot(root.path());
        let response = run(request(&record, true));
        assert_eq!(response.status, "blocked");
        assert!(!project(&record).exists());
        assert_eq!(before, tree_snapshot(root.path()));
    }

    #[test]
    fn observation_leases_exclude_mutators_without_touching_nested_stage() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture_layout(root.path(), true);
        seed_execution_job(&record);
        let before = tree_snapshot(root.path());
        {
            let _directory = crate::runtime::JobDirectoryLock::observe_existing(&record).unwrap();
            assert!(crate::runtime::JobDirectoryLock::acquire(&record, "contender").is_err());
        }
        {
            let _attempt =
                crate::runtime::registry::RunLease::observe_existing(&record.stage_dir()).unwrap();
            assert!(crate::runtime::registry::RunLease::acquire(&record.stage_dir()).is_err());
            assert_eq!(run(request(&record, true)).status, "blocked");
        }
        {
            let job = crate::runtime::job_execution::Job::read(&record)
                .unwrap()
                .unwrap();
            let _execution = crate::runtime::registry::RunLease::observe_existing(
                &job.root.join("execution-operation"),
            )
            .unwrap();
            assert!(job.lock().is_err());
            assert_eq!(run(request(&record, true)).status, "blocked");
        }
        assert_eq!(before, tree_snapshot(root.path()));
        assert_eq!(run(request(&record, true)).status, "ready");
        assert_eq!(before, tree_snapshot(root.path()));
    }

    #[test]
    fn rootstage_project_preview_is_readonly_and_partial_apply_preserves_environment() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let before = fs::read(record.stage_dir().join("run.json")).unwrap();
        let mut preview_request = request(&record, true);
        preview_request.selection.excludes = vec!["b".into()];
        let preview = run(preview_request);
        assert_eq!(preview.status, "ready", "{:?}", preview.blockers);
        assert_eq!(preview.mode, "preview");
        assert_eq!(preview.scope.kind, "project");
        assert_eq!(
            preview
                .selected
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert!(preview.required.is_empty());
        assert_eq!(preview.remaining.in_scope, Some(1));
        assert!(preview.remaining.outside_scope.unwrap() >= 1);
        assert_eq!(
            before,
            fs::read(record.stage_dir().join("run.json")).unwrap()
        );
        assert!(!record.stage_dir().join("apply-ledger.json").exists());
        assert!(
            core::load_apply_records(&record.stage_dir())
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(project(&record).join("a")).unwrap(), b"old");
        let mut execute = request(&record, false);
        execute.selection.excludes = vec!["b".into()];
        execute.expected_plan = preview.plan;
        let applied = run(execute);
        assert_eq!(applied.status, "applied", "{:?}", applied.blockers);
        assert_eq!(applied.mode, "execute");
        assert_eq!(applied.execution.unwrap().applied, Some(1));
        assert_eq!(fs::read(project(&record).join("a")).unwrap(), b"new");
        assert_eq!(fs::read(project(&record).join("b")).unwrap(), b"old");
        assert_eq!(
            fs::read(root.path().join("environment")).unwrap(),
            b"old env"
        );
        let ledger = core::load_apply_records(&record.stage_dir()).unwrap();
        assert_eq!(ledger[0].schema_version, 3);
        assert!(ledger[0].changes.iter().all(|c| {
            c.relative_path()
                .starts_with(project(&record).strip_prefix("/").unwrap())
        }));
        let current = RunRecord::read(&record.stage_dir()).unwrap();
        let mut next = request(&current, false);
        next.selection.paths = vec!["b".into()];
        next.all = false;
        assert_eq!(run(next).status, "applied");
        assert_eq!(
            fs::read(root.path().join("environment")).unwrap(),
            b"old env"
        );
    }

    #[test]
    fn rootstage_missing_contract_rebinding_and_completion_tampering_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut record = fixture(root.path());
        let original = record.clone();
        // A missing legacy contract must not be inferred from cwd/workspace.
        record.completion_protocol = None;
        record.schema_version = 1;
        record.project_apply_scope = None;
        record.write().unwrap();
        let blocked = run(request(&record, false));
        assert_eq!(blocked.status, "blocked");
        assert!(blocked.execution.is_none());
        original.write().unwrap();
        // Changed Run authority is refused even when completion validation is memoized.
        let _ = RunRecord::read(&original.stage_dir()).unwrap();
        record = original.clone();
        record.project_apply_scope = None;
        record.write().unwrap();
        assert!(RunRecord::read(&record.stage_dir()).is_err());
        original.write().unwrap();
        fs::rename(project(&original), root.path().join("old-project")).unwrap();
        fs::create_dir(project(&original)).unwrap();
        let blocked = run(request(&original, true));
        assert_eq!(blocked.status, "blocked");
        assert!(blocked.blockers.join(" ").contains("identity changed"));
        assert!(ProjectApplyScope::capture(Path::new("/")).is_err());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(project(&original), &alias).unwrap();
        assert!(ProjectApplyScope::capture(&alias).is_err());
    }

    #[test]
    fn rootstage_globs_are_scope_relative_and_conflicts_never_write() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let mut req = request(&record, true);
        req.all = false;
        req.selection.includes = vec!["a".into()];
        let preview = run(req);
        assert_eq!(preview.status, "ready", "{:?}", preview.blockers);
        assert_eq!(preview.selected.len(), 1);
        fs::write(project(&record).join("a"), b"concurrent").unwrap();
        let conflict = run(request(&record, true));
        assert_eq!(conflict.status, "blocked");
        assert_eq!(conflict.remaining.in_scope, None);
        assert_eq!(conflict.remaining.outside_scope, None);
        assert!(!conflict.candidates.is_empty());
        assert!(
            core::load_apply_records(&record.stage_dir())
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(project(&record).join("b")).unwrap(), b"old");
    }

    #[test]
    fn changed_plan_requires_confirmation_and_invalid_selections_do_not_execute() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let preview = run(request(&record, true));
        let upper = record.overlay.as_ref().unwrap().upper.path();
        fs::write(
            upper.join(project(&record).join("a").strip_prefix("/").unwrap()),
            b"changed since preview",
        )
        .unwrap();
        let mut req = request(&record, false);
        req.expected_plan = preview.plan;
        let blocked = run(req);
        assert_eq!(blocked.status, "blocked");
        assert!(blocked.plan.is_some());
        assert!(blocked.execution.is_none());
        for path in ["/absolute", "../escape", "a/../../escape"] {
            let mut req = request(&record, false);
            req.all = false;
            req.selection.paths = vec![path.into()];
            assert_eq!(run(req).status, "blocked");
        }
        let mut req = request(&record, false);
        req.all = false;
        req.selection.excludes = vec!["b".into()];
        assert_eq!(run(req).status, "blocked");
        assert_eq!(fs::read(project(&record).join("a")).unwrap(), b"old");
    }

    #[test]
    fn pending_preview_and_execution_never_implicitly_recover() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let overlay = record.overlay.as_ref().unwrap();
        let intent = ApplyRecord {
            schema_version: 1,
            apply_id: "pending".into(),
            created_at_unix_ms: 1,
            overlay_id: overlay.id.clone(),
            overlay_generation: overlay.generation,
            target: "/".into(),
            selection: Default::default(),
            changes: vec![],
            planned_paths: vec![],
            preimages: vec![],
            state: ApplyRecordState::Prepared,
            remaining_changes: 0,
        };
        let path = record.stage_dir().join("apply-ledger.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"schema_version":1,"records":[intent]}))
                .unwrap(),
        )
        .unwrap();
        let bytes = fs::read(&path).unwrap();
        for dry in [true, false] {
            let blocked = run(request(&record, dry));
            assert_eq!(blocked.status, "blocked", "{:?}", blocked.blockers);
            assert!(blocked.blockers.join(" ").contains("pending"));
            assert_eq!(bytes, fs::read(&path).unwrap());
        }
        assert_eq!(fs::read(project(&record).join("a")).unwrap(), b"old");
    }

    #[test]
    fn mapped_exclusions_and_cross_scope_hardlinks_cannot_expand_authority() {
        for outside in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let record = fixture(root.path());
            let upper = record.overlay.as_ref().unwrap().upper.path();
            let a = upper.join(project(&record).join("a").strip_prefix("/").unwrap());
            let other = if outside {
                root.path().join("environment")
            } else {
                project(&record).join("b")
            };
            let other = upper.join(other.strip_prefix("/").unwrap());
            fs::remove_file(&other).unwrap();
            fs::hard_link(&a, &other).unwrap();
            let mut req = request(&record, true);
            req.all = false;
            req.selection.paths = vec!["a".into()];
            if !outside {
                req.selection.excludes = vec!["b".into()];
            }
            let response = run(req);
            assert_eq!(response.status, "blocked", "{:?}", response.blockers);
            assert!(response.execution.is_none());
            assert!(
                core::load_apply_records(&record.stage_dir())
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(fs::read(project(&record).join("a")).unwrap(), b"old");
        }
    }

    #[test]
    fn ordinary_workspace_preview_and_execute_use_recorded_target_contract() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let stage = root.path().join("stage");
        for path in [
            &target,
            &stage,
            &stage.join("upper"),
            &stage.join("work"),
            &stage.join("merged"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(target.join("a"), b"old").unwrap();
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"legacy-command","attempt_id":"attempt-legacy",
            "session_id":"legacy-command","agent":"test","pid":0,"command":["true"],
            "state":"completed","started_at_unix_ms":1,"finished_at_unix_ms":2,
            "storage":stage,"gateway_listen":null,"network":{},
            "overlay":{"id":"legacy-command","generation":0,"target":target,
                "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                "stage_dir":stage,"merged_dir":stage.join("merged"),"state":"staged","auto_apply":false}
        })).unwrap();
        let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            Some(stage.join("preimages")),
        )
        .unwrap();
        fs::write(core.copy_up(Path::new("a")).unwrap(), b"new").unwrap();
        drop(core);
        record.write().unwrap();
        // Legacy fixture predates launch-time observer initialization. Seed locks
        // through its real mutating APIs before taking the read-only snapshot.
        drop(crate::runtime::JobDirectoryLock::acquire(&record, "legacy-fixture").unwrap());
        drop(crate::runtime::registry::RunLease::acquire(&stage).unwrap());
        // The request contract has no target override; both phases use this owning target.
        let declaration = include_str!("apply_command.rs")
            .split("pub struct ApplyCommandRequest {")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert!(!declaration.contains("pub target:"));
        let preview = run(request(&record, true));
        assert_eq!(preview.status, "ready", "{:?}", preview.blockers);
        assert_eq!(preview.remaining.in_scope, Some(0));
        assert_eq!(fs::read(target.join("a")).unwrap(), b"old");
        let mut execute = request(&record, false);
        execute.expected_plan = preview.plan;
        let applied = run(execute);
        assert_eq!(applied.status, "applied", "{:?}", applied.blockers);
        assert_eq!(fs::read(target.join("a")).unwrap(), b"new");
    }

    #[test]
    fn picker_empty_selection_only_observes_candidates_and_all_preview_selects_them() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let mut picker = request(&record, true);
        picker.all = false;
        let response = run(picker);
        assert_eq!(response.mode, "preview");
        assert_eq!(response.status, "ready", "{:?}", response.blockers);
        assert_eq!(response.candidates.len(), 2);
        assert!(response.selected.is_empty() && response.required.is_empty());
        assert_eq!(response.remaining.in_scope, Some(2));
        assert!(response.execution.is_none());
        let all = run(request(&record, true));
        assert_eq!(all.status, "ready", "{:?}", all.blockers);
        assert_eq!(all.selected.len(), 2);
        assert_eq!(all.candidates.len(), 2);
        assert_eq!(all.remaining.in_scope, Some(0));
        assert_eq!(
            all.remaining.outside_scope,
            response.remaining.outside_scope
        );
        let mut confirm = request(&record, false);
        confirm.all = false;
        confirm.selection.paths = vec!["a".into()];
        confirm.expected_plan = response.plan;
        assert_eq!(run(confirm).status, "applied");
    }

    #[test]
    fn preview_remaining_deducts_actual_required_paths_and_not_unmatched_patterns() {
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let upper = record.overlay.as_ref().unwrap().upper.path();
        let a = upper.join(project(&record).join("a").strip_prefix("/").unwrap());
        let b = upper.join(project(&record).join("b").strip_prefix("/").unwrap());
        fs::remove_file(&b).unwrap();
        fs::hard_link(a, b).unwrap();
        let mut req = request(&record, true);
        req.all = false;
        req.selection.paths = vec!["a".into()];
        let response = run(req);
        assert_eq!(response.status, "ready", "{:?}", response.blockers);
        assert_eq!(response.selected.len(), 1);
        assert_eq!(response.required.len(), 1);
        assert_eq!(response.remaining.in_scope, Some(0));
        let mut unmatched = request(&record, true);
        unmatched.all = false;
        unmatched.selection.paths = vec!["not-staged".into()];
        let response = run(unmatched);
        assert_eq!(response.status, "ready", "{:?}", response.blockers);
        assert!(response.selected.is_empty() && response.required.is_empty());
        assert_eq!(response.remaining.in_scope, Some(2));
        assert!(
            core::load_apply_records(&record.stage_dir())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn mismatched_record_workspace_cannot_authorize_rootstage_project_apply() {
        let root = tempfile::tempdir().unwrap();
        let mut record = fixture(root.path());
        record.workspace = Some(root.path().join("other-project"));
        record.write().unwrap();
        let response = run(request(&record, true));
        assert_eq!(response.status, "blocked");
        assert!(
            response
                .blockers
                .join(" ")
                .contains("must match recorded workspace")
        );
        assert_eq!(response.remaining.in_scope, None);
        assert!(response.execution.is_none());
    }

    #[test]
    fn execution_error_reports_uncertain_counts_and_ledger_evidence() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = tempfile::tempdir().unwrap();
        let record = fixture(root.path());
        let calls = AtomicUsize::new(0);
        let check = |_: &RunRecord| {
            if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                fs::write(project(&record).join("a"), b"late conflict")?;
            }
            Ok(())
        };
        let context = ServiceContext {
            check_record: Some(&check),
            ..Default::default()
        };
        let response = RuntimeJobService::apply_command(&context, request(&record, false)).unwrap();
        assert_eq!(response.status, "error", "{:?}", response.blockers);
        let execution = response.execution.unwrap();
        assert_eq!(execution.applied, None);
        assert!(execution.error.is_some());
        assert_eq!(response.remaining.in_scope, None);
        assert_eq!(fs::read(project(&record).join("b")).unwrap(), b"old");
    }
}
