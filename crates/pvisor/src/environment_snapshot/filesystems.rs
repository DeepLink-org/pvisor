//! Authenticated immutable tree copies with durable per-Attempt references.
//! Only marker inodes are hard-linked; filesystem data/topology stays intact.
use super::{TreeInventory, copy_guest_tree, native_path, store, verify_tree};
use anyhow::{Context, ensure};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct SharedFilesystemLayer {
    pub id: String,
    root: PathBuf,
    pub(super) pool: PathBuf,
    // The disk reference survives supervisor exit for retained Run records. This
    // lock additionally fences GC if an active Attempt's reference is removed.
    _active: File,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct LayerManifest<'a> {
    version: u32,
    logical_root: &'a [u8],
    filesystem: &'a TreeInventory,
}
impl SharedFilesystemLayer {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn pool(&self) -> &Path {
        &self.pool
    }
    /// The native supervisor uses this while the producer remains frozen.
    /// Verify unseen data too; saved guest inode digests alone are insufficient.
    #[cfg(test)]
    pub(crate) fn verify_source(&self, source: &Path) -> anyhow::Result<()> {
        self.verify_projected_source(source, &[])
    }
    pub(crate) fn verify_projected_source(
        &self,
        source: &Path,
        excluded: &[PathBuf],
    ) -> anyhow::Result<()> {
        let (_, expected) = seal(self)?;
        verify_tree(self.root(), &expected)?;
        if source != self.root() {
            ensure!(
                super::inventory_projected(source, excluded)? == expected,
                "live immutable lower differs from its retained seal"
            );
        }
        Ok(())
    }
}

fn marker(path: &Path, id: &str) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() == 64,
        "invalid filesystem reference marker"
    );
    let mut bytes = Vec::new();
    (&file).take(65).read_to_end(&mut bytes)?;
    ensure!(
        bytes == id.as_bytes(),
        "filesystem reference marker mismatch"
    );
    Ok(file)
}

pub(super) fn share(
    store_root: &Path,
    source: &Path,
    logical_root: &[u8],
    expected: &TreeInventory,
    references: &Path,
    excluded: &[PathBuf],
) -> anyhow::Result<SharedFilesystemLayer> {
    let _gate = store::gate(store_root, false)?;
    ensure!(
        fs::symlink_metadata(references)?.is_dir(),
        "filesystem references must be an owned directory"
    );
    let references = references.canonicalize()?;
    ensure!(
        !references.starts_with(store_root),
        "Attempt references must be outside the snapshot store"
    );
    ensure!(
        super::inventory_projected(source, excluded)? == *expected,
        "shared layer source does not match its seal"
    );
    // Equal trees at distinct logical roots must retain distinct inode
    // identities within one overlay; hard-link origin tables depend on it.
    let manifest = serde_json::to_vec(&LayerManifest {
        version: 1,
        logical_root,
        filesystem: expected,
    })?;
    ensure!(
        expected.entries.len() <= 65_536 && manifest.len() <= 16 * 1024 * 1024,
        "immutable lower exceeds metadata limit"
    );
    let id = store::digest(&manifest);
    let content = store_root.join("filesystems");
    ensure!(
        fs::metadata(&content)?.dev() == fs::metadata(&references)?.dev(),
        "shared filesystem references require the same filesystem"
    );
    let object = content.join(&id);
    // Hold the pool gate before opening/waiting on this key lock. GC can then
    // remove idle lock files under its exclusive gate without splitting a key
    // between a waiter on an unlinked inode and a new creator. Hits need no key
    // lock, and unrelated trees remain concurrent.
    let _creating = if !object.try_exists()? {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(store_root.join("filesystem-build-locks").join(&id))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 1,
            "invalid filesystem creation lock"
        );
        fs2::FileExt::lock_exclusive(&file)?;
        Some(file)
    } else {
        None
    };
    if !object.try_exists()? {
        // The store gate excludes GC throughout preparation/publication. A
        // crash leaves ordinary pending state for the existing collector.
        let temporary = tempfile::Builder::new()
            .prefix("filesystem-")
            .tempdir_in(store_root.join("pending"))?;
        let _cleanup = StagingCleanup(temporary.path().join("tree"));
        let copied = copy_guest_tree(source, &temporary.path().join("tree"), excluded)?;
        ensure!(
            copied == *expected,
            "filesystem changed during shared layer creation"
        );
        for (name, bytes) in [
            ("manifest.json", manifest.as_slice()),
            ("reference", id.as_bytes()),
        ] {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(temporary.path().join(name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        File::open(temporary.path())?.sync_all()?;
        let old = native_path(temporary.path())?;
        let new = native_path(&object)?;
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
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(old.as_ptr(), new.as_ptr(), libc::RENAME_EXCL) };
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.kind() == std::io::ErrorKind::AlreadyExists,
                "shared filesystem publication: {error}"
            );
        }
        File::open(&content)?.sync_all()?;
        File::open(store_root.join("pending"))?.sync_all()?;
    }
    ensure!(
        fs::symlink_metadata(&object)?.is_dir(),
        "invalid shared filesystem object"
    );
    ensure!(
        read_manifest(&object.join("manifest.json"))? == manifest,
        "shared filesystem manifest mismatch"
    );
    verify_tree(&object.join("tree"), expected).context("shared filesystem content mismatch")?;
    let active = marker(&object.join("reference"), &id)?;
    fs2::FileExt::try_lock_shared(&active)?;
    let reference = references.join(&id);
    match fs::hard_link(object.join("reference"), &reference) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let existing = fs::symlink_metadata(&reference)?;
    let actual = active.metadata()?;
    ensure!(
        existing.is_file() && existing.dev() == actual.dev() && existing.ino() == actual.ino(),
        "filesystem reference identity mismatch"
    );
    File::open(references)?.sync_all()?;
    Ok(SharedFilesystemLayer {
        id,
        root: object.join("tree"),
        pool: store_root.to_owned(),
        _active: active,
    })
}

