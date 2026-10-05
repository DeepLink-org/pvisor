//! Atomic environment objects with raw or durable compressed RAM. VM freezing belongs to the executor.
use super::{
    RamBlocks, RawRamIndex, SnapshotRamReader, TreeInventory, blocks, copy_owned_tree,
    copy_owned_tree_checked, copy_sealed_tree, file_hash, native_path, verify_tree,
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
    /// Ordered immutable generations. None identifies the legacy full-tree profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_bases: Option<Vec<super::BaseReference>>,
}

pub struct SnapshotStore {
    pub(super) root: PathBuf,
}
pub struct PendingEnvironment {
    store: PathBuf,
    staging: tempfile::TempDir,
    _writer: File,
}
pub struct PublishedEnvironment {
    path: PathBuf,
    manifest: EnvironmentManifest,
    // Consume exactly the state authenticated at open, without a second file
    // read between verification and deserialization by the restore caller.
    machine: Vec<u8>,
    // Permanent store lock is outside removable objects. This guard holds a
    // shared reference until RAM/state/worktree preparation is complete.
    _reference: File,
    bases: Vec<super::SnapshotBase>,
}

pub(super) fn digest(bytes: &[u8]) -> String {
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
pub(super) fn gate(root: &Path, exclusive: bool) -> anyhow::Result<File> {
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
pub(super) fn write_synced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
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
        for name in ["objects", "pending", "deleted", "content", "bases"] {
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
        self.open_checked(id, expected, false, false)
    }

    /// Validate seals and inventories now; verify indexed RAM on first access.
    /// Legacy raw snapshots without block digests retain full upfront validation.
    pub fn open_for_restore(
        &self,
        id: &str,
        expected: &Compatibility,
    ) -> anyhow::Result<PublishedEnvironment> {
        self.open_checked(id, expected, true, false)
    }

    /// Warm restore of a locally published, store-owned immutable stage.
    /// Publication authenticates content; this open checks the bound manifest,
    /// machine state, base pins and stage metadata/topology without rereading
    /// stage file content. External/manual writes to published payloads are
    /// unsupported. Use open/open_for_restore for a fresh full content audit.
    /// The reference gate must remain held through materialization.
    pub(crate) fn open_owned_stage_for_restore(
        &self,
        id: &str,
        expected: &Compatibility,
    ) -> anyhow::Result<PublishedEnvironment> {
        self.open_checked(id, expected, true, true)
    }

    fn open_checked(
        &self,
        id: &str,
        expected: &Compatibility,
        lazy: bool,
        owned_stage: bool,
    ) -> anyhow::Result<PublishedEnvironment> {
        valid_id(id)?;
        let reference = gate(&self.root, false)?;
        let path = self.root.join("objects").join(id);
        let manifest = self.read_manifest(id, expected)?;
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
        let machine = self.read_machine(id, &manifest)?;
        if owned_stage {
            ensure!(
                manifest.stage_bases.is_some(),
                "owned restore requires a stage snapshot"
            );
            super::verify_tree_metadata(&path.join("rootfs"), &manifest.filesystem)?;
        } else {
            verify_tree(&path.join("rootfs"), &manifest.filesystem)?;
        }
        let bases = if let Some(references) = &manifest.stage_bases {
            ensure!(!references.is_empty(), "stage snapshot has no base");
            let mut ids = std::collections::BTreeSet::new();
            references
                .iter()
                .map(|reference| {
                    ensure!(ids.insert(&reference.id), "duplicate stage base");
                    let base = super::base::open(&self.root, reference)?;
                    base.verify_pin(&path.join("base-refs").join(&reference.id))?;
                    Ok(base)
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(PublishedEnvironment {
            path,
            manifest,
            machine,
            _reference: reference,
            bases,
        })
    }

    /// Startup preflight only: authenticates metadata without scanning payloads.
    /// This is not a restore capability and carries no payload/base lease.
    /// The runner must open/validate the object before creating an instance.
    pub(crate) fn restore_metadata(
        &self,
        id: &str,
        expected: &Compatibility,
    ) -> anyhow::Result<(EnvironmentManifest, Vec<u8>)> {
        let _reference = gate(&self.root, false)?;
        let manifest = self.read_manifest(id, expected)?;
        let machine = self.read_machine(id, &manifest)?;
        Ok((manifest, machine))
    }

    fn read_manifest(
        &self,
        id: &str,
        expected: &Compatibility,
    ) -> anyhow::Result<EnvironmentManifest> {
        valid_id(id)?;
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
                (
                    manifest.version,
                    &manifest.ram_blocks,
                    &manifest.ram_index,
                    &manifest.stage_bases
                ),
                (1, None, None, None)
                    | (2, Some(_), None, None)
                    | (3, None, Some(_), None)
                    | (4, None, Some(_), Some(_))
                    | (5, Some(_), None, Some(_))
            ) && manifest.compatibility == *expected,
            "environment compatibility mismatch"
        );
        valid_id(&manifest.ram_sha256)?;
        valid_id(&manifest.machine_sha256)?;
        Ok(manifest)
    }

    fn read_machine(&self, id: &str, manifest: &EnvironmentManifest) -> anyhow::Result<Vec<u8>> {
        let machine = fs::read(self.root.join("objects").join(id).join("machine.json"))?;
        ensure!(
            digest(&machine) == manifest.machine_sha256,
            "environment machine digest mismatch"
        );
        Ok(machine)
    }
    /// Read the digest-bound profile; callers still perform full compatibility validation.
    pub fn profile(&self, id: &str) -> anyhow::Result<String> {
        valid_id(id)?;
        let _reference = gate(&self.root, false)?;
        let bytes = fs::read(self.root.join("objects").join(id).join("manifest.json"))?;
        ensure!(digest(&bytes) == id, "environment manifest digest mismatch");
        Ok(serde_json::from_slice::<EnvironmentManifest>(&bytes)?
            .compatibility
            .profile)
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
        removed += super::base::collect(&self.root)?;
        Ok(removed)
    }
}

impl PendingEnvironment {
    pub(crate) fn directory(&self) -> &Path {
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
        self.publish_with_ram(source, machine, compatibility, false, &[])
    }
    /// Durable compressed content uses the resident pool codec; no live pool is required.
    pub fn publish_compressed(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(source, machine, compatibility, true, &[])
    }
    /// Capture a complete writable stage, not its immutable lower trees. The
    /// supplied leases must belong to this store and remain immutable. Freeze
    /// requirements are identical to `publish`; every lower needs a lease.
    pub fn publish_stage(
        self,
        source: &Path,
        bases: &[super::SnapshotBase],
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
    ) -> anyhow::Result<String> {
        ensure!(!bases.is_empty(), "stage snapshot has no base");
        self.publish_with_ram(source, machine, compatibility, compressed, bases)
    }
    fn publish_with_ram(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        bases: &[super::SnapshotBase],
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
        let stage_bases = if bases.is_empty() {
            None
        } else {
            fs::create_dir(self.staging.path().join("base-refs"))?;
            let mut ids = std::collections::BTreeSet::new();
            let references = bases
                .iter()
                .map(|base| {
                    ensure!(ids.insert(&base.reference().id), "duplicate stage base");
                    base.pin(
                        &self.store,
                        &self
                            .staging
                            .path()
                            .join("base-refs")
                            .join(&base.reference().id),
                    )?;
                    ensure!(
                        !source.starts_with(base.root()) && !base.root().starts_with(&source),
                        "stage overlaps its base"
                    );
                    Ok(base.reference().clone())
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            File::open(self.staging.path().join("base-refs"))?.sync_all()?;
            Some(references)
        };
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
            version: match (compressed, stage_bases.is_some()) {
                (false, false) => 3,
                (true, false) => 2,
                (false, true) => 4,
                (true, true) => 5,
            },
            compatibility,
            source_root: source.as_os_str().as_bytes().to_vec(),
            filesystem,
            ram_sha256: capture_hash,
            ram_blocks,
            ram_index,
            stage_bases,
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
        Ok(self.machine.clone())
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
            // `open_for_restore` authenticates raw RAM lazily. Eager callers
            // must validate the complete digest before obtaining this mapping.
            ensure!(
                file_hash(&self.path.join("ram.bin"))? == self.manifest.ram_sha256,
                "environment RAM digest mismatch"
            );
            Ok(File::open(self.path.join("ram.bin"))?)
        }
    }
    /// Pin backing independently of the object/store gate. Deleting the saved
    /// environment remains safe while a restored VM faults in previously cold RAM.
    pub fn ram_reader(&self) -> anyhow::Result<SnapshotRamReader> {
        SnapshotRamReader::new(&self.path, &self.manifest)
    }
    /// Fault-serving process independent of the VM's kernel teardown. The
    /// published lease stays held until the server has acquired backing pins.
    pub fn ram_mount(
        &self,
        executable: &Path,
        directory: &Path,
    ) -> std::io::Result<(super::SnapshotRamMount, File)> {
        super::SnapshotRamMount::external(&self.path, &self.manifest, directory, executable)
    }
    /// Independent leases must outlive restored VMs, including after object deletion.
    pub fn base_leases(&self) -> anyhow::Result<Vec<super::SnapshotBase>> {
        self.bases
            .iter()
            .map(super::SnapshotBase::try_clone)
            .collect()
    }
    pub fn materialize(&self, destination: &Path) -> anyhow::Result<()> {
        ensure!(
            self.manifest.stage_bases.is_none(),
            "stage snapshot requires materialize_stage"
        );
        self.copy_payload(destination)
    }
    /// Restore the whole stage container (upper, work and preimages), without bases.
    pub fn materialize_stage(&self, destination: &Path) -> anyhow::Result<()> {
        ensure!(
            self.manifest.stage_bases.is_some(),
            "full snapshot is not a stage snapshot"
        );
        self.copy_payload(destination)
    }
    /// Internal warm restore from a store-owned immutable published payload. The
    /// store owns its sealed tree and exposes no writable payload descriptor;
    /// this borrow keeps the reference gate held through copying. External
    /// writers/manual store edits during the lease are unsupported, as with
    /// SnapshotBase. Use materialize_stage for a fresh full content audit.
    /// Reuse the inventory checked at open; source metadata/topology are
    /// checked again after copy, as is the destination. Data-copy
    /// fallback validates destination content. No immutable base is copied.
    pub(crate) fn materialize_owned_stage(&self, destination: &Path) -> anyhow::Result<()> {
        ensure!(
            self.manifest.stage_bases.is_some(),
            "full snapshot is not a stage snapshot"
        );
        copy_sealed_tree(
            &self.path.join("rootfs"),
            destination,
            &self.manifest.filesystem,
        )?;
        Ok(())
    }

    fn copy_payload(&self, destination: &Path) -> anyhow::Result<()> {
        copy_owned_tree_checked(
            &self.path.join("rootfs"),
            destination,
            Some(&self.manifest.filesystem),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod owned_stage_tests {
    use super::*;

    fn fixture(temp: &Path, count: usize, bytes: usize) -> (SnapshotStore, String, Compatibility) {
        let base = temp.join("base");
        fs::create_dir(&base).unwrap();
        let stage = temp.join("stage");
        for part in ["upper", "work", "preimages"] {
            fs::create_dir_all(stage.join(part)).unwrap();
        }
        let content = vec![0x5a; bytes];
        for index in 0..count {
            fs::write(stage.join(format!("upper/{index:04}")), &content).unwrap();
        }
        fs::write(stage.join("work/state"), b"metadata").unwrap();
        fs::write(stage.join("preimages/entry"), b"preimage").unwrap();
        let store = SnapshotStore::new(&temp.join("store")).unwrap();
        let base = store.import_base(&base).unwrap();
        let binding = Compatibility {
            host_boot: "boot".into(),
            build: "build".into(),
            firmware: "firmware".into(),
            profile: "stage".into(),
        };
        let pending = store.begin().unwrap();
        pending
            .create_ram()
            .unwrap()
            .write_all(&[7; 65536])
            .unwrap();
        let id = pending
            .publish_stage(&stage, &[base], b"machine", binding.clone(), false)
            .unwrap();
        (store, id, binding)
    }

    #[test]
    fn owned_stage_restore_retains_lease_and_branch_independence() {
        let temp = tempfile::tempdir().unwrap();
        let (store, id, binding) = fixture(temp.path(), 2, 16);
        let snapshot = store.open_owned_stage_for_restore(&id, &binding).unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        snapshot.materialize_owned_stage(&first).unwrap();
        assert!(store.delete(&id).is_err());
        snapshot.materialize_owned_stage(&second).unwrap();
        super::super::verify_tree(&first, &snapshot.manifest.filesystem).unwrap();
        super::super::verify_tree(&second, &snapshot.manifest.filesystem).unwrap();
        fs::write(first.join("upper/0000"), b"branch changes").unwrap();
        assert_eq!(fs::read(second.join("upper/0000")).unwrap(), vec![0x5a; 16]);
        assert_eq!(fs::read(second.join("work/state")).unwrap(), b"metadata");
        assert_eq!(
            fs::read(second.join("preimages/entry")).unwrap(),
            b"preimage"
        );
        drop(snapshot);
        store.delete(&id).unwrap();
        assert_eq!(fs::read(second.join("upper/0000")).unwrap(), vec![0x5a; 16]);
    }

    #[test]
    fn owned_stage_restore_rejects_structure_changes_and_cleans_destination() {
        let temp = tempfile::tempdir().unwrap();
        let (store, id, binding) = fixture(temp.path(), 2, 16);
        let snapshot = store.open_for_restore(&id, &binding).unwrap();
        // Metadata checking still catches this unsupported external edit.
        fs::write(snapshot.path.join("rootfs/upper/unexpected"), b"extra").unwrap();
        let destination = temp.path().join("branch");
        assert!(snapshot.materialize_owned_stage(&destination).is_err());
        assert!(!destination.exists());
        assert!(store.open_owned_stage_for_restore(&id, &binding).is_err());
    }

    #[test]
    #[ignore = "manual paired immutable stage open measurement"]
    fn owned_stage_open_paired_benchmark() {
        use std::time::Instant;
        for (name, count, bytes) in [("small-2048", 2048, 16), ("large-128", 128, 1024 * 1024)] {
            let temp = tempfile::tempdir().unwrap();
            let (store, id, binding) = fixture(temp.path(), count, bytes);
            for round in 0..18 {
                let mut elapsed = [0u128; 2];
                let order = if round % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for owned in order {
                    let start = Instant::now();
                    let snapshot = if owned {
                        store.open_owned_stage_for_restore(&id, &binding).unwrap()
                    } else {
                        store.open_for_restore(&id, &binding).unwrap()
                    };
                    elapsed[usize::from(owned)] = start.elapsed().as_nanos();
                    assert_eq!(snapshot.machine_bytes().unwrap(), b"machine");
                    assert!(store.delete(&id).is_err());
                    drop(snapshot);
                }
                if round >= 3 {
                    println!(
                        "PVISOR_STAGE_OPEN_BENCH {}",
                        serde_json::json!({
                            "case": name, "round": round - 3,
                            "strict_ns": elapsed[0], "owned_ns": elapsed[1],
                            "files": count + 2, "bytes": count * bytes + 16,
                        })
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "manual opt-in complete stage copy profiling"]
    fn owned_stage_materialization_diagnostic() {
        let temp = tempfile::tempdir_in("/private/tmp").unwrap();
        let (store, id, binding) = fixture(temp.path(), 2048, 16);
        let snapshot = store.open_for_restore(&id, &binding).unwrap();
        let branch = temp.path().join("branch");
        snapshot.materialize_owned_stage(&branch).unwrap();
        super::super::verify_tree(&branch, &snapshot.manifest.filesystem).unwrap();
    }

    #[test]
    #[ignore = "manual paired complete stage materialization measurement"]
    fn owned_stage_materialization_paired_benchmark() {
        use std::time::Instant;
        for (name, count, bytes) in [("small-2048", 2048, 16), ("large-128", 128, 1024 * 1024)] {
            let temp = tempfile::tempdir_in("/private/tmp").unwrap();
            let (store, id, binding) = fixture(temp.path(), count, bytes);
            let snapshot = store.open_for_restore(&id, &binding).unwrap();
            for round in 0..18 {
                let mut elapsed = [0u128; 2];
                let order = if round % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for owned in order {
                    let branch = temp.path().join(format!("branch-{round}-{owned}"));
                    let start = Instant::now();
                    if owned {
                        snapshot.materialize_owned_stage(&branch).unwrap();
                    } else {
                        snapshot.materialize_stage(&branch).unwrap();
                    }
                    elapsed[usize::from(owned)] = start.elapsed().as_nanos();
                    super::super::verify_tree(&branch, &snapshot.manifest.filesystem).unwrap();
                    fs::remove_dir_all(branch).unwrap();
                }
                if round >= 3 {
                    println!(
                        "PVISOR_STAGE_MATERIALIZE_BENCH {}",
                        serde_json::json!({
                            "case": name, "round": round - 3, "strict_ns": elapsed[0], "owned_ns": elapsed[1],
                            "files": count + 2, "bytes": count * bytes + 16,
                        })
                    );
                }
            }
        }
    }
}
