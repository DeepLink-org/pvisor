use syn::{Item, Visibility};

#[test]
fn only_api_is_public_and_implementations_have_no_public_inherent_methods() {
    let root = syn::parse_file(include_str!("../src/lib.rs")).unwrap();
    for item in root.items {
        match item {
            Item::Mod(module) if matches!(module.vis, Visibility::Public(_)) => {
                assert_eq!(module.ident, "api")
            }
            Item::Mod(_) => {}
            Item::Use(import) => assert!(matches!(import.vis, Visibility::Inherited)),
            _ => panic!("unexpected root API declaration"),
        }
    }
    let api = syn::parse_file(include_str!("../src/api.rs")).unwrap();
    struct NoConditionalApi;
    impl<'ast> syn::visit::Visit<'ast> for NoConditionalApi {
        fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
            assert!(
                !attribute.path().is_ident("cfg") && !attribute.path().is_ident("cfg_attr"),
                "conditional API declaration"
            );
        }
    }
    syn::visit::Visit::visit_file(&mut NoConditionalApi, &api);
    for item in api.items {
        assert!(
            !matches!(item, Item::Impl(_) | Item::Fn(_)),
            "API contains implementation"
        );
        if let Item::Trait(item) = item {
            for member in item.items {
                if let syn::TraitItem::Fn(method) = member {
                    assert!(method.default.is_none());
                }
            }
        }
    }
    let implementation = syn::parse_file(include_str!("../src/exec.rs")).unwrap();
    for item in implementation.items {
        if let Item::Struct(owner) = &item {
            for field in &owner.fields {
                assert!(matches!(field.vis, Visibility::Inherited));
            }
        }
        if let Item::Impl(item) = item
            && item.trait_.is_none()
        {
            for member in item.items {
                if let syn::ImplItem::Fn(method) = member {
                    assert!(!matches!(method.vis, Visibility::Public(_)));
                }
            }
        }
    }
}