/// Snapshot references are separate from Attempt references. A child keeps its
/// own marker inode link; deleting an ancestor cannot drop the child's data.
pub(super) fn retain(
    pool: &Path,
    layer: &super::layers::FilesystemLayer,
    references: &Path,
    create: bool,
) -> anyhow::Result<SharedFilesystemLayer> {
    let _gate = store::gate(pool, false)?;
    layer.validate()?;
    ensure!(
        fs::symlink_metadata(references)?.is_dir(),
        "invalid snapshot layer references"
    );
    let references = references.canonicalize()?;
    ensure!(
        !references.starts_with(pool.join("filesystems")),
        "references cannot alias immutable filesystem data"
    );
    let manifest = serde_json::to_vec(&LayerManifest {
        version: 1,
        logical_root: &layer.logical_root,
        filesystem: &layer.filesystem,
    })?;
    ensure!(
        store::digest(&manifest) == layer.id,
        "filesystem layer identity mismatch"
    );
    let object = pool.join("filesystems").join(&layer.id);
    ensure!(
        fs::symlink_metadata(&object)?.is_dir(),
        "filesystem layer manifest mismatch"
    );
    let stored = read_manifest(&object.join("manifest.json"))?;
    ensure!(
        store::digest(&stored) == layer.id && stored == manifest,
        "filesystem layer manifest collision or corruption"
    );
    verify_tree(&object.join("tree"), &layer.filesystem)
        .context("retained filesystem layer does not match its seal")?;
    let active = marker(&object.join("reference"), &layer.id)?;
    fs2::FileExt::try_lock_shared(&active)?;
    let reference = references.join(&layer.id);
    if create {
        match fs::hard_link(object.join("reference"), &reference) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let actual = active.metadata()?;
    let existing = fs::symlink_metadata(&reference)?;
    ensure!(
        existing.is_file() && existing.dev() == actual.dev() && existing.ino() == actual.ino(),
        "snapshot filesystem reference identity mismatch"
    );
    if create {
        File::open(&references)?.sync_all()?;
    }
    Ok(SharedFilesystemLayer {
        id: layer.id.clone(),
        root: object.join("tree"),
        pool: pool.to_owned(),
        _active: active,
    })
}

/// Read metadata only when sealing a new checkpoint. Live Attempt owners stay
/// small and do not retain a complete lower inventory per running VM.
pub(super) fn seal(owner: &SharedFilesystemLayer) -> anyhow::Result<(Vec<u8>, TreeInventory)> {
    ensure!(
        owner.root == owner.pool.join("filesystems").join(&owner.id).join("tree"),
        "immutable owner root mismatch"
    );
    metadata(&owner.pool, &owner.id)
}

fn metadata(pool: &Path, id: &str) -> anyhow::Result<(Vec<u8>, TreeInventory)> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredManifest {
        version: u32,
        logical_root: Vec<u8>,
        filesystem: TreeInventory,
    }
    store::valid_id(id)?;
    let object = pool.join("filesystems").join(id);
    ensure!(
        fs::symlink_metadata(&object)?.is_dir(),
        "invalid immutable object"
    );
    let bytes = read_manifest(&object.join("manifest.json"))?;
    ensure!(
        store::digest(&bytes) == id,
        "immutable layer manifest mismatch"
    );
    let stored: StoredManifest = serde_json::from_slice(&bytes)?;
    ensure!(stored.version == 1, "unsupported immutable layer manifest");
    Ok((stored.logical_root, stored.filesystem))
}

