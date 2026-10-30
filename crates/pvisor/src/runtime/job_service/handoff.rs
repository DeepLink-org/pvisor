//! Non-consuming project artifact handoff; deliberately not apply or a snapshot.
#![deny(missing_docs)]

use super::{JobSelection, RuntimeJobService, ServiceContext};
use anyhow::ensure;
use pvisor_core::overlay::ChangeEntry;
use std::path::{Path, PathBuf};

/// Explicit selection of retained project changes, never arbitrary rootfs paths.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    /// Persisted Job selector and storage; normal admission and identity fences apply.
    pub job: JobSelection,
    /// New output directory. Its parent must exist; existing outputs are never replaced.
    pub output: PathBuf,
    /// Project-relative file or subtree selections. Empty, absolute and parent components
    /// are rejected even with `all`. Selections do not pull in unselected dependencies.
    pub paths: Vec<PathBuf>,
    /// Explicitly select all changes within the recorded non-root project workspace.
    /// Mutually exclusive with nonempty `paths`.
    pub all: bool,
}

/// Published artifact inventory, also written verbatim to `manifest.json`.
/// Only regular staged contents appear beneath `files/`; directories and deletions
/// are inventory entries. No xattrs, ownership, whiteouts or backing permissions
/// are copied. This is neither a merged snapshot nor an applicable patch.
#[derive(Debug, serde::Serialize)]
pub struct ExportResponse {
    /// Artifact manifest schema version; currently always 1.
    pub schema_version: u32,
    /// Attempt identity from the validated historical Bundle, or None if absent.
    pub attempt_id: Option<String>,
    /// Execution outcome from the validated historical Bundle, not export success.
    pub state: pvisor_core::RunState,
    /// Recorded process exit code; None when the Bundle has no exit code.
    pub exit_code: Option<i32>,
    /// True for Failed, Cancelled, or Completed with a recorded nonzero exit code.
    /// False does not certify artifact completeness, correctness or dependency closure.
    pub partial_results: bool,
    /// Locked, completion-validated Job identity.
    pub job_id: String,
    /// Canonical parent plus the requested new directory name.
    pub destination: PathBuf,
    /// Strict absolute lexical non-root project scope from the Run record.
    /// Opened component-wise without following symlinks; never relocated via aliases.
    pub project_root: PathBuf,
    /// Selected non-deletion changes with project-relative paths and logical modes.
    /// Directory entries need not have content in `files/`. Ancestor directories
    /// used for packaging have private packaging modes, not staged logical modes.
    pub entries: Vec<ChangeEntry>,
    /// Selected deletions; informational only, never performed on the project.
    pub deletions: Vec<ChangeEntry>,
}

impl RuntimeJobService {
    /// Export selected artifacts without consuming the stage or modifying the project.
    ///
    /// Requires exactly one of `all` or nonempty project-relative `paths`, a completed/sealed stage
    /// and a readable validated Run Bundle (committed completion for modern Jobs).
    /// Failed/cancelled Attempts with committed results are eligible. Live, Unknown,
    /// active, applied and dropped stages are refused. Job-directory, execution-Job
    /// and review's Attempt leases remain held through publication; context admission
    /// and cancellation are rechecked before publication. Callbacks must not take leases.
    ///
    /// Linux directory uppers are supported. Other platforms/backends return
    /// Unsupported. Selected symlinks, special files, type replacements and opaque
    /// dependencies (including ancestors outside the selected project) are Unsupported
    /// before publication. Ordinary directory additions/deletions are manifest-only.
    /// An empty selected project inventory is refused without publishing an output.
    /// Workspace, historical scope and target comparisons are lexical; workspace and
    /// all its ancestors must open nofollow. No historical directory identity is stored,
    /// so replacement of an ordinary directory since execution cannot be detected.
    /// Selection is lexical, not a build/package dependency closure: unselected files,
    /// lower-only contents and external effects are not exported.
    ///
    /// Source reads use component-wise nofollow descriptors and the caller's effective
    /// identity; no sudo identity override or privilege escalation is attempted. Output
    /// writes use an opened parent and exclusive creation, then atomic no-replace rename.
    /// Parent namespace rebinding cannot redirect writes to a replacement directory.
    /// Errors leave no published output from this call and retain the stage for retry;
    /// an existing/concurrently created output is untouched. As with existing runtime
    /// leases, this does not defend against a hostile process with unrestricted same-UID
    /// access to the retained stage. Publication is not a power-loss durability claim.
    pub fn export(
        context: &ServiceContext<'_>,
        request: ExportRequest,
    ) -> anyhow::Result<ExportResponse> {
        ensure!(
            request.all || !request.paths.is_empty(),
            "export requires --all or nonempty project-relative paths"
        );
        ensure!(
            !request.all || request.paths.is_empty(),
            "export --all conflicts with project-relative paths"
        );
        for path in &request.paths {
            validate_relative(path)?;
        }
        #[cfg(target_os = "linux")]
        {
            linux::export(context, request)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = context;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "project export requires Linux directory uppers",
            )
            .into())
        }
    }
}

