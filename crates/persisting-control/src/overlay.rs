//! Shared Overlay review/apply records and local Run inspection messages.
//!
//! Filesystem operations, journal storage, mount ownership, and request handling
//! remain in the Overlay drivers and pVisor. These types preserve their existing
//! JSON representation; no new transport or protocol version is introduced.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::file_access::{FileAccessDecision, FileAccessPolicy};

/// Local Run inspection request, encoded as one JSON line on `control.sock`.
/// This endpoint is separate from the cooperative AgentCtl protocol.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RunControlRequest {
    Ping,
    VmStatus,
    Pause,
    Resume,
    /// Start asynchronous reclaim; completion is reported through VmStatus.
    Offload {
        bytes: u64,
    },
    OverlayStatus,
    Observations,
    MountInspect,
    UnmountInspect {
        id: String,
    },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_status: Option<VmRuntimeStatus>,
}

/// Resident VM lifecycle state; paused VMs still own their process and leases.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VmRuntimeState {
    Unsupported,
    Starting,
    Running,
    Pausing,
    Paused,
    Resuming,
    Faulted,
    Stopped,
}

impl VmRuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Resuming => "resuming",
            Self::Faulted => "faulted",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmRuntimeStatus {
    pub supported: bool,
    pub state: VmRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<VmMemoryStatus>,
}

/// Cgroup accounting, not a byte-exact inventory of guest RAM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmMemorySample {
    pub current_bytes: u64,
    pub swap_bytes: u64,
    pub anon_bytes: u64,
    pub file_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VmOffloadState {
    Reclaiming,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmOffloadReport {
    pub operation_id: u64,
    pub state: VmOffloadState,
    pub requested_bytes: u64,
    pub before: VmMemorySample,
    pub after: Option<VmMemorySample>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmMemoryStatus {
    pub cgroup: PathBuf,
    pub sample: Option<VmMemorySample>,
    pub sample_error: Option<String>,
    pub last_offload: Option<VmOffloadReport>,
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
