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
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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
    lease: File,
}
impl SnapshotBase {
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
            lease: self.lease.try_clone()?,
        })
    }
    pub fn verify(&self) -> anyhow::Result<()> {
        let seal = check(&self.directory, &self.reference)?;
        let bytes = fs::read(self.directory.join("inventory.json"))?;
        ensure!(
            digest(&bytes) == seal.inventory_sha256,
            "base inventory digest mismatch"
        );
        let inventory: TreeInventory = serde_json::from_slice(&bytes)?;
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
    ensure!(seal.version == 1, "unsupported base seal");
    valid_id(&seal.inventory_sha256)?;
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
    check(&directory, reference).context("immutable stage base unavailable")?;
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
        let bytes = serde_json::to_vec(&inventory)?;
        write_synced(&pending.directory().join("inventory.json"), &bytes)?;
        let root = fs::symlink_metadata(pending.directory().join("rootfs"))?;
        let seal = Seal {
            version: 1,
            inventory_sha256: digest(&bytes),
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
