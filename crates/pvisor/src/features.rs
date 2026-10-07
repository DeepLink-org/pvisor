//! Explicit runtime experiments; independent of Cargo build features.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Feature {
    #[serde(rename = "workload-aware-memory-offloading")]
    WorkloadAwareMemoryOffloading,
}

#[derive(Debug, Serialize)]
pub struct FeatureDefinition {
    pub name: &'static str,
    pub stage: &'static str,
    pub default: bool,
    pub description: &'static str,
    #[serde(skip)]
    pub feature: Feature,
}

pub const REGISTRY: &[FeatureDefinition] = &[FeatureDefinition {
    name: "workload-aware-memory-offloading",
    stage: "experimental",
    default: false,
    description: "EXP-001 M0: native VM vCPU wait observation only; no automatic offload",
    feature: Feature::WorkloadAwareMemoryOffloading,
}];

impl std::str::FromStr for Feature {
    type Err = String;
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        REGISTRY
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.feature)
            .ok_or_else(|| format!("unknown feature '{name}'; use `pvisor feature list`"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureSettings {
    #[serde(rename = "workload-aware-memory-offloading")]
    pub workload_aware_memory_offloading: bool,
}

impl Default for FeatureSettings {
    fn default() -> Self {
        Self {
            workload_aware_memory_offloading: REGISTRY[0].default,
        }
    }
}

impl FeatureSettings {
    pub fn enable(&mut self, feature: Feature) {
        match feature {
            Feature::WorkloadAwareMemoryOffloading => self.workload_aware_memory_offloading = true,
        }
    }
    pub fn enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::WorkloadAwareMemoryOffloading => self.workload_aware_memory_offloading,
        }
    }
    pub fn validate(&self, executor: crate::RunExecutorKind) -> anyhow::Result<()> {
        if self.workload_aware_memory_offloading {
            anyhow::ensure!(
                executor == crate::RunExecutorKind::Vm,
                "feature workload-aware-memory-offloading requires --executor vm (or run.executor = 'vm')"
            );
            anyhow::ensure!(
                cfg!(any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64")
                )),
                "feature workload-aware-memory-offloading is unsupported by this build; requires Linux x86_64/KVM or Apple Silicon macOS/HVF"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_is_strict_default_off_and_roundtrips() {
        let default: crate::RunConfig = toml::from_str("").unwrap();
        assert!(!default.features.workload_aware_memory_offloading);
        let config: crate::RunConfig =
            toml::from_str("[features]\nworkload-aware-memory-offloading = true").unwrap();
        assert!(config.features.workload_aware_memory_offloading);
        let wire = serde_json::to_value(&config).unwrap();
        assert_eq!(wire["features"]["workload-aware-memory-offloading"], true);
        let roundtrip: crate::RunConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(roundtrip.features, config.features);
        assert!(toml::from_str::<crate::RunConfig>("[features]\nunknown = false").is_err());
        assert!(
            toml::from_str::<crate::RunConfig>(
                "[features]\nworkload-aware-memory-offloading = 'true'"
            )
            .is_err()
        );
        assert!(
            "unknown"
                .parse::<Feature>()
                .unwrap_err()
                .contains("unknown feature")
        );
    }
    #[test]
    fn old_feature_name_is_rejected_without_an_alias() {
        assert!("vm-vcpu-observe".parse::<Feature>().is_err());
        assert!(serde_json::from_str::<Feature>("\"vm-vcpu-observe\"").is_err());
        for value in ["false", "true"] {
            let error = toml::from_str::<crate::RunConfig>(&format!(
                "[features]\nvm-vcpu-observe = {value}"
            ))
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unknown field `vm-vcpu-observe`")
            );
        }
        assert!(
            serde_json::from_value::<crate::RunConfig>(serde_json::json!({
                "features": { "vm-vcpu-observe": false }
            }))
            .is_err()
        );
    }

    #[test]
    fn vm_only_is_not_silently_ignored() {
        let settings = FeatureSettings {
            workload_aware_memory_offloading: true,
        };
        for executor in [
            crate::RunExecutorKind::Host,
            crate::RunExecutorKind::Container,
        ] {
            assert!(
                settings
                    .validate(executor)
                    .unwrap_err()
                    .to_string()
                    .contains("requires --executor vm")
            );
        }
        assert_eq!(
            settings.validate(crate::RunExecutorKind::Vm).is_ok(),
            cfg!(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))
        );
    }
}
