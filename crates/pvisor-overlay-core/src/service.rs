//! Shared file-operation service used by host FUSE and VM virtio-fs adapters.
use crate::{BackingResolution, DirectoryEntry, OverlayCore, Resolved, ResolvedMetadata, backend};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

/// Request-local open preparation, not a capability for a future open.
/// Writable files are already copied up; read-only files retain checked parent
/// identities for native inode lookup without resolving the path again.
#[allow(clippy::large_enum_variant)] // Request-local stack value; avoid an allocation per read-only open.
pub enum OpenBacking {
    Writable(PathBuf),
    ReadOnly(BackingResolution),
}

/// Shared policy and overlay semantics. Adapters own protocol identifiers,
/// descriptors, permission translation and operation serialization. The core
/// remains private: adding a core method does not expand this service's API.
pub struct FilesystemService {
    core: OverlayCore,
}

/// Suspends an explicitly configured physical-upper absence cache for one mutation.
///
/// The caller must already hold its exclusive namespace-operation lock before
/// construction, and keep that lock until after this guard is dropped. This
/// guard does not acquire that lock or drain asynchronous I/O. Construction
/// disables cache lookup/publication and clears prior observations; drop clears
/// again, including after errors or partial mutations, then restores the configured
/// preference. Enabled-owner mutation guards must not nest. Disabled owners
/// accept this guard as a no-op. Neither cache contents nor guards are snapshot state.
/// Mutations include directory traversal permissions and relevant metadata/xattrs,
/// not only creating, deleting or renaming directory entries.
#[must_use = "hold the mutation guard until all namespace work and cleanup have finished"]
pub struct UpperNamespaceMutation<'a> {
    core: &'a OverlayCore,
    armed: bool,
}

impl Drop for UpperNamespaceMutation<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.core.end_upper_namespace_mutation();
        }
    }
}

