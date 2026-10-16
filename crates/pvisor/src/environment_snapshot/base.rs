//! Private, owned generations for stage snapshots. Import/audit traverses the
//! base; warm save/restore only checks its seal and generation identity.
//!
//! Immutability is an ownership contract, not protection against a hostile host
//! or a same-UID process modifying store files. No API returns a writable base
//! descriptor. Attach roots ONLY as overlay lowers; never as passthrough roots
//! or apply destinations. External writers and manual store edits are unsupported.
use super::store::{digest, gate, valid_id, write_synced};
use super::{SnapshotStore, TreeInventory, copy_owned_tree, verify_tree};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub use crate::config::BaseReference;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    version: u32,
    inventory_sha256: String,
    content_index_sha256: String,
    device: u64,
    inode: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedRootfsRecord {
    version: u32,
    base: BaseReference,
}

// Keep the rootfs v1 decoder above strict, including unknown/null fields. A
// workspace entry requires its own domain even when a valid catalog directory
// is moved with its authentic base marker still attached.
#[derive(Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "kebab-case", deny_unknown_fields)]
enum PreparedWorkspaceRecord {
    Workspace { version: u32, base: BaseReference },
}

#[derive(Clone, Copy)]
enum PreparedPurpose {
    Rootfs,
    Workspace,
}

impl PreparedPurpose {
    fn name(self) -> &'static str {
        match self {
            Self::Rootfs => "rootfs",
            Self::Workspace => "workspace",
        }
    }

    fn catalog(self) -> &'static str {
        match self {
            Self::Rootfs => "prepared-rootfs",
            Self::Workspace => "prepared-workspace",
        }
    }

    fn encode_record(self, base: &BaseReference) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Rootfs => Ok(serde_json::to_vec(&PreparedRootfsRecord {
                version: 1,
                base: base.clone(),
            })?),
            Self::Workspace => Ok(serde_json::to_vec(&PreparedWorkspaceRecord::Workspace {
                version: 1,
                base: base.clone(),
            })?),
        }
    }

    fn check_record(self, file: File, expected: &BaseReference) -> anyhow::Result<()> {
        let (version, base) = match self {
            Self::Rootfs => {
                let record: PreparedRootfsRecord = serde_json::from_reader(file)?;
                (record.version, record.base)
            }
            Self::Workspace => {
                let PreparedWorkspaceRecord::Workspace { version, base } =
                    serde_json::from_reader(file)?;
                (version, base)
            }
        };
        ensure!(
            version == 1 && &base == expected,
            "prepared {} identity mismatch",
            self.name()
        );
        Ok(())
    }
}

/// Lease an immutable generation independently of any published snapshot.
/// Keep this lease alive for the entire VM lifetime. Use `verify` for an
/// explicit full integrity audit; normal opens do not reread all base content.
pub struct SnapshotBase {
    store: PathBuf,
    directory: PathBuf,
    reference: BaseReference,
    content_index_sha256: String,
    lease: File,
}
impl SnapshotBase {
    /// Runtime-only admission receipt: authenticate workspace catalog membership
    /// and acquire an independent owner without exposing mutable store state.
    pub(crate) fn workspace_owner(&self) -> anyhow::Result<(Self, PathBuf)> {
        let _reading = gate(&self.store, false)?;
        let owner = open(&self.store, &self.reference)?;
        check_prepared_entry(&self.store, &owner, PreparedPurpose::Workspace)?;
        ensure!(owner.root() == self.root(), "workspace generation mismatch");
        Ok((owner, self.store.clone()))
    }

