use std::{fs, path::Path};
use syn::{
    Visibility,
    visit::{self, Visit},
};

#[test]
fn only_api_is_public_and_declarations_are_uniform() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = syn::parse_file(&fs::read_to_string(root.join("lib.rs")).unwrap()).unwrap();
    assert!(lib.attrs.iter().any(|a| a.path().is_ident("deny")
        && a.meta.require_list().unwrap().tokens.to_string() == "missing_docs"));
    let mut public = Vec::new();
    for item in lib.items {
        let syn::Item::Mod(module) = item else {
            panic!("lib.rs must contain only module declarations")
        };
        assert!(
            module.content.is_none(),
            "implementation belongs in a private file"
        );
        if matches!(module.vis, Visibility::Public(_)) {
            assert!(module.attrs.is_empty(), "public API must be unconditional");
            public.push(module.ident.to_string());
        }
    }
    assert_eq!(public, ["api"]);
    struct Declarations;
    impl<'ast> Visit<'ast> for Declarations {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(
                !attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"),
                "keep platform selection in implementations"
            );
            visit::visit_attribute(self, attr);
        }
        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("move API implementations into private modules");
        }
        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("declare behavior in an API trait");
        }
        fn visit_trait_item_fn(&mut self, method: &'ast syn::TraitItemFn) {
            assert!(
                method.default.is_none(),
                "API traits must not have default bodies"
            );
            visit::visit_trait_item_fn(self, method);
        }
        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
            panic!("API must not expose nested implementation modules");
        }
        fn visit_macro(&mut self, _: &'ast syn::Macro) {
            panic!("API declarations must not be hidden in macros");
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            if matches!(item.vis, Visibility::Public(_)) {
                let mut path = Vec::new();
                let mut tree = &item.tree;
                while let syn::UseTree::Path(item) = tree {
                    path.push(item.ident.to_string());
                    tree = &item.tree;
                }
                let syn::UseTree::Name(item) = tree else {
                    panic!("explicit owner required")
                };
                path.push(item.ident.to_string());
                let path = path.join("::");
                assert!(
                    matches!(
                        path.as_str(),
                        "crate::daemon::Daemon"
                            | "crate::runtime::NativeRuntime"
                            | "crate::image_repository::PreparedCatalog"
                    ),
                    "only explicit opaque owners may be re-exported"
                );
            }
            visit::visit_item_use(self, item);
        }
    }
    Declarations
        .visit_file(&syn::parse_file(&fs::read_to_string(root.join("api.rs")).unwrap()).unwrap());
}

#[test]
fn private_sources_cannot_add_public_paths_or_inherent_methods() {
    struct Private;
    impl<'ast> Visit<'ast> for Private {
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            if matches!(
                item.ident.to_string().as_str(),
                "Daemon" | "NativeRuntime" | "PreparedCatalog"
            ) {
                assert!(matches!(item.vis, Visibility::Public(_)));
                assert!(
                    item.fields
                        .iter()
                        .all(|field| matches!(field.vis, Visibility::Inherited)),
                    "opaque owner state must remain private"
                );
            } else {
                visit::visit_item_struct(self, item);
            }
        }
        fn visit_visibility(&mut self, vis: &'ast Visibility) {
            assert!(
                !matches!(vis, Visibility::Public(_)),
                "declare public contracts in api.rs; use crate visibility internally"
            );
        }
    }
    fn scan(path: &Path) {
        let file = syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap();
        Private.visit_file(&file);
        fn modules(items: &[syn::Item], directory: &Path) {
            for item in items {
                if let syn::Item::Mod(module) = item {
                    let child = directory.join(module.ident.to_string());
                    if let Some((_, items)) = &module.content {
                        modules(items, &child);
                    } else {
                        let file = child.with_extension("rs");
                        scan(&if file.exists() {
                            file
                        } else {
                            child.join("mod.rs")
                        });
                    }
                }
            }
        }
        let directory = if path.file_name().unwrap() == "mod.rs" {
            path.parent().unwrap().to_path_buf()
        } else {
            path.with_extension("")
        };
        modules(&file.items, &directory);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = syn::parse_file(&fs::read_to_string(root.join("lib.rs")).unwrap()).unwrap();
    for item in lib.items {
        if let syn::Item::Mod(module) = item
            && module.ident != "api"
        {
            let file = root.join(format!("{}.rs", module.ident));
            scan(&if file.exists() {
                file
            } else {
                root.join(module.ident.to_string()).join("mod.rs")
            });
        }
    }
}

