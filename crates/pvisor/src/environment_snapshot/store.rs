//! Atomic environment objects with raw or durable compressed RAM. VM freezing belongs to the executor.
use super::{
    FilesystemLayer, RamBlocks, RawRamIndex, SnapshotLayer, SnapshotRamReader, TreeInventory,
    blocks, copy_owned_tree, copy_owned_tree_checked, copy_sealed_tree, file_hash, native_path,
    verify_tree,
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub use pvisor_core::operation::SnapshotCompatibility as Compatibility;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentManifest {
    pub version: u32,
    pub compatibility: Compatibility,
    pub source_root: Vec<u8>,
    pub filesystem: TreeInventory,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filesystem_layers: Vec<FilesystemLayer>,
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_blocks: Option<super::FilesystemBlocks>,
    pub ram_sha256: String,
    pub machine_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_blocks: Option<RamBlocks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_index: Option<RawRamIndex>,
    /// Ordered immutable generations. None identifies the full-tree profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_bases: Option<Vec<super::BaseReference>>,
}

pub struct SnapshotStore {
    pub(super) root: PathBuf,
    filesystem_pool: PathBuf,
}
pub struct PendingEnvironment {
    store: PathBuf,
    filesystem_pool: PathBuf,
    staging: tempfile::TempDir,
    _writer: File,
}
pub struct PublishedEnvironment {
    path: PathBuf,
    manifest: EnvironmentManifest,
    // Consume the exact machine bytes authenticated while opening the object.
    machine: Vec<u8>,
    bases: Vec<super::SnapshotBase>,
    filesystem_pool: PathBuf,
    _layers: Vec<super::SharedFilesystemLayer>,
    // Permanent store lock is outside removable objects. This guard holds a
    // shared reference until RAM/state/worktree preparation is complete.
    _reference: File,
}

enum PublicationSource<'a> {
    Borrowed,
    Stage(&'a [super::SnapshotBase]),
    #[cfg(target_os = "linux")]
    Chunked,
    Layered(&'a [SnapshotLayer<'a>]),
    Native {
        ram: &'a File,
        delta: Option<(
            &'a super::blocks::PinnedRamBlocks,
            super::blocks::RamDelta<'a>,
        )>,
        layers: &'a [super::CapturedFilesystemLayer],
        private_sources: &'a [super::CapturedFilesystemSource],
    },
}

impl EnvironmentManifest {
    pub(super) fn validate_filesystem_format(&self) -> anyhow::Result<()> {
        if let Some(bases) = &self.stage_bases {
            ensure!(
                !bases.is_empty()
                    && self.filesystem_layers.is_empty()
                    && matches!(
                        (self.version, &self.ram_blocks, &self.ram_index),
                        (4, None, Some(_)) | (5, Some(_), None)
                    ),
                "invalid stage filesystem version"
            );
            #[cfg(target_os = "linux")]
            ensure!(
                self.filesystem_blocks.is_none(),
                "stage cannot contain packed filesystem blocks"
            );
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if self.version == 5 {
            return self
                .filesystem_blocks
                .as_ref()
                .context("missing private filesystem blocks")?
                .validate(&self.filesystem);
        }
        #[cfg(target_os = "linux")]
        ensure!(
            self.filesystem_blocks.is_none(),
            "private filesystem blocks require v5"
        );
        ensure!(
            (self.version == 4) == !self.filesystem_layers.is_empty(),
            "invalid filesystem layer version"
        );
        Ok(())
    }
}

pub(crate) struct NativeLayerCapture<'a> {
    pub layers: &'a [super::CapturedFilesystemLayer],
    pub private_sources: &'a [super::CapturedFilesystemSource],
    pub delta: Option<(
        &'a super::blocks::PinnedRamBlocks,
        super::blocks::RamDelta<'a>,
    )>,
}

pub(crate) struct NativePublication {
    pub id: String,
    // Keep already verified owner locks through the supervisor handoff. This
    // avoids reopening/decoding the entire published snapshot just to pin lowers.
    pub lowers: Vec<super::SharedFilesystemLayer>,
    pub private_files: Option<std::sync::Arc<PrivateFilesystemOwner>>,
    #[cfg(target_os = "linux")]
    pub filesystem_capture_stats: Option<super::filesystem_blocks::CaptureStats>,
}

pub(crate) struct PrivateFilesystemOwner {
    _references: PendingEnvironment,
}
impl std::fmt::Debug for PrivateFilesystemOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateFilesystemOwner")
    }
}

