use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Optional in-process FUSE overlay root for one Attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayHint {
    /// None preserves the configured persistence policy.
    pub durability: Option<pvisor_core::overlay::StageDurability>,
    pub access_policy: pvisor_core::overlay::FileAccessPolicy,
    /// Shared read-only lower layers (host paths).
    pub lower_dirs: Vec<PathBuf>,
    /// Explicit physical-lower lifetime stability promises in `lower_dirs` order.
    /// Empty means all mutable. Caller owns proof and backing lifetime; a read-only
    /// mount, image digest or frozen baseline alone is insufficient. Runtime
    /// rejects nonempty declarations unless the final normalized stack matches.
    pub lower_mutability: Vec<pvisor_overlay_core::LayerMutability>,
    /// Durable staging root containing upper storage and the merged mount.
    pub stage_dir: Option<PathBuf>,
    /// Writable upper directory for this Attempt.
    pub upper_dir: Option<PathBuf>,
    /// Work directory required by overlay implementations.
    pub work_dir: Option<PathBuf>,
    /// Merged mount point visible to the Agent as cwd/root when set.
    pub merged_dir: Option<PathBuf>,
    /// Apply staged changes when the Run exits successfully or unsuccessfully.
    pub auto_apply: bool,
    /// Discard staged changes when the Run exits.
    pub auto_discard: bool,
    /// Reject apply so an immutable image/cache lower cannot be mutated.
    pub protect_target: bool,
    /// Verified backing from a full execution snapshot. Open it without
    /// initialization writes and retain its target, baseline and exclusions.
    pub execution_snapshot: Option<ExecutionOverlayHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOverlayHint {
    pub target: PathBuf,
    pub baseline_lower: Option<PathBuf>,
    pub excluded_paths: Vec<PathBuf>,
}

/// Environment + cwd plan injected beside the Agent process.
#[derive(Debug, Clone, Default)]
pub struct ImplantPlan {
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub overlay: OverlayHint,
    pub notes: Vec<String>,
}

impl ImplantPlan {
    pub fn marker_env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("PVISOR_RUNTIME".into(), "1".into());
        env.insert("PVISOR_ROLE".into(), "supervisor".into());
        env
    }

    pub fn as_metadata_json(&self) -> serde_json::Value {
        json!({
            "env_keys": self.env.keys().cloned().collect::<Vec<_>>(),
            "cwd": self.cwd.as_ref().map(|p| p.display().to_string()),
            "overlay_merged": self.overlay.merged_dir.as_ref().map(|p| p.display().to_string()),
            "overlay_stage": self.overlay.stage_dir.as_ref().map(|p| p.display().to_string()),
            "notes": self.notes,
        })
    }
}
