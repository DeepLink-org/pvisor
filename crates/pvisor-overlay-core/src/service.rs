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
impl FilesystemService {
    pub fn new(core: OverlayCore) -> Self {
        Self { core }
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
    fn prepare_write_open(&self, rel: &Path) -> io::Result<PathBuf> {
        if self.core.metadata(rel)?.file_type().is_symlink() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        self.core.copy_up(rel)
    }
    /// Resolve a no-follow open, recording reads or copying up writes.
    /// The adapter owns the native descriptor and must open it without following links.
    pub fn prepare_open(&self, rel: &Path, flags: i32) -> io::Result<PathBuf> {
        if Self::is_write_open(flags) {
            self.prepare_write_open(rel)
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
            self.prepare_write_open(rel).map(OpenBacking::Writable)
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
    pub fn list_entries(&self, rel: &Path) -> io::Result<Vec<DirectoryEntry>> {
        self.core.list_entries(rel)
    }
    pub fn metadata(&self, rel: &Path) -> io::Result<Metadata> {
        self.core.metadata(rel)
    }
    pub fn metadata_for_backing_lookup(&self, rel: &Path) -> io::Result<BackingResolution> {
        self.core.metadata_for_backing_lookup(rel)
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
    pub fn resolve(&self, rel: &Path) -> Option<Resolved> {
        self.core.resolve(rel)
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