    /// Recognize an imported generation and acquire its GC lease. Ordinary
    /// mutable roots have no receipt. A matching store seal must authenticate
    /// before its digests can be attached to a VM.
    pub(crate) fn lease_root(root: &Path) -> anyhow::Result<Option<Self>> {
        let root = root.canonicalize()?;
        let Some(directory) = root
            .parent()
            .filter(|_| root.file_name() == Some("rootfs".as_ref()))
        else {
            return Ok(None);
        };
        let Some(bases) = directory
            .parent()
            .filter(|p| p.file_name() == Some("bases".as_ref()))
        else {
            return Ok(None);
        };
        let Some(id) = directory
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|id| valid_id(id).is_ok())
        else {
            return Ok(None);
        };
        if !directory.join("seal.json").try_exists()? {
            return Ok(None);
        }
        let store = bases.parent().context("base has no snapshot store")?;
        let _reading = gate(store, false)?;
        open(store, &BaseReference { id: id.to_owned() }).map(Some)
    }

    pub fn root(&self) -> PathBuf {
        self.directory.join("rootfs")
    }
    pub fn reference(&self) -> &BaseReference {
        &self.reference
    }
    pub fn try_clone(&self) -> anyhow::Result<Self> {
        Ok(Self {
            store: self.store.clone(),
            directory: self.directory.clone(),
            reference: self.reference.clone(),
            content_index_sha256: self.content_index_sha256.clone(),
            lease: self.lease.try_clone()?,
        })
    }
    /// A digest-bound receipt file built from the verified import inventory.
    /// Only attach it to this base's lower; keep this lease alive while using it.
    pub fn content_index(&self) -> (PathBuf, String) {
        // The seal binds this receipt to the leased generation.
        (
            self.directory.join("content-index.bin"),
            self.content_index_sha256.clone(),
        )
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        let seal = check(&self.directory, &self.reference)?;
        let bytes = fs::read(self.directory.join("inventory.json"))?;
        ensure!(
            digest(&bytes) == seal.inventory_sha256,
            "base inventory digest mismatch"
        );
        let inventory: TreeInventory = serde_json::from_slice(&bytes)?;
        let index = fs::read(self.directory.join("content-index.bin"))?;
        ensure!(
            digest(&index) == seal.content_index_sha256,
            "base content index digest mismatch"
        );
        verify_tree(&self.root(), &inventory)
    }
    pub(super) fn pin(&self, store: &Path, destination: &Path) -> anyhow::Result<()> {
        ensure!(store == self.store, "base belongs to another store");
        check(&self.directory, &self.reference)?;
        fs::hard_link(self.directory.join("reference"), destination)?;
        Ok(())
    }
    pub(super) fn verify_pin(&self, path: &Path) -> anyhow::Result<()> {
        let expected = self.lease.metadata()?;
        let actual = fs::symlink_metadata(path)?;
        ensure!(
            actual.is_file() && (actual.dev(), actual.ino()) == (expected.dev(), expected.ino()),
            "stage base reference mismatch"
        );
        Ok(())
    }
}

fn check(directory: &Path, reference: &BaseReference) -> anyhow::Result<Seal> {
    valid_id(&reference.id)?;
    ensure!(
        fs::symlink_metadata(directory)?.is_dir(),
        "invalid base object"
    );
    let bytes = fs::read(directory.join("seal.json"))?;
    ensure!(digest(&bytes) == reference.id, "base seal digest mismatch");
    let seal: Seal = serde_json::from_slice(&bytes)?;
    ensure!(seal.version == 2, "unsupported base seal");
    valid_id(&seal.inventory_sha256)?;
    valid_id(&seal.content_index_sha256)?;
    let root = fs::symlink_metadata(directory.join("rootfs"))?;
    ensure!(
        root.is_dir()
            && (
                root.dev(),
                root.ino(),
                root.mtime(),
                root.mtime_nsec(),
                root.ctime(),
                root.ctime_nsec()
            ) == (
                seal.device,
                seal.inode,
                seal.mtime,
                seal.mtime_nsec,
                seal.ctime,
                seal.ctime_nsec
            ),
        "base generation was replaced or changed"
    );
    Ok(seal)
}