#[cfg(target_os = "linux")]
fn pin_private_files(
    store: &Path,
    pool: &Path,
    blocks: &super::FilesystemBlocks,
    source: &Path,
) -> anyhow::Result<std::sync::Arc<PrivateFilesystemOwner>> {
    let _pool = gate(pool, false)?;
    let pending = if store == pool {
        SnapshotStore::new(store)?
    } else {
        SnapshotStore::with_filesystem_pool(store, pool)?
    }
    .begin()?;
    let references = pending.directory().join("filesystem-blocks");
    fs::create_dir(&references)?;
    let mut seen = std::collections::BTreeSet::new();
    for id in blocks
        .files
        .iter()
        .flat_map(|file| file.blocks.iter().flatten())
    {
        if seen.insert(id) {
            let origin = source.join(id);
            ensure!(
                fs::symlink_metadata(&origin)?.is_file(),
                "invalid private filesystem frame reference"
            );
            fs::hard_link(origin, references.join(id))?;
        }
    }
    File::open(&references)?.sync_all()?;
    File::open(pending.directory())?.sync_all()?;
    Ok(std::sync::Arc::new(PrivateFilesystemOwner {
        _references: pending,
    }))
}

// Copy logical bytes to a distinct fresh inode, retaining zero-filled holes.
// The source owner must prevent writes until publication has completed.
fn copy_sparse_ram(source: &File, destination: &File) -> anyhow::Result<()> {
    ensure!(
        destination.metadata()?.len() == 0,
        "RAM copy destination must be empty"
    );
    let length = source.metadata()?.len();
    let mut buffer = vec![0; 1024 * 1024];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(buffer.len() as u64) as usize;
        source.read_exact_at(&mut buffer[..count], offset)?;
        if buffer[..count].iter().any(|byte| *byte != 0) {
            destination.write_all_at(&buffer[..count], offset)?;
        }
        offset += count as u64;
    }
    ensure!(
        source.metadata()?.len() == length,
        "RAM source changed size while copying"
    );
    destination.set_len(length)?;
    destination.sync_all()?;
    Ok(())
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



fn read_environment_manifest(
    path: &Path,
    id: &str,
    expected: &Compatibility,
) -> anyhow::Result<EnvironmentManifest> {
    let bytes = fs::read(path.join("manifest.json"))?;
    ensure!(digest(&bytes) == id, "environment manifest digest mismatch");
    let manifest: EnvironmentManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        matches!(
            (manifest.version, &manifest.ram_blocks, &manifest.ram_index),
            (1, None, None)
                | (2, Some(_), None)
                | (3, None, Some(_))
                | (4, Some(_), None)
                | (4, None, Some(_))
                | (5, Some(_), None)
                | (5, None, Some(_))
        ) && manifest.compatibility == *expected
            && (manifest.version != 5
                || cfg!(target_os = "linux")
                || manifest.stage_bases.is_some()),
        "environment compatibility mismatch"
    );
    manifest.validate_filesystem_format()?;
    valid_id(&manifest.ram_sha256)?;
    valid_id(&manifest.machine_sha256)?;
    Ok(manifest)
}

fn read_environment_machine(
    path: &Path,
    manifest: &EnvironmentManifest,
) -> anyhow::Result<Vec<u8>> {
    let machine = fs::read(path.join("machine.json"))?;
    ensure!(
        digest(&machine) == manifest.machine_sha256,
        "environment machine digest mismatch"
    );
    Ok(machine)
}

