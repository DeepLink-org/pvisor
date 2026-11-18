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
            assert!(
                !matches!(item.vis, Visibility::Public(_)),
                "declare replay data contracts in api.rs"
            );
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
        fn visit_visibility(&mut self, vis: &'ast Visibility) {
            assert!(
                !matches!(vis, Visibility::Public(_)),
                "declare public contracts in api.rs; use crate visibility internally"
            );
        }
    }
    fn scan(dir: &Path, root: &Path) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, root);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path != root.join("api.rs")
                && path != root.join("lib.rs")
            {
                Private.visit_file(&syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    scan(&root, &root);
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("api.rs"), "pub struct Contract;").unwrap();
    fs::write(fixture.path().join("lib.rs"), "pub mod api;").unwrap();
    scan(fixture.path(), fixture.path());
    let nested = fixture.path().join("nested");
    fs::create_dir(&nested).unwrap();
    for name in ["api.rs", "lib.rs", "new_adapter.rs"] {
        let path = nested.join(name);
        fs::write(&path, "impl Owner { pub fn bypass() {} }").unwrap();
        assert!(
            std::panic::catch_unwind(|| scan(fixture.path(), fixture.path())).is_err(),
            "guard missed {}",
            path.display()
        );
        fs::remove_file(path).unwrap();
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
            "AgentProfile",
            vec!["as_str", "supported_version", "profile"],
        ),
        ("ReplayConfiguration", vec!["from_file", "into_request"]),
        ("ReplayRequest", vec!["from_json", "execute"]),
        (
            "ReplayErrorClassification",
            vec!["category", "exit_code", "retryable"],
        ),
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
    assert_eq!(
        constants,
        ["REQUEST_SCHEMA_VERSION", "RESULT_SCHEMA_VERSION"]
    );
}
