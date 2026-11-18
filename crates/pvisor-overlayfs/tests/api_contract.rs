//! Public contract checks without a host FUSE mount.
use proptest::prelude::*;
use pvisor_overlayfs::api::{
    FilesystemMetrics, FsMetrics, OverlayConfiguration, OverlayFs, OverlayMountConfig,
    OverlayMounting, OverlaySession, OverlaySessionControl,
};
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

#[test]
fn api_is_unconditional_and_declarations_only() {
    struct Guard;
    impl<'ast> Visit<'ast> for Guard {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(!attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"));
            visit::visit_attribute(self, attr);
        }
        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("API implementations belong in private adapters");
        }
        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("API function bodies belong in private adapters");
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            assert!(item.default.is_none(), "no default trait implementations");
            visit::visit_trait_item_fn(self, item);
        }
        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
            panic!("API must not expose nested implementation modules");
        }
        fn visit_item_macro(&mut self, _: &'ast syn::ItemMacro) {
            panic!("API declarations must not hide implementations in macros");
        }
    }
    Guard.visit_file(&syn::parse_file(include_str!("../src/api.rs")).unwrap());
    let root = syn::parse_file(include_str!("../src/lib.rs")).unwrap();
    let mut public_modules = Vec::new();
    for item in root.items {
        match item {
            syn::Item::Mod(module) => {
                if matches!(module.vis, syn::Visibility::Public(_)) {
                    public_modules.push(module.ident.to_string());
                }
            }
            _ => panic!("lib.rs must contain only module declarations"),
        }
    }
    assert_eq!(public_modules, ["api"]);
}

#[test]
fn private_adapters_do_not_add_public_inherent_methods_or_modules() {
    struct Guard;
    impl<'ast> Visit<'ast> for Guard {
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if item.trait_.is_none() {
                for member in &item.items {
                    if let syn::ImplItem::Fn(method) = member {
                        assert!(
                            !matches!(method.vis, syn::Visibility::Public(_)),
                            "public method must be declared in an API trait: {}",
                            method.sig.ident
                        );
                    }
                }
            }
            visit::visit_item_impl(self, item);
        }
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            assert!(!matches!(item.vis, syn::Visibility::Public(_)));
            visit::visit_item_mod(self, item);
        }
    }
    for source in [
        include_str!("../src/fs.rs"),
        include_str!("../src/mount.rs"),
        include_str!("../src/observation.rs"),
        include_str!("../src/cache.rs"),
        include_str!("../src/dispatch.rs"),
    ] {
        Guard.visit_file(&syn::parse_file(source).unwrap());
    }
}

#[test]
fn owners_implement_public_contracts() {
    fn configuration<T: OverlayConfiguration>() {}
    fn mounting<T: OverlayMounting>() {}
    fn session<T: OverlaySessionControl>() {}
    fn metrics<T: FilesystemMetrics + Clone + Default + Send + Sync>() {}
    configuration::<OverlayMountConfig>();
    mounting::<OverlayFs>();
    session::<OverlaySession>();
    metrics::<FsMetrics>();
    let _: fn(OverlayMountConfig) -> anyhow::Result<OverlaySession> = OverlayFs::mount;
    let _: fn(OverlayMountConfig) -> anyhow::Result<()> = OverlayFs::run_foreground;
    let _: fn(&Path) -> bool = OverlayFs::is_mountpoint;
    let _: fn(&OverlaySession) -> &Path = OverlaySession::mountpoint;
    let _: fn(&OverlaySession) -> bool = OverlaySession::has_exited;
    let _: fn(OverlaySession) -> anyhow::Result<()> = OverlaySession::unmount;
    let _: &dyn FilesystemMetrics = &FsMetrics::default();
}

#[rstest::rstest]
fn construction_retains_paths_and_defaults_without_io() {
    let root = tempfile::tempdir().unwrap();
    assert_construction(
        vec![root.path().join("top"), root.path().join("base")],
        root.path().join("upper"),
        Some(root.path().join("work")),
        root.path().join("merged"),
    );
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
}

#[rstest::rstest]
fn invalid_lower_mutability_fails_before_preparation() {
    let root = tempfile::tempdir().unwrap();
    let mut config = OverlayMountConfig::new(
        vec![root.path().join("lower")],
        root.path().join("upper"),
        None,
        root.path().join("merged"),
    );
    config.backend = None;
    config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable; 2];
    for foreground in [false, true] {
        let error = if foreground {
            OverlayFs::run_foreground(config.clone()).unwrap_err()
        } else {
            OverlayFs::mount(config.clone()).unwrap_err()
        };
        assert!(
            error
                .to_string()
                .contains("lower mutability length mismatch")
        );
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }
}