pub(super) fn open(store: &Path, reference: &BaseReference) -> anyhow::Result<SnapshotBase> {
    valid_id(&reference.id)?;
    let directory = store.join("bases").join(&reference.id);
    let seal = check(&directory, reference).context("immutable stage base unavailable")?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("reference"))?;
    ensure!(lease.metadata()?.is_file(), "invalid base reference");
    fs2::FileExt::try_lock_shared(&lease).context("base is being collected")?;
    Ok(SnapshotBase {
        store: store.to_owned(),
        directory,
        reference: reference.clone(),
        content_index_sha256: seal.content_index_sha256,
        lease,
    })
}
impl SnapshotStore {
    /// Copy a quiescent rootfs into this private store and durably retain it.
    ///
    /// The source and store must be independent canonical trees. Import verifies
    /// content and native metadata before publication; special objects and
    /// external hardlinks are rejected. The persistent catalog pin survives all
    /// processes and GC, including stopped Jobs, until a future controlled
    /// removal API is provided. External store modifications are unsupported.
    /// A failed import/pin never returns an unretained base as success; an
    /// unreferenced imported generation may remain for normal GC.
    pub fn prepare_rootfs(&self, source: &Path) -> anyhow::Result<SnapshotBase> {
        self.prepare_base_for(source, PreparedPurpose::Rootfs)
    }

    /// Copy a quiescent workspace into this private store and durably retain it.
    ///
    /// The source and store must be independent canonical trees. Import verifies
    /// contents and native metadata before publication, rejecting special objects
    /// and external hardlinks. The workspace catalog pin survives process exit
    /// and GC until a future controlled removal API; it does not select a CLI
    /// workspace, create a stage or change apply/VM behavior. Keep the returned
    /// lease throughout any use of this generation as a lower, never as an apply
    /// destination. External store changes are unsupported. On failure no
    /// unretained base is returned; an imported generation may remain for GC.
    pub fn prepare_workspace(&self, source: &Path) -> anyhow::Result<SnapshotBase> {
        self.prepare_base_for(source, PreparedPurpose::Workspace)
    }

    /// Durably retain an already imported base belonging to this exact store.
    ///
    /// This is idempotent only when the existing catalog record and marker
    /// identify the same sealed generation. Publication is atomic and synced;
    /// malformed records, symlinks and substituted markers are errors. The
    /// caller's lease protects the import-to-pin interval. No tree rescan is
    /// performed and no removal operation is exposed.
    pub fn retain_prepared_rootfs(&self, base: &SnapshotBase) -> anyhow::Result<()> {
        self.retain_prepared(base, PreparedPurpose::Rootfs)
    }

    /// Durably retain this store's sealed base in the workspace catalog.
    ///
    /// The caller's lease protects the import-to-pin interval. Atomic, synced
    /// publication is idempotent only for the same authenticated workspace record
    /// and marker; corrupt records, symlinks, substituted markers and another
    /// store's base fail without replacing an existing entry. This neither scans
    /// the tree nor grants rootfs catalog membership. No removal API is exposed.
    pub fn retain_prepared_workspace(&self, base: &SnapshotBase) -> anyhow::Result<()> {
        self.retain_prepared(base, PreparedPurpose::Workspace)
    }

    /// Authenticate a durably prepared rootfs and acquire an independent lease.
    ///
    /// Checks the bounded catalog record, hardlink identity and small generation
    /// seal under the store gate, without copying or hashing the tree. Keep the
    /// returned owner throughout execution; `VmExecutor::with_rootfs_base` may
    /// consume it. Missing/unpinned or corrupted entries fail rather than falling
    /// back to a mutable directory. The private store must remain externally
    /// unmodified. `SnapshotBase::verify` is the separate full audit operation.
    pub fn open_prepared_rootfs(&self, reference: &BaseReference) -> anyhow::Result<SnapshotBase> {
        self.open_prepared(reference, PreparedPurpose::Rootfs)
    }

    /// Authenticate a workspace catalog entry and acquire an independent lease.
    ///
    /// Under the store gate, checks its bounded purpose-bound record, reference
    /// hardlink and generation seal without copying or hashing the tree. Invalid
    /// IDs, missing/unpinned entries, rootfs-only entries and corruption fail;
    /// there is no fallback to a live directory. Keep the owner through teardown
    /// of every consumer. This is catalog access, not CLI/stage/apply admission.
    /// The private store must remain externally unmodified; use
    /// `SnapshotBase::verify` for an explicit full content audit.
    pub fn open_prepared_workspace(
        &self,
        reference: &BaseReference,
    ) -> anyhow::Result<SnapshotBase> {
        self.open_prepared(reference, PreparedPurpose::Workspace)
    }

