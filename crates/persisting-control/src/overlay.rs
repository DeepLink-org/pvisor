//! Shared Overlay review/apply records and local Run inspection messages.
//!
//! Filesystem operations, journal storage, mount ownership, and request handling
//! remain in the Overlay drivers and pVisor. These types preserve their existing
//! JSON representation; no new transport or protocol version is introduced.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Local Run inspection request, encoded as one JSON line on `control.sock`.
/// This endpoint is separate from the cooperative AgentCtl protocol.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RunControlRequest {
    Ping,
    OverlayStatus,
    MountInspect,
    UnmountInspect { id: String },
}

/// Response to a local Run inspection request. Optional fields remain explicit
/// JSON nulls for compatibility with existing clients.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunControlResponse {
    pub ok: bool,
    pub id: Option<String>,
    pub mountpoint: Option<PathBuf>,
    pub error: Option<String>,
    pub overlay_status: Option<OverlayStatus>,
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
    pub upper: OverlayUpper,
    pub merged_dir: PathBuf,
    pub stage_dir: PathBuf,
    /// Paths relative to the overlay root that are inaccessible through the
    /// merged view. Root overlays use this to hide their own backing state.
    #[serde(default)]
    pub excluded_paths: Vec<PathBuf>,
    pub auto_apply: bool,
    #[serde(default)]
    pub auto_discard: bool,
    /// Immutable lower targets (for example OCI cache entries) reject apply.
    #[serde(default)]
    pub protect_target: bool,
    pub state: OverlayState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayUpper {
    Directory {
        upper_dir: PathBuf,
        work_dir: PathBuf,
    },
    Jujutsu {
        store_path: PathBuf,
        workspace: String,
        upper_dir: PathBuf,
    },
}

impl OverlayUpper {
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory { upper_dir, .. } => upper_dir,
            Self::Jujutsu { upper_dir, .. } => upper_dir,
        }
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
    pub path: String,
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

/// User-facing selection for one staged apply operation. Exact paths select
/// the path and its descendants; include/exclude values use git-style glob
/// matching against slash-separated paths relative to the overlay root.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplySelection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    },
    Directory {
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_seconds: i64,
        mtime_nanoseconds: i64,
    },
    Symlink {
        target: Vec<u8>,
        uid: u32,
        gid: u32,
    },
    Other {
        mode: u32,
        uid: u32,
        gid: u32,
        rdev: u64,
    },
}
