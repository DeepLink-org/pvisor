//! OCI bundle loading helpers.
//!
//! The shim consumes `config.json` from the bundle directory containerd
//! prepares. `Spec::load` keeps the JSON canonical, so parsing errors carry
//! the file path context here once instead of at every call site.

use std::path::Path;

use anyhow::{Context, Result};
use oci_spec::runtime::Spec;

/// Annotation prefix reserved for pVisor policy steering inside bundles.
pub const ANNOTATION_PREFIX: &str = "io.pvisor.";

/// Load and lightly validate the OCI runtime spec of a bundle directory.
pub fn load_bundle_spec(bundle: &Path) -> Result<Spec> {
    let config = bundle.join("config.json");
    let spec = Spec::load(&config)
        .with_context(|| format!("failed to load OCI config {}", config.display()))?;
    Ok(spec)
}

/// Read one `io.pvisor.*` annotation (key given without the prefix).
pub fn pvisor_annotation<'a>(spec: &'a Spec, key: &str) -> Option<&'a str> {
    let annotations = spec.annotations().as_ref()?;
    annotations
        .get(&format!("{ANNOTATION_PREFIX}{key}"))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_from_json(json: &str) -> Spec {
        serde_json::from_str(json).expect("parse spec")
    }

    #[test]
    fn annotation_lookup_requires_the_pvisor_prefix() {
        let spec = spec_from_json(
            r#"{"ociVersion":"1.0.2","annotations":{"io.pvisor.executor":"vm","other":"x"}}"#,
        );
        assert_eq!(pvisor_annotation(&spec, "executor"), Some("vm"));
        assert_eq!(pvisor_annotation(&spec, "missing"), None);
    }

    #[test]
    fn missing_annotations_table_yields_none() {
        let spec = spec_from_json(r#"{"ociVersion":"1.0.2"}"#);
        assert!(pvisor_annotation(&spec, "executor").is_none());
    }

    #[test]
    fn load_reports_the_config_path_on_error() {
        let error = load_bundle_spec(Path::new("/nonexistent-bundle"))
            .expect_err("missing bundle must fail");
        assert!(error.to_string().contains("config.json"));
    }
}
