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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseReference {
    pub id: String,
}

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
