use pvisor_overlay_core::{
    FileAccessPolicy, LayerMutability, OverlayCore, OverlayLayout, profile::Profile,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn core(lowers: Vec<PathBuf>, upper: PathBuf, declarations: Vec<LayerMutability>) -> OverlayCore {
    let layout = OverlayLayout::new(lowers.clone(), lowers.last().unwrap().clone())
        .unwrap()
        .with_lower_mutability(declarations)
        .unwrap();
    OverlayCore::new_for_layout(layout, upper, None, vec![], None)
        .unwrap()
        .with_profile(Profile::enabled("lower-cache-test"))
}

fn units(core: &OverlayCore, name: &str) -> u64 {
    core.profile_report()
        .unwrap()
        .measurements
        .get(name)
        .map_or(0, |m| m.units)
}

#[test]
fn declarations_default_mutable_and_validate_order_length_and_serde() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    let layout = OverlayLayout::new(vec![lower.clone()], lower.clone())
        .unwrap()
        .with_lower_mutability(vec![])
        .unwrap();
    assert_eq!(layout.lower_mutability(), &[LayerMutability::Mutable]);
    assert_eq!(LayerMutability::default(), LayerMutability::Mutable);
    assert_eq!(
        OverlayLayout::new(vec![lower.clone()], lower)
            .unwrap()
            .with_lower_mutability(vec![LayerMutability::Immutable; 2])
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        serde_json::to_string(&LayerMutability::Immutable).unwrap(),
        "\"immutable\""
    );
    assert_eq!(
        serde_json::from_str::<LayerMutability>("\"mutable\"").unwrap(),
        LayerMutability::Mutable
    );
}

#[test]
fn live_external_metadata_namespace_and_parent_replacement_remain_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir_all(lower.join("a/b")).unwrap();
    fs::write(lower.join("a/b/file"), b"old").unwrap();
    let core = core(vec![lower.clone()], tmp.path().join("upper"), vec![]);
    let path = Path::new("a/b/file");
    let before = core.metadata_for_backing_lookup(path).unwrap();
    fs::rename(lower.join("a"), lower.join("retired")).unwrap();
    fs::create_dir_all(lower.join("a/b")).unwrap();
    fs::write(lower.join(path), b"replacement").unwrap();
    let after = core.metadata_for_backing_lookup(path).unwrap();
    assert_ne!(before.parents, after.parents);
    assert_eq!(after.entry.metadata.len(), 11);
    fs::remove_file(lower.join(path)).unwrap();
    assert!(core.resolve_checked(path).unwrap().is_none());
    fs::write(lower.join(path), b"new").unwrap();
    assert_eq!(core.metadata(path).unwrap().len(), 3);
    assert_eq!(units(&core, "immutable_lower_cache_hits"), 0);
}

#[test]
fn mixed_precedence_upper_rename_copyup_whiteout_and_opaque_stay_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("live");
    let stable = tmp.path().join("stable");
    let upper = tmp.path().join("upper");
    for root in [&live, &stable] {
        fs::create_dir_all(root.join("a/b")).unwrap();
    }
    fs::write(stable.join("a/b/file"), b"stable").unwrap();
    let core = core(
        vec![live.clone(), stable.clone()],
        upper.clone(),
        vec![LayerMutability::Mutable, LayerMutability::Immutable],
    );
    let path = Path::new("a/b/file");
    let first = core.metadata_for_backing_lookup(path).unwrap();
    let second = core.metadata_for_backing_lookup(path).unwrap();
    assert_eq!(first.parents, second.parents);
    assert_eq!(second.entry.layer, 2);
    assert!(units(&core, "immutable_lower_cache_hits") > 0);
    fs::write(live.join(path), b"live winner").unwrap();
    assert_eq!(core.metadata_resolved(path).unwrap().layer, 1);
    fs::remove_file(live.join(path)).unwrap();
    assert_eq!(core.metadata_resolved(path).unwrap().layer, 2);
    core.copy_up(path).unwrap();
    assert_eq!(fs::read(upper.join(path)).unwrap(), b"stable");
    fs::write(upper.join(path), b"upper winner").unwrap();
    assert_eq!(core.metadata(path).unwrap().len(), 12);
    core.rename(path, Path::new("a/b/renamed"), false).unwrap();
    assert!(core.resolve_checked(path).unwrap().is_none());
    assert!(
        core.metadata_resolved(Path::new("a/b/renamed"))
            .unwrap()
            .resolved
            .is_upper
    );
    core.remove(Path::new("a/b/renamed"), false).unwrap();
    assert!(
        core.resolve_checked(Path::new("a/b/renamed"))
            .unwrap()
            .is_none()
    );
    // External upper creation and deletion must also override a warm lower.
    fs::remove_file(upper.join("a/b/.wh.file")).unwrap();
    assert_eq!(core.metadata_resolved(path).unwrap().layer, 2);
    fs::write(upper.join(path), b"created").unwrap();
    assert!(core.metadata_resolved(path).unwrap().resolved.is_upper);
    fs::remove_file(upper.join(path)).unwrap();
    fs::write(upper.join("a/b/.wh..wh..opq"), b"").unwrap();
    assert!(core.resolve_checked(path).unwrap().is_none());
    fs::remove_file(upper.join("a/b/.wh..wh..opq")).unwrap();
    assert_eq!(core.metadata_resolved(path).unwrap().layer, 2);
}

