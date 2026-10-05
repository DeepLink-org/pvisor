//! Snapshot-owned references to complete, authenticated immutable lower trees.
//! Physical pool locations are trusted host configuration, never wire metadata.
use super::{SharedFilesystemLayer, TreeInventory, TreeObject};
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::{
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};

/// Captured immutable data. The binding is relocation metadata only: consumers
/// resolve the id in their configured pool and never open this source path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemLayer {
    pub path: Vec<u8>,
    pub source: Vec<u8>,
    pub id: String,
    pub logical_root: Vec<u8>,
    pub filesystem: TreeInventory,
}

/// The caller retains this owner and must enforce read-only lower access. This
/// API cannot infer whether an opaque machine's upper/work roles overlap it;
/// the native coordinator validates those roles before requesting publication.
pub struct SnapshotLayer<'a> {
    pub path: &'a Path,
    pub source: &'a Path,
    pub owner: &'a SharedFilesystemLayer,
}

/// Native capture's compact relocation record. The producer's missing id
/// names a host-bound original read-only source; the supervisor validates and
/// pins it before sealing. The SDK also accepts a missing id for an exclusively
/// owned lower in the captured forest. An id always names a pinned pool object.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedFilesystemLayer {
    pub path: std::path::PathBuf,
    pub source: std::path::PathBuf,
    pub id: Option<String>,
}

/// Frozen original private backing. The native supervisor authenticates this
/// against launch slots before the sealer reads it. Readers use only the logical
/// directory and never dereference the recorded source during import/restore.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapturedFilesystemSource {
    pub path: std::path::PathBuf,
    pub source: std::path::PathBuf,
}

impl FilesystemLayer {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        let path = Path::new(std::ffi::OsStr::from_bytes(&self.path));
        ensure!(
            self.path.len() <= 255
                && path.components().count() == 1
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                && path.as_os_str().as_bytes() == self.path
                && !self.path.contains(&0)
                && !self.path.contains(&b'/'),
            "snapshot layer must be one canonical directory"
        );
        let source = Path::new(std::ffi::OsStr::from_bytes(&self.source));
        ensure!(
            !self.source.contains(&0)
                && self.source.len() <= 4096
                && source.is_absolute()
                && source != Path::new("/")
                && source
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
                && source.as_os_str().as_bytes() == self.source
                && !self
                    .source
                    .split(|byte| *byte == b'/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == b"." || part == b".."),
            "invalid logical filesystem binding"
        );
        ensure!(
            !self.logical_root.is_empty()
                && self.logical_root.len() <= 4096
                && !self.logical_root.contains(&0),
            "invalid logical filesystem identity"
        );
        super::store::valid_id(&self.id)?;
        ensure!(
            self.filesystem.version == 1
                && !self.filesystem.entries.is_empty()
                && self.filesystem.entries[0].path.is_empty()
                && self.filesystem.entries[0].object == TreeObject::Directory,
            "missing complete immutable layer inventory"
        );
        Ok(())
    }

    pub(super) fn from_owner(value: &SnapshotLayer<'_>) -> anyhow::Result<Self> {
        let (logical_root, filesystem) = super::filesystems::seal(value.owner)?;
        let layer = Self {
            path: value.path.as_os_str().as_bytes().to_vec(),
            source: value.source.as_os_str().as_bytes().to_vec(),
            id: value.owner.id.clone(),
            logical_root,
            filesystem,
        };
        layer.validate()?;
        Ok(layer)
    }
}

/// Flatten only logical metadata for transport/materialization. Every layer is
/// independently closed under its hard-link topology, as are private data.
pub(super) fn complete_inventory(
    private: &TreeInventory,
    layers: &[FilesystemLayer],
) -> anyhow::Result<TreeInventory> {
    let mut bindings = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::new();
    let mut identities = std::collections::BTreeSet::new();
    ensure!(layers.len() <= 128, "too many immutable snapshot layers");
    if !layers.is_empty() {
        let entries = layers
            .iter()
            .try_fold(private.entries.len(), |total, layer| {
                total.checked_add(layer.filesystem.entries.len())
            })
            .ok_or_else(|| anyhow::anyhow!("immutable inventory size overflow"))?;
        ensure!(
            entries <= 65_536,
            "immutable snapshot exceeds inventory limit"
        );
    }
    let mut complete = private.clone();
    for layer in layers {
        layer.validate()?;
        ensure!(
            paths.insert(&layer.path)
                && bindings.insert(&layer.source)
                && identities.insert(&layer.id),
            "duplicate immutable layer binding"
        );
        ensure!(
            !private.entries.iter().any(|entry| entry.path == layer.path
                || entry
                    .path
                    .starts_with(&[layer.path.as_slice(), b"/"].concat())),
            "immutable layer overlaps private snapshot data"
        );
        let prefix = |path: &[u8]| {
            if path.is_empty() {
                layer.path.clone()
            } else {
                [layer.path.as_slice(), b"/", path].concat()
            }
        };
        for entry in &layer.filesystem.entries {
            let mut entry = entry.clone();
            entry.path = prefix(&entry.path);
            match &mut entry.object {
                TreeObject::File { hardlink, .. } | TreeObject::Symlink { hardlink, .. } => {
                    *hardlink = prefix(hardlink);
                }
                TreeObject::Directory => {}
            }
            complete.entries.push(entry);
        }
    }
    if !layers.is_empty() {
        // Match inventory's sorted directory traversal. Byte sorting entire
        // paths would put a sibling "a-" before "a/child", breaking topology
        // and canonical hard-link origins despite otherwise identical data.
        complete.entries.sort_by(|left, right| {
            left.path
                .split(|byte| *byte == b'/')
                .cmp(right.path.split(|byte| *byte == b'/'))
        });
    }
    Ok(complete)
}

/// Cleanup requires exclusive ownership: an unpublished private copy, a
/// deletion tombstone, or a pooled object with no references or active owners.
/// Never follows symlinks or changes a referenced immutable tree's metadata.
pub(super) fn remove_private_tree(path: &Path) -> anyhow::Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt};
    fn prepare(path: &Path) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for child in fs::read_dir(path)? {
                prepare(&child?.path())?;
            }
        }
        Ok(())
    }
    prepare(path)?;
    Ok(std::fs::remove_dir_all(path)?)
}
