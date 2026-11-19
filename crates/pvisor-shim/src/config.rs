//! Node-level shim defaults, re-read at every Create like the injection
//! catalogue: editing the file applies to the next task without a restart.
//! An absent file keeps every default; a present-but-invalid file fails
//! Create so a typo never silently widens or narrows a node's posture.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Default config location; `PVISOR_SHIM_CONFIG` overrides it (tests, staging).
const DEFAULT_CONFIG_PATH: &str = "/etc/pvisor-shim/config.json";

fn config_path() -> PathBuf {
    std::env::var_os("PVISOR_SHIM_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
}

/// Node-wide settings; all sections optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct NodeConfig {
    /// VM-task defaults (feature `vm`).
    pub(crate) vm: VmNodeConfig,
}

/// VM-task node defaults. `default_network` uses the annotation's value
/// syntax (`off`, `public`, `allowlist=<entries>`) and only applies where
/// the pod sets no `io.pvisor.vm.network` annotation; `auto_expose` lets
/// the runner expose guest TCP listeners in the pod network namespace as
/// the control channel reports them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct VmNodeConfig {
    pub(crate) default_network: Option<String>,
    pub(crate) auto_expose: bool,
    pub(crate) expose_budget: usize,
}

impl Default for VmNodeConfig {
    /// The exposure budget keeps its documented default both for a missing
    /// file and for a `vm` object without `expose_budget`.
    fn default() -> Self {
        Self {
            default_network: None,
            auto_expose: false,
            expose_budget: default_expose_budget(),
        }
    }
}

/// Exposure proxies per task before further listeners are skipped (logged).
fn default_expose_budget() -> usize {
    64
}

/// Read the node config. A missing file is the all-defaults config; any
/// other read or parse failure names the file and fails the Create.
pub(crate) fn load() -> Result<NodeConfig, String> {
    load_from(&config_path())
}

fn load_from(path: &Path) -> Result<NodeConfig, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NodeConfig::default());
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let config: NodeConfig = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if config.vm.expose_budget == 0 {
        return Err(format!(
            "parse {}: vm.expose_budget must be nonzero",
            path.display()
        ));
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(contents: &str) -> PathBuf {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(&path, contents).unwrap();
        // Keep the file after the handle drops: load_from only needs the path.
        directory.keep();
        path
    }

    #[test]
    fn missing_file_is_all_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let config = load_from(&directory.path().join("absent.json")).unwrap();
        assert_eq!(config.vm.default_network, None);
        assert!(!config.vm.auto_expose);
        assert_eq!(config.vm.expose_budget, 64);
    }

    #[test]
    fn full_config_parses_with_documented_fields() {
        let path = temporary_path(
            r#"{"vm":{"default_network":"public","auto_expose":true,"expose_budget":16}}"#,
        );
        let config = load_from(&path).unwrap();
        assert_eq!(config.vm.default_network.as_deref(), Some("public"));
        assert!(config.vm.auto_expose);
        assert_eq!(config.vm.expose_budget, 16);
    }

    #[test]
    fn invalid_json_unknown_fields_and_zero_budget_fail_closed() {
        let not_json = temporary_path("not json");
        assert!(load_from(&not_json).unwrap_err().contains("parse"));

        let unknown = temporary_path(r#"{"vm":{},"unexpected":1}"#);
        assert!(load_from(&unknown).unwrap_err().contains("unexpected"));

        let zero = temporary_path(r#"{"vm":{"expose_budget":0}}"#);
        assert!(load_from(&zero).unwrap_err().contains("expose_budget"));
    }
}
