//! Shared Overlay review/apply records and local Run inspection messages.
//!
//! Filesystem operations, journal storage, mount ownership, and request handling
//! remain in the Overlay drivers and pVisor. Optional fields extend old records;
//! UTF-8 paths retain their JSON strings, other Unix paths use explicit bytes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::file_access::{FileAccessContext, FileAccessDecision, FileAccessPolicy};

/// Local Run inspection request, encoded as one JSON line on `control.sock`.
/// This endpoint is separate from the cooperative AgentCtl protocol.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RunControlRequest {
    Ping,
    OverlayStatus,
    Observations,
    MountInspect,
    UnmountInspect { id: String },
}

/// Response to a local Run inspection request. Existing optional fields remain
/// explicit JSON nulls; the newer observations field is absent when unused.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunControlResponse {
    pub ok: bool,
    pub id: Option<String>,
    pub mountpoint: Option<PathBuf>,
    pub error: Option<String>,
    pub overlay_status: Option<OverlayStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observations: Option<serde_json::Value>,
}

/// Durable record of one overlay staging workspace (survives Attempt teardown).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayRecord {
    pub id: String,
    /// Monotonic reusable-environment generation. Terminal overlays are never
    /// reopened; a reset creates the next generation over the same stage.
    #[serde(default)]
    pub generation: u64,
    /// Target filesystem (apply destination + primary lower).
    pub target: PathBuf,
    /// Explicit read baseline when runtime snapshots the apply target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_lower: Option<PathBuf>,
    pub upper: OverlayUpper,
    pub merged_dir: PathBuf,
    pub stage_dir: PathBuf,
    /// Paths relative to the overlay root that are inaccessible through the
    /// merged view. Root overlays use this to hide their own backing state.
    #[serde(default)]
    pub excluded_paths: Vec<PathBuf>,
    #[serde(default)]
    pub access_policy: FileAccessPolicy,
    pub auto_apply: bool,
    #[serde(default)]
    pub auto_discard: bool,
    /// Immutable lower targets (for example OCI cache entries) reject apply.
    #[serde(default)]
    pub protect_target: bool,
    pub state: OverlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlayUpper {
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
}

impl OverlayUpper {
    pub fn path(&self) -> &Path {
        &self.upper_dir
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OverlayState {
    /// Mounted / Agent may write.
    Active,
    /// Unmounted; upper retained for review.
    Staged,
    /// Upper applied onto target.
    Applied,
    /// Upper discarded.
    Discarded,
}

/// Summary of files present in upper (not a full recursive diff vs target).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayStatus {
    pub changed_files: usize,
    pub whiteouts: usize,
    pub sample_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    TypeChanged,
    Opaque,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeEntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeEntry {
    /// Human-readable display. Mutation must use relative_path(), not this string.
    pub path: String,
    /// Lossless Unix identity when the display string cannot represent the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_bytes: Option<Vec<u8>>,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_type: Option<ChangeEntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_type: Option<ChangeEntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

impl ChangeEntry {
    pub fn relative_path(&self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            self.path_bytes
                .as_ref()
                .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes.clone())))
                .unwrap_or_else(|| PathBuf::from(&self.path))
        }
        #[cfg(not(unix))]
        {
            PathBuf::from(&self.path)
        }
    }
}

/// User-facing selection for one staged apply operation. Exact paths select
/// the path and its descendants; include/exclude values use git-style glob
/// matching against slash-separated paths relative to the overlay root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplySelection {
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "unix_paths")]
    pub paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
}

