//! Native observations and independent expected-tree representation.
use pvisor_overlay_core::apply::{ChangeEntry, ChangeKind, OverlayRecord, overlay_changes};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Directory,
    File(Vec<u8>),
}

pub(super) type Tree = BTreeMap<PathBuf, Entry>;

// Atime changes during inspection and is not a mutation invariant.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Observation {
    entry: Entry,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

pub(super) fn snapshot(root: &Path) -> BTreeMap<PathBuf, Observation> {
    fn visit(root: &Path, relative: &Path, result: &mut BTreeMap<PathBuf, Observation>) {
        for child in fs::read_dir(root.join(relative)).unwrap() {
            let child = child.unwrap();
            let path = relative.join(child.file_name());
            let metadata = fs::symlink_metadata(child.path()).unwrap();
            let entry = if metadata.is_dir() {
                Entry::Directory
            } else {
                assert!(metadata.is_file(), "unexpected fixture type: {path:?}");
                Entry::File(fs::read(child.path()).unwrap())
            };
            result.insert(
                path.clone(),
                Observation {
                    entry,
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    mode: metadata.mode(),
                    uid: metadata.uid(),
                    gid: metadata.gid(),
                    mtime: (metadata.mtime(), metadata.mtime_nsec()),
                    ctime: (metadata.ctime(), metadata.ctime_nsec()),
                },
            );
            if metadata.is_dir() {
                visit(root, &path, result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, Path::new(""), &mut result);
    result
}

pub(super) fn tree(root: &Path) -> Tree {
    snapshot(root)
        .into_iter()
        .map(|(path, value)| (path, value.entry))
        .collect()
}

pub(super) fn paths(changes: &[ChangeEntry]) -> std::collections::BTreeSet<PathBuf> {
    changes.iter().map(ChangeEntry::relative_path).collect()
}

pub(super) fn changes(record: &OverlayRecord, lower: &[PathBuf]) -> BTreeMap<PathBuf, ChangeKind> {
    overlay_changes(record, lower)
        .unwrap()
        .into_iter()
        .map(|change| (change.relative_path(), change.kind))
        .collect()
}
