//! Public contract checks without a host FUSE mount.
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

#[test]
fn construction_retains_paths_and_defaults_without_io() {
    let root = tempfile::tempdir().unwrap();
    let lowers = vec![root.path().join("top"), root.path().join("base")];
    let upper = root.path().join("upper");
    let work = root.path().join("work");
    let mountpoint = root.path().join("merged");
    let config = OverlayMountConfig::new(
        lowers.clone(),
        upper.clone(),
        Some(work.clone()),
        mountpoint.clone(),
    );
    assert_eq!(config.lower_dirs, lowers);
    assert_eq!(config.apply_target, lowers.last().cloned());
    assert_eq!(config.upper_dir, upper);
    assert_eq!(config.work_dir, Some(work));
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
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    let copy = config.clone();
    assert_eq!(copy.lower_dirs, config.lower_dirs);
    assert_eq!(copy.mountpoint, config.mountpoint);
}

#[test]
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

#[test]
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

#[test]
fn ordinary_directory_is_not_a_mountpoint() {
    let root = tempfile::tempdir().unwrap();
    let directory: PathBuf = root.path().join("ordinary");
    std::fs::create_dir(&directory).unwrap();
    let directory = std::fs::canonicalize(directory).unwrap();
    assert!(!OverlayFs::is_mountpoint(&directory));
}
