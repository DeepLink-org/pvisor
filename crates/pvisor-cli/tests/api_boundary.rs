//! Structural guards run on every source, including inactive platform code.
use std::{fs, path::Path};
use syn::{
    Visibility,
    visit::{self, Visit},
};

fn requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
            use syn::parse::Parser;
            let terms = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .unwrap();
            if list.path.is_ident("all") {
                terms.iter().any(requires_test)
            } else {
                !terms.is_empty() && terms.iter().all(requires_test)
            }
        }
        _ => false,
    }
}

#[test]
fn production_guard_only_excludes_test_only_conditions() {
    for (condition, expected) in [
        ("test", true),
        ("all(test, windows)", true),
        ("any(test, all(test, unix))", true),
        ("any(test, unix)", false),
        ("not(test)", false),
        ("all(unix, not(test))", false),
    ] {
        assert_eq!(
            requires_test(&syn::parse_str(condition).unwrap()),
            expected,
            "{condition}"
        );
    }
}

#[test]
fn only_api_is_public() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let file = syn::parse_file(&fs::read_to_string(root.join("lib.rs")).unwrap()).unwrap();
    let public: Vec<_> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => {
                assert!(module.content.is_none());
                if matches!(module.vis, Visibility::Public(_)) {
                    assert!(module.attrs.is_empty(), "API must be unconditional");
                    Some(module.ident.to_string())
                } else {
                    None
                }
            }
            _ => panic!("lib.rs must contain only module declarations"),
        })
        .collect();
    assert_eq!(public, ["api"]);
}

#[test]
fn api_has_only_uniform_documented_declarations() {
    struct Api;
    impl<'ast> Visit<'ast> for Api {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(!attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"));
            visit::visit_attribute(self, attr);
        }

        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("API implementations belong in private modules");
        }

        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("declare behavior in API traits");
        }

        fn visit_trait_item_fn(&mut self, method: &'ast syn::TraitItemFn) {
            assert!(method.default.is_none());
            visit::visit_trait_item_fn(self, method);
        }

        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
            panic!("API must not expose implementation modules");
        }

        fn visit_macro(&mut self, _: &'ast syn::Macro) {
            panic!("API declarations must not be hidden in macros");
        }

        fn visit_item_use(&mut self, _: &'ast syn::ItemUse) {
            panic!("CLI contracts must be declared in api.rs");
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let file = syn::parse_file(&fs::read_to_string(root.join("api.rs")).unwrap()).unwrap();
    Api.visit_file(&file);
}

#[test]
fn implementations_cannot_introduce_public_methods_or_exports() {
    struct Private;
    impl<'ast> Visit<'ast> for Private {
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            assert!(!matches!(item.vis, Visibility::Public(_)));
            visit::visit_item_fn(self, item);
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            assert!(!matches!(item.vis, Visibility::Public(_)));
            visit::visit_impl_item_fn(self, item);
        }

        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            assert!(!matches!(item.vis, Visibility::Public(_)));
            visit::visit_item_mod(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            assert!(!matches!(item.vis, Visibility::Public(_)));
            visit::visit_item_use(self, item);
        }
    }
    fn scan(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().unwrap() != "api.rs"
                && path.file_name().unwrap() != "lib.rs"
            {
                Private.visit_file(&syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap());
            }
        }
    }
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}

#[test]
fn production_sources_do_not_unwrap_results_or_options() {
    struct Production;
    impl<'ast> Visit<'ast> for Production {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let attrs = match item {
                syn::Item::Mod(item) => &item.attrs,
                syn::Item::Fn(item) => &item.attrs,
                _ => return visit::visit_item(self, item),
            };
            if attrs.iter().any(|attr| {
                attr.path().is_ident("cfg") && requires_test(&attr.parse_args().unwrap())
            }) {
                return;
            }
            visit::visit_item(self, item);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            assert!(
                call.method != "unwrap" && call.method != "expect",
                "production {} must use typed state or propagate an error",
                call.method
            );
            visit::visit_expr_method_call(self, call);
        }
    }
    fn scan(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                Production.visit_file(
                    &syn::parse_file(&fs::read_to_string(&path).unwrap())
                        .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
                );
            }
        }
    }
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}
