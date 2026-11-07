use pvisor_overlay_core::{
    OverlayCore, OverlayLayout, load_preimages,
    stage::{self, StageDurability},
};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

#[test]
fn managed_seal_publication_obstruction_retains_candidates_and_allows_owned_retry() {
    for durability in [StageDurability::Checkpoint, StageDurability::Strict] {
        let temporary = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = temporary.path();
        let lower = root.join("lower");
        let upper = root.join("upper");
        let work = root.join("work");
        let journal = root.join("preimages");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("kept"), b"host kept").unwrap();
        fs::write(lower.join("next"), b"host next").unwrap();
        stage::begin(&journal, durability).unwrap();
        let core = OverlayCore::new_for_layout_with_compact_preimages(
            OverlayLayout::new(vec![lower.clone()], lower.clone()).unwrap(),
            upper.clone(),
            Some(work.clone()),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        fs::write(core.copy_up(Path::new("kept")).unwrap(), b"staged kept").unwrap();
        core.sync_preimages().unwrap();
        // Release the writer before completion; reopen below takes ownership
        // anew rather than continuing to use a pre-completion writable handle.
        drop(core);
        let observations = load_preimages(&journal).unwrap();
        assert_eq!(observations.len(), 1);
        let policy = fs::read(journal.join("durability-v1")).unwrap();
        let log = fs::read(journal.join("log-v2")).unwrap();
        let candidate = fs::symlink_metadata(upper.join("kept")).unwrap();
        let identity = |metadata: &fs::Metadata| {
            (
                metadata.dev(),
                metadata.ino(),
                metadata.uid(),
                metadata.gid(),
            )
        };
        let upper_identity = identity(&fs::symlink_metadata(&upper).unwrap());
        let work_identity = identity(&fs::symlink_metadata(&work).unwrap());
        let reopen = || {
            OverlayCore::open_existing(
                vec![lower.clone()],
                upper.clone(),
                Some(work.clone()),
                vec![],
                Some(journal.clone()),
            )
        };
        let seal = journal.join("sealed-v1");
        fs::create_dir(&seal).unwrap();
        fs::write(seal.join("owned-sentinel"), b"obstruction").unwrap();
        let names = || {
            let mut names = fs::read_dir(&journal)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let original_names = names();
        for _ in 0..2 {
            // This directory can be drained normally. The failure must be the
            // final atomic marker replacement, not an earlier journal error.
            stage::sync_journal(&journal).unwrap();
            let failure = stage::complete(&upper, &journal).unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains(&format!("replace {}", seal.display())),
                "unexpected failure before seal publication: {failure}"
            );
            assert!(stage::require_sealed(&journal).is_err());
            assert!(stage::begin(&journal, durability).is_err());
            // An invalid seal blocks writable owner construction, not just
            // trusted-result/reuse admission. No mutation handle is returned.
            assert!(reopen().is_err());
            assert_eq!(fs::read(upper.join("kept")).unwrap(), b"staged kept");
            assert_eq!(
                identity(&fs::symlink_metadata(upper.join("kept")).unwrap()),
                identity(&candidate)
            );
            assert_eq!(
                identity(&fs::symlink_metadata(&upper).unwrap()),
                upper_identity
            );
            assert_eq!(
                identity(&fs::symlink_metadata(&work).unwrap()),
                work_identity
            );
            assert!(!upper.join("next").exists());
            assert_eq!(fs::read(lower.join("kept")).unwrap(), b"host kept");
            assert_eq!(fs::read(lower.join("next")).unwrap(), b"host next");
            assert_eq!(
                fs::read(seal.join("owned-sentinel")).unwrap(),
                b"obstruction"
            );
            assert_eq!(fs::read(journal.join("durability-v1")).unwrap(), policy);
            assert_eq!(fs::read(journal.join("log-v2")).unwrap(), log);
            assert_eq!(load_preimages(&journal).unwrap(), observations);
            assert_eq!(
                names(),
                original_names,
                "publication leaked temporary files"
            );
            assert!(!journal.join("completed-volatile-v1").exists());
        }
        fs::remove_file(seal.join("owned-sentinel")).unwrap();
        fs::remove_dir(&seal).unwrap();
        // Removing the obstruction does not invent completion or admit reuse.
        assert!(stage::require_sealed(&journal).is_err());
        assert!(stage::begin(&journal, durability).is_err());
        let core = reopen().unwrap();
        // The recovered compact-log owner still excludes competing writers.
        assert_eq!(
            reopen().err().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
        fs::write(core.copy_up(Path::new("next")).unwrap(), b"staged next").unwrap();
        core.sync_preimages().unwrap();
        drop(core);
        let recovered_observations = load_preimages(&journal).unwrap();
        assert_eq!(recovered_observations.len(), 2);
        assert!(recovered_observations.contains(&observations[0]));
        stage::complete(&upper, &journal).unwrap();
        stage::require_sealed(&journal).unwrap();
        assert_eq!(fs::read(&seal).unwrap(), b"pvisor.stage.sealed/1\n");
        let core = reopen().unwrap();
        for name in ["kept", "next"] {
            assert_eq!(
                core.copy_up(Path::new(name)).unwrap_err().raw_os_error(),
                Some(libc::EROFS)
            );
        }
        assert_eq!(
            core.remove(Path::new("kept"), false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        drop(core);
        // Only explicit same-policy reuse clears a valid completion marker.
        stage::begin(&journal, durability).unwrap();
        assert!(!seal.exists());
        let core = reopen().unwrap();
        assert_eq!(
            fs::read(core.copy_up(Path::new("kept")).unwrap()).unwrap(),
            b"staged kept"
        );
        assert_eq!(fs::read(upper.join("next")).unwrap(), b"staged next");
        assert_eq!(fs::read(lower.join("kept")).unwrap(), b"host kept");
        assert_eq!(fs::read(lower.join("next")).unwrap(), b"host next");
        assert_eq!(load_preimages(&journal).unwrap(), recovered_observations);
    }
}
