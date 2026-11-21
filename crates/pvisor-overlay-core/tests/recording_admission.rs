//! Recording admission through all public Core construction paths.
use proptest::prelude::*;
use pvisor_overlay_core::{
    OverlayCore, OverlayLayout, fingerprint_at, load_preimages,
    stage::{self, StageDurability, StageRecording},
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

#[derive(Clone, Copy, Debug)]
enum Mode {
    Legacy,
    Compact,
    Existing,
}

impl Mode {
    fn build(
        self,
        lower: &Path,
        upper: &Path,
        work: Option<&Path>,
        journal: Option<&Path>,
    ) -> io::Result<OverlayCore> {
        let constructor = match self {
            Self::Legacy => OverlayCore::new_for_layout,
            Self::Compact => OverlayCore::new_for_layout_with_compact_preimages,
            Self::Existing => OverlayCore::open_existing_for_layout,
        };
        constructor(
            OverlayLayout::new(vec![lower.to_path_buf()], lower.to_path_buf())?,
            upper.to_path_buf(),
            work.map(Path::to_path_buf),
            vec![],
            journal.map(Path::to_path_buf),
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Policy {
    NoJournal,
    Unmanaged,
    Checkpoint,
    Strict,
    Ephemeral,
    Unrecorded,
}

impl Policy {
    fn expected(self) -> Option<(StageDurability, StageRecording, Option<&'static [u8]>)> {
        Some(match self {
            Self::NoJournal => return None,
            Self::Unmanaged => (StageDurability::Strict, StageRecording::On, None),
            Self::Checkpoint => (
                StageDurability::Checkpoint,
                StageRecording::On,
                Some(b"pvisor.stage.checkpoint/1\n"),
            ),
            Self::Strict => (
                StageDurability::Strict,
                StageRecording::On,
                Some(b"pvisor.stage.strict/1\n"),
            ),
            Self::Ephemeral => (
                StageDurability::Ephemeral,
                StageRecording::On,
                Some(b"pvisor.stage.ephemeral/1\n"),
            ),
            Self::Unrecorded => (
                StageDurability::Ephemeral,
                StageRecording::Off,
                Some(b"pvisor.stage.ephemeral-unrecorded/1\n"),
            ),
        })
    }
}

fn assert_recording(mode: Mode, policy: Policy, with_work: bool, payload: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    let journal_path = root.path().join("journal");
    let work = with_work.then(|| root.path().join("work"));
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    let original = fingerprint_at(&lower, Path::new("file")).unwrap();
    let expected = policy.expected();
    let journal = expected.map(|_| journal_path.as_path());
    if let Some((durability, recording, Some(_))) = expected {
        stage::begin_with_recording(&journal_path, durability, recording).unwrap();
    }
    if matches!(mode, Mode::Existing) {
        drop(
            Mode::Legacy
                .build(&lower, &upper, work.as_deref(), journal)
                .unwrap(),
        );
    }
    let core = mode
        .build(&lower, &upper, work.as_deref(), journal)
        .unwrap();
    core.observe_read(Path::new("file")).unwrap();
    let copied = core.copy_up(Path::new("file")).unwrap();
    fs::write(copied, payload).unwrap();
    core.sync_preimages().unwrap();
    assert_eq!(fs::read(upper.join("file")).unwrap(), payload);
    assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
    drop(core);
    let reopened = Mode::Existing
        .build(&lower, &upper, work.as_deref(), journal)
        .unwrap();
    let resolved = reopened
        .resolve_checked(Path::new("file"))
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(resolved.path).unwrap(), payload);
    drop(reopened);
    match expected {
        None => assert!(!journal_path.exists()),
        Some((durability, recording, marker)) => {
            assert_eq!(stage::policy(&journal_path).unwrap(), durability);
            assert_eq!(stage::recording(&journal_path).unwrap(), recording);
            if let Some(marker) = marker {
                assert_eq!(
                    fs::read(journal_path.join("durability-v1")).unwrap(),
                    marker
                );
            } else {
                assert!(!journal_path.join("durability-v1").exists());
            }
            if recording == StageRecording::Off {
                for name in ["entries", "log-v2", "complete-v1"] {
                    assert!(!journal_path.join(name).exists());
                }
            } else {
                let observations = load_preimages(&journal_path).unwrap();
                let file = observations
                    .iter()
                    .find(|entry| entry.relative_path() == Path::new("file"))
                    .unwrap();
                assert_eq!(file.state, original);
            }
        }
    }
}

#[rstest::rstest]
#[case::no_journal(Policy::NoJournal)]
#[case::unmanaged(Policy::Unmanaged)]
#[case::checkpoint(Policy::Checkpoint)]
#[case::strict(Policy::Strict)]
#[case::ephemeral(Policy::Ephemeral)]
#[case::unrecorded(Policy::Unrecorded)]
fn valid_recording_admission_retains_defaults_and_observation_semantics(
    #[case] policy: Policy,
    #[values(Mode::Legacy, Mode::Compact, Mode::Existing)] mode: Mode,
    #[values(false, true)] with_work: bool,
) {
    assert_recording(mode, policy, with_work, b"staged result");
}

proptest! {
    #[test]
    fn generated_recording_admission_preserves_bytes_and_first_observations(
        mode in prop::sample::select(vec![Mode::Legacy, Mode::Compact, Mode::Existing]),
        policy in prop::sample::select(vec![
            Policy::NoJournal, Policy::Unmanaged, Policy::Checkpoint,
            Policy::Strict, Policy::Ephemeral, Policy::Unrecorded,
        ]),
        with_work in any::<bool>(),
        payload in prop::collection::vec(any::<u8>(), 0..129),
    ) {
        assert_recording(mode, policy, with_work, &payload);
    }
}

fn journal_inventory(root: &Path) -> BTreeMap<OsString, (u64, u32, Vec<u8>)> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let bytes = if metadata.file_type().is_symlink() {
                fs::read_link(entry.path())
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else if metadata.is_file() {
                fs::read(entry.path()).unwrap()
            } else {
                vec![]
            };
            (entry.file_name(), (metadata.ino(), metadata.mode(), bytes))
        })
        .collect()
}

#[rstest::rstest]
#[case::legacy(Mode::Legacy)]
#[case::compact(Mode::Compact)]
#[case::existing(Mode::Existing)]
fn invalid_recording_admission_precedes_backing_creation_and_preserves_metadata(
    #[case] mode: Mode,
    #[values("unknown", "directory", "symlink", "entries", "complete-v1", "log-v2")] fault: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    let work = root.path().join("work");
    let journal = root.path().join("journal");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    stage::begin_with_recording(&journal, StageDurability::Ephemeral, StageRecording::Off).unwrap();
    let policy = journal.join("durability-v1");
    let expected = match fault {
        "unknown" => {
            fs::write(&policy, b"unknown\n").unwrap();
            Some("unsupported stage durability policy")
        }
        "directory" => {
            fs::remove_file(&policy).unwrap();
            fs::create_dir(&policy).unwrap();
            Some("stage policy must be a regular file")
        }
        "symlink" => {
            fs::remove_file(&policy).unwrap();
            let outside = root.path().join("outside-policy");
            fs::write(&outside, b"pvisor.stage.ephemeral-unrecorded/1\n").unwrap();
            std::os::unix::fs::symlink(outside, &policy).unwrap();
            None
        }
        "entries" => {
            fs::create_dir(journal.join(fault)).unwrap();
            Some("unrecorded stage contains observation data")
        }
        "complete-v1" | "log-v2" => {
            fs::write(journal.join(fault), b"retained observation data").unwrap();
            Some("unrecorded stage contains observation data")
        }
        _ => unreachable!(),
    };
    let before = journal_inventory(&journal);
    let metadata = fs::symlink_metadata(&journal).unwrap();
    let identity = (metadata.ino(), metadata.mode());
    let error = mode
        .build(&lower, &upper, Some(&work), Some(&journal))
        .err()
        .unwrap();
    if let Some(expected) = expected {
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), expected);
    } else {
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(
            fs::read(root.path().join("outside-policy")).unwrap(),
            b"pvisor.stage.ephemeral-unrecorded/1\n"
        );
    }
    assert!(!upper.exists());
    assert!(!work.exists());
    assert_eq!(journal_inventory(&journal), before);
    let metadata = fs::symlink_metadata(&journal).unwrap();
    assert_eq!((metadata.ino(), metadata.mode()), identity);
    assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
}
