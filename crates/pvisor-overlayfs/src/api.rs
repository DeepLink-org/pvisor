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
use std::time::Duration;

pub use crate::mount::OverlaySession;
pub use crate::observation::FsMetrics;

/// Explicit host-kernel caching strategy; never inferred from source classification.
/// No strategy enables writeback caching. Extended strategies require a Linux
/// owned view. Metadata supports adapter-mediated writes; MetadataAndData is
/// read-only. macOS is explicitly rejected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KernelCachePolicy {
    /// Preserve legacy behavior: one-second entry/attribute TTL, no negative
    /// caching, no KEEP_CACHE and no additional ownership requirements.
    #[default]
    Disabled,
    /// Explicit metadata-cache-off A/B control: zero entry/attribute/negative
    /// TTL, no KEEP_CACHE. Normal page caching within an open file remains on.
    /// Like Disabled, does not require an owned-view contract.
    Uncached,
    /// Bounded entry, attribute and negative caching; no KEEP_CACHE across opens.
    /// Writable Linux views use asynchronous mutation invalidation and forced
    /// teardown on transport failure. Mount admission exercises detach/abort;
    /// catastrophic OS teardown failure still requires caller containment of users.
    Metadata,
    /// Metadata caching plus KEEP_CACHE for stable read-only regular-file mappings.
    /// Writable configurations are explicitly rejected.
    MetadataAndData,
}

/// Caller-owned proof, not a property established by an advisory coordination lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnedViewContract {
    /// Upper/work and their physical ancestors/mount identities are exclusively
    /// owned until actual mount detachment, including aliases and other sessions.
    /// No host-side writer, apply, checkpoint restore or backing replacement may
    /// run concurrently. All mutations must go through this adapter. Writable
    /// cached views must not be exported through bind mounts/namespace copies,
    /// overmounted or replaced; forced teardown must cover their only view.
    /// Preserve mount namespace, credentials, helper availability and detach
    /// permissions for the session lifetime. Callers must supervise all users
    /// and stop them if the server exits or termination fails: process abort
    /// alone cannot revoke another process's warm metadata or held descriptors.
    /// Do not opt in if that failure-containment contract cannot be honored.
    pub exclusive_upper_and_work: bool,
    /// Permissions, ownership, xattrs, namespace and all hardlink aliases of
    /// every backing object remain fixed except for adapter-mediated mutations.
    /// Reads must not change backing atime (caller must arrange noatime or an
    /// equivalent guarantee). Read-only FUSE alone does not establish this.
    pub fixed_metadata_and_aliases: bool,
}

/// Which observations the caller expects when kernel caches satisfy requests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadObservationSemantics {
    /// Preserve per-request callback observations; incompatible with extended
    /// kernel caching. Journals and metrics are never silently disabled.
    #[default]
    RequestCallbacks,
    /// Caller accepts that cache hits do not reach the adapter. This is not an
    /// audit of reads or a first-content-observation journal. Existing journal,
    /// metrics and custom path-policy configurations are still rejected.
    StableView,
}

/// Explicit cache admission inputs. Default is disabled, with no ownership proof.
/// Validation is side-effect free. For enabled policies TTL must be greater than
/// zero and at most 60 seconds; it applies equally to entry/attr/negative replies.
/// No environment variable can enable this policy. VM virtio-fs has no equivalent
/// mode: this DTO belongs only to the host FUSE adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelCacheConfig {
    /// Same-artifact A/B strategy; changing it does not alter lower declarations.
    pub policy: KernelCachePolicy,
    /// Reviewable, finite kernel TTL; ignored for Disabled/Uncached. Default is
    /// 60 seconds. Writable replies temporarily use zero while effects are pending.
    pub ttl: Duration,
    /// Explicit lifetime promise; `None` is rejected for enabled policies.
    pub owned_view: Option<OwnedViewContract>,
    /// Explicit agreement to cached-read observations; defaults to callbacks.
    pub read_observation: ReadObservationSemantics,
}

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
    /// Per-physical-lower stability promises, in `lower_dirs` order. Empty means
    /// all mutable; any other length mismatch is rejected during mounting.
    /// Caller must preserve contents, metadata, namespace and mount identities
    /// until session teardown. Read-only mounts and frozen baselines do not prove
    /// this promise. Upper and merged views are never covered by it.
    pub lower_mutability: Vec<pvisor_overlay_core::LayerMutability>,
    /// Explicit host kernel cache admission and lifetime contract. Defaults to
    /// disabled. Enabled policies reject unsupported configurations before any
    /// preparation I/O; they never silently fall back or skip journals.
    pub kernel_cache: KernelCacheConfig,
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

    /// Validate cache admission without filesystem I/O or acquiring locks. This
    /// does not prove the caller's ownership contract, validate canonical paths,
    /// mount FUSE or negotiate kernel support. Enabled modes require explicit
    /// immutable declarations for every lower, an affirmed owned-view contract,
    /// stable-view read semantics, OS permission checks, owner-only access and
    /// no path policy, exclusions, journal or metrics. Linux Metadata permits
    /// writes; MetadataAndData requires read-only. Actual writable mounting also
    /// requires the connection's fusectl abort file and a successful sacrificial
    /// mount/detach/abort probe at the actual mountpoint before serving requests.
    /// On notification/queue failure, a worker detaches using Linux umount or
    /// fusermount3/fusermount, verifies absence in mountinfo, then writes abort.
    /// Rejected admission performs no further mutation; already-mutated failed
    /// replies await completed teardown. Normal/stop queues and effects are
    /// bounded; helper, termination and shutdown waits have deadlines. Fatal
    /// teardown failure/deadline terminates the server, but is NOT a guarantee
    /// that other processes' caches were revoked: the caller's user-containment
    /// obligation still applies. EIO is not a general mount-detachment fence.
    /// Held descriptors must be released on failure; changes may remain in upper.
    /// Validation failure leaves configuration unchanged.
    fn validate_kernel_cache(&self) -> anyhow::Result<()>;
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
    /// retain stage paths for recovery. Writable cache notification failures are
    /// reported here even if the mount was already forcibly detached; mutations
    /// may have reached upper before failure, and are not rolled back. Drop
    /// attempts the same cleanup but
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
