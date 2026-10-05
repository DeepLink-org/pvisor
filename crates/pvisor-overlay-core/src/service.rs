//! Shared file-operation service used by host FUSE and VM virtio-fs adapters.
use crate::{OverlayCore, backend};
use std::{fs::File, io, os::unix::fs::FileExt, path::Path};

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
}
impl std::ops::Deref for FilesystemService {
    type Target = OverlayCore;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