impl FilesystemService {
    pub fn new(core: OverlayCore) -> Self {
        Self { core }
    }
    /// Configure once before serving requests; the default is disabled.
    ///
    /// Enabling is a caller-owned admission contract: all physical upper
    /// namespace and directory-permission changes must be serialized by an
    /// exclusive operation lock and `upper_namespace_mutation`, while every
    /// cached resolution holds the matching shared lock. The upper root,
    /// physical ancestors and mount identity must remain stable for this owner.
    /// External writers, lazy upper projections and hidden namespace mutations
    /// during shared operations are unsupported. Descriptor-bound writes to
    /// existing regular files may continue if they cannot change namespace or
    /// directory traversal permissions. This does not declare the upper or
    /// merged view immutable, cache authorization, or extend guest validity.
    ///
    /// Canonical upper/preimage paths must be disjoint (equal, parent/child and
    /// symlink-alias overlap return `InvalidInput`); read observations can publish
    /// journal entries without an exclusive operation. Validation failure before
    /// the first configuration leaves it unconfigured and disabled. Reconfiguring
    /// an already configured owner is `InvalidInput` and preserves its preference.
    /// Disabled configuration preserves existing layouts, including overlapping
    /// journals, without enabling caching.
    pub fn configure_exclusive_upper_absence_cache(&mut self, enabled: bool) -> io::Result<()> {
        self.core.configure_exclusive_upper_absence_cache(enabled)
    }
    /// Return the configured feature preference, including during mutation suspension.
    /// This is false before configuration and on disabled owners. It is not
    /// evidence that the caller's namespace-ownership promise has been enforced.
    pub fn exclusive_upper_absence_cache_enabled(&self) -> bool {
        self.core.exclusive_upper_absence_cache_enabled()
    }
    /// Validate upper/journal separation for a caller-owned exclusive namespace.
    ///
    /// Canonical upper and preimage directories must not overlap in either
    /// direction, including symlink aliases. Overlap is `InvalidInput`; native
    /// canonicalization errors propagate. No cache, lock or ownership promise
    /// is installed or changed. The caller must separately serialize namespace
    /// and directory-permission mutations and preserve roots/ancestors/mounts.
    pub fn validate_exclusive_upper_namespace(&self) -> io::Result<()> {
        self.core.validate_exclusive_upper_namespace()
    }
    /// Disable and invalidate the physical-upper absence cache for a mutation.
    /// Caller holds its exclusive operation lock through this guard's drop;
    /// enabled-owner guards must not nest. No caller lock or I/O drain is acquired.
    pub fn upper_namespace_mutation(&self) -> UpperNamespaceMutation<'_> {
        UpperNamespaceMutation {
            core: &self.core,
            armed: self.core.begin_upper_namespace_mutation(),
        }
    }
    /// Prepare byte copying without publishing namespace entries. The adapter
    /// serializes mutations while metadata may continue; see Core for policy,
    /// source-change, truncation and cancellation contracts.
    pub fn prepare_copy_ups(
        &self,
        paths: &[PathBuf],
        truncate: bool,
    ) -> io::Result<Vec<crate::PreparedCopyUp>> {
        self.core.prepare_copy_ups(paths, truncate)
    }
    /// Install request-scoped copies on their original owner. Normal operations
    /// perform validation and publication; drop cancels unused copies. Mutation
    /// serialization is the caller's responsibility, including through drop.
    pub fn use_prepared_copy_ups(
        &self,
        copies: Vec<crate::PreparedCopyUp>,
    ) -> io::Result<crate::PreparedCopyUps<'_>> {
        self.core.use_prepared_copy_ups(copies)
    }
    pub fn with_profile(mut self, profile: crate::profile::Profile) -> Self {
        self.core = self.core.with_profile(profile);
        self
    }
    pub fn with_access_policy(mut self, policy: &crate::FileAccessPolicy) -> Self {
        self.core = self.core.with_access_policy(policy);
        self
    }
    pub fn with_immutable_content_index(
        mut self,
        root: &Path,
        index: std::path::PathBuf,
        sha256: &str,
    ) -> io::Result<Self> {
        self.core = self
            .core
            .with_immutable_content_index(root, index, sha256)?;
        Ok(self)
    }
    /// Keep an opened native descriptor's identity across unlink/replacement.
    /// Remote backing is immutable and is addressed by its captured backing path.
    pub fn read_at(
        &self,
        backing: &Path,
        file: &File,
        bytes: &mut [u8],
        offset: u64,
    ) -> io::Result<usize> {
        if let Some(data) = self.read_remote(
            backing,
            offset,
            bytes
                .len()
                .try_into()
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
        )? {
            if data.len() > bytes.len() {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            bytes[..data.len()].copy_from_slice(&data);
            Ok(data.len())
        } else {
            file.read_at(bytes, offset)
        }
    }
    /// Native adapters retain their descriptor-based zero-copy fast path.
    pub fn read_remote(
        &self,
        backing: &Path,
        offset: u64,
        size: u32,
    ) -> io::Result<Option<Vec<u8>>> {
        backend::read_at(backing, offset, size)
    }
    /// Classify flags once for both adapters' operation-lock and copy-up decisions.
    pub fn is_write_open(flags: i32) -> bool {
        flags & libc::O_ACCMODE != libc::O_RDONLY || flags & (libc::O_APPEND | libc::O_TRUNC) != 0
    }
    fn prepare_write_open(&self, rel: &Path, flags: i32) -> io::Result<PathBuf> {
        if self.core.metadata(rel)?.file_type().is_symlink() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        self.core.copy_up_for_open(rel, flags & libc::O_TRUNC != 0)
    }
    /// Resolve a no-follow open, recording reads or copying up writes.
    /// The adapter owns the native descriptor and must open it without following links.
    pub fn prepare_open(&self, rel: &Path, flags: i32) -> io::Result<PathBuf> {
        if Self::is_write_open(flags) {
            self.prepare_write_open(rel, flags)
        } else {
            Ok(self.core.prepare_file_read(rel)?.resolved.path)
        }
    }
    /// Same open semantics with parent identities for the native inode adapter.
    pub fn prepare_open_for_backing_lookup(
        &self,
        rel: &Path,
        flags: i32,
    ) -> io::Result<OpenBacking> {
        if Self::is_write_open(flags) {
            self.prepare_write_open(rel, flags)
                .map(OpenBacking::Writable)
        } else {
            self.core
                .prepare_file_read_for_backing_lookup(rel)
                .map(OpenBacking::ReadOnly)
        }
    }
    pub fn capture_hard_link_sources(&self) -> io::Result<Vec<(u64, u64, PathBuf)>> {
        self.core.capture_hard_link_sources()
    }
    pub fn capture_hard_links(&self) -> io::Result<Vec<(u64, u64, Vec<PathBuf>)>> {
        self.core.capture_hard_links()
    }
    pub fn copy_up(&self, rel: &Path) -> io::Result<PathBuf> {
        self.core.copy_up(rel)
    }
    pub fn create_dir(&self, rel: &Path, mode: u32) -> io::Result<()> {
        self.core.create_dir(rel, mode)
    }
    pub fn create_file(&self, rel: &Path, mode: u32, flags: i32) -> io::Result<File> {
        self.core.create_file(rel, mode, flags)
    }
    pub fn create_node(&self, rel: &Path, mode: u32, rdev: u32) -> io::Result<()> {
        self.core.create_node(rel, mode, rdev)
    }
    pub fn create_symlink(&self, rel: &Path, target: &Path) -> io::Result<()> {
        self.core.create_symlink(rel, target)
    }
    pub fn directory_candidates(&self, rel: &Path) -> io::Result<Vec<(OsString, u32)>> {
        self.core.directory_candidates(rel)
    }
    pub fn directory_entry(&self, rel: &Path, name: &OsStr) -> io::Result<Option<DirectoryEntry>> {
        self.core.directory_entry(rel, name)
    }
    pub fn directory_entry_for_backing_lookup(
        &self,
        rel: &Path,
        name: &OsStr,
    ) -> io::Result<Option<BackingResolution>> {
        self.core.directory_entry_for_backing_lookup(rel, name)
    }
    pub fn emit_profile_checkpoint(&self) {
        self.core.emit_profile_checkpoint()
    }
    pub fn exchange(&self, first: &Path, second: &Path) -> io::Result<()> {
        self.core.exchange(first, second)
    }
    pub fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.core.hard_link(source, destination)
    }
    pub fn metadata(&self, rel: &Path) -> io::Result<Metadata> {
        self.core.metadata(rel)
    }
    /// Checked physical metadata and optional surviving upper hardlink metadata
    /// from one merged resolution. Same errors, absence observations and
    /// request-local lifetime as Core's `metadata_with_copied_hard_link`;
    /// adapters retain namespace serialization and alias publication ordering.
    pub fn metadata_with_copied_hard_link(
        &self,
        rel: &Path,
    ) -> io::Result<(Metadata, Option<Metadata>)> {
        self.core.metadata_with_copied_hard_link(rel)
    }
    pub fn metadata_for_backing_lookup(&self, rel: &Path) -> io::Result<BackingResolution> {
        self.core.metadata_for_backing_lookup(rel)
    }
    /// Return positive backing evidence or successfully observed merged absence.
    ///
    /// The same checked resolution, policy/alias checks and absence publication
    /// as `metadata_for_backing_lookup` run in their original order. Only a
    /// successful missing result becomes `None`; surfaced errors, including
    /// exclusion/journal ENOENT, propagate. The existing resolver's interpretation
    /// of backend NotFound as absence is retained. `None` alone is not proof of
    /// permanent physical/remote absence: caching callers must separately own
    /// their backend and namespace stability. This is request-local evidence,
    /// not authorization or a lifetime namespace-stability guarantee.
    pub fn metadata_for_backing_lookup_optional(
        &self,
        rel: &Path,
    ) -> io::Result<Option<BackingResolution>> {
        self.core.metadata_for_backing_lookup_optional(rel)
    }
    pub fn metadata_resolved(&self, rel: &Path) -> io::Result<ResolvedMetadata> {
        self.core.metadata_resolved(rel)
    }
    pub fn observe_read(&self, rel: &Path) -> io::Result<()> {
        self.core.observe_read(rel)
    }
    pub fn observe_read_for_backing_lookup(&self, rel: &Path) -> io::Result<BackingResolution> {
        self.core.observe_read_for_backing_lookup(rel)
    }
    pub fn observe_read_resolved(&self, rel: &Path) -> io::Result<ResolvedMetadata> {
        self.core.observe_read_resolved(rel)
    }
    pub fn prepare_create(&self, rel: &Path) -> io::Result<()> {
        self.core.prepare_create(rel)
    }
    pub fn prepare_metadata_change(&self, rel: &Path) -> io::Result<PathBuf> {
        self.core.prepare_metadata_change(rel)
    }
    pub fn profile_report(&self) -> Option<crate::profile::ProfileReport> {
        self.core.profile_report()
    }
    pub fn remove(&self, rel: &Path, directory: bool) -> io::Result<()> {
        self.core.remove(rel, directory)
    }
    pub fn rename(&self, old: &Path, new: &Path, no_replace: bool) -> io::Result<()> {
        self.core.rename(old, new, no_replace)
    }
    /// Validate and observe a serialized rename/exchange without publishing it;
    /// returned roots need recursive copy preparation. See Core for ordering.
    pub fn prepare_rename_copy_up(
        &self,
        old: &Path,
        new: &Path,
        no_replace: bool,
        exchange: bool,
    ) -> io::Result<Vec<PathBuf>> {
        self.core
            .prepare_rename_copy_up(old, new, no_replace, exchange)
    }
    /// Resolve checked namespace selection without recording an absence.
    /// Policy, traversal and I/O errors propagate; only checked absence is `None`.
    pub fn resolve_checked(&self, rel: &Path) -> io::Result<Option<Resolved>> {
        self.core.resolve_checked(rel)
    }
    pub fn restore_hard_link_sources(&self, sources: &[(u64, u64, PathBuf)]) -> io::Result<()> {
        self.core.restore_hard_link_sources(sources)
    }
    pub fn restore_hard_links(&self, saved: &[(u64, u64, Vec<PathBuf>)]) -> io::Result<()> {
        self.core.restore_hard_links(saved)
    }
    pub fn sync_preimages(&self) -> io::Result<()> {
        self.core.sync_preimages()
    }
    pub fn upper(&self) -> &Path {
        self.core.upper()
    }
    pub fn upper_path(&self, rel: &Path) -> PathBuf {
        self.core.upper_path(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn exclusive_upper_guard_resumes_after_partial_failure_without_retaining_absence() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        std::fs::create_dir_all(lower.join("a")).unwrap();
        std::fs::write(lower.join("a/file"), b"lower").unwrap();
        let mut service =
            FilesystemService::new(OverlayCore::new(vec![lower], upper.clone(), None).unwrap())
                .with_profile(crate::profile::Profile::enabled("upper-guard"));
        assert!(!service.exclusive_upper_absence_cache_enabled());
        service
            .configure_exclusive_upper_absence_cache(true)
            .unwrap();
        let units = |label: &str| {
            service
                .profile_report()
                .unwrap()
                .measurements
                .get(label)
                .map_or(0, |measurement| measurement.units)
        };
        for _ in 0..2 {
            assert!(
                !service
                    .metadata_resolved(Path::new("a/file"))
                    .unwrap()
                    .resolved
                    .is_upper
            );
        }
        assert!(units("exclusive_upper_absence_cache_hits") > 0);
        let before = (
            units("exclusive_upper_absence_cache_hits"),
            units("exclusive_upper_absence_cache_misses"),
        );
        let fail_partially = || -> io::Result<()> {
            let _mutation = service.upper_namespace_mutation();
            assert!(service.exclusive_upper_absence_cache_enabled());
            assert!(
                !service
                    .metadata_resolved(Path::new("a/file"))?
                    .resolved
                    .is_upper
            );
            std::fs::create_dir(upper.join("a"))?;
            assert!(
                !service
                    .metadata_resolved(Path::new("a/file"))?
                    .resolved
                    .is_upper
            );
            assert_eq!(
                before,
                (
                    units("exclusive_upper_absence_cache_hits"),
                    units("exclusive_upper_absence_cache_misses")
                )
            );
            std::fs::write(upper.join("a/file"), b"partially published")?;
            Err(io::Error::from_raw_os_error(libc::EIO))
        };
        assert!(fail_partially().is_err());
        let entry = service.metadata_resolved(Path::new("a/file")).unwrap();
        assert!(entry.resolved.is_upper);
        assert_eq!(entry.metadata.len(), 19);
        assert_eq!(units("exclusive_upper_absence_cache_invalidations"), 2);
        assert_eq!(units("exclusive_upper_absence_cache_enabled"), 1);
        // A later read epoch can cache again, rather than remaining suspended.
        for _ in 0..2 {
            assert!(service.metadata_resolved(Path::new("missing")).is_err());
        }
        assert!(units("exclusive_upper_absence_cache_hits") > before.0);
    }

    #[test]
    fn unconfigured_upper_guard_keeps_the_live_default_and_is_a_noop() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        std::fs::create_dir(&lower).unwrap();
        let service =
            FilesystemService::new(OverlayCore::new(vec![lower], upper.clone(), None).unwrap())
                .with_profile(crate::profile::Profile::enabled("default-upper"));
        assert!(service.metadata_resolved(Path::new("file")).is_err());
        {
            let _mutation = service.upper_namespace_mutation();
            std::fs::write(upper.join("file"), b"live").unwrap();
        }
        assert_eq!(service.metadata(Path::new("file")).unwrap().len(), 4);
        assert!(!service.exclusive_upper_absence_cache_enabled());
        assert!(
            !service
                .profile_report()
                .unwrap()
                .measurements
                .contains_key("exclusive_upper_absence_cache_invalidations")
        );
    }

    #[test]
    fn fresh_truncate_skips_copy_but_preserves_preimage_and_hardlinks() {
        for compact in [false, true] {
            for native in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let lower = temp.path().join("lower");
                let upper = temp.path().join("upper");
                let journal = temp.path().join("preimages");
                std::fs::create_dir(&lower).unwrap();
                std::fs::write(lower.join("file"), vec![42; 131_073]).unwrap();
                std::fs::write(lower.join("linked"), b"shared").unwrap();
                std::fs::hard_link(lower.join("linked"), lower.join("alias")).unwrap();
                let before = crate::fingerprint_at(&lower, Path::new("file")).unwrap();
                let profile = crate::profile::Profile::enabled("truncate-test");
                let mut core = OverlayCore::new_with_exclusions_and_preimages(
                    vec![lower.clone()],
                    upper.clone(),
                    None,
                    vec![],
                    Some(journal.clone()),
                )
                .unwrap();
                if compact {
                    core = core.with_compact_preimages().unwrap();
                }
                let service = FilesystemService::new(core).with_profile(profile.clone());
                let prepare = |name: &str| {
                    let flags = libc::O_WRONLY | libc::O_TRUNC;
                    if native {
                        match service
                            .prepare_open_for_backing_lookup(Path::new(name), flags)
                            .unwrap()
                        {
                            OpenBacking::Writable(path) => path,
                            _ => panic!("expected writable backing"),
                        }
                    } else {
                        service.prepare_open(Path::new(name), flags).unwrap()
                    }
                };
                let copied = prepare("file");
                assert!(std::fs::read(copied).unwrap().is_empty());
                assert_eq!(crate::load_preimages(&journal).unwrap()[0].state, before);
                let report = profile.report().unwrap();
                assert_eq!(
                    report.measurements["copy_up_truncate_skipped_bytes"].units,
                    131_073
                );
                assert!(!report.measurements.contains_key("copy_up_bytes"));
                // Hardlinks stay on the normal path; the adapter truncates the
                // shared upper inode only after alias rebinding is complete.
                assert_eq!(std::fs::read(prepare("linked")).unwrap(), b"shared");
                service.copy_up(Path::new("alias")).unwrap();
                std::fs::write(upper.join("linked"), b"edited").unwrap();
                assert_eq!(std::fs::read(upper.join("alias")).unwrap(), b"edited");
                assert_eq!(std::fs::read(lower.join("alias")).unwrap(), b"shared");
            }
        }
    }

    #[test]
    fn host_and_native_open_share_read_copy_up_and_no_follow_semantics() {
        for native in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            let upper = temp.path().join("upper");
            std::fs::create_dir(&lower).unwrap();
            std::fs::write(lower.join("file"), b"original").unwrap();
            symlink("file", lower.join("alias")).unwrap();
            let service = FilesystemService::new(
                OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap(),
            );
            let open = |path: &Path, flags| {
                if native {
                    service
                        .prepare_open_for_backing_lookup(path, flags)
                        .map(|b| match b {
                            OpenBacking::Writable(path) => path,
                            OpenBacking::ReadOnly(backing) => backing.entry.resolved.path,
                        })
                } else {
                    service.prepare_open(path, flags)
                }
            };
            assert_eq!(
                open(Path::new("file"), libc::O_RDONLY).unwrap(),
                lower.join("file")
            );
            assert!(!upper.join("file").exists());
            assert_eq!(
                open(Path::new("file"), libc::O_RDWR).unwrap(),
                upper.join("file")
            );
            std::fs::write(upper.join("file"), b"changed").unwrap();
            assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"original");
            assert_eq!(
                open(Path::new("file"), libc::O_RDONLY).unwrap(),
                upper.join("file")
            );
            for flags in [
                libc::O_RDONLY,
                libc::O_WRONLY,
                libc::O_RDONLY | libc::O_APPEND,
                libc::O_RDONLY | libc::O_TRUNC,
            ] {
                assert_eq!(
                    open(Path::new("alias"), flags).unwrap_err().raw_os_error(),
                    Some(libc::ELOOP)
                );
            }
            assert!(!upper.join("alias").exists());
        }
    }

    #[test]
    fn both_open_interfaces_enforce_path_policy_before_copy_up() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("private"), b"secret").unwrap();
        let policy = crate::FileAccessPolicy::new(vec!["private".into()], vec![]).unwrap();
        let service =
            FilesystemService::new(OverlayCore::new(vec![lower], upper.clone(), None).unwrap())
                .with_access_policy(&policy);
        for flags in [libc::O_RDONLY, libc::O_RDWR] {
            assert!(service.prepare_open(Path::new("private"), flags).is_err());
            assert!(
                service
                    .prepare_open_for_backing_lookup(Path::new("private"), flags)
                    .is_err()
            );
        }
        assert!(!upper.join("private").exists());
    }
}
