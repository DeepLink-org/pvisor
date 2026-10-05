//! Guard the repository migration: external consumers use the sole Rust facade.
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