#[test]
fn public_behavior_has_one_canonical_surface() {
    use std::collections::BTreeMap;
    let api = syn::parse_file(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api.rs")).unwrap(),
    )
    .unwrap();
    let mut traits = BTreeMap::new();
    let mut constants = Vec::new();
    for item in &api.items {
        match item {
            syn::Item::Trait(item) => {
                let methods = item
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        syn::TraitItem::Fn(method) => Some(method.sig.ident.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                traits.insert(item.ident.to_string(), methods);
            }
            syn::Item::Const(item) => constants.push(item.ident.to_string()),
            _ => {}
        }
    }
    let expected = BTreeMap::from([
        (
            "PreparedCatalogService",
            vec![
                "new",
                "list",
                "inspect",
                "push",
                "pull",
                "prepare",
                "registries",
                "update_registries",
            ],
        ),
        ("RuntimeValidation", vec!["validate"]),
        (
            "Runtime",
            vec![
                "preflight",
                "create",
                "inspect",
                "pause",
                "resume",
                "delete",
                "endpoint",
                "e2b_template",
            ],
        ),
        (
            "DaemonService",
            vec![
                "open",
                "open_native",
                "stop_warm_pool",
                "router",
                "start_maintenance",
            ],
        ),
        (
            "NativeRuntimeService",
            vec!["new", "with_memory_pool", "run_supervisor"],
        ),
        ("MemoryPoolService", vec!["ensure_service", "run"]),
        ("PressureService", vec!["serve", "exchange"]),
    ])
    .into_iter()
    .map(|(name, methods)| {
        (
            name.to_owned(),
            methods.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        )
    })
    .collect::<BTreeMap<_, _>>();
    assert_eq!(
        traits, expected,
        "new behavior needs one owning contract, not a duplicate entry"
    );
    assert_eq!(constants, ["OPENSANDBOX_VERSION", "OPENSANDBOX_COMMIT"]);
}

#[test]
fn callers_use_declared_api_items_even_in_platform_specific_sources() {
    use std::collections::BTreeSet;
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let api = syn::parse_file(&fs::read_to_string(manifest.join("src/api.rs")).unwrap()).unwrap();
    let names: BTreeSet<String> = api
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Enum(item) => Some(item.ident.to_string()),
            syn::Item::Trait(item) => Some(item.ident.to_string()),
            syn::Item::Const(item) => Some(item.ident.to_string()),
            syn::Item::Use(item) if matches!(item.vis, Visibility::Public(_)) => {
                let mut tree = &item.tree;
                while let syn::UseTree::Path(path) = tree {
                    tree = &path.tree;
                }
                let syn::UseTree::Name(name) = tree else {
                    panic!("explicit owner required")
                };
                Some(name.ident.to_string())
            }
            _ => None,
        })
        .collect();
    struct Caller<'a> {
        names: &'a BTreeSet<String>,
        file: &'a Path,
    }
    impl Caller<'_> {
        fn check(&self, path: &[String]) {
            if path.first().is_some_and(|name| name == "pvisor_daemon") {
                assert_eq!(
                    path.get(1).map(String::as_str),
                    Some("api"),
                    "{}: use api",
                    self.file.display()
                );
                if let Some(name) = path.get(2) {
                    assert!(
                        self.names.contains(name),
                        "{}: undeclared API item {name}",
                        self.file.display()
                    );
                }
            }
        }
        fn imports(&self, tree: &syn::UseTree, mut path: Vec<String>) {
            match tree {
                syn::UseTree::Path(item) => {
                    path.push(item.ident.to_string());
                    self.imports(&item.tree, path);
                }
                syn::UseTree::Group(group) => {
                    for item in &group.items {
                        self.imports(item, path.clone());
                    }
                }
                syn::UseTree::Name(item) => {
                    if item.ident != "self" {
                        path.push(item.ident.to_string());
                    }
                    self.check(&path);
                }
                syn::UseTree::Rename(item) => {
                    if item.ident != "self" {
                        path.push(item.ident.to_string());
                    }
                    self.check(&path);
                }
                syn::UseTree::Glob(_) => {
                    path.push("*".into());
                    self.check(&path);
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Caller<'_> {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.check(
                &path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>(),
            );
            visit::visit_path(self, path);
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            self.imports(&item.tree, Vec::new());
        }
    }
    fn scan(dir: &Path, names: &BTreeSet<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, names);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                if source.contains("pvisor_daemon::") {
                    Caller { names, file: &path }.visit_file(&syn::parse_file(&source).unwrap());
                }
            }
        }
    }
    scan(manifest.parent().unwrap(), &names);
}