fn validate_environment_checked(
    path: &Path,
    id: &str,
    expected: &Compatibility,
    lazy: bool,
    pool: &Path,
    owned_stage: bool,
) -> anyhow::Result<(
    EnvironmentManifest,
    Vec<super::SharedFilesystemLayer>,
    Vec<u8>,
)> {
    let manifest = read_environment_manifest(path, id, expected)?;
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
    let machine = read_environment_machine(path, &manifest)?;
    if owned_stage {
        ensure!(
            manifest.stage_bases.is_some(),
            "owned restore requires a stage snapshot"
        );
    }
    #[cfg(target_os = "linux")]
    if let Some(blocks) = &manifest.filesystem_blocks {
        ensure!(
            !path.join("rootfs").try_exists()?,
            "packed filesystem cannot retain private tree data"
        );
        blocks.verify(&path.join("filesystem-blocks"), &manifest.filesystem)?;
    } else {
        if owned_stage {
            super::verify_tree_metadata(&path.join("rootfs"), &manifest.filesystem)?;
        } else {
            verify_tree(&path.join("rootfs"), &manifest.filesystem)?;
        }
    }
    #[cfg(not(target_os = "linux"))]
    if owned_stage {
        super::verify_tree_metadata(&path.join("rootfs"), &manifest.filesystem)?;
    } else {
        verify_tree(&path.join("rootfs"), &manifest.filesystem)?;
    }
    super::layers::complete_inventory(&manifest.filesystem, &manifest.filesystem_layers)?;
    let mut layers = Vec::new();
    for layer in &manifest.filesystem_layers {
        layers.push(super::filesystems::retain(
            pool,
            layer,
            &path.join("filesystem-references"),
            false,
        )?);
    }
    Ok((manifest, layers, machine))
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
        for name in [
            "objects",
            "pending",
            "deleted",
            "content",
            "bases",
            "filesystems",
            "filesystem-build-locks",
        ] {
            let path = root.join(name);
            directory(&path)?;
        }
        Ok(Self {
            filesystem_pool: root.clone(),
            root,
        })
    }

    /// An explicitly configured, host-owned pool permits checkpoints belonging
    /// to different Jobs to retain the same immutable lower without data copies.
    /// Wire manifests cannot select this location. Both directories must be on
    /// the same filesystem for hard-linked immutable markers/encoded frames;
    /// guest filesystem data inodes are never linked across stores.
    pub fn with_filesystem_pool(root: &Path, pool: &Path) -> anyhow::Result<Self> {
        let mut store = Self::new(root)?;
        let pool = Self::new(pool)?.root;
        use std::os::unix::fs::MetadataExt;
        ensure!(
            fs::metadata(&store.root)?.dev() == fs::metadata(&pool)?.dev(),
            "snapshot filesystem pool must be on the store's volume"
        );
        ensure!(
            store.root == pool
                || (!pool.starts_with(&store.root) && !store.root.starts_with(&pool)),
            "snapshot store and pool must be independent roots"
        );
        store.filesystem_pool = pool;
        Ok(store)
    }

    /// Native supervisor only: the producer remains frozen and the caller
    /// has authenticated this source as a read-only launch role. First users
    /// create one pool tree; later Jobs reuse it without temporary data copies.
    #[cfg(test)]
    pub(crate) fn retain_live_lower(
        &self,
        source: &Path,
        logical_root: &Path,
        references: &Path,
    ) -> anyhow::Result<super::SharedFilesystemLayer> {
        self.retain_projected_lower(source, logical_root, references, &[])
    }
    pub(crate) fn retain_projected_lower(
        &self,
        source: &Path,
        logical_root: &Path,
        references: &Path,
        excluded: &[PathBuf],
    ) -> anyhow::Result<super::SharedFilesystemLayer> {
        ensure!(
            self.filesystem_pool != self.root,
            "live lower sealing requires an independent pool"
        );
        ensure!(
            source.is_absolute() && source != Path::new("/") && source.canonicalize()? == source,
            "invalid live lower root"
        );
        ensure!(
            logical_root.components().count() == 1
                && logical_root
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "invalid live lower namespace"
        );
        let expected = super::inventory_projected(source, excluded)?;
        super::filesystems::share(
            &self.filesystem_pool,
            source,
            logical_root.as_os_str().as_bytes(),
            &expected,
            references,
            excluded,
        )
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
            filesystem_pool: self.filesystem_pool.clone(),
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
    pub fn open_owned_stage_for_restore(
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
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "invalid environment object"
        );
        let (manifest, layers, machine) = validate_environment_checked(
            &path,
            id,
            expected,
            lazy,
            &self.filesystem_pool,
            owned_stage,
        )?;
        let bases = self.open_stage_bases(&path, &manifest)?;
        Ok(PublishedEnvironment {
            path,
            manifest,
            machine,
            bases,
            filesystem_pool: self.filesystem_pool.clone(),
            _layers: layers,
            _reference: reference,
        })
    }
    fn open_stage_bases(
        &self,
        path: &Path,
        manifest: &EnvironmentManifest,
    ) -> anyhow::Result<Vec<super::SnapshotBase>> {
        let Some(references) = &manifest.stage_bases else {
            return Ok(Vec::new());
        };
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
            .collect()
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
        super::layers::remove_private_tree(&tombstone)?;
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
            // Writer/reader TempDir cleanup can race this directory listing.
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
            match super::layers::remove_private_tree(&path) {
                Ok(()) => removed += 1,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        for entry in fs::read_dir(self.root.join("deleted"))? {
            let path = entry?.path();
            ensure!(
                fs::symlink_metadata(&path)?.is_dir(),
                "invalid deletion tombstone"
            );
            super::layers::remove_private_tree(&path)?;
            removed += 1;
        }
        File::open(self.root.join("pending"))?.sync_all()?;
        File::open(self.root.join("deleted"))?.sync_all()?;
        removed += blocks::collect(&self.root)?;
        removed += super::base::collect(&self.root)?;
        if self.filesystem_pool == self.root {
            removed += super::filesystems::collect(&self.root)?;
        } else {
            removed += SnapshotStore::new(&self.filesystem_pool)?.collect_abandoned()?;
        }
        Ok(removed)
    }
}