#[rstest::rstest]
fn invalid_input_fails_before_fuse_mounting() {
    let root = tempfile::tempdir().unwrap();
    let mut config = OverlayMountConfig::new(
        vec![],
        root.path().join("upper"),
        None,
        root.path().join("merged"),
    );
    // Skip the macOS installation probe, so this test requires neither macFUSE
    // nor a device. Validation still rejects the empty lower list first.
    config.backend = None;
    for foreground in [false, true] {
        let error = if foreground {
            OverlayFs::run_foreground(config.clone()).unwrap_err()
        } else {
            OverlayFs::mount(config.clone()).unwrap_err()
        };
        assert_eq!(error.to_string(), "lowerdir must list at least one path");
    }
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());

    let lower = root.path().join("lower");
    std::fs::create_dir(&lower).unwrap();
    let mut overlap = OverlayMountConfig::new(
        vec![lower.clone()],
        lower.clone(),
        None,
        root.path().join("merged"),
    );
    overlap.backend = None;
    let error = OverlayFs::mount(overlap).unwrap_err();
    assert!(error.to_string().contains("lowerdir must not overlap"));
    // Preparation is intentionally not transactional.
    assert!(root.path().join("merged").is_dir());
}

#[rstest::rstest]
fn default_metrics_snapshots_are_independent_owned_records() {
    let metrics = FsMetrics::default();
    let shared = metrics.clone();
    let mut snapshot = metrics.snapshot();
    assert!(snapshot.paths.is_empty() && snapshot.rules.is_empty());
    assert_eq!(snapshot.overflow_hits, 0);
    snapshot.overflow_hits = 123;
    assert_eq!(shared.snapshot().overflow_hits, 0);
    let thread = std::thread::spawn(move || shared.snapshot());
    assert_eq!(thread.join().unwrap().overflow_hits, 0);
    assert_eq!(FsMetrics::default().snapshot().overflow_hits, 0);
}

#[rstest::rstest]
fn ordinary_directory_is_not_a_mountpoint() {
    let root = tempfile::tempdir().unwrap();
    let directory: PathBuf = root.path().join("ordinary");
    std::fs::create_dir(&directory).unwrap();
    let directory = std::fs::canonicalize(directory).unwrap();
    assert!(!OverlayFs::is_mountpoint(&directory));
}

fn assert_construction(
    lowers: Vec<PathBuf>,
    upper: PathBuf,
    work: Option<PathBuf>,
    mountpoint: PathBuf,
) {
    let config = OverlayMountConfig::new(
        lowers.clone(),
        upper.clone(),
        work.clone(),
        mountpoint.clone(),
    );
    assert_eq!(config.lower_dirs, lowers);
    assert_eq!(
        config.lower_mutability,
        vec![pvisor_overlay_core::LayerMutability::Immutable; lowers.len()]
    );
    assert_eq!(config.apply_target, lowers.last().cloned());
    assert_eq!(config.upper_dir, upper);
    assert_eq!(config.work_dir, work);
    assert_eq!(config.mountpoint, mountpoint);
    assert!(config.default_permissions);
    assert!(!config.allow_other && !config.allow_root && !config.read_only && !config.debug);
    assert_eq!(config.fsname, "pvisor-overlayfs");
    assert_eq!(
        config.backend.as_deref(),
        if cfg!(target_os = "macos") {
            Some("fskit")
        } else {
            None
        }
    );
    assert!(config.baseline_lower.is_none() && config.preimage_dir.is_none());
    assert!(!config.compact_preimages && config.excluded_paths.is_empty());
    assert!(config.observation.is_none());
    let copy = config.clone();
    assert_eq!(copy.lower_dirs, config.lower_dirs);
    assert_eq!(copy.mountpoint, config.mountpoint);
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/api_contract.txt")
        ))), ..ProptestConfig::default()
    })]

    #[test]
    fn generated_paths_keep_order_duplicates_defaults_and_optional_work(
        names in prop::collection::vec("[A-Za-z0-9 _.-]{1,24}", 0..9), work in any::<bool>(), duplicate in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut lowers: Vec<_> = names.iter().map(|name| root.path().join(name)).collect();
        if duplicate && !lowers.is_empty() { lowers.push(lowers[0].clone()); }
        assert_construction(lowers, root.path().join("upper"), work.then(|| root.path().join("work")), root.path().join("merged"));
        prop_assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

    #[test]
    fn generated_declaration_mismatches_fail_before_preparation(
        count in 1usize..9, excess in 1usize..9, shorter in any::<bool>(), foreground in any::<bool>(),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut config = OverlayMountConfig::new((0..count).map(|i| root.path().join(format!("lower-{i}"))).collect(),
            root.path().join("upper"), None, root.path().join("merged"));
        config.backend = None;
        let declarations = if shorter && count > 1 { 1 + excess % (count - 1) } else { count + excess };
        config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable; declarations];
        let error = if foreground { OverlayFs::run_foreground(config).unwrap_err() }
                    else { OverlayFs::mount(config).unwrap_err() };
        prop_assert!(error.to_string().contains("lower mutability length mismatch"));
        prop_assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    }

}