pub(super) fn captured_layer(
    pool: &Path,
    capture_root: &Path,
    private_root: &Path,
    captured: &super::CapturedFilesystemLayer,
    root: &super::TreeEntry,
) -> anyhow::Result<super::FilesystemLayer> {
    use std::os::unix::ffi::OsStrExt;
    let path = captured.path.as_os_str().as_bytes().to_vec();
    let source = captured.source.as_os_str().as_bytes().to_vec();
    // Validate path/binding before joining either with a storage root. The
    // placeholder inventory supplies only the required root shape.
    let mut layer = super::FilesystemLayer {
        path: path.clone(),
        source,
        id: captured.id.clone().unwrap_or_else(|| "0".repeat(64)),
        logical_root: path,
        filesystem: TreeInventory {
            version: 1,
            entries: vec![root.clone()],
        },
    };
    layer.validate()?;
    if let Some(id) = &captured.id {
        ensure!(
            captured.source == pool.join("filesystems").join(id).join("tree")
                && !private_root.join(&captured.path).try_exists()?,
            "retained native lower must resolve only in its configured pool"
        );
        let (logical_root, filesystem) = metadata(pool, id)?;
        layer.logical_root = logical_root;
        layer.filesystem = filesystem;
    } else {
        ensure!(
            captured.source == capture_root.join(&captured.path),
            "fresh native lower escapes its captured forest"
        );
        layer.filesystem = super::inventory(&private_root.join(&captured.path))?;
        let bytes = serde_json::to_vec(&LayerManifest {
            version: 1,
            logical_root: &layer.logical_root,
            filesystem: &layer.filesystem,
        })?;
        layer.id = store::digest(&bytes);
    }
    layer.validate()?;
    Ok(layer)
}

fn read_manifest(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
        "immutable layer manifest exceeds limit"
    );
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "immutable layer manifest exceeds limit"
    );
    Ok(bytes)
}

pub(super) fn validate_identity(layer: &super::layers::FilesystemLayer) -> anyhow::Result<()> {
    layer.validate()?;
    let manifest = serde_json::to_vec(&LayerManifest {
        version: 1,
        logical_root: &layer.logical_root,
        filesystem: &layer.filesystem,
    })?;
    ensure!(
        store::digest(&manifest) == layer.id,
        "filesystem layer identity mismatch"
    );
    Ok(())
}

