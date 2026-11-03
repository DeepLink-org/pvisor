//! Guard the runtime boundary: external consumers use the sole Rust facade.
use std::path::Path;
use syn::visit::{self, Visit};

const OLD_PACKAGES: &[&str] = &[
    "libkrun",
    "krun-vmm",
    "krun-devices",
    "krun-hvf",
    "krun-arch",
    "krun-arch-gen",
    "krun-cpuid",
    "krun-kernel",
    "krun-utils",
    "krun-polly",
    "krun-smbios",
];

fn inspect_manifest(value: &toml::Value) {
    if let toml::Value::Table(table) = value {
        for (key, value) in table {
            assert!(
                !OLD_PACKAGES.contains(&key.as_str()),
                "old runtime dependency {key}"
            );
            if let Some(package) = value.as_str().filter(|_| key == "package") {
                assert!(
                    !OLD_PACKAGES.contains(&package),
                    "aliased old runtime dependency {package}"
                );
            }
            inspect_manifest(value);
        }
    }
}

#[test]
fn workspace_has_no_old_runtime_dependency() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let manifest: toml::Value = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    inspect_manifest(&manifest);
    for member in manifest["workspace"]["members"].as_array().unwrap() {
        let manifest: toml::Value =
            std::fs::read_to_string(root.join(member.as_str().unwrap()).join("Cargo.toml"))
                .unwrap()
                .parse()
                .unwrap();
        inspect_manifest(&manifest);
    }
}

#[test]
fn retired_optional_devices_are_not_build_profiles() {
    let manifest: toml::Value = include_str!("../Cargo.toml").parse().unwrap();
    let features = manifest["features"].as_table().unwrap();
    for feature in ["gpu", "snd", "input", "virgl_resource_map2"] {
        assert!(!features.contains_key(feature), "retired feature {feature}");
    }
    let dependencies = manifest["dependencies"].as_table().unwrap();
    for dependency in [
        "krun_display",
        "krun_input",
        "pw",
        "rutabaga_gfx",
        "thiserror",
        "zerocopy",
    ] {
        assert!(
            !dependencies.contains_key(dependency),
            "retired device dependency {dependency}"
        );
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for device in ["gpu", "snd", "input"] {
        assert!(
            !root
                .join("src/devices/virtio")
                .join(device)
                .join("mod.rs")
                .exists(),
            "retired device implementation {device}"
        );
    }
    // These are product/validation profiles, not leftovers of the retired devices.
    for feature in ["net", "init-blob", "test_utils"] {
        assert!(features.contains_key(feature), "required feature {feature}");
    }
    assert!(root.join("src/windows/vmm/windows/vstate.rs").is_file());
}

#[test]
fn byte_order_uses_one_private_source_for_both_backends() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let assembly = std::fs::read_to_string(root.join("src/windows/utils/mod.rs")).unwrap();
    assert!(has_shared_byte_order_declaration(&assembly));
    assert!(root.join("src/utils/byte_order.rs").is_file());
    assert!(!root.join("src/windows/utils/byte_order.rs").exists());
}

fn has_shared_byte_order_declaration(assembly: &str) -> bool {
    let mut lines = assembly.lines();
    let mut previous = lines.next();
    for line in lines {
        if previous == Some("#[path = \"../../utils/byte_order.rs\"]")
            && line == "pub mod byte_order;"
        {
            return true;
        }
        previous = Some(line);
    }
    false
}

#[test]
fn byte_order_guard_preserves_paired_declaration_with_windows_line_endings() {
    for newline in ["\n", "\r\n"] {
        assert!(has_shared_byte_order_declaration(&format!(
            "#[path = \"../../utils/byte_order.rs\"]{newline}pub mod byte_order;{newline}"
        )));
        assert!(!has_shared_byte_order_declaration(&format!(
            "#[path = \"../../utils/byte_order.rs\"]{newline}pub mod other;{newline}pub mod byte_order;"
        )));
    }
}

struct CallGuard;
impl<'ast> Visit<'ast> for CallGuard {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(first) = path.segments.first() {
            let name = first.ident.to_string();
            assert!(
                !["krun", "libkrun", "krun_vmm", "krun_devices", "krun_hvf"]
                    .contains(&name.as_str()),
                "old runtime import/call: {name}"
            );
        }
        visit::visit_path(self, path);
    }
}
fn inspect_sources(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // Benchmark downloads contain third-party rootfs/toolchain sources,
            // which are not callers owned by this repository.
            let name = path.file_name().unwrap();
            if name != "pvisor-vm" && name != ".data" {
                inspect_sources(&path);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            CallGuard.visit_file(
                &syn::parse_file(&std::fs::read_to_string(&path).unwrap())
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
            );
        }
    }
}
#[test]
fn external_rust_callers_do_not_import_the_old_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    inspect_sources(&root.join("crates"));
    inspect_sources(&root.join("benchmark"));
    inspect_sources(&root.join("tools"));
}
