//! Native filesystem oracle and owned backing for namespace traces.
use pvisor_overlay_core::{
    OverlayCore, PathFingerprint, fingerprint_at, load_preimages, service::FilesystemService,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
pub(super) enum JournalMode {
    Disabled,
    Legacy,
    Compact,
}

impl JournalMode {
    pub(super) const ALL: [Self; 3] = [Self::Disabled, Self::Legacy, Self::Compact];
}

#[derive(Clone, Copy)]
pub(super) enum ErrorComparison {
    Exact,
    Outcome,
}

#[derive(Clone, Debug)]
pub(super) enum Operation {
    Rename(&'static str, &'static str),
    Remove(&'static str, bool),
    Create(&'static str),
    Mkdir(&'static str),
    Write(&'static str, Vec<u8>),
}

impl Operation {
    fn native(&self, root: &Path) -> io::Result<()> {
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
            Self::Write(path, bytes) => fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(root.join(path))?
                .write_all(bytes),
        }
    }

    fn overlay(&self, service: &FilesystemService) -> io::Result<()> {
        match self {
            Self::Rename(old, new) => service.rename(Path::new(old), Path::new(new), false),
            Self::Remove(path, directory) => service.remove(Path::new(path), *directory),
            Self::Create(path) => service
                .create_file(
                    Path::new(path),
                    0o644,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                )?
                .write_all(b"created"),
            Self::Mkdir(path) => service.create_dir(Path::new(path), 0o755),
            Self::Write(path, bytes) => fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(service.prepare_open(Path::new(path), libc::O_WRONLY | libc::O_TRUNC)?)?
                .write_all(bytes),
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

pub(super) fn run_trace(
    mode: JournalMode,
    operations: &[Operation],
    comparison: ErrorComparison,
) -> usize {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let native = root.path().join("native");
    let upper = root.path().join("upper");
    let work = root.path().join("work");
    populate(&lower);
    populate(&native);
    let original = native_tree(&lower);
    let mut baselines: BTreeMap<_, _> = original
        .keys()
        .map(|path| (path.clone(), fingerprint_at(&lower, path).unwrap()))
        .collect();
    baselines.insert(
        PathBuf::new(),
        fingerprint_at(&lower, Path::new("")).unwrap(),
    );
    let journal = match mode {
        JournalMode::Disabled => None,
        JournalMode::Legacy | JournalMode::Compact => Some(root.path().join("preimages")),
    };
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![lower.clone()],
        upper.clone(),
        Some(work.clone()),
        vec![],
        journal.clone(),
    )
    .unwrap();
    let core = if matches!(mode, JournalMode::Compact) {
        core.with_compact_preimages().unwrap()
    } else {
        core
    };
    let mut service = FilesystemService::new(core);
    let mut failures = 0;
    let mut observed = BTreeSet::new();
    for (index, operation) in operations.iter().enumerate() {
        let before = native_tree(&native);
        let expected = operation.native(&native);
        let actual = operation.overlay(&service);
        assert_result(operation, &before, &actual, &expected, comparison, index);
        failures += usize::from(expected.is_err());
        if expected.is_err() {
            assert_eq!(
                native_tree(&native),
                before,
                "native failure at {index}: {operation:?}"
            );
            assert_eq!(
                overlay_tree(&service),
                before,
                "overlay failure at {index}: {operation:?}"
            );
        }
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
        if let Some(journal) = &journal {
            let current = assert_baselines(journal, &baselines);
            assert!(
                current.is_superset(&observed),
                "lost observations at step {index}"
            );
            observed = current;
        }
    }
    drop(service);
    if let Some(journal) = journal {
        assert_eq!(assert_baselines(&journal, &baselines), observed);
        let entries = load_preimages(&journal).unwrap();
        let observed = entries
            .iter()
            .find(|entry| entry.relative_path() == Path::new("a"))
            .unwrap();
        assert_eq!(observed.state, baselines[Path::new("a")]);
    }
    failures
}

fn assert_result(
    operation: &Operation,
    before: &Tree,
    actual: &io::Result<()>,
    expected: &io::Result<()>,
    comparison: ErrorComparison,
    index: usize,
) {
    let directory_unlink = matches!(
        operation,
        Operation::Remove(path, false) if before.get(Path::new(path)) == Some(&None)
    );
    if directory_unlink {
        // POSIX permits EPERM; Core's adapter contract is always EISDIR.
        assert_eq!(
            actual.as_ref().err().and_then(|e| e.raw_os_error()),
            Some(libc::EISDIR),
            "step {index}: {operation:?}, actual={actual:?}"
        );
        assert!(
            matches!(
                expected.as_ref().err().and_then(|e| e.raw_os_error()),
                Some(libc::EPERM | libc::EISDIR)
            ),
            "step {index}: {operation:?}, expected={expected:?}"
        );
    } else if matches!(comparison, ErrorComparison::Exact) {
        assert_eq!(
            actual.as_ref().err().map(|error| error.raw_os_error()),
            expected.as_ref().err().map(|error| error.raw_os_error()),
            "step {index}: {operation:?}, actual={actual:?}, expected={expected:?}"
        );
    } else {
        // Multiple invalid conditions can have different errno precedence.
        assert_eq!(
            actual.is_ok(),
            expected.is_ok(),
            "step {index}: {operation:?}, actual={actual:?}, expected={expected:?}"
        );
    }
}

fn assert_baselines(
    journal: &Path,
    baselines: &BTreeMap<PathBuf, PathFingerprint>,
) -> BTreeSet<PathBuf> {
    let mut seen = BTreeSet::new();
    for entry in load_preimages(journal).unwrap() {
        let path = entry.relative_path();
        assert!(seen.insert(path.clone()), "duplicate observation: {path:?}");
        let expected = baselines.get(&path).unwrap_or(&PathFingerprint::Absent);
        assert_eq!(&entry.state, expected, "first-touch baseline: {path:?}");
    }
    seen
}