/// Adopt only an independently verified caller-owned staging tree. Imported
/// data is synced before this call; no data inode is linked to another tree.
/// If the pool already has this identity, validate it and retain its marker.
#[cfg(target_os = "linux")]
pub(super) fn adopt(
    pool: &Path,
    source: &Path,
    layer: &super::layers::FilesystemLayer,
    references: &Path,
) -> anyhow::Result<SharedFilesystemLayer> {
    let _gate = store::gate(pool, false)?;
    validate_identity(layer)?;
    ensure!(
        source.canonicalize()? == source && fs::symlink_metadata(source)?.is_dir(),
        "immutable adoption requires a canonical owned staging directory"
    );
    verify_tree(source, &layer.filesystem)?;
    ensure!(
        fs::metadata(source)?.dev() == fs::metadata(pool)?.dev(),
        "immutable adoption requires the same filesystem"
    );
    let object = pool.join("filesystems").join(&layer.id);
    if !object.try_exists()? {
        let temporary = tempfile::Builder::new()
            .prefix("filesystem-")
            .tempdir_in(pool.join("pending"))?;
        let _cleanup = StagingCleanup(temporary.path().join("tree"));
        // Linux may require write access to a moved directory when changing
        // its parent. Only this verified, unpublished staging root changes;
        // restore its complete metadata before exposing an immutable object.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source, fs::Permissions::from_mode(0o700))?;
        fs::rename(source, temporary.path().join("tree"))?;
        File::open(source.parent().context("missing staging parent")?)?.sync_all()?;
        super::linux::restore_metadata(
            &temporary.path().join("tree"),
            &layer.filesystem.entries[0],
        )?;
        File::open(temporary.path().join("tree"))?.sync_all()?;
        let manifest = serde_json::to_vec(&LayerManifest {
            version: 1,
            logical_root: &layer.logical_root,
            filesystem: &layer.filesystem,
        })?;
        for (name, bytes) in [
            ("manifest.json", manifest.as_slice()),
            ("reference", layer.id.as_bytes()),
        ] {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(temporary.path().join(name))?;
            output.write_all(bytes)?;
            output.sync_all()?;
        }
        File::open(temporary.path())?.sync_all()?;
        let old = native_path(temporary.path())?;
        let new = native_path(&object)?;
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
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.kind() == std::io::ErrorKind::AlreadyExists,
                "immutable layer adoption: {error}"
            );
            // A simultaneous publisher owns the winning copy. Our private
            // losing tree is never exposed and can be cleaned independently.
            super::layers::remove_private_tree(&temporary.path().join("tree"))?;
        }
        File::open(pool.join("filesystems"))?.sync_all()?;
        File::open(pool.join("pending"))?.sync_all()?;
    }
    retain(pool, layer, references, true)
}

/// The caller holds the exclusive store gate. Live disk references or active
/// owners prevent removal; there is no reconstructable in-memory refcount.
pub(super) fn collect(store_root: &Path) -> anyhow::Result<usize> {
    let content = store_root.join("filesystems");
    let mut removed = 0;
    for entry in fs::read_dir(&content)? {
        let object = entry?.path();
        let id = object
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid shared filesystem identity")?;
        store::valid_id(id)?;
        ensure!(
            fs::symlink_metadata(&object)?.is_dir(),
            "invalid shared filesystem object"
        );
        let reference = marker(&object.join("reference"), id)?;
        if reference.metadata()?.nlink() != 1 {
            continue;
        }
        match fs2::FileExt::try_lock_exclusive(&reference) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        }
        // The exclusive marker lock and absence of disk references prove this
        // tree has no live owner. Read-only directory metadata must not prevent
        // final reclamation; referenced/shared trees are never made writable.
        super::layers::remove_private_tree(&object)?;
        removed += 1;
    }
    File::open(content)?.sync_all()?;
    // collect() is called with the pool's exclusive gate. All creation-lock
    // openers/waiters hold a shared gate, so none can retain these old inodes.
    let locks = store_root.join("filesystem-build-locks");
    for entry in fs::read_dir(&locks)? {
        let path = entry?.path();
        store::valid_id(
            path.file_name()
                .and_then(|name| name.to_str())
                .context("invalid filesystem creation lock identity")?,
        )?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file() && metadata.len() == 0 && metadata.nlink() == 1,
            "invalid filesystem creation lock"
        );
        fs::remove_file(path)?;
    }
    File::open(locks)?.sync_all()?;
    Ok(removed)
}

struct StagingCleanup(PathBuf);
impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.0.try_exists().unwrap_or(false) {
            let _ = super::layers::remove_private_tree(&self.0);
        }
    }
}