#[test]
fn warm_metadata_does_not_bypass_denied_hardlink_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("secret"), b"secret").unwrap();
    fs::hard_link(lower.join("secret"), lower.join("alias")).unwrap();
    let core = core(
        vec![lower],
        tmp.path().join("upper"),
        vec![LayerMutability::Immutable],
    );
    core.metadata(Path::new("alias")).unwrap();
    let core =
        core.with_access_policy(&FileAccessPolicy::new(vec!["secret".into()], vec![]).unwrap());
    for _ in 0..2 {
        assert_eq!(
            core.metadata(Path::new("alias"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }
    assert!(units(&core, "immutable_lower_cache_hits") > 0);
}

#[test]
fn cache_is_bounded_and_eviction_preserves_results() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    for i in 0..4100 {
        fs::write(lower.join(format!("file-{i}")), b"stable").unwrap();
    }
    let core = core(
        vec![lower],
        tmp.path().join("upper"),
        vec![LayerMutability::Immutable],
    );
    for i in 0..4100 {
        assert_eq!(
            core.metadata(Path::new(&format!("file-{i}")))
                .unwrap()
                .len(),
            6
        );
    }
    assert!(units(&core, "immutable_lower_cache_evictions") > 0);
    assert_eq!(core.metadata(Path::new("file-0")).unwrap().len(), 6);
}

#[test]
fn immutable_cache_hits_do_not_skip_live_baseline_read_observations() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    let upper = tmp.path().join("upper");
    let journal = tmp.path().join("journal");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"stable").unwrap();
    let expected = pvisor_overlay_core::fingerprint_at(&lower, Path::new("file")).unwrap();
    let layout = OverlayLayout::new(vec![lower.clone()], lower)
        .unwrap()
        .with_lower_mutability(vec![LayerMutability::Immutable])
        .unwrap();
    let core = OverlayCore::new_for_layout(layout, upper, None, vec![], Some(journal.clone()))
        .unwrap()
        .with_profile(Profile::enabled("read-observation"));
    core.metadata(Path::new("file")).unwrap();
    assert!(
        pvisor_overlay_core::load_preimages(&journal)
            .unwrap()
            .is_empty()
    );
    for _ in 0..2 {
        core.prepare_file_read(Path::new("file")).unwrap();
    }
    let observations = pvisor_overlay_core::load_preimages(&journal).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].state, expected);
    assert!(units(&core, "immutable_lower_cache_hits") > 0);
}

#[test]
fn frozen_baseline_does_not_infer_physical_immutability() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    let baseline = tmp.path().join("baseline");
    for root in [&target, &baseline] {
        fs::create_dir(root).unwrap();
    }
    fs::write(baseline.join("file"), b"initial").unwrap();
    let layout =
        OverlayLayout::with_baseline(vec![baseline.clone()], target, Some(&baseline)).unwrap();
    assert_eq!(layout.lower_mutability(), &[LayerMutability::Mutable]);
    let core = OverlayCore::new_for_layout(layout, tmp.path().join("upper"), None, vec![], None)
        .unwrap()
        .with_profile(Profile::enabled("frozen"));
    core.metadata(Path::new("file")).unwrap();
    fs::write(baseline.join("file"), b"externally changed").unwrap();
    assert_eq!(core.metadata(Path::new("file")).unwrap().len(), 18);
    assert_eq!(units(&core, "immutable_lower_cache_hits"), 0);
}

#[test]
fn diagnostic_disable_keeps_immutable_contract_but_performs_fresh_stats() {
    const CHILD: &str = "PVISOR_LOWER_CACHE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostic_disable_keeps_immutable_contract_but_performs_fresh_stats",
            ])
            .env(CHILD, "1")
            .env("PVISOR_DISABLE_IMMUTABLE_LOWER_CACHE", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"stable").unwrap();
    let core = core(
        vec![lower],
        tmp.path().join("upper"),
        vec![LayerMutability::Immutable],
    );
    for _ in 0..2 {
        assert_eq!(core.metadata(Path::new("file")).unwrap().len(), 6);
    }
    assert_eq!(units(&core, "immutable_lower_cache_hits"), 0);
    assert_eq!(units(&core, "immutable_lower_cache_misses"), 0);
    assert_eq!(units(&core, "layer_leaf_stats"), 4);
}
