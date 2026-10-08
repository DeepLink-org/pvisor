//! Execute the TOML examples and check documented raw defaults against serde.
use pvisor::{OverlayFsSettings, RunConfig};

fn toml_examples(page: &str) -> impl Iterator<Item = &str> {
    page.split("```toml\n")
        .skip(1)
        .map(|part| part.split_once("\n```").expect("closed TOML fence").0)
}

#[test]
fn documented_configuration_examples_deserialize() {
    let page = include_str!("../../../docs/src/en/reference/config.md");
    let mut count = 0;
    for example in toml_examples(page) {
        let config: RunConfig = toml::from_str(example).expect("documented RunConfig TOML");
        assert!(!config.run.command.is_empty(), "example supplies a command");
        count += 1;
    }
    assert!(count >= 1);
}

#[test]
fn documented_policy_examples_deserialize() {
    let page = include_str!("../../../docs/src/en/reference/policy.md");
    let mut count = 0;
    for example in toml_examples(page) {
        if example.contains("[policies.") {
            let _: RunConfig = toml::from_str(example).expect("scoped RunConfig policy");
        } else {
            let _: pvisor_core::PolicyLayer = toml::from_str(example).expect("policy file");
        }
        count += 1;
    }
    assert!(count >= 1);
}

#[test]
fn documented_invalid_fields_and_file_paths_are_rejected() {
    assert!(toml::from_str::<RunConfig>("[filesystem]\nmode = 'sandbox'").is_err());
    assert!(toml::from_str::<RunConfig>("[vm]\nunknown = 1").is_err());
    for path in ["/secrets/**", "../secrets/**", "a//b", "a/./b"] {
        let source = format!("[filesystem]\ndeny = ['{path}']");
        assert!(
            toml::from_str::<pvisor_core::PolicyLayer>(&source).is_err(),
            "{path}"
        );
    }
}

#[test]
fn documented_raw_defaults_match_serialization() {
    let page = include_str!("../../../docs/src/en/reference/config.md");
    let table = page
        .split("<!-- config-fields:start -->")
        .nth(1)
        .unwrap()
        .split("<!-- config-fields:end -->")
        .next()
        .unwrap();
    let raw = serde_json::to_value(RunConfig::default()).unwrap();
    let overlay = serde_json::to_value(OverlayFsSettings::default()).unwrap();
    let mut checked = 0;
    for row in table.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<_> = row.split('|').map(|s| s.trim().trim_matches('`')).collect();
        let path = cells[1];
        let default = cells[3];
        // Array-entry shapes and scoped shared shapes have no entry in an empty config.
        if path.contains("[]")
            || path.starts_with("network_")
            || path.starts_with("bandwidth_")
            || path.starts_with("policy_layer.")
        {
            continue;
        }
        let (value, lookup) = if let Some(rest) = path.strip_prefix("overlayfs.") {
            (&overlay, rest)
        } else {
            (&raw, path)
        };
        let pointer = format!("/{}", lookup.replace('.', "/"));
        let actual = value.pointer(&pointer);
        if default == "unset" {
            assert!(
                actual.is_none_or(serde_json::Value::is_null),
                "{path}: {actual:?}"
            );
        } else if default == "{}" {
            assert!(actual.is_some_and(serde_json::Value::is_object), "{path}");
        } else {
            let expected: serde_json::Value = serde_json::from_str(default).unwrap();
            assert_eq!(actual, Some(&expected), "{path}");
        }
        checked += 1;
    }
    assert!(checked >= 60, "checked {checked} defaults");
}