    fn prepare_base_for(
        &self,
        source: &Path,
        purpose: PreparedPurpose,
    ) -> anyhow::Result<SnapshotBase> {
        let source = source.canonicalize()?;
        ensure!(
            !source.starts_with(&self.root) && !self.root.starts_with(&source),
            "prepared {} source and store must be independent trees",
            purpose.name()
        );
        let base = self.import_base(&source)?;
        self.retain_prepared(&base, purpose)?;
        Ok(base)
    }

    fn retain_prepared(&self, base: &SnapshotBase, purpose: PreparedPurpose) -> anyhow::Result<()> {
        ensure!(base.store == self.root, "base belongs to another store");
        check(&base.directory, &base.reference)?;
        let pending = self.begin()?;
        write_synced(
            &pending.directory().join("record.json"),
            &purpose.encode_record(base.reference())?,
        )?;
        let _publishing = gate(&self.root, true)?;
        // Complete initialization durability even when another initializer
        // observed an existing store before its creator synced the parent.
        File::open(&self.root)?.sync_all()?;
        File::open(self.root.parent().context("snapshot store has no parent")?)?.sync_all()?;
        let catalog = self.root.join(purpose.catalog());
        ensure!(
            fs::symlink_metadata(&catalog)?.is_dir(),
            "invalid prepared {} catalog",
            purpose.name()
        );
        let destination = catalog.join(&base.reference.id);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                self.check_prepared(base, purpose)?;
                // Also complete durability if a prior publisher committed the
                // rename but failed before syncing its parent directory.
                File::open(&destination)?.sync_all()?;
                File::open(&catalog)?.sync_all()?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        base.pin(&self.root, &pending.directory().join("reference"))?;
        File::open(pending.directory())?.sync_all()?;
        fs::rename(pending.directory(), &destination)?;
        File::open(&catalog)?.sync_all()?;
        File::open(self.root.join("pending"))?.sync_all()?;
        Ok(())
    }

    fn open_prepared(
        &self,
        reference: &BaseReference,
        purpose: PreparedPurpose,
    ) -> anyhow::Result<SnapshotBase> {
        let _reading = gate(&self.root, false)?;
        let base = open(&self.root, reference)?;
        self.check_prepared(&base, purpose)?;
        Ok(base)
    }

    fn check_prepared(&self, base: &SnapshotBase, purpose: PreparedPurpose) -> anyhow::Result<()> {
        check_prepared_entry(&self.root, base, purpose)
    }

    /// Import a mutable source ONCE into an independently owned immutable
    /// generation. Original source deletion/modification cannot affect branches.
    /// Content and native metadata are verified before atomic publication.
    pub fn import_base(&self, source: &Path) -> anyhow::Result<SnapshotBase> {
        let pending = self.begin()?;
        let inventory = copy_owned_tree(source, &pending.directory().join("rootfs"))?;
        let index = pvisor_overlay_core::encode_content_index(
            inventory.entries.iter().filter_map(|entry| {
                if let super::TreeObject::File { sha256, .. } = &entry.object {
                    Some((
                        Path::new(std::ffi::OsStr::from_bytes(&entry.path)),
                        sha256.as_str(),
                    ))
                } else {
                    None
                }
            }),
        )?;
        write_synced(&pending.directory().join("content-index.bin"), &index)?;
        let bytes = serde_json::to_vec(&inventory)?;
        write_synced(&pending.directory().join("inventory.json"), &bytes)?;
        let root = fs::symlink_metadata(pending.directory().join("rootfs"))?;
        let seal = Seal {
            version: 2,
            inventory_sha256: digest(&bytes),
            content_index_sha256: digest(&index),
            device: root.dev(),
            inode: root.ino(),
            mtime: root.mtime(),
            mtime_nsec: root.mtime_nsec(),
            ctime: root.ctime(),
            ctime_nsec: root.ctime_nsec(),
        };
        let bytes = serde_json::to_vec(&seal)?;
        let reference = BaseReference { id: digest(&bytes) };
        write_synced(&pending.directory().join("seal.json"), &bytes)?;
        write_synced(&pending.directory().join("reference"), b"")?;
        File::open(pending.directory())?.sync_all()?;
        let _publishing = gate(&self.root, true)?;
        // Keep the live writer lock in the base directory: moving the pending
        // directory cannot race collection. It is harmless after publication.
        fs::rename(
            pending.directory(),
            self.root.join("bases").join(&reference.id),
        )?;
        File::open(self.root.join("bases"))?.sync_all()?;
        File::open(self.root.join("pending"))?.sync_all()?;
        open(&self.root, &reference)
    }
    pub fn open_base(&self, reference: &BaseReference) -> anyhow::Result<SnapshotBase> {
        let _reading = gate(&self.root, false)?;
        open(&self.root, reference)
    }
}

