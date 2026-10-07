//! Platform-independent contracts for the host FUSE overlay (Linux and macOS).
//!
//! Import the corresponding trait to call a constructor or method. Implementation,
//! FUSE state and platform dispatch are private. This API does not own Run
//! lifecycle, host sandbox installation, review, apply or drop.
//!
//! Mounting consumes configuration and may create directories or initialize a
//! journal before returning an error; preparation is not a rollback transaction.
//! Callers own stage exclusivity and cleanup of these paths. FUSE availability,
//! host permissions and macFUSE installation are checked at runtime.

use std::path::{Path, PathBuf};

pub use crate::mount::OverlaySession;
pub use crate::observation::FsMetrics;

/// Owned mount inputs. Cloning copies configuration but shares any metrics sink.
/// Lower paths are canonicalized and checked during mounting, not construction.
#[derive(Clone, Debug)]
pub struct OverlayMountConfig {
    /// Destination identity for apply conflict tracking; `None` falls back to
    /// the last lower. Mounting itself never applies staged changes there.
    pub apply_target: Option<PathBuf>,
    /// Optional baseline lower identity for the shared overlay layout.
    pub baseline_lower: Option<PathBuf>,
    /// Nonempty ordered lower directories, highest priority first (below upper).
    pub lower_dirs: Vec<PathBuf>,
    /// Writable stage directory; created if absent, even for inspection mounts.
    pub upper_dir: PathBuf,
    /// Optional copy-up work directory, created if absent. Must differ from upper
    /// and be on the same filesystem; lower/mountpoint overlap is rejected.
    pub work_dir: Option<PathBuf>,
    /// Merged-view path. Non-FSKit mounts create it if absent; FSKit requires a
    /// canonical existing parent and a path under `/Volumes`.
    pub mountpoint: PathBuf,
    /// Request FUSE access for other users, subject to host FUSE configuration.
    /// With FSKit this disables the default owner-only (`0700`) root. This is
    /// not a host sandbox or an authorization guarantee.
    pub allow_other: bool,
    /// Request FUSE access for root. Rejected with FSKit, which lacks caller
    /// credentials for filtering; host support governs other backends.
    pub allow_root: bool,
    /// Request OS permission checks. Required with FSKit; disabling this on
    /// other backends does not replace checks with a host security boundary.
    pub default_permissions: bool,
    /// Reject mounted-view mutations and request a read-only FUSE mount.
    /// Preparation may still create paths and open/initialize journal state.
    pub read_only: bool,
    /// Filesystem name passed to FUSE for diagnostics.
    pub fsname: String,
    /// macFUSE backend (`kernel` or `fskit`). Constructor defaults to FSKit on
    /// macOS and `None` on Linux. Explicit values are validated and forwarded
    /// to FUSE; accepting a value does not promise backend support on Linux.
    /// FSKit requires macFUSE 5.4.0 or newer.
    pub backend: Option<String>,
    /// Forward the FUSE debug mount option; does not initialize logging.
    pub debug: bool,
    /// Optional durable first-touch journal for later apply conflict detection.
    pub preimage_dir: Option<PathBuf>,
    /// Select compact first observations only when exclusively initializing a
    /// fresh writable stage. Existing journals/nonempty uppers retain their
    /// format; read-only inspection never selects a new format.
    pub compact_preimages: bool,
    /// Overlay-root-relative exclusions, absent from every lower and upper and
    /// not recreatable through the mount. These do not hide host paths outside
    /// the mounted view. Shared overlay-core validates their syntax.
    pub excluded_paths: Vec<PathBuf>,
    /// Shared overlay-core path access policy for requests through this view,
    /// not for unrelated host accesses. FSKit permission limitations still apply.
    pub access_policy: pvisor_overlay_core::FileAccessPolicy,
    /// Optional shared Run-scoped metrics sink; standalone mounts leave it unset.
    pub observation: Option<FsMetrics>,
}

/// Construction of mount configuration without I/O or validation.
pub trait OverlayConfiguration: Sized {
    /// Retain supplied paths; choose the last lower as apply target. Defaults:
    /// writable, OS permission checks enabled, no expanded user access, standard
    /// filesystem name, platform backend, no journal, exclusions or metrics,
    /// and the shared overlay-core default access policy.
    fn new(
        lower_dirs: Vec<PathBuf>,
        upper_dir: PathBuf,
        work_dir: Option<PathBuf>,
        mountpoint: PathBuf,
    ) -> Self;
}

/// Marker for host FUSE mounting services; contains no filesystem state.
pub struct OverlayFs;

/// Mount operations with identical signatures on Linux and macOS.
pub trait OverlayMounting {
    /// Consume configuration, prepare the overlay, mount it and start its request
    /// thread. The returned owner must outlive filesystem users. Invalid paths,
    /// overlapping layouts, incompatible flags, journal or FUSE failures return
    /// errors; preparation side effects are not rolled back. On macOS, FSKit
    /// version checking precedes path validation.
    fn mount(config: OverlayMountConfig) -> anyhow::Result<OverlaySession>;

    /// Consume configuration and serve requests on the calling thread until
    /// unmount or a request-loop error. No session owner is returned. Preparation
    /// and errors have the same side-effect limits as background mounting.
    fn run_foreground(config: OverlayMountConfig) -> anyhow::Result<()>;

    /// Probe whether a path appears mounted; this is not an atomic cleanup guard.
    /// macOS compares the supplied path verbatim against the mount table without
    /// contacting FUSE and returns `true` on table errors. Linux follows metadata
    /// and compares device/inode with the parent: failures return `false` and
    /// same-device bind mounts may be missed. Use a canonical mountpoint path.
    fn is_mountpoint(path: &Path) -> bool;
}

/// Lifecycle control of an opaque, uniquely owned background mount.
pub trait OverlaySessionControl: Sized {
    /// Canonical mountpoint retained at mounting time; borrowed from this owner.
    fn mountpoint(&self) -> &Path;

    /// Whether the request thread has finished. Does not report its error or
    /// prove mount detachment; no joining or unmounting is performed.
    fn has_exited(&self) -> bool;

    /// Consume the owner, unmount and stop the request loop, then poll detachment
    /// for up to about five seconds. The underlying unmount/join can take longer.
    /// Errors do not return ownership or guarantee detachment; callers must
    /// retain stage paths for recovery. Drop attempts the same cleanup but
    /// discards errors. Shutdown should be serialized by the owning caller.
    fn unmount(self) -> anyhow::Result<()>;
}

/// Diagnostic snapshots of requests that reached the mounted FUSE view.
pub trait FilesystemMetrics {
    /// Clone counters under the shared sink's mutex, recovering poisoned locks.
    /// `Clone` shares the sink across threads (`Send + Sync`); `Default` creates
    /// an independent empty sink. At most 8192 distinct paths are retained;
    /// additional hits increment `overflow_hits`. Rule counters are not subject
    /// to that path cap. Snapshotting neither resets counters nor freezes the
    /// filesystem, and is not a complete audit of host filesystem activity.
    fn snapshot(&self) -> pvisor_core::operation::FilesystemObservation;
}