impl Drop for PendingEnvironment {
    fn drop(&mut self) {
        // This writer still owns its lock. Publication moves the directory,
        // so cleanup only sees unpublished, unshared private data. TempDir's
        // ordinary removal cannot unlink children of read-only directories.
        if self.staging.path().try_exists().unwrap_or(false) {
            let _ = super::layers::remove_private_tree(self.staging.path());
        }
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
    /// Copy frozen captured RAM into this pending object without allocating
    /// zero-filled chunks. The source must remain immutable through publish.
    pub fn copy_ram_from(&self, source: &File) -> anyhow::Result<()> {
        copy_sparse_ram(source, &self.create_ram()?)
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
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            false,
            PublicationSource::Borrowed,
        )
    }
    /// Durable compressed content uses the resident pool codec; no live pool is required.
    pub fn publish_compressed(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            true,
            PublicationSource::Borrowed,
        )
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
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            PublicationSource::Stage(bases),
        )
    }
    /// Publish private data plus independently retained immutable lower trees.
    /// RAM/state must have been captured in the same full-machine freeze. The
    /// caller supplies only verified owners; arbitrary paths are not accepted.
    pub fn publish_layered(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        layers: &[SnapshotLayer<'_>],
    ) -> anyhow::Result<String> {
        ensure!(
            !layers.is_empty(),
            "layered snapshot requires immutable lowers"
        );
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            PublicationSource::Layered(layers),
        )
    }

    /// Internal native coordinator path: consume its freshly copied private
    /// capture forest while every source CPU/device remains frozen. The native
    /// producer must retain no writable FDs to this forest and external writers
    /// are excluded. Borrowed source trees must use the ordinary copying APIs.
    /// Failure after transfer is uncertain and requires native fail-stop.
    #[cfg(any(
        test,
        not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))
    ))]
    pub(crate) fn publish_native_capture(
        self,
        source: &Path,
        ram: &File,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            PublicationSource::Native {
                ram,
                delta: None,
                layers: &[],
                private_sources: &[],
            },
        )
    }

    /// Retain private file contents as independently owned compressed frames.
    /// Metadata and hard-link topology remain per snapshot; every restore gets
    /// new writable data inodes. The caller must freeze the borrowed source.
    #[cfg(target_os = "linux")]
    pub fn publish_chunked_filesystem(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            PublicationSource::Chunked,
        )
    }

    #[cfg(test)]
    pub(crate) fn publish_native_delta(
        self,
        source: &Path,
        ram: &File,
        machine: &[u8],
        compatibility: Compatibility,
        base: &super::blocks::PinnedRamBlocks,
        delta: &super::blocks::RamDelta<'_>,
    ) -> anyhow::Result<String> {
        self.publish_with_ram(
            source,
            machine,
            compatibility,
            true,
            PublicationSource::Native {
                ram,
                delta: Some((base, *delta)),
                layers: &[],
                private_sources: &[],
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn publish_native_layered(
        self,
        source: &Path,
        ram: &File,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        capture: NativeLayerCapture<'_>,
    ) -> anyhow::Result<String> {
        self.publish_native_layered_retained(
            source,
            ram,
            machine,
            compatibility,
            compressed,
            capture,
        )
        .map(|publication| publication.id)
    }

    #[cfg(test)]
    pub(crate) fn publish_native_layered_retained(
        self,
        source: &Path,
        ram: &File,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        capture: NativeLayerCapture<'_>,
    ) -> anyhow::Result<NativePublication> {
        ensure!(
            self.filesystem_pool != self.store && !capture.layers.is_empty(),
            "native layered capture requires an independent host pool"
        );
        self.publish_native_retained(source, ram, machine, compatibility, compressed, capture)
    }

    pub(crate) fn publish_native_retained(
        self,
        source: &Path,
        ram: &File,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        capture: NativeLayerCapture<'_>,
    ) -> anyhow::Result<NativePublication> {
        ensure!(
            capture.layers.is_empty() || self.filesystem_pool != self.store,
            "native layered capture requires an independent host pool"
        );
        self.publish_retained_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            PublicationSource::Native {
                ram,
                delta: capture.delta,
                layers: capture.layers,
                private_sources: capture.private_sources,
            },
        )
    }

    fn publish_with_ram(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        publication_source: PublicationSource<'_>,
    ) -> anyhow::Result<String> {
        self.publish_retained_with_ram(
            source,
            machine,
            compatibility,
            compressed,
            publication_source,
        )
        .map(|publication| publication.id)
    }

    fn publish_retained_with_ram(
        self,
        source: &Path,
        machine: &[u8],
        compatibility: Compatibility,
        compressed: bool,
        publication_source: PublicationSource<'_>,
    ) -> anyhow::Result<NativePublication> {
        ensure!(!machine.is_empty(), "missing machine state");
        ensure!(
            !compatibility.host_boot.is_empty()
                && !compatibility.build.is_empty()
                && !compatibility.firmware.is_empty()
                && !compatibility.profile.is_empty(),
            "incomplete compatibility binding"
        );
        let source_root = source.canonicalize()?;
        let stage_bases = if let PublicationSource::Stage(bases) = &publication_source {
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
                        !source_root.starts_with(base.root())
                            && !base.root().starts_with(&source_root),
                        "stage overlaps its base"
                    );
                    Ok(base.reference().clone())
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            File::open(self.staging.path().join("base-refs"))?.sync_all()?;
            Some(references)
        } else {
            None
        };
        // Use the verified native RAM FD directly. Creating another pending
        // capture.ram would add a full sparse-copy pass before sealing. The
        // final raw inode or compressed blocks still detach writable sources.
        let native_ram = match &publication_source {
            #[cfg(target_os = "linux")]
            PublicationSource::Chunked => None,
            PublicationSource::Borrowed
            | PublicationSource::Stage(_)
            | PublicationSource::Layered(_) => None,
            PublicationSource::Native { ram, .. } => {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    matches!(fs::symlink_metadata(self.staging.path().join("capture.ram")),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                    "native publication cannot retain a pending RAM writer"
                );
                let path = source
                    .parent()
                    .context("missing native capture parent")?
                    .join("capture.ram");
                let metadata = fs::symlink_metadata(&path)?;
                let pinned = ram.metadata()?;
                ensure!(
                    metadata.is_file()
                        && pinned.is_file()
                        && metadata.dev() == pinned.dev()
                        && metadata.ino() == pinned.ino(),
                    "native RAM descriptor does not belong to this capture"
                );
                Some((path, ram.try_clone()?))
            }
        };
        let destination = self.staging.path().join("rootfs");
        #[cfg(target_os = "linux")]
        let direct_sources = match &publication_source {
            PublicationSource::Native {
                private_sources,
                layers,
                ..
            } if !private_sources.is_empty() => {
                ensure!(
                    self.filesystem_pool != self.store
                        && layers.iter().all(|layer| layer.id.is_some()),
                    "direct capture requires sealed lowers and an independent host pool"
                );
                for captured in *private_sources {
                    for excluded in [&self.store, &self.filesystem_pool, &source_root] {
                        ensure!(
                            !captured.source.starts_with(excluded)
                                && !excluded.starts_with(&captured.source),
                            "direct private source overlaps publication storage"
                        );
                    }
                }
                Some(*private_sources)
            }
            _ => None,
        };
        #[cfg(not(target_os = "linux"))]
        if let PublicationSource::Native {
            private_sources, ..
        } = &publication_source
        {
            ensure!(
                private_sources.is_empty(),
                "direct native filesystem capture requires Linux"
            );
        }
        let mut filesystem = match &publication_source {
            #[cfg(target_os = "linux")]
            PublicationSource::Chunked => copy_owned_tree(&source_root, &destination)?,
            // Use the supplied spelling for strict ancestry validation. Keep
            // the original logical root in the manifest after it is consumed.
            PublicationSource::Native { .. } => {
                super::owned::take_native_tree(&self.store, source, &destination)?
            }
            PublicationSource::Borrowed
            | PublicationSource::Stage(_)
            | PublicationSource::Layered(_) => copy_owned_tree(&source_root, &destination)?,
        };
        let mut filesystem_layers = Vec::new();
        let mut retained = Vec::new();
        #[cfg(target_os = "linux")]
        if let PublicationSource::Native { layers, .. } = &publication_source
            && !layers.is_empty()
        {
            ensure!(layers.len() <= 128, "too many native immutable layers");
            let root = filesystem.entries[0].clone();
            let references = self.staging.path().join("filesystem-references");
            fs::create_dir(&references)?;
            for captured in *layers {
                let layer = super::filesystems::captured_layer(
                    &self.filesystem_pool,
                    &source_root,
                    &destination,
                    captured,
                    &root,
                )?;
                let owner = if captured.id.is_some() {
                    super::filesystems::retain(&self.filesystem_pool, &layer, &references, true)?
                } else {
                    let path = destination.join(&captured.path);
                    let owner = super::filesystems::adopt(
                        &self.filesystem_pool,
                        &path,
                        &layer,
                        &references,
                    )?;
                    if path.try_exists()? {
                        super::layers::remove_private_tree(&path)?;
                    }
                    owner
                };
                retained.push(owner);
                filesystem_layers.push(layer);
            }
            super::linux::restore_metadata(&destination, &root)?;
            File::open(&destination)?.sync_all()?;
            filesystem = super::inventory(&destination)?;
            super::layers::complete_inventory(&filesystem, &filesystem_layers)?;
        }
        #[cfg(not(target_os = "linux"))]
        if let PublicationSource::Native { layers, .. } = &publication_source {
            ensure!(layers.is_empty(), "native lower retention requires Linux");
        }
        if let PublicationSource::Layered(layers) = &publication_source {
            for layer in *layers {
                ensure!(
                    layer.owner.pool == self.filesystem_pool,
                    "immutable layer belongs to a different configured pool"
                );
                filesystem_layers.push(FilesystemLayer::from_owner(layer)?);
            }
            super::layers::complete_inventory(&filesystem, &filesystem_layers)?;
            fs::create_dir(self.staging.path().join("filesystem-references"))?;
            for layer in &filesystem_layers {
                retained.push(super::filesystems::retain(
                    &self.filesystem_pool,
                    layer,
                    &self.staging.path().join("filesystem-references"),
                    true,
                )?);
            }
        }
        #[cfg(target_os = "linux")]
        let mut filesystem_capture_stats = None;
        #[cfg(target_os = "linux")]
        let filesystem_blocks = if matches!(publication_source, PublicationSource::Chunked)
            || matches!(publication_source, PublicationSource::Native { .. })
                && self.filesystem_pool != self.store
        {
            let blocks = if let Some(sources) = direct_sources {
                let (tree, blocks, stats) = super::FilesystemBlocks::capture_sources(
                    &self.filesystem_pool,
                    &filesystem,
                    sources,
                    &self.staging.path().join("filesystem-blocks"),
                )?;
                filesystem = tree;
                filesystem_capture_stats = Some(stats);
                super::layers::complete_inventory(&filesystem, &filesystem_layers)?;
                blocks
            } else {
                let (blocks, stats) = super::FilesystemBlocks::capture_with_stats(
                    &self.filesystem_pool,
                    &destination,
                    &filesystem,
                    &self.staging.path().join("filesystem-blocks"),
                )?;
                filesystem_capture_stats = Some(stats);
                blocks
            };
            super::layers::remove_private_tree(&destination)?;
            Some(blocks)
        } else {
            None
        };
        write_synced(&self.staging.path().join("machine.json"), machine)?;
        // Detach the sealed RAM inode from writable capture descriptors which
        // callers may still hold. They cannot mutate the published payload.
        let remove_capture = native_ram.is_none();
        let (capture_path, mut capture) = if let Some(native) = native_ram {
            native
        } else {
            let path = self.staging.path().join("capture.ram");
            let file = File::open(&path)?;
            (path, file)
        };
        capture.seek(SeekFrom::Start(0))?;
        capture.sync_all()?;
        ensure!(capture.metadata()?.len() > 0, "empty captured RAM");
        let capture_hash = file_hash(&capture_path)?;
        let mut ram_hash = capture_hash.clone();
        let ram_blocks = if compressed {
            let _publishing = gate(&self.store, false)?;
            let references = self.staging.path().join("ram-blocks");
            if let PublicationSource::Native {
                delta: Some((base, delta)),
                ..
            } = &publication_source
            {
                let (blocks, hash) =
                    RamBlocks::capture_delta(&self.store, &references, &capture, base, delta)?;
                ram_hash = hash;
                Some(blocks)
            } else {
                Some(RamBlocks::capture(&self.store, &references, &mut capture)?)
            }
        } else {
            let sealed_path = self.staging.path().join("ram.bin");
            let sealed = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&sealed_path)?;
            copy_sparse_ram(&capture, &sealed)?;
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
        if remove_capture {
            fs::remove_file(capture_path)?;
        }
        let ram_index = if compressed {
            None
        } else {
            Some(RawRamIndex::capture(&File::open(
                self.staging.path().join("ram.bin"),
            )?)?)
        };
        let manifest = EnvironmentManifest {
            version: if stage_bases.is_some() {
                if compressed { 5 } else { 4 }
            } else if cfg!(target_os = "linux") && {
                #[cfg(target_os = "linux")]
                {
                    filesystem_blocks.is_some()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    false
                }
            } {
                5
            } else if !filesystem_layers.is_empty() {
                4
            } else if compressed {
                2
            } else {
                3
            },
            compatibility,
            stage_bases,
            source_root: source_root.as_os_str().as_bytes().to_vec(),
            filesystem,
            filesystem_layers,
            #[cfg(target_os = "linux")]
            filesystem_blocks,
            ram_sha256: ram_hash,
            ram_blocks,
            ram_index,
            machine_sha256: digest(machine),
        };
        let bytes = serde_json::to_vec(&manifest)?;
        ensure!(
            manifest.stage_bases.is_some()
                || manifest.version < 4
                || bytes.len() <= 16 * 1024 * 1024,
            "layered snapshot manifest exceeds metadata limit"
        );
        let id = digest(&bytes);
        write_synced(&self.staging.path().join("manifest.json"), &bytes)?;
        File::open(self.staging.path())?.sync_all()?;
        #[cfg(target_os = "linux")]
        let private_files = if matches!(publication_source, PublicationSource::Native { .. }) {
            manifest
                .filesystem_blocks
                .as_ref()
                .map(|blocks| {
                    pin_private_files(
                        &self.store,
                        &self.filesystem_pool,
                        blocks,
                        &self.staging.path().join("filesystem-blocks"),
                    )
                })
                .transpose()?
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let private_files = None;
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
        Ok(NativePublication {
            id,
            lowers: retained,
            private_files,
            #[cfg(target_os = "linux")]
            filesystem_capture_stats,
        })
    }
}
impl PublishedEnvironment {
    #[cfg(target_os = "linux")]
    pub(crate) fn pin_private_files(
        &self,
        store: &SnapshotStore,
    ) -> anyhow::Result<Option<std::sync::Arc<PrivateFilesystemOwner>>> {
        self.manifest
            .filesystem_blocks
            .as_ref()
            .map(|blocks| {
                pin_private_files(
                    &store.root,
                    &store.filesystem_pool,
                    blocks,
                    &self.path.join("filesystem-blocks"),
                )
            })
            .transpose()
    }
    #[cfg(target_os = "linux")]
    #[cfg(test)]
    pub(super) fn directory(&self) -> &Path {
        &self.path
    }
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
            // Eager mappings must authenticate all raw RAM, even after a lazy open.
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
        #[cfg(target_os = "linux")]
        if let Some(blocks) = &self.manifest.filesystem_blocks {
            blocks.materialize(
                &self.path.join("filesystem-blocks"),
                &self.manifest.filesystem,
                None,
                destination,
            )?;
            let result = (|| -> anyhow::Result<()> {
                fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
                for layer in &self.manifest.filesystem_layers {
                    self.materialize_layer(
                        Path::new(std::ffi::OsStr::from_bytes(&layer.path)),
                        &destination.join(std::ffi::OsStr::from_bytes(&layer.path)),
                    )?;
                }
                super::linux::restore_metadata(
                    destination,
                    &self.manifest.filesystem.entries[0],
                )?;
                verify_tree(destination, &self.complete_inventory()?)
            })();
            if result.is_err() {
                super::layers::remove_private_tree(destination)?;
            }
            return result;
        }
        copy_owned_tree_checked(
            &self.path.join("rootfs"),
            destination,
            Some(&self.manifest.filesystem),
        )?;
        if !self.manifest.filesystem_layers.is_empty() {
            let result = (|| -> anyhow::Result<()> {
                // This is an unpublished private copy. A sealed read-only
                // root may need temporary traversal/write access to add layers.
                let source = self.path.join("rootfs");
                #[cfg(target_os = "linux")]
                let metadata = fs::symlink_metadata(&source)?;
                fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
                for layer in &self.manifest.filesystem_layers {
                    self.materialize_layer(
                        Path::new(std::ffi::OsStr::from_bytes(&layer.path)),
                        &destination.join(std::ffi::OsStr::from_bytes(&layer.path)),
                    )?;
                }
                #[cfg(target_os = "linux")]
                super::linux::copy_entry(&source, destination, &metadata)?;
                #[cfg(target_os = "macos")]
                ensure!(
                    unsafe {
                        libc::copyfile(
                            native_path(&source)?.as_ptr(),
                            native_path(destination)?.as_ptr(),
                            std::ptr::null_mut(),
                            libc::COPYFILE_METADATA | libc::COPYFILE_NOFOLLOW,
                        )
                    } == 0,
                    "restore private root metadata: {}",
                    std::io::Error::last_os_error()
                );
                verify_tree(destination, &self.complete_inventory()?)
            })();
            if result.is_err() {
                super::layers::remove_private_tree(destination)?;
            }
            result?;
        }
        Ok(())
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
    pub fn materialize_owned_stage(&self, destination: &Path) -> anyhow::Result<()> {
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

    /// Complete logical payload, including immutable trees absent from rootfs.
    pub fn complete_inventory(&self) -> anyhow::Result<TreeInventory> {
        super::layers::complete_inventory(
            &self.manifest.filesystem,
            &self.manifest.filesystem_layers,
        )
    }

    /// Stream a regular file from a verified snapshot without a temporary data
    /// tree. Keep this publication alive; external writers must remain excluded.
    #[cfg(target_os = "linux")]
    pub fn open_file(&self, relative: &Path) -> anyhow::Result<Box<dyn std::io::Read + '_>> {
        ensure!(
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                && !relative.as_os_str().is_empty(),
            "snapshot file path must be relative"
        );
        let lookup = |tree: &TreeInventory, path: &[u8]| {
            tree.entries
                .binary_search_by(|entry| {
                    entry
                        .path
                        .split(|byte| *byte == b'/')
                        .cmp(path.split(|byte| *byte == b'/'))
                })
                .ok()
        };
        let layered = self.manifest.filesystem_layers.iter().find_map(|layer| {
            relative
                .strip_prefix(Path::new(std::ffi::OsStr::from_bytes(&layer.path)))
                .ok()
                .map(|tail| (layer, tail))
        });
        let entry = if let Some((layer, tail)) = layered {
            let index = lookup(&layer.filesystem, tail.as_os_str().as_bytes())
                .context("missing immutable snapshot file")?;
            &layer.filesystem.entries[index]
        } else {
            let index = lookup(&self.manifest.filesystem, relative.as_os_str().as_bytes())
                .context("missing private snapshot file")?;
            &self.manifest.filesystem.entries[index]
        };
        let super::TreeObject::File {
            bytes, hardlink, ..
        } = &entry.object
        else {
            anyhow::bail!("snapshot payload is not a regular file");
        };
        if layered.is_none()
            && let Some(blocks) = &self.manifest.filesystem_blocks
            && blocks.file(hardlink).is_some()
        {
            return Ok(Box::new(blocks.reader(
                &self.path.join("filesystem-blocks"),
                hardlink,
                *bytes,
            )?));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.file_path(relative.as_os_str().as_bytes())?)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == *bytes,
            "snapshot file changed size/type"
        );
        Ok(Box::new(file))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn file_path(&self, relative: &[u8]) -> anyhow::Result<PathBuf> {
        let path = Path::new(std::ffi::OsStr::from_bytes(relative));
        for layer in &self.manifest.filesystem_layers {
            let prefix = Path::new(std::ffi::OsStr::from_bytes(&layer.path));
            if let Ok(tail) = path.strip_prefix(prefix) {
                let owner = self
                    ._layers
                    .iter()
                    .find(|owner| owner.id == layer.id)
                    .context("missing active immutable layer owner")?;
                return Ok(owner.root().join(tail));
            }
        }
        Ok(self.path.join("rootfs").join(path))
    }



    fn layer(&self, relative: &Path) -> anyhow::Result<(PathBuf, TreeInventory)> {
        use super::{TreeEntry, TreeObject};
        use std::path::Component;
        ensure!(
            relative.components().count() == 1
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "snapshot layer must be one owned directory"
        );
        let prefix = relative.as_os_str().as_bytes();
        if let Some(layer) = self
            .manifest
            .filesystem_layers
            .iter()
            .find(|layer| layer.path == prefix)
        {
            let owner = self
                ._layers
                .iter()
                .find(|owner| owner.id == layer.id)
                .context("missing immutable layer owner")?;
            return Ok((owner.root().to_owned(), layer.filesystem.clone()));
        }
        let mut entries = Vec::new();
        let within = |path: &[u8]| -> anyhow::Result<Vec<u8>> {
            if path == prefix {
                return Ok(Vec::new());
            }
            let tail = path
                .strip_prefix(prefix)
                .context("hardlink escapes snapshot layer")?;
            ensure!(
                tail.first() == Some(&b'/'),
                "hardlink escapes snapshot layer"
            );
            Ok(tail[1..].to_vec())
        };
        for entry in &self.manifest.filesystem.entries {
            if entry.path != prefix
                && !(entry.path.starts_with(prefix) && entry.path.get(prefix.len()) == Some(&b'/'))
            {
                continue;
            }
            let mut entry: TreeEntry = entry.clone();
            entry.path = within(&entry.path)?;
            match &mut entry.object {
                TreeObject::File { hardlink, .. } | TreeObject::Symlink { hardlink, .. } => {
                    *hardlink = within(hardlink)?
                }
                TreeObject::Directory => {}
            }
            entries.push(entry);
        }
        ensure!(
            entries
                .first()
                .is_some_and(|e| e.path.is_empty() && e.object == TreeObject::Directory),
            "missing owned snapshot layer"
        );
        let source = self.path.join("rootfs").join(relative);
        #[cfg(target_os = "linux")]
        let packed = self.manifest.filesystem_blocks.is_some();
        #[cfg(not(target_os = "linux"))]
        let packed = false;
        ensure!(
            packed || fs::symlink_metadata(&source)?.is_dir(),
            "invalid owned snapshot layer"
        );
        Ok((
            source,
            TreeInventory {
                version: self.manifest.filesystem.version,
                entries,
            },
        ))
    }

    /// Read a verified owned layer while this publication's reference is held.
    /// Consumers must treat it as immutable and retain `self` for the read.
    pub fn owned_layer_path(&self, relative: &Path) -> anyhow::Result<PathBuf> {
        #[cfg(target_os = "linux")]
        ensure!(
            self.manifest.filesystem_blocks.is_none()
                || self
                    .manifest
                    .filesystem_layers
                    .iter()
                    .any(|layer| layer.path == relative.as_os_str().as_bytes()),
            "packed private layers must be materialized"
        );
        Ok(self.layer(relative)?.0)
    }

    /// Copy one independently owned layer, keeping writable data private.
    pub fn materialize_layer(&self, relative: &Path, destination: &Path) -> anyhow::Result<()> {
        let (source, expected) = self.layer(relative)?;
        #[cfg(target_os = "linux")]
        if let Some(blocks) = &self.manifest.filesystem_blocks
            && !self
                .manifest
                .filesystem_layers
                .iter()
                .any(|layer| layer.path == relative.as_os_str().as_bytes())
        {
            blocks.materialize(
                &self.path.join("filesystem-blocks"),
                &self.manifest.filesystem,
                Some(relative.as_os_str().as_bytes()),
                destination,
            )?;
            verify_tree(destination, &expected)?;
            return Ok(());
        }
        let copied = copy_owned_tree(&source, destination)?;
        ensure!(
            copied == expected,
            "snapshot layer changed during restore copy"
        );
        Ok(())
    }

    /// Share a fully verified tree through persistent per-Attempt marker links.
    /// The consumer MUST enforce read-only access. Writable backing must use
    /// materialize/materialize_layer instead. Dropping this owner preserves
    /// the disk reference until the retained Attempt itself is collected.
    pub fn share_readonly_layer(
        &self,
        relative: &Path,
        references: &Path,
    ) -> anyhow::Result<super::SharedFilesystemLayer> {
        let (source, expected) = self.layer(relative)?;
        if let Some(layer) = self
            .manifest
            .filesystem_layers
            .iter()
            .find(|layer| layer.path == relative.as_os_str().as_bytes())
        {
            return super::filesystems::retain(&self.filesystem_pool, layer, references, true);
        }
        #[cfg(target_os = "linux")]
        ensure!(
            self.manifest.filesystem_blocks.is_none(),
            "packed private layers must be materialized before sharing"
        );
        super::filesystems::share(
            &self.filesystem_pool,
            &source,
            relative.as_os_str().as_bytes(),
            &expected,
            references,
            &[],
        )
    }

    pub fn can_share_layers(&self, references: &Path) -> anyhow::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            fs::symlink_metadata(references)?.is_dir(),
            "invalid Attempt filesystem references"
        );
        Ok(
            fs::metadata(self.filesystem_pool.join("filesystems"))?.dev()
                == fs::metadata(references)?.dev(),
        )
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