fn check_prepared_entry(
    store: &Path,
    base: &SnapshotBase,
    purpose: PreparedPurpose,
) -> anyhow::Result<()> {
    let catalog = store.join(purpose.catalog());
    ensure!(
        fs::symlink_metadata(&catalog)?.is_dir(),
        "invalid prepared {} catalog",
        purpose.name()
    );
    let entry = catalog.join(&base.reference.id);
    ensure!(
        fs::symlink_metadata(&entry)?.is_dir(),
        "invalid prepared {} entry",
        purpose.name()
    );
    let record = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(entry.join("record.json"))?;
    ensure!(
        record.metadata()?.is_file() && record.metadata()?.len() <= 1024,
        "invalid prepared {} record",
        purpose.name()
    );
    purpose.check_record(record, base.reference())?;
    base.verify_pin(&entry.join("reference"))
}

/// Called only under the exclusive store gate. Durable snapshot hardlinks and
/// independent runtime leases both prevent removal of a still-required base.
pub(super) fn collect(store: &Path) -> anyhow::Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(store.join("bases"))? {
        let path = entry?.path();
        ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "invalid base directory"
        );
        let pin = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.join("reference"))?;
        ensure!(pin.metadata()?.is_file(), "invalid base reference");
        if pin.metadata()?.nlink() != 1 {
            continue;
        }
        match fs2::FileExt::try_lock_exclusive(&pin) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        }
        fs::remove_dir_all(&path)?;
        removed += 1;
    }
    File::open(store.join("bases"))?.sync_all()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_discovery_authenticates_the_generation_and_keeps_gc_out() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"content").unwrap();
        assert!(SnapshotBase::lease_root(&source).unwrap().is_none());
        let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
        let base = store.import_base(&source).unwrap();
        let root = base.root();
        let lease = SnapshotBase::lease_root(&root).unwrap().unwrap();
        assert_eq!(lease.content_index(), base.content_index());
        drop(base);
        assert_eq!(collect(&store.root).unwrap(), 0);
        fs::write(root.parent().unwrap().join("seal.json"), b"forged").unwrap();
        assert!(SnapshotBase::lease_root(&root).is_err());
        drop(lease);
        assert_eq!(collect(&store.root).unwrap(), 1);
    }

    #[test]
    fn imported_content_receipts_are_bound_to_the_owned_generation_and_audited() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"original").unwrap();
        let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
        let base = store.import_base(&source).unwrap();
        let (index, sha256) = base.content_index();
        fs::write(source.join("file"), b"later source edit").unwrap();
        let journal = temp.path().join("preimages");
        let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
            vec![base.root()],
            temp.path().join("upper"),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap()
        .with_immutable_content_index(&base.root(), index.clone(), &sha256)
        .unwrap();
        let copied = core.copy_up(Path::new("file")).unwrap();
        assert_eq!(fs::read(copied).unwrap(), b"original");
        assert_eq!(
            pvisor_overlay_core::load_preimages(&journal).unwrap()[0].state,
            pvisor_overlay_core::fingerprint_at(&base.root(), Path::new("file")).unwrap()
        );
        base.verify().unwrap();
        fs::write(index, b"corrupted index").unwrap();
        assert!(
            base.verify()
                .unwrap_err()
                .to_string()
                .contains("content index digest mismatch")
        );
    }
    #[test]
    fn seals_without_content_receipts_are_rejected() {
        let bytes = br#"{"version":1,"inventory_sha256":"0000000000000000000000000000000000000000000000000000000000000000","device":1,"inode":2,"mtime":3,"mtime_nsec":4,"ctime":5,"ctime_nsec":6}"#;
        assert!(serde_json::from_slice::<Seal>(bytes).is_err());
    }
}
