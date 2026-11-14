use std::{fs, path::Path};
use syn::{
    Visibility,
    visit::{self, Visit},
};

#[test]
fn only_api_is_exported_and_it_contains_declarations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = syn::parse_file(&fs::read_to_string(root.join("lib.rs")).unwrap()).unwrap();
    let modules: Vec<_> = lib
        .items
        .iter()
        .filter_map(|i| match i {
            syn::Item::Mod(m) if matches!(m.vis, Visibility::Public(_)) => {
                Some(m.ident.to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(modules, ["api"]);
    let api = syn::parse_file(&fs::read_to_string(root.join("api.rs")).unwrap()).unwrap();
    struct Check;
    impl<'ast> Visit<'ast> for Check {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            assert!(
                !attr.path().is_ident("cfg") && !attr.path().is_ident("cfg_attr"),
                "conditional public contract"
            );
            visit::visit_attribute(self, attr);
        }
        fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {
            panic!("API contains a function body");
        }
        fn visit_item_impl(&mut self, _: &'ast syn::ItemImpl) {
            panic!("API contains implementation");
        }
        fn visit_trait_item_fn(&mut self, m: &'ast syn::TraitItemFn) {
            assert!(m.default.is_none(), "API trait contains a default body");
        }
    }
    Check.visit_file(&api);
    fn inspect(dir: &Path) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                inspect(&path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().unwrap() != "api.rs"
            {
                struct Methods;
                impl<'ast> Visit<'ast> for Methods {
                    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
                        if item.trait_.is_none() {
                            for method in &item.items {
                                if let syn::ImplItem::Fn(m) = method {
                                    assert!(
                                        !matches!(m.vis, Visibility::Public(_)),
                                        "public inherent method {}",
                                        m.sig.ident
                                    );
                                }
                            }
                        }
                        visit::visit_item_impl(self, item);
                    }
                }
                Methods.visit_file(&syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap());
            }
        }
    }
    inspect(&root);
}
