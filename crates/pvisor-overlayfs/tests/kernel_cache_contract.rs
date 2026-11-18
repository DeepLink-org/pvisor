//! Portable, side-effect-free admission checks through the public API.
use proptest::prelude::*;
use pvisor_overlay_core::{FileAccessPolicy, LayerMutability};
use pvisor_overlayfs::api::{
    FsMetrics, KernelCacheConfig, KernelCachePolicy, OverlayConfiguration, OverlayFs,
    OverlayMountConfig, OverlayMounting, OwnedViewContract, ReadObservationSemantics,
};
use std::path::Path;
use std::time::Duration;

fn candidate(root: &Path) -> OverlayMountConfig {
    let mut config = OverlayMountConfig::new(
        vec![root.join("lower")],
        root.join("upper"),
        Some(root.join("work")),
        root.join("merged"),
    );
    config.backend = None;
    config.read_only = true;
    config.lower_mutability = vec![LayerMutability::Immutable];
    config.kernel_cache = KernelCacheConfig {
        policy: KernelCachePolicy::Metadata,
        ttl: Duration::from_secs(60),
        owned_view: Some(OwnedViewContract {
            exclusive_upper_and_work: true,
            fixed_metadata_and_aliases: true,
        }),
        read_observation: ReadObservationSemantics::StableView,
    };
    config
}