impl ApplySelection {
    pub fn is_all(&self) -> bool {
        self.paths.is_empty() && self.includes.is_empty() && self.excludes.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyRecord {
    pub schema_version: u32,
    pub apply_id: String,
    pub created_at_unix_ms: u64,
    pub overlay_id: String,
    #[serde(default)]
    pub overlay_generation: u64,
    pub target: PathBuf,
    pub selection: ApplySelection,
    pub changes: Vec<ChangeEntry>,
    /// Exact dependency-closed paths selected by the prepared transaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "unix_paths")]
    pub planned_paths: Vec<PathBuf>,
    /// Target state captured when each path was first mutated in the overlay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preimages: Vec<PathPreimage>,
    /// Old ledgers contain only successful records and therefore deserialize
    /// as committed.
    #[serde(default = "committed_apply_state")]
    pub state: ApplyRecordState,
    pub remaining_changes: usize,
}

/// Keep existing UTF-8 JSON strings; encode other Unix paths without loss.
#[cfg(unix)]
mod unix_paths {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum WirePath {
        Text(String),
        Bytes { bytes: Vec<u8> },
    }
    pub fn serialize<S: serde::Serializer>(
        paths: &[PathBuf],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        paths
            .iter()
            .map(|path| match path.to_str() {
                Some(text) => WirePath::Text(text.into()),
                None => WirePath::Bytes {
                    bytes: path.as_os_str().as_bytes().to_vec(),
                },
            })
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PathBuf>, D::Error> {
        Ok(Vec::<WirePath>::deserialize(deserializer)?
            .into_iter()
            .map(|path| match path {
                WirePath::Text(text) => PathBuf::from(text),
                WirePath::Bytes { bytes } => PathBuf::from(std::ffi::OsString::from_vec(bytes)),
            })
            .collect())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyRecordState {
    Prepared,
    TargetApplied,
    Committed,
}

fn committed_apply_state() -> ApplyRecordState {
    ApplyRecordState::Committed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub apply_id: String,
    pub applied: Vec<ChangeEntry>,
    pub remaining: Vec<ChangeEntry>,
}

/// Durable first-touch state of one apply target path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathPreimage {
    /// Raw Unix path bytes relative to the overlay root.
    pub path: Vec<u8>,
    pub state: PathFingerprint,
}

#[cfg(unix)]
impl PathPreimage {
    pub fn relative_path(&self) -> PathBuf {
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(std::ffi::OsString::from_vec(self.path.clone()))
    }
}

/// Content and metadata relevant to detecting a destructive apply conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathFingerprint {
    Absent,
    File {
        sha256: String,
        mode: u32,
        uid: u32,
        gid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        xattrs: Option<XattrFingerprint>,
    },
    Directory {
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_seconds: i64,
        mtime_nanoseconds: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        xattrs: Option<XattrFingerprint>,
    },
    Symlink {
        target: Vec<u8>,
        uid: u32,
        gid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        xattrs: Option<XattrFingerprint>,
    },
    Other {
        mode: u32,
        uid: u32,
        gid: u32,
        rdev: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        xattrs: Option<XattrFingerprint>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XattrFingerprint {
    Unsupported,
    Values { entries: Vec<(Vec<u8>, String)> },
}

impl PathFingerprint {
    pub fn xattrs(&self) -> Option<&XattrFingerprint> {
        match self {
            Self::Absent => None,
            Self::File { xattrs, .. }
            | Self::Directory { xattrs, .. }
            | Self::Symlink { xattrs, .. }
            | Self::Other { xattrs, .. } => xattrs.as_ref(),
        }
    }
    /// Legacy first-touch records did not measure xattrs; do not invent evidence.
    pub fn matches(&self, current: &Self) -> bool {
        if self.xattrs().is_some() {
            return self == current;
        }
        let mut compatible = current.clone();
        match &mut compatible {
            Self::Absent => {}
            Self::File { xattrs, .. }
            | Self::Directory { xattrs, .. }
            | Self::Symlink { xattrs, .. }
            | Self::Other { xattrs, .. } => *xattrs = None,
        }
        self == &compatible
    }
}

#[cfg(not(unix))]
mod unix_paths {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        paths: &[PathBuf],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        paths.serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PathBuf>, D::Error> {
        Vec::<PathBuf>::deserialize(deserializer)
    }
}

/// Filesystem overlay settings (same capture TOML; applied by pVisor).
///
/// Model: **target** (read-only base / apply destination) + **staging** (upper
/// holds deltas). The Agent sees `merged`; changes do **not** touch `target`
/// until an explicit runtime overlay is applied.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverlayConfig {
    #[serde(default)]
    pub access_policy: FileAccessPolicy,
    /// When true, pVisor mounts its embedded OverlayFS for the Attempt.
    #[serde(default)]
    pub enabled: bool,
    /// Target filesystem: primary lower layer and destination for `apply`.
    /// Prefer this over listing the same path in `lower_dirs`.
    #[serde(default)]
    pub target: Option<String>,
    /// Prevent explicit or automatic apply from modifying the lower target.
    #[serde(default)]
    pub protect_target: bool,
    /// Read-only compose layers stacked above `target`, highest priority first.
    #[serde(default)]
    pub lower_dirs: Vec<String>,
    /// Root for staging (`upper` / `work` / `merged`). Default:
    /// `{capture_storage}/.overlay/{session_id}/`.
    #[serde(default)]
    pub stage_dir: Option<String>,
    /// Writable upper directory (overrides `{stage_dir}/upper` when set).
    #[serde(default)]
    pub upper_dir: Option<String>,
    /// Overlay work directory (overrides `{stage_dir}/work` when set).
    #[serde(default)]
    pub work_dir: Option<String>,
    /// Merged mount point (overrides `{stage_dir}/merged` when set).
    #[serde(default)]
    pub merged_dir: Option<String>,
    /// If true, apply staging onto `target` automatically when the Attempt ends.
    /// Default false — review then `pvisor apply` or `pvisor drop`.
    #[serde(default)]
    pub auto_apply: bool,
    /// If true, discard staging automatically when the Attempt ends.
    #[serde(default)]
    pub auto_discard: bool,
}

#[cfg(all(test, unix))]
mod path_encoding_tests {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    #[test]
    fn selection_keeps_non_utf8_identity_and_legacy_strings() {
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
        let selection = ApplySelection {
            paths: vec![PathBuf::from("plain"), path.clone()],
            ..Default::default()
        };
        let json = serde_json::to_value(&selection).unwrap();
        assert_eq!(json["paths"][0], "plain");
        assert_eq!(json["paths"][1]["bytes"], serde_json::json!([120, 255]));
        let decoded: ApplySelection = serde_json::from_value(json).unwrap();
        assert_eq!(
            decoded.paths[1].as_os_str().as_bytes(),
            path.as_os_str().as_bytes()
        );
    }
}