fn validate_relative(path: &Path) -> anyhow::Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "export paths must be nonempty project-relative paths without . or ..: {}",
        path.display()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::super::ReviewRequest;
    use super::*;
    use anyhow::Context;
    use pvisor_core::overlay::{ChangeEntryType, ChangeKind, OverlayState};
    use std::ffi::{CString, OsStr};
    use std::fs::{File, Permissions};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    };

    fn unsupported(message: &str) -> anyhow::Error {
        io::Error::new(io::ErrorKind::Unsupported, message).into()
    }
    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
    }
    fn cvt(rc: i32) -> io::Result<()> {
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    fn open_at(
        parent: &File,
        component: &OsStr,
        directory: bool,
        create: bool,
    ) -> io::Result<File> {
        let component = name(component)?;
        let flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory {
                libc::O_RDONLY | libc::O_DIRECTORY
            } else if create {
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            } else {
                libc::O_RDONLY
            };
        // SAFETY: a live parent descriptor and NUL-terminated single component.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn validate_absolute(path: &Path) -> anyhow::Result<()> {
        ensure!(
            path.is_absolute()
                && !path
                    .as_os_str()
                    .as_bytes()
                    .split(|b| *b == b'/')
                    .any(|c| c == b"." || c == b".."),
            "scope must be a strict absolute lexical path without . or ..: {}",
            path.display()
        );
        Ok(())
    }
    fn absolute_dir(path: &Path) -> anyhow::Result<File> {
        ensure!(path.is_absolute(), "directory must be absolute");
        let mut directory = File::open("/")?;
        for c in path.strip_prefix("/")?.components() {
            let std::path::Component::Normal(c) = c else {
                anyhow::bail!("invalid directory path");
            };
            directory = open_at(&directory, c, true, false)?;
        }
        Ok(directory)
    }
    fn rooted(root: &File, path: &Path, directory: bool) -> anyhow::Result<File> {
        let mut current = root.try_clone()?;
        let components: Vec<_> = path.components().collect();
        for (i, c) in components.iter().enumerate() {
            let std::path::Component::Normal(c) = c else {
                anyhow::bail!("invalid rooted path");
            };
            current = open_at(&current, c, i + 1 != components.len() || directory, false)?;
        }
        Ok(current)
    }
    fn attr(file: &File, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let key = CString::new(key)?;
        let mut bytes = vec![0; 4096];
        // SAFETY: valid descriptor, name and writable buffer; bounded control values.
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                key.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n < 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
                return Ok(None);
            }
            return Err(error.into());
        }
        bytes.truncate(n as usize);
        Ok(Some(bytes))
    }
    fn check_directory(file: &File) -> anyhow::Result<()> {
        for key in ["trusted.overlay.opaque", "user.overlay.opaque"] {
            if attr(file, key)?.is_some_and(|v| v == b"y" || v == b"Y" || v == b"1") {
                return Err(unsupported(
                    "opaque directory dependency cannot be exported",
                ));
            }
        }
        let marker = CString::new(pvisor_overlay_core::OPAQUE_NAME)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid dirfd, name and output. Never follows a marker symlink.
        let rc = unsafe {
            libc::fstatat(
                file.as_raw_fd(),
                marker.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            return Err(unsupported(
                "opaque directory dependency cannot be exported",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOENT) {
            return Err(error.into());
        }
        Ok(())
    }
    fn check_ancestors(root: &File, path: &Path) -> anyhow::Result<()> {
        check_directory(root)?;
        let mut current = root.try_clone()?;
        for c in path.components() {
            let std::path::Component::Normal(c) = c else {
                anyhow::bail!("invalid ancestor");
            };
            current = open_at(&current, c, true, false)?;
            check_directory(&current)?;
        }
        Ok(())
    }
    fn disjoint(a: &Path, b: &Path) -> anyhow::Result<()> {
        ensure!(
            !a.starts_with(b) && !b.starts_with(a),
            "export destination overlaps stage, project or authority: {}",
            b.display()
        );
        Ok(())
    }
    fn same_directory(file: &File, path: &Path) -> anyhow::Result<()> {
        let current = absolute_dir(path)?.metadata()?;
        let held = file.metadata()?;
        ensure!(
            current.dev() == held.dev() && current.ino() == held.ino(),
            "export parent changed"
        );
        Ok(())
    }

    pub(super) fn export(
        context: &ServiceContext<'_>,
        request: ExportRequest,
    ) -> anyhow::Result<ExportResponse> {
        let selected = RuntimeJobService::resolve(context, &request.job)?;
        let _directory_lock =
            crate::runtime::registry::JobDirectoryLock::acquire(&selected, "export")?;
        let execution_job = super::super::lock_selected_job(context, &selected)?;
        let selected_upper = absolute_dir(
            selected
                .overlay
                .as_ref()
                .context("export requires a workspace stage")?
                .upper
                .path(),
        )?;
        // review owns the sole RunLease and performs completion/Bundle validation.
        let review = RuntimeJobService::review(
            context,
            ReviewRequest {
                job: request.job,
                checkpoint: None,
            },
        )?;
        super::super::check_selected_record(&selected, &review.record)?;
        let record = &review.record;
        ensure!(
            matches!(
                record.state,
                crate::RunRecordState::Completed
                    | crate::RunRecordState::Failed
                    | crate::RunRecordState::Cancelled
            ),
            "export requires a completed, failed or cancelled Attempt"
        );
        let overlay = record
            .overlay
            .as_ref()
            .context("export requires a retained workspace stage")?;
        ensure!(
            overlay.state == OverlayState::Staged,
            "export requires a sealed staged workspace, not active/applied/dropped"
        );

        let workspace = record
            .workspace
            .as_ref()
            .context("Job has no recorded project workspace")?;
        validate_absolute(workspace)?;
        let project = workspace.clone();
        ensure!(
            project != Path::new("/"),
            "root is not a project export scope"
        );
        let _project = absolute_dir(&project)?;
        if let Some((job, _)) = &execution_job
            && let Some(value) = job.spec.metadata.get("pvisor.workspace")
        {
            let historical: PathBuf = serde_json::from_value(value.clone())?;
            ensure!(
                validate_absolute(&historical).is_ok() && historical == project,
                "historical spec workspace disagrees with Run record"
            );
        }
        // Older/extended Bundles can carry spec metadata even though today's typed
        // Bundle does not retain it. Read only the already validated historical file.
        let historical: serde_json::Value = serde_json::from_slice(
            &crate::runtime::completion::read_file(&crate::RunBundle::path(&record.stage_dir()))?,
        )?;
        if let Some(value) = historical.pointer("/spec/metadata/pvisor.workspace") {
            let path: PathBuf = serde_json::from_value(value.clone())?;
            ensure!(
                validate_absolute(&path).is_ok() && path == project,
                "Bundle spec workspace disagrees with Run record"
            );
        }
        validate_absolute(&overlay.target)?;
        let target = &overlay.target;
        let prefix = if target == Path::new("/") {
            project.strip_prefix("/")?.to_path_buf()
        } else {
            ensure!(
                target == &project,
                "stage target does not match recorded workspace"
            );
            PathBuf::new()
        };
        let upper_path = overlay.upper.path().canonicalize()?;
        // Open the original path too, so a symlink upper is not silently accepted.
        let upper = absolute_dir(overlay.upper.path())?;
        same_directory(&upper, &upper_path)?;
        let before = selected_upper.metadata()?;
        let current = upper.metadata()?;
        ensure!(
            before.dev() == current.dev() && before.ino() == current.ino(),
            "stage upper changed during review"
        );
        let private_backing = match attr(&upper, pvisor_overlay_core::PRIVATE_BACKING_XATTR)? {
            None => false,
            Some(value) if value == b"1" => true,
            Some(_) => anyhow::bail!("invalid private backing marker"),
        };
        check_ancestors(&upper, &prefix).map_err(|error| {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
            {
                anyhow::anyhow!(
                    "no selected project changes: project prefix is absent from stage upper"
                )
            } else {
                error
            }
        })?;
        // Validate explicit selections even when an inventory has no leaf below a
        // symlink/special ancestor. A missing upper component can be a deletion
        // or lower-only selection; never follow it into a lower filesystem.
        for selection in &request.paths {
            let full = prefix.join(selection);
            let components: Vec<_> = full.components().collect();
            let mut current = upper.try_clone()?;
            for (index, component) in components.iter().enumerate() {
                let std::path::Component::Normal(component) = component else {
                    unreachable!()
                };
                match open_at(&current, component, index + 1 != components.len(), false) {
                    Ok(next) => {
                        let metadata = next.metadata()?;
                        if metadata.is_dir() {
                            check_directory(&next)?;
                        } else if !metadata.is_file() {
                            return Err(unsupported("selected special file is unsupported"));
                        }
                        current = next;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => break,
                    Err(error)
                        if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) =>
                    {
                        return Err(unsupported(
                            "selected symlink or non-directory ancestor is unsupported",
                        ));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let output_name = request
            .output
            .file_name()
            .context("export output needs a new directory name")?;
        let parent_path = request
            .output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        let destination = parent_path.join(output_name);
        for protected in [
            project.clone(),
            record.stage_dir().canonicalize()?,
            record.storage.canonicalize()?,
            upper_path,
            crate::host_transport::host_authority_root()?,
        ] {
            disjoint(&destination, &protected)?;
        }
        let parent = absolute_dir(&parent_path)?;
        let filesystem = review
            .bundle
            .filesystem
            .as_ref()
            .context("Bundle has no workspace evidence")?;
        let selected =
            |path: &Path| request.all || request.paths.iter().any(|p| path.starts_with(p));
        let mut entries = Vec::new();
        let mut deletions = Vec::new();
        let mut sources = Vec::new();
        for change in &filesystem.changes {
            let stage_path = change.relative_path();
            // Dependencies above the scope must not be silently omitted.
            if change.kind == ChangeKind::Opaque && prefix.starts_with(&stage_path) {
                return Err(unsupported("project depends on opaque ancestor"));
            }
            let Ok(path) = stage_path.strip_prefix(&prefix) else {
                continue;
            };
            if !selected(path) {
                continue;
            }
            if change.kind == ChangeKind::Opaque || change.kind == ChangeKind::TypeChanged {
                return Err(unsupported("opaque/type replacement export is unsupported"));
            }
            if path.as_os_str().is_empty() {
                continue;
            }
            validate_relative(path)?;
            let mut entry = change.clone();
            entry.path = path
                .to_str()
                .context("non-UTF8 export paths are unsupported")?
                .to_owned();
            entry.path_bytes = None;
            check_ancestors(&upper, stage_path.parent().unwrap_or(Path::new("")))?;
            if change.kind == ChangeKind::Deleted {
                if matches!(
                    change.old_type,
                    Some(ChangeEntryType::Symlink | ChangeEntryType::Other)
                ) {
                    return Err(unsupported(
                        "symlink/special deletion export is unsupported",
                    ));
                }
                deletions.push(entry);
                continue;
            }
            let directory = change.new_type == Some(ChangeEntryType::Directory);
            if !directory && change.new_type != Some(ChangeEntryType::File) {
                return Err(unsupported(
                    "selected symlink/special export is unsupported",
                ));
            }
            let source = rooted(&upper, &stage_path, directory)?;
            let metadata = source.metadata()?;
            ensure!(
                if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                },
                "staged source type changed"
            );
            if directory {
                check_directory(&source)?;
            }
            let logical = match attr(&source, pvisor_overlay_core::OVERRIDE_STAT_XATTR)? {
                Some(bytes) => pvisor_overlay_core::backend::UnixIdentity::parse(&bytes)?.mode,
                None if private_backing => {
                    anyhow::bail!("private backing source lacks logical metadata")
                }
                None => metadata.mode(),
            };
            ensure!(
                logical & libc::S_IFMT == 0
                    || logical & libc::S_IFMT == metadata.mode() & libc::S_IFMT,
                "logical source type disagrees with backing"
            );
            entry.mode = Some(logical & 0o7777);
            entry.size_bytes = (!directory).then_some(metadata.len());
            if !directory {
                sources.push((path.to_path_buf(), source));
            }
            entries.push(entry);
        }
        ensure!(
            !entries.is_empty() || !deletions.is_empty(),
            "no selected project changes"
        );
        let run = &review.bundle.run;
        let response = ExportResponse {
            schema_version: 1,
            attempt_id: (!run.attempt_id.is_empty()).then(|| run.attempt_id.clone()),
            state: run.state,
            exit_code: run.exit_code,
            partial_results: matches!(
                run.state,
                pvisor_core::RunState::Failed | pvisor_core::RunState::Cancelled
            ) || (run.state == pvisor_core::RunState::Completed
                && run.exit_code.is_some_and(|code| code != 0)),
            job_id: run.run_id.clone(),
            destination,
            project_root: project.clone(),
            entries,
            deletions,
        };
        let pending = Pending::new(parent)?;
        let files = pending.mkdir(&pending.root, OsStr::new("files"))?;
        for (path, mut source) in sources {
            let mut directory = files.try_clone()?;
            for c in path.parent().unwrap_or(Path::new("")).components() {
                let std::path::Component::Normal(c) = c else {
                    unreachable!()
                };
                directory = pending.mkdir(&directory, c)?;
            }
            let mut output = open_at(&directory, path.file_name().unwrap(), false, true)?;
            io::copy(&mut source, &mut output)?;
            output.set_permissions(Permissions::from_mode(0o600))?;
        }
        let manifest = open_at(&pending.root, OsStr::new("manifest.json"), false, true)?;
        serde_json::to_writer_pretty(manifest, &response)?;
        context.check(record)?;
        // Recheck the lexical workspace after callbacks; this compares identities
        // only within this call, not against an unavailable historical identity.
        same_directory(&_project, &project)?;
        same_directory(&pending.parent, &parent_path)?;
        pending.publish(output_name)?;
        Ok(response)
    }

    struct Pending {
        parent: File,
        root: File,
        name: CString,
        published: bool,
    }
    impl Pending {
        fn new(parent: File) -> anyhow::Result<Self> {
            let name = CString::new(format!(".pvisor-export-{}", uuid::Uuid::new_v4()))?;
            // SAFETY: exclusive mkdir under the held parent.
            cvt(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })?;
            let root = open_at(&parent, OsStr::from_bytes(name.as_bytes()), true, false)?;
            Ok(Self {
                parent,
                root,
                name,
                published: false,
            })
        }
        fn mkdir(&self, parent: &File, component: &OsStr) -> anyhow::Result<File> {
            let name = name(component)?;
            // SAFETY: single component under a held dirfd; existing directories
            // are reopened nofollow (only packaging ancestors may already exist).
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if rc != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error().into());
            }
            Ok(open_at(parent, component, true, false)?)
        }
        fn publish(mut self, output: &OsStr) -> anyhow::Result<()> {
            let current = open_at(
                &self.parent,
                OsStr::from_bytes(self.name.as_bytes()),
                true,
                false,
            )?
            .metadata()?;
            let held = self.root.metadata()?;
            ensure!(
                current.dev() == held.dev() && current.ino() == held.ino(),
                "pending export changed"
            );
            let output = name(output)?;
            // SAFETY: both names are single components anchored to the same parent.
            cvt(unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    self.parent.as_raw_fd(),
                    self.name.as_ptr(),
                    self.parent.as_raw_fd(),
                    output.as_ptr(),
                    libc::RENAME_NOREPLACE,
                ) as i32
            })?;
            self.published = true;
            Ok(())
        }
    }
    fn clean(directory: &File) {
        // Linux proc descriptor traversal stays bound to the opened directory.
        if let Ok(entries) = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd())) {
            for entry in entries.flatten() {
                let component = entry.file_name();
                let Ok(name) = name(&component) else {
                    continue;
                };
                let flags = if let Ok(child) = open_at(directory, &component, true, false) {
                    clean(&child);
                    libc::AT_REMOVEDIR
                } else {
                    0
                };
                // SAFETY: unlink affects only this held directory entry, never its target.
                unsafe {
                    libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags);
                }
            }
        }
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            if !self.published {
                clean(&self.root);
                // SAFETY: remove only the private pending directory, never the output.
                unsafe {
                    libc::unlinkat(
                        self.parent.as_raw_fd(),
                        self.name.as_ptr(),
                        libc::AT_REMOVEDIR,
                    );
                }
            }
        }
    }
}
