//! Atomic environment objects with raw or durable compressed RAM. VM freezing belongs to the executor.
use super::{
    RamBlocks, RawRamIndex, SnapshotRamReader, TreeInventory, blocks, copy_owned_tree, file_hash,
    native_path, verify_tree,
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub host_boot: String,
    pub build: String,
    pub firmware: String,
    pub profile: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentManifest {
    pub version: u32,
    pub compatibility: Compatibility,
    pub source_root: Vec<u8>,
    pub filesystem: TreeInventory,
    pub ram_sha256: String,
    pub machine_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_blocks: Option<RamBlocks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_index: Option<RawRamIndex>,
}

pub struct SnapshotStore {
    root: PathBuf,
}
pub struct PendingEnvironment {
    store: PathBuf,
    staging: tempfile::TempDir,
    _writer: File,
}
pub struct PublishedEnvironment {
    path: PathBuf,
    manifest: EnvironmentManifest,
    // Permanent store lock is outside removable objects. This guard holds a
    // shared reference until RAM/state/worktree preparation is complete.
    _reference: File,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(super) fn valid_id(id: &str) -> anyhow::Result<()> {
    ensure!(
        id.len() == 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid snapshot identity"
    );
    Ok(())
}
fn gate(root: &Path, exclusive: bool) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("references.lock"))?;
    if exclusive {
        fs2::FileExt::try_lock_exclusive(&file).context("snapshot references are still active")?;
    } else {
        fs2::FileExt::try_lock_shared(&file).context("snapshot store is being changed")?;
    }
    Ok(file)
}
fn write_synced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

impl SnapshotStore {
    pub fn new(root: &Path) -> anyhow::Result<Self> {
        // mkdir is the arbitration point for concurrent first use. An exists
        // check followed by mkdir spuriously fails when another runner wins.
        // Create private directories from the outset, then validate even when
        // mkdir reports AlreadyExists (including files and dangling symlinks).
        fn directory(path: &Path) -> anyhow::Result<()> {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(
                fs::symlink_metadata(path)?.is_dir(),
                "invalid snapshot store directory"
            );
            Ok(())
        }
        directory(root)?;
        ensure!(
            fs::symlink_metadata(root)?.is_dir(),
            "snapshot store must not be a symlink"
        );
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let root = root.canonicalize()?;
        for name in ["objects", "pending", "deleted", "content"] {
            let path = root.join(name);
            directory(&path)?;
        }
        Ok(Self { root })
    }
    pub fn begin(&self) -> anyhow::Result<PendingEnvironment> {
        // Protect the interval before the per-writer lock is installed.
        let _creating = gate(&self.root, false)?;
        let staging = tempfile::Builder::new()
            .prefix("environment-")
            .tempdir_in(self.root.join("pending"))?;
        let writer = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(staging.path().join("writer.lock"))?;
        fs2::FileExt::try_lock_exclusive(&writer)?;
        Ok(PendingEnvironment {
            store: self.root.clone(),
            staging,
            _writer: writer,
        })
    }
    pub fn open(&self, id: &str, expected: &Compatibility) -> anyhow::Result<PublishedEnvironment> {
        self.open_checked(id, expected, false)
    }

    /// Validate seals and inventories now; verify indexed RAM on first access.
    /// Legacy raw snapshots without block digests retain full upfront validation.
    pub fn open_for_restore(
        &self,
        id: &str,
        expected: &Compatibility,
    ) -> anyhow::Result<PublishedEnvironment> {
        self.open_checked(id, expected, true)
    }

