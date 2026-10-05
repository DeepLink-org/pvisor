//! Architectural guards and ownership checks; no hypervisor is started.
use pvisor_vm::api::RestoreState;
use pvisor_vm::api::{
    self, SnapshotControl, VmBuilder, VmConfig, VmConfiguration, VmControl, VmRuntime,
};
use syn::visit::{self, Visit};

#[test]
fn api_is_unconditional_and_contains_no_implementation() {
    struct Guard;
    impl<'ast> Visit<'ast> for Guard {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(
                !attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"),
                "API shape must not depend on a target/feature"
            );
            visit::visit_attribute(self, attr);
        }
        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("API implementations must live in private adapters")
        }
        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("API function bodies must live in private adapters")
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            assert!(
                item.default.is_none(),
                "API traits declare contracts, not default implementations"
            );
            visit::visit_trait_item_fn(self, item);
        }
    }
    Guard.visit_file(&syn::parse_file(include_str!("../src/api.rs")).unwrap());
    let modules = syn::parse_file(include_str!("../src/runtime_modules.rs")).unwrap();
    let public = modules
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(m) if matches!(m.vis, syn::Visibility::Public(_)) => {
                Some(m.ident.to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(public, ["api"]);
}

#[test]
fn concrete_owners_implement_the_declared_contracts() {
    fn config<T: VmConfiguration + VmRuntime>() {}
    fn control<T: VmControl + SnapshotControl + api::ColdRamControl>() {}
    fn frozen<T: api::SnapshotCapture + api::FrozenMemory>() {}
    fn snapshot<T: api::SnapshotState>() {}
    fn restore<T: api::RestoreState>() {}
    fn ram<T: api::RamAccess>() {}
    config::<VmBuilder>();
    control::<api::VmmHandle>();
    frozen::<api::FrozenMachine<'_>>();
    snapshot::<api::MachineSnapshot>();
    restore::<api::MachineRestore>();
    ram::<api::RamBlock>();
    fn platform<T: api::RuntimeSupport>() {}
    platform::<api::VmPlatform>();
    let _object_safe: Option<&dyn VmControl> = None;
    assert!(VmBuilder::from_config(VmConfig {
        cpus: 0,
        memory_mib: 128
    })
    .is_err());
    assert!(VmBuilder::from_config(VmConfig {
        cpus: 1,
        memory_mib: 0
    })
    .is_err());
}

#[test]
fn invalid_attachment_does_not_poison_configuration() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = VmBuilder::new(1, 128).unwrap();
    vm.filesystem("/dev/root", root.path(), 0).unwrap();
    assert!(vm.disable_implicit_init().is_err());
    assert!(vm.filesystem("/dev/root", root.path(), 0).is_err());
    assert!(vm
        .virtual_file("/dev/root", "/../escape", vec![1], 0o400, false)
        .is_err());
    vm.virtual_file("/dev/root", "/config", vec![1, 2, 3], 0o400, false)
        .unwrap();
    assert!(vm
        .virtual_file("/dev/root", "/config", vec![4], 0o400, false)
        .is_err());
    vm.virtual_file("/dev/root", "/another", vec![5], 0o400, false)
        .unwrap();
}

#[test]
fn snapshot_ram_shape_is_portable_and_mapping_is_private() {
    use std::sync::Arc;
    use vm_memory::{Bytes, GuestAddress};
    let file = tempfile::tempfile().unwrap();
    // macOS host pages are 16 KiB, Linux usually uses 4 KiB.
    file.set_len(65536).unwrap();
    let state = serde_json::from_value(serde_json::json!({
        "version":1,"cpus":[],"devices":[],
        "ram":[{"base":0,"len":65536,"file_offset":0}]
    }))
    .unwrap();
    let restore = api::MachineRestore {
        state,
        ram_file: Arc::new(file),
    };
    let first = restore.map_ram(&[(GuestAddress(0), 65536)]).unwrap();
    let second = restore.map_ram(&[(GuestAddress(0), 65536)]).unwrap();
    first.write_slice(b"private", GuestAddress(0)).unwrap();
    let mut bytes = [1; 7];
    second.read_slice(&mut bytes, GuestAddress(0)).unwrap();
    assert_eq!(bytes, [0; 7]);
    assert!(restore.map_ram(&[(GuestAddress(4096), 65536)]).is_err());
    assert!(restore.validate(1).is_err()); // RAM mapping is not full-machine validation.
}

#[test]
fn private_adapters_do_not_define_public_inherent_methods() {
    for source in [
        include_str!("../src/portable.rs"),
        include_str!("../src/handle.rs"),
        include_str!("../src/memory.rs"),
        include_str!("../src/firmware_store.rs"),
        include_str!("../src/cold_ram.rs"),
        include_str!("../src/ram_file.rs"),
    ] {
        for item in syn::parse_file(source).unwrap().items {
            if let syn::Item::Impl(implementation) = item {
                if implementation.trait_.is_none() {
                    for member in implementation.items {
                        if let syn::ImplItem::Fn(method) = member {
                            assert!(
                                !matches!(method.vis, syn::Visibility::Public(_)),
                                "public methods must be declared in api.rs traits: {}",
                                method.sig.ident
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_overlay_path_does_not_poison_the_builder() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    std::fs::create_dir_all(&lower).unwrap();
    std::fs::create_dir_all(&upper).unwrap();
    let mut builder = VmBuilder::new(1, 64).unwrap();
    let mut overlay = api::OverlayConfig {
        lower_dirs: vec![lower],
        upper_dir: upper,
        work_dir: None,
        preimage_dir: None,
        apply_target: None,
        baseline_lower: None,
        baseline_content_index: None,
        excluded_paths: vec![],
        access_policy: Default::default(),
        semantics: api::PermissionSemantics::LinuxComplete,
    };
    let valid = overlay.clone();
    overlay
        .excluded_paths
        .push(std::ffi::OsString::from_vec(vec![0xff]).into());
    assert_eq!(
        builder.overlay("workspace", overlay, 0).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    let mut invalid_index = valid.clone();
    invalid_index.baseline_content_index = Some(api::BaselineContentIndex {
        root: std::ffi::OsString::from_vec(vec![0xff]).into(),
        file: root.path().join("index"),
        sha256: "00".repeat(32),
    });
    assert_eq!(
        builder
            .overlay("workspace", invalid_index, 0)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    builder.overlay("workspace", valid, 0).unwrap();
}