#[rstest::rstest]
fn default_is_disabled_without_ownership_or_cached_observation_optin() {
    let cache = KernelCacheConfig::default();
    assert_eq!(cache.policy, KernelCachePolicy::Disabled);
    assert_eq!(cache.ttl, Duration::from_secs(60));
    assert_eq!(cache.owned_view, None);
    assert_eq!(
        cache.read_observation,
        ReadObservationSemantics::RequestCallbacks
    );
    let root = tempfile::tempdir().unwrap();
    let fresh = OverlayMountConfig::new(
        vec![],
        root.path().join("upper"),
        None,
        root.path().join("merged"),
    );
    assert_eq!(fresh.kernel_cache, cache);
    let mut config = candidate(root.path());
    config.kernel_cache = cache;
    config.read_only = false;
    config.lower_mutability.clear();
    config.validate_kernel_cache().unwrap();
    config.kernel_cache.policy = KernelCachePolicy::Uncached;
    config.validate_kernel_cache().unwrap();
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

#[rstest::rstest]
fn stable_view_admission_is_linux_host_only_and_ttl_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    for policy in [
        KernelCachePolicy::Metadata,
        KernelCachePolicy::MetadataAndData,
    ] {
        for ttl in [Duration::from_nanos(1), Duration::from_secs(60)] {
            let mut config = candidate(root.path());
            config.kernel_cache.policy = policy;
            config.kernel_cache.ttl = ttl;
            let result = config.validate_kernel_cache();
            if cfg!(target_os = "linux") {
                result.unwrap();
            } else {
                assert!(result.unwrap_err().to_string().contains("Linux HOST"));
            }
        }
    }
    for ttl in [
        Duration::ZERO,
        Duration::from_secs(60) + Duration::from_nanos(1),
        Duration::MAX,
    ] {
        let mut config = candidate(root.path());
        config.kernel_cache.ttl = ttl;
        assert!(
            config
                .validate_kernel_cache()
                .unwrap_err()
                .to_string()
                .contains("TTL")
        );
    }
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

#[rstest::rstest]
fn each_missing_proof_and_observation_policy_is_explicitly_rejected_before_io() {
    let root = tempfile::tempdir().unwrap();
    let base = candidate(root.path());
    let mut cases = Vec::new();
    let mut c = base.clone();
    c.lower_mutability.clear();
    cases.push((c, "Immutable"));
    let mut c = base.clone();
    c.lower_mutability = vec![LayerMutability::Mutable];
    cases.push((c, "Immutable"));
    let mut c = base.clone();
    c.lower_mutability.push(LayerMutability::Immutable);
    cases.push((c, "Immutable"));
    let mut c = base.clone();
    c.kernel_cache.owned_view = None;
    cases.push((c, "owned-view contract"));
    let mut c = base.clone();
    c.kernel_cache
        .owned_view
        .as_mut()
        .unwrap()
        .exclusive_upper_and_work = false;
    cases.push((c, "exclusive upper/work"));
    let mut c = base.clone();
    c.kernel_cache
        .owned_view
        .as_mut()
        .unwrap()
        .fixed_metadata_and_aliases = false;
    cases.push((c, "backing atime"));
    let mut c = base.clone();
    c.kernel_cache.read_observation = ReadObservationSemantics::RequestCallbacks;
    cases.push((c, "StableView"));
    let mut c = base.clone();
    c.preimage_dir = Some(root.path().join("preimages"));
    cases.push((c, "first-content"));
    let mut c = base.clone();
    c.compact_preimages = true;
    cases.push((c, "first-content"));
    let mut c = base.clone();
    c.observation = Some(FsMetrics::default());
    cases.push((c, "read metrics"));
    for policy in [
        FileAccessPolicy::new(vec!["secret/**".into()], vec![]).unwrap(),
        FileAccessPolicy::new_with_ask(vec![], vec!["**".into()], vec![]).unwrap(),
        FileAccessPolicy::new(vec![], vec!["**".into()]).unwrap(),
        FileAccessPolicy::new_with_allow(vec![], vec![], vec![], vec!["public/**".into()]).unwrap(),
    ] {
        let mut c = base.clone();
        c.access_policy = policy;
        cases.push((c, "path access policy"));
    }
    let mut c = base.clone();
    c.excluded_paths.push("hidden".into());
    cases.push((c, "exclusions"));
    let mut c = base.clone();
    c.default_permissions = false;
    cases.push((c, "default_permissions"));
    let mut c = base.clone();
    c.allow_other = true;
    cases.push((c, "owner-only"));
    let mut c = base.clone();
    c.allow_root = true;
    cases.push((c, "owner-only"));
    for (config, expected) in cases {
        assert_rejected_before_io(root.path(), config, expected);
    }
}

#[rstest::rstest]
fn writable_and_macfuse_views_do_not_silently_downgrade() {
    let root = tempfile::tempdir().unwrap();
    let mut config = candidate(root.path());
    config.read_only = false;
    if cfg!(target_os = "linux") {
        config.validate_kernel_cache().unwrap();
    } else {
        assert!(
            config
                .validate_kernel_cache()
                .unwrap_err()
                .to_string()
                .contains("Linux HOST")
        );
    }
    config.kernel_cache.policy = KernelCachePolicy::MetadataAndData;
    let error = config.validate_kernel_cache().unwrap_err().to_string();
    assert!(error.contains(if cfg!(target_os = "linux") {
        "writable MetadataAndData"
    } else {
        "Linux HOST"
    }));
    config.read_only = true;
    for backend in ["kernel", "fskit"] {
        config.backend = Some(backend.into());
        let error = OverlayFs::mount(config.clone()).unwrap_err();
        assert!(error.to_string().contains("Linux HOST"));
    }
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

fn assert_rejected_before_io(root: &Path, config: OverlayMountConfig, expected: &str) {
    let before = format!("{config:?}");
    assert!(
        config
            .validate_kernel_cache()
            .unwrap_err()
            .to_string()
            .contains(expected)
    );
    assert_eq!(format!("{config:?}"), before);
    for foreground in [false, true] {
        let error = if foreground {
            OverlayFs::run_foreground(config.clone()).unwrap_err()
        } else {
            OverlayFs::mount(config.clone()).unwrap_err()
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
    assert!(std::fs::read_dir(root).unwrap().next().is_none());
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/kernel_cache_contract.txt")
        ))), ..ProptestConfig::default()
    })]

    #[test]
    fn valid_cache_inputs_respect_platform_and_writable_data_constraints(
        nanos in 1u64..=60_000_000_000, count in 1usize..9,
        data in any::<bool>(), read_only in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut config = candidate(root.path());
        config.lower_dirs = (0..count).map(|i| root.path().join(format!("lower-{i}"))).collect();
        config.lower_mutability = vec![LayerMutability::Immutable; count];
        config.kernel_cache.ttl = Duration::from_nanos(nanos);
        config.kernel_cache.policy = if data { KernelCachePolicy::MetadataAndData } else { KernelCachePolicy::Metadata };
        config.read_only = read_only;
        let before = format!("{config:?}");
        let result = config.validate_kernel_cache();
        prop_assert_eq!(result.is_ok(), cfg!(target_os = "linux") && (!data || read_only));
        if let Err(error) = result {
            let expected = if cfg!(target_os = "linux") { "writable MetadataAndData" } else { "Linux HOST" };
            prop_assert!(error.to_string().contains(expected));
        }
        prop_assert_eq!(format!("{config:?}"), before);
        prop_assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn invalid_ttls_always_reject_before_mount_preparation(
        ttl in prop_oneof![Just(Duration::ZERO), (60_000_000_001u64..=u64::MAX).prop_map(Duration::from_nanos)],
        data in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut config = candidate(root.path());
        config.kernel_cache.ttl = ttl;
        config.kernel_cache.policy = if data { KernelCachePolicy::MetadataAndData } else { KernelCachePolicy::Metadata };
        assert_rejected_before_io(root.path(), config, "TTL");
    }

    #[test]
    fn missing_cache_contracts_cannot_be_hidden_by_platform_rejection(
        fault in 0u8..14, nanos in 1u64..=60_000_000_000,
        count in 1usize..9, selected in any::<usize>(), name in "[a-z]{1,20}",
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut config = candidate(root.path());
        config.lower_dirs = (0..count).map(|i| root.path().join(format!("lower-{i}"))).collect();
        config.lower_mutability = vec![LayerMutability::Immutable; count];
        config.kernel_cache.ttl = Duration::from_nanos(nanos);
        let expected = match fault {
            0 => { config.kernel_cache.owned_view = None; "owned-view contract" }
            1 => { config.kernel_cache.owned_view.as_mut().unwrap().exclusive_upper_and_work = false; "exclusive upper/work" }
            2 => { config.kernel_cache.owned_view.as_mut().unwrap().fixed_metadata_and_aliases = false; "backing atime" }
            3 => { config.kernel_cache.read_observation = ReadObservationSemantics::RequestCallbacks; "StableView" }
            4 => { config.lower_mutability[selected % count] = LayerMutability::Mutable; "Immutable" }
            5 => { config.lower_mutability.clear(); "Immutable" }
            6 => { config.lower_mutability.push(LayerMutability::Immutable); "Immutable" }
            7 => { config.preimage_dir = Some(root.path().join(name)); "first-content" }
            8 => { config.compact_preimages = true; "first-content" }
            9 => { config.observation = Some(FsMetrics::default()); "read metrics" }
            10 => { config.excluded_paths.push(name.into()); "exclusions" }
            11 => { config.default_permissions = false; "default_permissions" }
            12 => { config.allow_other = true; "owner-only" }
            _ => { config.allow_root = true; "owner-only" }
        };
        assert_rejected_before_io(root.path(), config, expected);
    }

    #[test]
    fn disabled_and_uncached_validation_do_not_require_owned_view_proofs(
        uncached in any::<bool>(), seconds in any::<u64>(), read_only in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut config = candidate(root.path());
        config.kernel_cache.policy = if uncached { KernelCachePolicy::Uncached } else { KernelCachePolicy::Disabled };
        config.kernel_cache.ttl = Duration::from_secs(seconds);
        config.kernel_cache.owned_view = None;
        config.kernel_cache.read_observation = ReadObservationSemantics::RequestCallbacks;
        config.lower_mutability.clear();
        config.read_only = read_only;
        config.validate_kernel_cache().unwrap();
        prop_assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }
}