    fn open_checked(
        &self,
        id: &str,
        expected: &Compatibility,
        lazy: bool,
    ) -> anyhow::Result<PublishedEnvironment> {
        valid_id(id)?;
        let reference = gate(&self.root, false)?;
        let path = self.root.join("objects").join(id);
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "invalid environment object"
        );
        let bytes = fs::read(path.join("manifest.json"))?;
        ensure!(digest(&bytes) == id, "environment manifest digest mismatch");
        let manifest: EnvironmentManifest = serde_json::from_slice(&bytes)?;
        ensure!(
            matches!(
                (manifest.version, &manifest.ram_blocks, &manifest.ram_index),
                (1, None, None) | (2, Some(_), None) | (3, None, Some(_))
            ) && manifest.compatibility == *expected,
            "environment compatibility mismatch"
        );
        valid_id(&manifest.ram_sha256)?;
        valid_id(&manifest.machine_sha256)?;
        if let Some(index) = &manifest.ram_index {
            index.validate()?;
            let meta = fs::symlink_metadata(path.join("ram.bin"))?;
            ensure!(
                meta.is_file() && meta.len() == index.length,
                "RAM file size mismatch"
            );
        }
        if let Some(blocks) = &manifest.ram_blocks {
            blocks.validate()?;
            ensure!(
                fs::symlink_metadata(path.join("ram-blocks"))?.is_dir(),
                "invalid RAM references"
            );
            if !lazy {
                blocks.decode(
                    &path.join("ram-blocks"),
                    std::io::sink(),
                    &manifest.ram_sha256,
                )?;
            }
        } else if !lazy || manifest.ram_index.is_none() {
            ensure!(
                file_hash(&path.join("ram.bin"))? == manifest.ram_sha256,
                "environment RAM digest mismatch"
            );
        }
        ensure!(
            file_hash(&path.join("machine.json"))? == manifest.machine_sha256,
            "environment machine digest mismatch"
        );
        verify_tree(&path.join("rootfs"), &manifest.filesystem)?;
        Ok(PublishedEnvironment {
            path,
            manifest,
            _reference: reference,
        })
    }
    /// Deletion is refused while any published object is referenced. The
    /// first version deliberately uses one conservative store-wide gate.
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        valid_id(id)?;
        let _exclusive = gate(&self.root, true)?;
        let source = self.root.join("objects").join(id);
        ensure!(
            fs::symlink_metadata(&source)?.is_dir(),
            "invalid environment object"
        );
        let tombstone = self
            .root
            .join("deleted")
            .join(uuid::Uuid::new_v4().to_string());
        fs::rename(source, &tombstone)?;
        File::open(self.root.join("objects"))?.sync_all()?;
        File::open(self.root.join("deleted"))?.sync_all()?;
        fs::remove_dir_all(tombstone)?;
        File::open(self.root.join("deleted"))?.sync_all()?;
        Ok(())
    }

    /// Reap unpublished writers, tombstones and unreferenced RAM content.
    /// No published environment is removed. A live writer
    /// keeps its file lock even while the VM freezes or performs a long copy.
    pub fn collect_abandoned(&self) -> anyhow::Result<usize> {
        let _exclusive = gate(&self.root, true)?;
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("pending"))? {
            let path = entry?.path();
            // A reader/writer can drop its TempDir after read_dir yields the
            // entry. Its own cleanup needs no store gate; absence is benign.
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            ensure!(metadata.is_dir(), "invalid pending snapshot directory");
            let writer = match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path.join("writer.lock"))
            {
                Ok(file) => Some(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if let Some(file) = &writer {
                match fs2::FileExt::try_lock_exclusive(file) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            match fs::remove_dir_all(path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for entry in fs::read_dir(self.root.join("deleted"))? {
            let path = entry?.path();
            ensure!(
                fs::symlink_metadata(&path)?.is_dir(),
                "invalid deletion tombstone"
            );
            fs::remove_dir_all(path)?;
            removed += 1;
        }
        File::open(self.root.join("pending"))?.sync_all()?;
        File::open(self.root.join("deleted"))?.sync_all()?;
        removed += blocks::collect(&self.root)?;
        Ok(removed)
    }
}

impl PendingEnvironment {
    pub(super) fn directory(&self) -> &Path {
        self.staging.path()
    }
    pub fn create_ram(&self) -> anyhow::Result<File> {
        Ok(OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.staging.path().join("capture.ram"))?)
    }
    /// The caller must capture CPU/devices/RAM and call publish within ONE
    /// full-device freeze transaction, with exclusive ownership of source.
    /// Machine bytes are opaque here; their typed validation is the executor's
    /// responsibility. No object can be opened before final directory rename.
    pub fn publish(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(source, machine, compatibility, false)
    }
    /// Durable compressed content uses the resident pool codec; no live pool is required.
    pub fn publish_compressed(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(source, machine, compatibility, true)
    }
    fn publish_with_ram(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
    ) -> anyhow::Result<String> {
        ensure!(!machine.is_empty(), "missing machine state");
        ensure!(
            !compatibility.host_boot.is_empty()
                && !compatibility.build.is_empty()
                && !compatibility.firmware.is_empty()
                && !compatibility.profile.is_empty(),
            "incomplete compatibility binding"
        );
        let source = source.canonicalize()?;
        let filesystem = copy_owned_tree(&source, &self.staging.path().join("rootfs"))?;
        write_synced(&self.staging.path().join("machine.json"), machine)?;
        // Detach the sealed RAM inode from writable capture descriptors which
        // callers may still hold. They cannot mutate the published payload.
        let capture_path = self.staging.path().join("capture.ram");
        let mut capture = File::open(&capture_path)?;
        capture.sync_all()?;
        ensure!(capture.metadata()?.len() > 0, "empty captured RAM");
        let capture_hash = file_hash(&capture_path)?;
        let ram_blocks = if compressed {
            let _publishing = gate(&self.store, false)?;
            Some(RamBlocks::capture(
                &self.store,
                &self.staging.path().join("ram-blocks"),
                &mut capture,
            )?)
        } else {
            let sealed_path = self.staging.path().join("ram.bin");
            let mut sealed = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&sealed_path)?;
            std::io::copy(&mut capture, &mut sealed)?;
            sealed.sync_all()?;
            ensure!(
                file_hash(&capture_path)? == capture_hash
                    && file_hash(&sealed_path)? == capture_hash,
                "captured RAM changed while sealing"
            );
            None
        };
        ensure!(
            file_hash(&capture_path)? == capture_hash,
            "captured RAM changed while sealing"
        );
        fs::remove_file(capture_path)?;
        let ram_index = if compressed {
            None
        } else {
            Some(RawRamIndex::capture(&File::open(
                self.staging.path().join("ram.bin"),
            )?)?)
        };
        let manifest = EnvironmentManifest {
            version: if compressed { 2 } else { 3 },
            compatibility,
            source_root: source.as_os_str().as_bytes().to_vec(),
            filesystem,
            ram_sha256: capture_hash,
            ram_blocks,
            ram_index,
            machine_sha256: digest(machine),
        };
        let bytes = serde_json::to_vec(&manifest)?;
        let id = digest(&bytes);
        write_synced(&self.staging.path().join("manifest.json"), &bytes)?;
        File::open(self.staging.path())?.sync_all()?;
        let _exclusive = gate(&self.store, true)?;
        // No collector can inspect this directory while publication holds the
        // store gate. The writer's FD stays locked until this owner is dropped.
        fs::remove_file(self.staging.path().join("writer.lock"))?;
        let destination = self.store.join("objects").join(&id);
        let old = native_path(self.staging.path())?;
        let new = native_path(&destination)?;
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(old.as_ptr(), new.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                old.as_ptr(),
                libc::AT_FDCWD,
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        ensure!(
            rc == 0,
            "environment publication: {}",
            std::io::Error::last_os_error()
        );
        File::open(self.store.join("objects"))?.sync_all()?;
        File::open(self.store.join("pending"))?.sync_all()?;
        Ok(id)
    }
}
impl PublishedEnvironment {
    pub fn manifest(&self) -> &EnvironmentManifest {
        &self.manifest
    }
    pub fn machine_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(fs::read(self.path.join("machine.json"))?)
    }
    pub fn ram_file(&self) -> anyhow::Result<File> {
        if let Some(blocks) = &self.manifest.ram_blocks {
            // Private, unlinked scratch retains the existing eager restore contract.
            // It is never a writable descriptor to published content.
            let mut temporary = tempfile::NamedTempFile::new()?;
            blocks.decode(
                &self.path.join("ram-blocks"),
                &mut temporary,
                &self.manifest.ram_sha256,
            )?;
            temporary.as_file().sync_all()?;
            let file = File::open(temporary.path())?;
            drop(temporary);
            Ok(file)
        } else {
            Ok(File::open(self.path.join("ram.bin"))?)
        }
    }
    /// Pin backing independently of the object/store gate. Deleting the saved
    /// environment remains safe while a restored VM faults in previously cold RAM.
    pub fn ram_reader(&self) -> anyhow::Result<SnapshotRamReader> {
        SnapshotRamReader::new(&self.path, &self.manifest)
    }
    pub fn materialize(&self, destination: &Path) -> anyhow::Result<()> {
        verify_tree(&self.path.join("rootfs"), &self.manifest.filesystem)?;
        let copied = copy_owned_tree(&self.path.join("rootfs"), destination)?;
        ensure!(
            copied == self.manifest.filesystem,
            "environment changed during restore copy"
        );
        Ok(())
    }
}
