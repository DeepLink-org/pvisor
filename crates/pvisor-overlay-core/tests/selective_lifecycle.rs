//! Generated, mount-free selective acceptance and discard through public APIs.
//! Host edits are scheduled between preview and execution, never during apply.
use pvisor_overlay_core::{
    OverlayCore,
    apply::{
        ApplyRecordState, ApplySelection, ChangeEntry, ChangeKind, OverlayRecord, OverlayState,
        OverlayUpper, apply_overlay_selected, discard_overlay, has_pending_applies,
        load_apply_records, load_overlay_record, overlay_changes, preview_overlay_apply,
        recover_pending_applies, write_overlay_record,
    },
    load_preimages,
    stage::{self, StageDurability},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
}
type Tree = BTreeMap<PathBuf, Entry>;

// Snapshot only the owned fixture. Atime is deliberately not an invariant:
// inspection can change it. Bytes, names, inode identity and mutation metadata
// are checked before/after read-only preview and rejected execution.
#[derive(Debug, PartialEq, Eq)]
struct Observation {
    entry: Entry,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Observation> {
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

fn tree(root: &Path) -> Tree {
    snapshot(root)
        .into_iter()
        .map(|(path, value)| (path, value.entry))
        .collect()
}

fn paths(changes: &[ChangeEntry]) -> BTreeSet<PathBuf> {
    changes.iter().map(ChangeEntry::relative_path).collect()
}

fn changes(record: &OverlayRecord, lower: &[PathBuf]) -> BTreeMap<PathBuf, ChangeKind> {
    overlay_changes(record, lower)
        .unwrap()
        .into_iter()
        .map(|change| (change.relative_path(), change.kind))
        .collect()
}

fn payload(seed: u64, tag: &str) -> Vec<u8> {
    let mut bytes = format!("{seed}:{tag}:界:\0\n").into_bytes();
    bytes.extend((0..seed % 257).map(|index| seed.wrapping_add(index) as u8));
    bytes
}

#[derive(Default, Debug)]
struct Counts {
    variants: usize,
    preview_rejections: usize,
    execution_rejections: usize,
    partial_applies: usize,
    committed_recoveries: usize,
    terminal_drops: usize,
}

fn run_case(seed: u64, compact: bool, strict: bool, counts: &mut Counts) {
    let durability = if strict {
        StageDurability::Strict
    } else {
        StageDurability::Checkpoint
    };
    let root = tempfile::Builder::new()
        .prefix(&format!(
            "sel4-{seed}-{}-{}-",
            u8::from(compact),
            u8::from(strict)
        ))
        .tempdir()
        .unwrap()
        .keep();
    // Retain every failed fixture, including setup errors. No failure-path sweep.
    fs::write(
        root.join("case.txt"),
        format!("seed={seed} compact={compact} strict={strict}\n"),
    )
    .unwrap();
    let target = root.join("target");
    let stage_dir = root.join("stage");
    let upper = stage_dir.join("upper");
    let work = stage_dir.join("work");
    let journal = stage_dir.join("preimages");
    fs::create_dir(&target).unwrap();
    let mut expected = Tree::new();
    for name in ["keep", "conflict", "pending", "source", "gone", "untouched"] {
        let bytes = payload(seed, name);
        fs::write(target.join(name), &bytes).unwrap();
        expected.insert(name.into(), Entry::File(bytes));
    }
    let original = expected.clone();
    let lower = vec![target.clone()];
    stage::begin(&journal, durability).unwrap();
    let core = OverlayCore::new_with_exclusions_and_preimages(
        lower.clone(),
        upper.clone(),
        Some(work.clone()),
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    let core = if compact {
        core.with_compact_preimages().unwrap()
    } else {
        core
    };
    for name in ["keep", "conflict", "pending"] {
        fs::write(
            core.copy_up(Path::new(name)).unwrap(),
            payload(seed, &format!("staged-{name}")),
        )
        .unwrap();
    }
    let renamed = if seed.is_multiple_of(2) {
        PathBuf::from(format!("renamed-{seed:x}-界"))
    } else {
        let mut bytes = format!("renamed-{seed:x}-").into_bytes();
        bytes.push(0xff);
        PathBuf::from(OsString::from_vec(bytes))
    };
    core.rename(Path::new("source"), &renamed, false).unwrap();
    core.remove(Path::new("gone"), false).unwrap();
    let mut directories = Vec::new();
    let mut parent = PathBuf::new();
    for depth in 0..1 + (seed / 24) % 3 {
        parent.push(format!("new-{depth}"));
        core.create_dir(&parent, 0o755).unwrap();
        directories.push(parent.clone());
    }
    let leaf = parent.join("leaf");
    core.create_file(&leaf, 0o644, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)
        .unwrap()
        .write_all(&payload(seed, "new-leaf"))
        .unwrap();
    core.sync_preimages().unwrap();
    drop(core);
    // Writer lifecycle is real: release it, reconstruct a new owner, verify
    // candidates, then release again before sealing or apply/recovery.
    let reopened = OverlayCore::open_existing(
        lower.clone(),
        upper.clone(),
        Some(work.clone()),
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    for name in ["keep", "conflict", "pending"] {
        assert_eq!(
            fs::read(
                reopened
                    .resolve_checked(Path::new(name))
                    .unwrap()
                    .unwrap()
                    .path
            )
            .unwrap(),
            payload(seed, &format!("staged-{name}"))
        );
    }
    assert_eq!(
        fs::read(reopened.resolve_checked(&renamed).unwrap().unwrap().path).unwrap(),
        payload(seed, "source")
    );
    assert!(
        reopened
            .resolve_checked(Path::new("source"))
            .unwrap()
            .is_none()
    );
    assert!(
        reopened
            .resolve_checked(Path::new("gone"))
            .unwrap()
            .is_none()
    );
    drop(reopened);
    stage::complete(&upper, &journal).unwrap();
    stage::require_sealed(&journal).unwrap();
    let mut record = OverlayRecord {
        id: format!("selective-{seed}-{compact}-{strict}"),
        generation: seed % 17,
        target: target.clone(),
        baseline_lower: None,
        upper: OverlayUpper {
            upper_dir: upper.clone(),
            work_dir: work.clone(),
        },
        merged_dir: stage_dir.join("merged"),
        stage_dir: stage_dir.clone(),
        excluded_paths: vec![],
        access_policy: Default::default(),
        auto_apply: false,
        auto_discard: false,
        protect_target: false,
        state: OverlayState::Staged,
    };
    write_overlay_record(&record).unwrap();
    let mut remaining = BTreeMap::from([
        (PathBuf::from("keep"), ChangeKind::Modified),
        (PathBuf::from("conflict"), ChangeKind::Modified),
        (PathBuf::from("pending"), ChangeKind::Modified),
        (PathBuf::from("source"), ChangeKind::Deleted),
        (renamed.clone(), ChangeKind::Added),
        (PathBuf::from("gone"), ChangeKind::Deleted),
        (leaf.clone(), ChangeKind::Added),
    ]);
    for directory in &directories {
        remaining.insert(directory.clone(), ChangeKind::Added);
    }
    assert_eq!(tree(&target), original);
    assert_eq!(changes(&record, &lower), remaining);
    let observations = load_preimages(&journal).unwrap();
    let conflict_baselines: Vec<_> = observations
        .iter()
        .filter(|entry| {
            ["conflict", "pending"]
                .iter()
                .any(|name| entry.relative_path() == Path::new(name))
        })
        .cloned()
        .collect();
    assert_eq!(conflict_baselines.len(), 2);

    let all = ApplySelection::default();
    let before = snapshot(&root);
    let preview = preview_overlay_apply(&record, &lower, &all).unwrap();
    assert_eq!(
        paths(&preview.selected),
        remaining.keys().cloned().collect()
    );
    assert!(preview.required.is_empty());
    assert_eq!(snapshot(&root), before);
    // A preview is not a reservation. These content-only writes do not change
    // the parent namespace; they deterministically interleave an external editor.
    for name in ["conflict", "pending"] {
        let bytes = payload(seed, &format!("host-{name}"));
        fs::write(target.join(name), &bytes).unwrap();
        expected.insert(name.into(), Entry::File(bytes));
    }
    for _ in 0..2 {
        record = load_overlay_record(&stage_dir).unwrap();
        let before = snapshot(&root);
        let error = preview_overlay_apply(&record, &lower, &all).unwrap_err();
        assert!(
            error.to_string().contains("target changed after staging"),
            "{error}"
        );
        counts.preview_rejections += 1;
        assert_eq!(snapshot(&root), before);
        let error = apply_overlay_selected(&mut record, &lower, &all).unwrap_err();
        assert!(
            error.to_string().contains("target changed after staging"),
            "{error}"
        );
        counts.execution_rejections += 1;
        assert_eq!(snapshot(&root), before);
        assert!(load_apply_records(&stage_dir).unwrap().is_empty());
        assert!(!has_pending_applies(&record).unwrap());
    }

    // Four disjoint healthy groups; 24 consecutive seeds exhaust their order
    // permutations. Rename's source/destination are selected together by this
    // caller, not asserted to be a built-in atomic rename-selection contract.
    let groups = [
        vec![PathBuf::from("keep")],
        vec![PathBuf::from("source"), renamed.clone()],
        vec![PathBuf::from("gone")],
        vec![leaf.clone()],
    ];
    let mut order = vec![0, 1, 2, 3];
    let mut permutation = seed % 24;
    for index in (1..4).rev() {
        order.swap(index, (permutation % (index as u64 + 1)) as usize);
        permutation /= index as u64 + 1;
    }
    for (index, group) in order.into_iter().enumerate() {
        record = load_overlay_record(&stage_dir).unwrap();
        let selection = ApplySelection {
            paths: groups[group].clone(),
            ..Default::default()
        };
        let before = snapshot(&root);
        let preview = preview_overlay_apply(&record, &lower, &selection).unwrap();
        let selected: BTreeSet<_> = groups[group].iter().cloned().collect();
        let required: BTreeSet<_> = if group == 3 {
            directories.iter().cloned().collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(paths(&preview.selected), selected);
        assert_eq!(paths(&preview.required), required);
        assert_eq!(
            paths(&preview.candidates),
            remaining.keys().cloned().collect()
        );
        assert_eq!(snapshot(&root), before);
        let outcome = apply_overlay_selected(&mut record, &lower, &selection).unwrap();
        let applied: BTreeSet<_> = selected.union(&required).cloned().collect();
        assert_eq!(paths(&outcome.applied), applied);
        for path in &applied {
            remaining.remove(path).unwrap();
        }
        match group {
            0 => {
                expected.insert("keep".into(), Entry::File(payload(seed, "staged-keep")));
            }
            1 => {
                expected.remove(Path::new("source")).unwrap();
                expected.insert(renamed.clone(), Entry::File(payload(seed, "source")));
            }
            2 => {
                expected.remove(Path::new("gone")).unwrap();
            }
            3 => {
                for path in &directories {
                    expected.insert(path.clone(), Entry::Directory);
                }
                expected.insert(leaf.clone(), Entry::File(payload(seed, "new-leaf")));
            }
            _ => unreachable!(),
        }
        assert_eq!(tree(&target), expected);
        assert_eq!(record.state, OverlayState::Staged);
        assert_eq!(
            paths(&outcome.remaining),
            remaining.keys().cloned().collect()
        );
        assert_eq!(changes(&record, &lower), remaining);
        assert_eq!(
            fs::read(upper.join("pending")).unwrap(),
            payload(seed, "staged-pending")
        );
        assert_eq!(
            fs::read(upper.join("conflict")).unwrap(),
            payload(seed, "staged-conflict")
        );
        let current = load_preimages(&journal).unwrap();
        for baseline in &conflict_baselines {
            assert!(current.contains(baseline));
        }
        let ledger = load_apply_records(&stage_dir).unwrap();
        assert_eq!(ledger.len(), index + 1);
        assert!(
            ledger
                .iter()
                .all(|entry| entry.state == ApplyRecordState::Committed)
        );
        assert_eq!(ledger.last().unwrap().apply_id, outcome.apply_id);
        assert_eq!(ledger.last().unwrap().remaining_changes, remaining.len());
        assert!(!has_pending_applies(&record).unwrap());
        counts.partial_applies += 1;
        if group == 0 {
            // Committed recovery must not replay an accepted write over a
            // subsequent host edit. The unrelated pending stage remains intact.
            let bytes = payload(seed, "host-after-accept");
            fs::write(target.join("keep"), &bytes).unwrap();
            expected.insert("keep".into(), Entry::File(bytes));
        }
        let before = snapshot(&root);
        assert!(
            recover_pending_applies(&mut record, &lower)
                .unwrap()
                .is_empty()
        );
        assert_eq!(snapshot(&root), before);
        assert_eq!(tree(&target), expected);
        counts.committed_recoveries += 1;
    }
    assert_eq!(
        remaining.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([PathBuf::from("conflict"), PathBuf::from("pending")])
    );
    let mut stale = load_overlay_record(&stage_dir).unwrap();
    discard_overlay(&mut record).unwrap();
    assert_eq!(record.state, OverlayState::Discarded);
    assert!(!upper.exists() && !work.exists());
    assert_eq!(tree(&target), expected);
    let terminal = snapshot(&root);
    discard_overlay(&mut stale).unwrap();
    assert_eq!(stale.state, OverlayState::Discarded);
    assert_eq!(snapshot(&root), terminal);
    assert!(apply_overlay_selected(&mut stale, &lower, &all).is_err());
    assert_eq!(snapshot(&root), terminal);
    assert_eq!(tree(&target), expected);
    assert_eq!(load_apply_records(&stage_dir).unwrap().len(), 4);
    counts.terminal_drops += 1;
    counts.variants += 1;
    fs::remove_dir_all(&root).unwrap(); // Only this confirmed-success private fixture.
}

fn parameter(name: &str, default: u64) -> u64 {
    std::env::var(name).map_or(default, |value| value.parse().expect(name))
}

#[test]
fn generated_selective_apply_conflicts_and_drop_preserve_host_and_pending_state() {
    let first = parameter("PVISOR_CHAOS_FIRST_SEED", 202611120001);
    let count = parameter("PVISOR_CHAOS_SEED_COUNT", 6);
    assert!((1..=65536).contains(&count), "seed count must be 1..65536");
    first.checked_add(count).expect("seed interval overflow");
    let start = Instant::now();
    let mut counts = Counts::default();
    for seed in first..first + count {
        for compact in [false, true] {
            for strict in [false, true] {
                run_case(seed, compact, strict, &mut counts);
            }
        }
    }
    eprintln!(
        "Selective lifecycle first={first} seeds={count} elapsed={:?} counts={counts:?}",
        start.elapsed()
    );
}
