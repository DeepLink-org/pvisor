//! Cross-platform API and implementation boundary guards. No hypervisor is required.
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
    fn control<
        T: VmControl
            + SnapshotControl
            + api::ColdRamControl
            + api::RamDedupControl
            + api::VcpuObservationControl,
    >() {
    }
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
    let _dedup_object_safe: Option<&dyn api::RamDedupControl> = None;
    let _observation_object_safe: Option<&dyn api::VcpuObservationControl> = None;
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
fn private_adapters_do_not_define_public_inherent_methods() {
    for source in [
        include_str!("../src/portable.rs"),
        include_str!("../src/windows/portable.rs"),
        include_str!("../src/windows/builder.rs"),
        include_str!("../src/transport.rs"),
        include_str!("../src/ram_delta.rs"),
        include_str!("../src/handle.rs"),
        include_str!("../src/memory.rs"),
        include_str!("../src/shared_kernel.rs"),
        include_str!("../src/ram_dedup.rs"),
        include_str!("../src/vcpu_observation.rs"),
        include_str!("../src/firmware_store.rs"),
        include_str!("../src/cold_ram.rs"),
        include_str!("../src/shared_ram_macos.rs"),
        include_str!("../src/cold_ram_scan.rs"),
        include_str!("../src/cold_ram_linux.rs"),
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
