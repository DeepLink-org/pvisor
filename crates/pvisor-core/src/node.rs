//! Immutable local image identity, independent of schedulers and mutable tags.
use serde::{Deserialize, Serialize};

/// A native cache revision and the manifest it must resolve to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLayer {
    pub handle: String,
    pub manifest_digest: String,
}
