//! Typed VM launch inputs and the existing RunSpec wire-format adapter.
#![deny(missing_docs)]
use pvisor_core::RunSpec;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Host-selected VM process inputs, validated before driver preparation.
/// Defaults preserve the ordinary guest launch. `apply_to` writes the existing
/// RunSpec metadata encoding, so retained Sessions and JSON callers keep their
/// wire format. Executors consume the admitted typed value through ExecutionContext.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmExecutionInputs {
    /// Absolute host workspace target; None installs no target override.
    pub overlay_target: Option<PathBuf>,
    /// Absolute Linux guest working directory; None selects `/`.
    pub guest_cwd: Option<PathBuf>,
    /// Environment names removed from inherited/image defaults. Explicit process
    /// environment still takes precedence. Names must be nonempty, without `=`/NUL.
    pub env_unset: Vec<String>,
    /// Optional attached guest terminal/process contract.
    pub stage_stdio: Option<VmStageStdio>,
}

/// Attached guest process IO. Omission preserves ordinary Session IO.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VmStageStdio {
    /// Retain pipe input when true; false supplies EOF to the workload.
    pub interactive: bool,
    /// Terminal rows and columns, both nonzero. None uses ordinary pipes.
    pub tty: Option<[u16; 2]>,
}
impl VmExecutionInputs {
    fn validate(&self) -> anyhow::Result<()> {
        if let Some(path) = &self.overlay_target {
            anyhow::ensure!(path.is_absolute(), "VM overlay_target must be absolute");
        }
        if let Some(path) = &self.guest_cwd {
            anyhow::ensure!(
                path.as_os_str().as_encoded_bytes().starts_with(b"/"),
                "VM guest_cwd must be an absolute Linux path"
            );
        }
        for path in self.overlay_target.iter().chain(self.guest_cwd.iter()) {
            anyhow::ensure!(
                !path.as_os_str().as_encoded_bytes().contains(&0),
                "VM launch path contains NUL"
            );
            anyhow::ensure!(
                !path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir),
                "VM launch path contains parent traversal"
            );
        }
        anyhow::ensure!(
            self.env_unset
                .iter()
                .all(|name| !name.is_empty() && !name.contains(['=', '\0'])),
            "invalid VM env_unset name"
        );
        anyhow::ensure!(
            self.stage_stdio
                .as_ref()
                .is_none_or(|s| s.tty.is_none_or(|size| !size.contains(&0))),
            "VM terminal dimensions must be nonzero"
        );
        Ok(())
    }
    /// Replace this complete group of VM launch inputs in a RunSpec. Validates
    /// and serializes before changing it; failure leaves the request unchanged.
    /// None/empty fields remove earlier overrides. This performs no execution or IO.
    pub fn apply_to(&self, spec: &mut RunSpec) -> anyhow::Result<()> {
        self.validate()?;
        let values = [
            (
                "pvisor.vm.overlay_target",
                self.overlay_target
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?,
            ),
            (
                "pvisor.vm.guest_cwd",
                self.guest_cwd
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?,
            ),
            (
                "pvisor.vm.env_unset",
                if self.env_unset.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(&self.env_unset)?)
                },
            ),
            (
                "pvisor.vm.stage_stdio",
                self.stage_stdio
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?,
            ),
        ];
        for (key, value) in values {
            match value {
                Some(value) => {
                    spec.metadata.insert(key.into(), value);
                }
                None => {
                    spec.metadata.remove(key);
                }
            }
        }
        Ok(())
    }
    pub(crate) fn from_spec(spec: &RunSpec) -> anyhow::Result<Self> {
        fn decode<T: serde::de::DeserializeOwned>(
            spec: &RunSpec,
            key: &str,
        ) -> anyhow::Result<Option<T>> {
            use anyhow::Context;
            spec.metadata
                .get(key)
                .map(|value| {
                    serde_json::from_value(value.clone()).with_context(|| format!("invalid {key}"))
                })
                .transpose()
        }
        let inputs = Self {
            overlay_target: decode(spec, "pvisor.vm.overlay_target")?,
            guest_cwd: decode(spec, "pvisor.vm.guest_cwd")?,
            env_unset: decode(spec, "pvisor.vm.env_unset")?.unwrap_or_default(),
            stage_stdio: decode(spec, "pvisor.vm.stage_stdio")?,
        };
        inputs.validate()?;
        Ok(inputs)
    }
}

/// Runtime-prepared overlay geometry for a guest filesystem adapter.
/// Borrowed from ExecutionContext; these paths do not grant ownership past the
/// executor's `execute` future. The executor must stop all writers before returning.
/// Runtime teardown retains responsibility for sealing, review/apply and cleanup.
/// Inode/handle tables and protocol state remain owned by the executor's adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestOverlay {
    pub(crate) lowers: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) lower_mutability: Vec<pvisor_core::overlay::LayerMutability>,
    #[serde(default)]
    pub(crate) apply_target: Option<PathBuf>,
    #[serde(default)]
    pub(crate) baseline_lower: Option<PathBuf>,
    pub(crate) upper: PathBuf,
    pub(crate) work: Option<PathBuf>,
    #[serde(default)]
    pub(crate) preimages: Option<PathBuf>,
    #[serde(default)]
    pub(crate) excluded: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) access_policy: pvisor_core::overlay::FileAccessPolicy,
}
impl GuestOverlay {
    /// Ordered lower layers. Mutability declarations have matching order.
    pub fn lowers(&self) -> &[PathBuf] {
        &self.lowers
    }
    /// Declared stability for each lower, not a physical snapshot guarantee.
    pub fn lower_mutability(&self) -> &[pvisor_core::overlay::LayerMutability] {
        &self.lower_mutability
    }
    /// Writable upper owned by the current Execution.
    pub fn upper(&self) -> &Path {
        &self.upper
    }
    /// Optional adapter work directory, owned by the current Execution.
    pub fn work(&self) -> Option<&Path> {
        self.work.as_deref()
    }
    /// Original review/apply destination, not a writable guest share.
    pub fn apply_target(&self) -> Option<&Path> {
        self.apply_target.as_deref()
    }
    /// Optional separate read baseline for conflict detection.
    pub fn baseline_lower(&self) -> Option<&Path> {
        self.baseline_lower.as_deref()
    }
    /// Preimage/observation journal path for the shared filesystem service.
    pub fn preimages(&self) -> Option<&Path> {
        self.preimages.as_deref()
    }
    /// Relative paths hidden from this guest view.
    pub fn excluded(&self) -> &[PathBuf] {
        &self.excluded
    }
    /// Effective policy to install in the guest filesystem service.
    pub fn access_policy(&self) -> &pvisor_core::overlay::FileAccessPolicy {
        &self.access_policy
    }
}
