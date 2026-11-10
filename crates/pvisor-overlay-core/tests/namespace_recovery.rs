//! Ordinary namespace errors/retries compared with a native disposable tree.
use pvisor_overlay_core::{OverlayCore, service::FilesystemService};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Rename(&'static str, &'static str),
    Remove(&'static str, bool),
    Create(&'static str),
    Mkdir(&'static str),
}

impl Operation {
    fn native(self, root: &Path) -> io::Result<()> {
        match self {
            Self::Rename(old, new) => fs::rename(root.join(old), root.join(new)),
            Self::Remove(path, true) => fs::remove_dir(root.join(path)),
            Self::Remove(path, false) => fs::remove_file(root.join(path)),
            Self::Create(path) => fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(path))?
                .write_all(b"created"),
            Self::Mkdir(path) => fs::create_dir(root.join(path)),
        }
    }

    fn overlay(self, service: &FilesystemService) -> io::Result<()> {
        match self {
            Self::Rename(old, new) => service.rename(Path::new(old), Path::new(new), false),
            Self::Remove(path, directory) => service.remove(Path::new(path), directory),
            Self::Create(path) => service
                .create_file(
                    Path::new(path),
                    0o644,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                )?
                .write_all(b"created"),
            Self::Mkdir(path) => service.create_dir(Path::new(path), 0o755),
        }
    }
}

type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

fn native_tree(root: &Path) -> Tree {
    fn walk(root: &Path, relative: &Path, tree: &mut Tree) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let path = relative.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                tree.insert(path.clone(), None);
                walk(root, &path, tree);
            } else {
                assert!(entry.file_type().unwrap().is_file());
                tree.insert(path, Some(fs::read(entry.path()).unwrap()));
            }
        }
    }
    let mut tree = Tree::new();
    walk(root, Path::new(""), &mut tree);
    tree
}

fn overlay_tree(service: &FilesystemService) -> Tree {
    fn walk(service: &FilesystemService, relative: &Path, tree: &mut Tree) {
        for (name, _) in service.directory_candidates(relative).unwrap() {
            let path = relative.join(name);
            let metadata = service.metadata(&path).unwrap();
            if metadata.is_dir() {
                tree.insert(path.clone(), None);
                walk(service, &path, tree);
            } else {
                assert!(metadata.is_file());
                let backing = service.prepare_open(&path, libc::O_RDONLY).unwrap();
                tree.insert(path, Some(fs::read(backing).unwrap()));
            }
        }
    }
    let mut tree = Tree::new();
    walk(service, Path::new(""), &mut tree);
    tree
}

fn populate(root: &Path) {
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::create_dir(root.join("occupied")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("a"), b"A").unwrap();
    fs::write(root.join("dir/inside"), b"nested").unwrap();
    fs::write(root.join("occupied/item"), b"stay").unwrap();
}

#[test]
fn namespace_type_errors_retries_and_reopens_match_native_without_changing_lower() {
    for compact in [None, Some(false), Some(true)] {
        namespace_trace(compact);
    }
}

fn namespace_trace(compact: Option<bool>) {
    use Operation::*;
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let native = root.path().join("native");
    let upper = root.path().join("upper");
    let work = root.path().join("work");
    populate(&lower);
    populate(&native);
    let original = native_tree(&lower);
    let baseline = pvisor_overlay_core::fingerprint_at(&lower, Path::new("a")).unwrap();
    let journal = compact.map(|_| root.path().join("preimages"));
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![lower.clone()],
        upper.clone(),
        Some(work.clone()),
        vec![],
        journal.clone(),
    )
    .unwrap();
    let core = if compact == Some(true) {
        core.with_compact_preimages().unwrap()
    } else {
        core
    };
    let mut service = FilesystemService::new(core);
    let operations = [
        Rename("dir", "occupied"),
        Remove("occupied", true),
        Rename("a", "occupied"),
        Rename("dir", "a"),
        Create("a"),
        Mkdir("dir"),
        Remove("dir", false),
        Remove("a", true),
        Rename("missing", "missing"),
        Remove("missing", false),
        Rename("empty", "occupied"),
        Rename("dir", "empty"),
        Remove("empty", true),
        Rename("empty/inside", "a"),
        Mkdir("empty/inside"),
        Create("empty/inside/new"),
        Rename("empty", "occupied"),
        Remove("occupied/item", false),
        Rename("empty", "occupied"),
        Remove("occupied/inside", true),
        Remove("occupied/inside/new", false),
        Remove("occupied/inside", true),
        Remove("occupied", true),
        Create("occupied"),
        Rename("occupied", "final"),
        Rename("final", "final"),
        Remove("final", false),
        Create("final"),
    ];
    let mut failures = 0;
    for (index, operation) in operations.into_iter().enumerate() {
        let before = native_tree(&native);
        let directory_unlink = matches!(
            operation,
            Remove(path, false) if before.get(Path::new(path)) == Some(&None)
        );
        let expected = operation.native(&native);
        let actual = operation.overlay(&service);
        if directory_unlink {
            // POSIX unlink specifies EPERM; its rationale documents Linux/LSB
            // EISDIR and tells portable callers to handle either. Core's
            // adapter-facing contract remains EISDIR on every platform.
            assert_eq!(
                actual.as_ref().err().and_then(|error| error.raw_os_error()),
                Some(libc::EISDIR),
                "step {index}: {operation:?}, actual={actual:?}"
            );
            assert!(
                matches!(
                    expected
                        .as_ref()
                        .err()
                        .and_then(|error| error.raw_os_error()),
                    Some(libc::EPERM | libc::EISDIR)
                ),
                "step {index}: {operation:?}, expected={expected:?}"
            );
            assert_eq!(
                native_tree(&native),
                before,
                "native unlink changed a directory"
            );
            assert_eq!(
                overlay_tree(&service),
                before,
                "overlay unlink changed a directory"
            );
        } else {
            assert_eq!(
                actual.as_ref().err().map(|error| error.raw_os_error()),
                expected.as_ref().err().map(|error| error.raw_os_error()),
                "step {index}: {operation:?}, actual={actual:?}, expected={expected:?}"
            );
        }
        failures += usize::from(expected.is_err());
        assert_eq!(
            overlay_tree(&service),
            native_tree(&native),
            "step {index}: {operation:?}"
        );
        assert_eq!(
            native_tree(&lower),
            original,
            "lower changed at step {index}"
        );
        // Reopen every checkpoint, including immediately after rejected calls.
        drop(service);
        service = FilesystemService::new(
            OverlayCore::open_existing(
                vec![lower.clone()],
                upper.clone(),
                Some(work.clone()),
                vec![],
                journal.clone(),
            )
            .unwrap(),
        );
        assert_eq!(
            overlay_tree(&service),
            native_tree(&native),
            "reopen {index}"
        );
        assert_eq!(
            fs::read_dir(&work).unwrap().count(),
            0,
            "scratch at {index}"
        );
    }
    assert_eq!(failures, 14);
    drop(service);
    if let Some(journal) = journal {
        let entries = pvisor_overlay_core::load_preimages(&journal).unwrap();
        let observed = entries
            .iter()
            .find(|entry| entry.relative_path() == Path::new("a"))
            .unwrap();
        assert_eq!(observed.state, baseline);
    }
}
