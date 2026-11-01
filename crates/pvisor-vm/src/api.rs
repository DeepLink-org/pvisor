//! The platform-independent public contract of pVisor's VM runtime.
//!
//! # Ownership and lifecycle
//! One builder owns one isolated runner's configuration. `run` consumes it and
//! enters the event loop; guest shutdown terminates the runner process. Kernel
//! mappings, synthetic file bytes and connected network streams remain owned
//! until VM teardown. Install the host sandbox and validate inherited descriptors
//! before passing ownership here. The ready callback must return promptly.
//!
//! # Capabilities and errors
//! All targets expose these same types and methods. Inspect [`RuntimeSupport::capabilities`];
//! unavailable operations return `Unsupported` (or an explicit control error)
//! before mutating VM state. Architecture and hypervisor selection is internal.
//! A capability describes implemented primitives, not evidence that a host has
//! granted virtualization permissions or that a sandbox was installed.
//! Windows x86-64 uses WHP with the default or no-default-features profile.
//! It implements boot, ordinary directory shares, root-level synthetic files and
//! vCPU pause/resume. Overlay, DAX, snapshots, cold RAM, vsock, Ethernet attachment,
//! custom guest commands and task-stdio descriptors currently return Unsupported.
//! Console output uses the runner's stdout; use the guest launch configuration
//! with `stdio_ports: Some([false; 3])`. Windows filesystem permissions are
//! synthesized; host ACLs apply and POSIX ownership/mode changes are unsupported.
//!
//! # Filesystems and networking
//! Disable implicit init before attaching a native-init `/dev/root`. Filesystem
//! tags and virtual paths are unique. Host paths currently require UTF-8. DAX is
//! disabled by default; only an explicit [`VmConfiguration::immutable_filesystem`]
//! attachment allocates a DAX window. Ordinary filesystems and pVisor overlays
//! use zero DAX size. Synthetic files own their bytes.
//! Vsock never implicitly enables TSI, even when no network device is configured.
//!
//! # Snapshots and transitions
//! Restore input requires caller-validated host/boot/build identity, sealed RAM,
//! an independent rootfs copy and exclusive execution ownership. Restored VMs
//! start paused; explicitly resume only after installation in the ready callback.
//! `MachineSnapshot` preserves backend state as an opaque serialization payload;
//! identical Rust API shapes do not promise cross-architecture restore.
//! Freeze/drain failure parks the VM and requires runner termination. Returning
//! from a resumable snapshot action resumes the source; save-and-stop capture
//! remains parked even on rejection and requires runner termination. Persistence
//! and atomic rootfs/RAM publication remain the caller's responsibility. Live
//! handles weakly reference the owning VM.
//!
//! # Boundary
//! This module contains declarations and exports only. Implementations and
//! register/device representations are private. Low-level hardware tests live
//! inside this crate. Public methods are implemented in the private adapters.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Portable machine sizing. Validation belongs to the runtime: CPUs and memory
/// must be nonzero and fit the selected backend's limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// Number of guest vCPUs; validated as nonzero within backend limits before boot.
    pub cpus: u8,
    /// Guest RAM size in MiB; must be nonzero and fit backend address-space limits.
    pub memory_mib: u32,
}

/// Runtime services with the same signatures on every supported target.
pub struct VmPlatform;
/// Process-local platform discovery and diagnostics; no VM is created by these queries.
pub trait RuntimeSupport {
    /// Report compiled architecture/backend primitives without probing host permissions.
    fn capabilities() -> Capabilities;
    /// Initialize process logging before constructing the VM.
    fn init_logging(filter: &str);
    /// Build-embedded kernel, if present. This does not load host firmware.
    /// The shared bytes are immutable; addresses describe the guest boot layout.
    fn embedded_kernel() -> Option<KernelImage>;
    /// Process-local outstanding cold-RAM work; zero when no pager is active.
    /// These counters are diagnostic bounds, not physical memory attribution.
    fn cold_ram_activity() -> ColdRamActivity;
    /// Firmware ABI library name selected internally for the host platform.
    fn firmware_name() -> &'static str;
    /// Firmware next to the current executable, if installed. No download or load.
    fn bundled_firmware_directory() -> Option<PathBuf>;
    /// Resolve the platform firmware in an explicit directory, or next to the
    /// current executable when `None`. Returns a canonical absolute regular-file
    /// path. Missing firmware returns `NotFound`; no acquisition, compilation,
    /// cache or loader environment search occurs. An explicit directory never
    /// falls back to packaged discovery. The caller must trust the file and keep
    /// it available until boot; resolution does not load or pin its contents.
    fn resolve_firmware_path(directory: Option<&Path>) -> io::Result<PathBuf>;
}

#[derive(Clone, Debug)]
/// Owned immutable kernel payload and its architecture-specific guest boot addresses.
pub struct KernelImage {
    /// Kernel bytes retained by shared ownership until all consumers release them.
    pub bytes: Arc<[u8]>,
    /// Guest physical load address, not a host pointer.
    pub guest_address: u64,
    /// Guest instruction address at first CPU entry.
    pub entry_address: u64,
}

/// Command consumed by a caller-supplied init that understands KRUN_INIT,
/// KRUN_WORKDIR and kernel-provided arguments/environment. It does not configure
/// the built-in Rust supervisor: use its launch JSON for normal pVisor workloads.
/// Nothing is inherited from the host environment. Paths must be absolute;
/// unsupported quoting/control characters and oversized commands are rejected.
#[derive(Clone, Debug)]
pub struct GuestCommand {
    /// Absolute guest executable path; validated with the custom-init command.
    pub executable: String,
    /// Guest argument vector; passed without shell expansion.
    pub arguments: Vec<String>,
    /// Explicit guest environment; host variables are not inherited implicitly.
    pub environment: std::collections::BTreeMap<String, String>,
    /// Absolute working directory interpreted by the caller-supplied init.
    pub working_directory: String,
}

/// Guest init networking behavior. It never enables host-network TSI bypass.
#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkOptions {
    /// Request DHCP from a caller-supplied init that supports KRUN_DHCP.
    pub guest_dhcp: bool,
}

/// Owned framed Ethernet transport. Unix streams convert through `From` on Unix.
/// A Windows `File` can transfer ownership into this type, but Windows Ethernet
/// attachment currently returns Unsupported. Rejected attachments drop the
/// supplied transport and preserve the current configuration. Successful Unix
/// attachments retain it until teardown; framing uses four-byte big-endian lengths.
pub struct NetworkStream {
    pub(crate) inner: crate::transport::Stream,
}

/// Configure owned resources before boot. Each successful call transfers the
/// supplied resource into this configuration. Failed calls leave the previous
/// configuration usable; init selection precedes root filesystem attachment.
pub trait VmConfiguration: Sized {
    /// Construct a configuration; equivalent to `from_config` with explicit sizing.
    fn new(cpus: u8, memory_mib: u32) -> io::Result<Self>;
    /// Validate sizing and construct an unbooted configuration; failure creates no VM.
    fn from_config(config: VmConfig) -> io::Result<Self>;
    /// Restore sealed, validated machine state. Captured kernel geometry avoids
    /// loading a fresh firmware payload; legacy states retain the normal loader.
    fn from_restore(config: VmConfig, restore: MachineRestore) -> io::Result<Self>;
    /// Select a trusted local firmware file. Rejects relative paths and non-files;
    /// canonicalizes before replacing the previous selection, without loading it.
    /// An already configured kernel or snapshot kernel takes priority; otherwise
    /// boot prefers the build-embedded kernel, then this file, then executable-
    /// adjacent firmware. Keep the file available until boot. Never searches
    /// LD_LIBRARY_PATH/DYLD_LIBRARY_PATH or downloads missing firmware.
    fn set_firmware_path(&mut self, path: PathBuf) -> io::Result<()>;
    /// Transfer an empty regular file as writable RAM backing. The runtime owns it
    /// and may resize/map it at boot; nonempty/nonregular inputs leave configuration unchanged.
    fn ram_backing(&mut self, file: File) -> io::Result<()>;
    /// Opt in to a common boot-kernel RAM baseline on Apple Silicon HVF.
    /// `directory` must be an existing absolute, same-user, private directory
    /// outside guest-visible trees. The runtime atomically publishes verified,
    /// read-only content-addressed kernel files there; callers must not modify
    /// those files. Equal firmware bytes use one inode, mapped MAP_PRIVATE:
    /// reads can share physical pages and CPU/device writes remain VM-private.
    /// Other guest RAM keeps its ordinary backing. The mapped descriptor pins
    /// the baseline through VM teardown, independently of the daemon or path.
    /// This does not clone a running guest or merge application RAM. Cache files
    /// have no automatic GC; unlinking an unused entry is safe for live mappings,
    /// but truncation/mutation is forbidden. Unsupported platforms/features and
    /// snapshot profiles return Unsupported; invalid directories leave the old
    /// configuration unchanged. Boot rejects incompatible payloads/geometries
    /// and corrupted cache entries, without falling back to an unshared copy.
    fn shared_kernel_base(&mut self, directory: PathBuf) -> io::Result<()>;
    /// Copy and validate a trusted kernel payload and guest load/entry addresses.
    /// Successful replacement retains its own bytes; invalid geometry preserves the prior kernel.
    fn embedded_kernel(&mut self, bytes: &[u8], guest_addr: u64, entry_addr: u64)
        -> io::Result<()>;
    /// Opt in to the versioned guest idle handoff. Requires a fresh Linux/KVM
    /// x86-64 VM with one or two vCPUs and matching patched firmware. Other targets,
    /// snapshots and unguarded devices are rejected. Optional RAM offload uses
    /// `GuestIdleControl::offload_guest_idle` after conditional pause. Disabled by
    /// default; configuration alone does not authorize a pause or offload.
    /// Two vCPUs require ABI v3: every CPU parks and
    /// revalidates interrupts after the rendezvous; the earliest timer wins.
    /// Normal PSI and TX-watchdog behavior is retained on two CPUs.
    /// The maintained firmware also enables conservative single-CPU system PSI
    /// idle quiescence: no restart solely for idle sleep without pending pressure
    /// or registered triggers. Accounting, real timers and external wakes remain.
    fn guest_idle_handoff(&mut self) -> io::Result<()>;
    /// Enable the same idle profile with Host-selected maintenance/admission policy.
    /// `disable_proactive_compaction` adds the Linux boot sysctl
    /// `vm.compaction_proactiveness=0`; demand-driven compaction remains enabled.
    /// Requires firmware supporting this sysctl; the maintained Linux kernel does.
    /// It can increase high-order allocation latency and is not a security boundary.
    /// `offload_minimum` must be >2 ms and <=24 hours. Reclaim requires strictly
    /// more than this budget at admission, after device-gate closure. A shorter
    /// budget retains pause-only operation, without extending the guest deadline.
    /// No-timer offers bypass this duration check, but still require a live epoch.
    /// A threshold is not a hard completion bound for non-interruptible writeback.
    /// Defaults via `guest_idle_handoff`: false and one second. Same platform and
    /// lifecycle restrictions apply. Failure leaves the previous policy unchanged.
    fn guest_idle_handoff_with_policy(
        &mut self,
        disable_proactive_compaction: bool,
        offload_minimum: Duration,
    ) -> io::Result<()>;
    /// Disable built-in init before attaching /dev/root. A root already attached
    /// returns InvalidInput without changing the previous init selection.
    fn disable_implicit_init(&mut self) -> io::Result<()>;
    /// Configure a custom init's command; requires implicit init to be disabled.
    /// Validate atomically before replacing the previous command.
    fn guest_command(&mut self, command: GuestCommand) -> io::Result<()>;
    /// Attach an ordinary writable passthrough directory. `shm_size` must be
    /// zero; nonzero windows return Unsupported before changing configuration.
    /// Entry/attribute validity is zero so reopened files observe other host/VM
    /// writers. Data retains close-to-open caching and mmap support; simultaneous
    /// writes or already-open mappings across VMs require caller synchronization.
    /// Use `immutable_filesystem` for the supported read-only DAX contract.
    fn filesystem(&mut self, tag: &str, path: &Path, shm_size: usize) -> io::Result<()>;
    /// Attach a host-enforced read-only directory without DAX or an immutability
    /// promise. All guest mutation requests fail even if the guest remounts it.
    /// The caller retains the directory and its mount identity until teardown.
    /// Entry/attribute validity is zero and data uses close-to-open caching;
    /// reopened files observe host changes. Already-open mappings across VMs
    /// require caller synchronization, as for writable passthrough attachments.
    /// Requires a nonempty unique tag and an absolute existing UTF-8 directory.
    /// Invalid input leaves existing attachments unchanged. TEE/Nitro builds
    /// return Unsupported. This does not enable immutable-data caching or add
    /// the DAX profile's snapshot restrictions.
    fn read_only_filesystem(&mut self, tag: &str, path: &Path) -> io::Result<()>;
    /// Attach a caller-owned private directory with persistent guest identities.
    /// The caller retains its root/mount identity, owns every backing inode and
    /// excludes host authority and external writers for the attachment lifetime.
    /// Multiple VMs may share the directory with caller-synchronized mutations;
    /// reopened files observe changes with zero attribute/entry validity.
    /// Guest UID/GID/mode are stored in private inode xattrs; native backing
    /// remains owned and accessible by the host account. Guest VFS permissions
    /// and host-enforced `read_only` still apply.
    /// Unannotated inodes initially represent guest UID/GID zero with native
    /// mode bits; initialize imported data's guest owners before non-root use.
    /// Internal metadata xattrs cannot
    /// be read or changed through guest xattr operations. Unmapped symlink and
    /// special-file ownership changes return explicit unsupported errors.
    /// Requires an absolute existing owned nonsymlink directory and a nonempty
    /// unique tag. Unsupported backing xattrs fail on metadata access/mutation;
    /// failed mutations can retain a newly created owned backing inode.
    /// Metadata write failure can also retain changed native backing modes;
    /// callers must inspect or discard failed private trees before reuse.
    /// Invalid attachment input leaves existing configuration unchanged.
    /// Supported on Linux; other platforms, snapshot/restore profiles, DAX
    /// and TEE/Nitro return Unsupported.
    fn managed_filesystem(&mut self, tag: &str, path: &Path, read_only: bool) -> io::Result<()>;
    /// Attach a guest-read-only, physically immutable directory with a DAX window.
    /// Calling this method explicitly enables DAX capability for this share.
    /// The caller must keep contents, metadata, namespace, ancestors and hardlink
    /// aliases unchanged until VM teardown. This promise is not inferred from a
    /// read-only mount. The guest mounts the tag with `ro,dax=inode` to use DAX,
    /// or `ro,dax=never` for ordinary reads. Disabling DAX in the guest retains
    /// the configured window and this attachment's lifecycle restrictions.
    /// Only regular backing files support DAX; virtual files use ordinary reads.
    /// OPEN authorizes the file; mappings require a live read-only handle for
    /// that inode. Linux's `fh=-1` mapping requests reuse such an opened FD.
    /// Guest writes cannot change the backing. Mapping requests may reserve zero
    /// padding beyond EOF, but normal file reads retain their original size.
    ///
    /// Requires Linux x86_64 without TEE/Nitro, an absolute existing directory,
    /// and a window of 2 MiB to 1 GiB in 2 MiB increments. Duplicate/empty tags
    /// and invalid inputs fail before attachment. Other platforms return
    /// Unsupported. DAX and snapshot profiles/restoration are mutually exclusive;
    /// freeze/capture also reject DAX. Failure leaves previous attachments usable.
    /// The window reserves additional guest address space, not ordinary guest RAM;
    /// resident file pages and guest-induced private COW pages consume host memory.
    fn immutable_filesystem(
        &mut self,
        tag: &str,
        path: &Path,
        dax_window_size: usize,
    ) -> io::Result<()>;
    /// Attach a writable merged overlay. `shm_size` must be zero; DAX for this
    /// merged view is Unsupported. Failure preserves previous attachments.
    fn overlay(&mut self, tag: &str, overlay: OverlayConfig, shm_size: usize) -> io::Result<()>;
    /// Add owned immutable bytes to an existing filesystem tag; a leading slash
    /// is optional. Virtual parent directories must already exist; empty/dot/parent/
    /// NUL components and duplicate names are rejected. Mode uses guest permission
    /// bits. A one-shot name is consumed by its first successful lookup (not listing);
    /// releasing its inode removes the remaining bytes. Validation failure preserves attachments.
    /// Windows currently accepts only root-level names; nested paths return Unsupported.
    fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: impl Into<Arc<[u8]>>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()>;
    /// Transfer a connected, framed Ethernet stream to the VM with this MAC.
    /// Backend startup enables nonblocking I/O while preserving other status
    /// flags; clones share those flags and must not reset them. Socket setup
    /// failure aborts startup rather than running a blocking device worker.
    fn network(&mut self, stream: impl Into<NetworkStream>, mac: [u8; 6]) -> io::Result<()>;
    /// Route the task-stdio console ports (`krun-stdin`/`krun-stdout`/
    /// `krun-stderr` plus the unnamed console) to these descriptors instead
    /// of this process's stdio. `-1` omits that stream's port. Descriptors
    /// are duplicated at boot, not consumed, and must stay open until then;
    /// the caller retains ownership. Terminal detection (raw mode, window
    /// size, tty wiring of the unnamed console) runs against the passed
    /// descriptors. Call at most once; a second call fails without changing
    /// configuration. Rejects configurations where every stream is omitted.
    /// This separates workload stdio from process diagnostics: stderr
    /// logging (`init_logging`) can target the process's own stderr while
    /// guest output still reaches the caller's descriptors.
    fn task_stdio(&mut self, stdin: i32, stdout: i32, stderr: i32) -> io::Result<()>;
    /// Transfer the framed Ethernet stream and explicit guest-init options.
    /// Has network() ownership/nonblocking requirements; unavailable networking
    /// returns Unsupported, and no implicit TSI path is installed.
    fn network_with_options(
        &mut self,
        stream: impl Into<NetworkStream>,
        mac: [u8; 6],
        options: NetworkOptions,
    ) -> io::Result<()>;
    /// Configure a unique guest vsock port mapped to a caller-owned host socket
    /// path; listen selects a host listener rather than a connector. The caller
    /// authorizes exposure and retains the path. Duplicate ports and guest-idle
    /// profiles reject the update; this never enables TSI networking.
    fn vsock_port(&mut self, guest_port: u32, host_path: PathBuf, listen: bool) -> io::Result<()>;
    /// Select snapshot-compatible devices before boot. DAX, nested virtualization
    /// and guest-idle configurations reject the change; no snapshot is captured.
    fn snapshot_profile(&mut self) -> io::Result<()>;
    /// Validate and consume restore state for this CPU topology, enabling the
    /// snapshot profile. Writable RAM backing and unsupported device/build profiles
    /// reject restoration; a successfully restored machine starts paused.
    fn machine_restore(&mut self, restore: MachineRestore) -> io::Result<()>;
}

/// Consume configuration and run one VM in its dedicated runner process. The
/// callback receives live control after installation and must return promptly.
/// Normal guest shutdown exits the process; setup failure returns an error.
pub trait VmRuntime: Sized {
    /// Consume the builder and retain resources through the VM event loop.
    /// Invoke on_ready before CPU execution; it must not block and may resume a
    /// restored VM. An error aborts startup; normal guest shutdown exits the runner.
    fn run(self, on_ready: impl FnOnce(VmmHandle) -> io::Result<()>) -> io::Result<()>;
}

/// Object-safe live control, suitable for supervision and mock implementations.
/// Implementations serialize transitions; handles do not extend VM lifetime.
/// A failed drain/offload transition requires terminating the runner.
pub trait VmControl: Send + Sync {
    /// Read the current pause flag under the VM lock; stopped/poisoned owners return an error.
    fn is_paused(&self) -> Result<bool, String>;
    /// Serialize a CPU pause; the owner retains all resources. Drain/transition
    /// failure requires runner termination; pause alone is not snapshot quiescence.
    fn pause(&self) -> Result<(), String>;
    /// Serialize resume after a valid pause/restore. It does not reacquire storage
    /// or restore a checkpoint; transition failure requires runner termination.
    fn resume(&self) -> Result<(), String>;
    /// Unconditional whole-VM offload. Guest-idle profiles reject this method;
    /// use `GuestIdleControl::offload_guest_idle` with a claimed epoch instead.
    fn offload_ram(&self) -> Result<RamReclaim, String>;
}

/// Explicit, optional RAM deduplication; independent of `VmControl` mocks.
/// Transitions serialize with other handle operations. Advice never replaces a
/// mapping, changes its backing, pauses the VM or makes an advice failure fatal.
/// Only ordinary private RAM is eligible, including anonymous pages created by
/// writes to private file mappings. Shared mappings and device/huge-page windows
/// are skipped. macOS reports unsupported through the same API.
///
/// Successful advice is asynchronous registration, NOT evidence of scanning,
/// merged bytes, physical savings, or an enabled host KSM scanner. No global
/// sysfs settings are read or changed. The caller authorizes sharing and its
/// side-channel/trust-domain implications; KSM has no per-pVisor domain selector.
/// Any accepted mapping excludes cold-pager/device preparation for this VM's
/// remaining lifetime. Repeated calls are permitted; there is no automatic
/// opt-in or hot switch to a different optimization strategy.
/// `Err` denotes a stopped VM or poisoned control lock; unavailable advice and
/// strategy conflicts are reported per mapping, leaving the healthy VM usable.
pub trait RamDedupControl: Send + Sync {
    /// Advise eligible mappings under the transition lock and return per-range
    /// results. Repeat calls may register the same bytes; results never prove savings.
    fn advise_ram_dedup(&self) -> Result<RamDedupReport, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Results of one best-effort deduplication advice call; not a memory-savings measurement.
pub struct RamDedupReport {
    /// Sum of mapping lengths accepted by this call, not merged bytes. Repeated
    /// calls can accept the same bytes again; do not sum reports as savings.
    pub accepted_bytes: u64,
    /// Per-mapping outcomes, including skipped and unsupported ranges.
    pub mappings: Vec<RamDedupMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One RAM mapping considered during an advice call.
pub struct RamDedupMapping {
    /// Starting guest physical address of the considered mapping.
    pub guest_address: u64,
    /// Mapping length in bytes; not necessarily resident bytes.
    pub length: u64,
    /// Registration outcome for this mapping; does not imply pages were merged.
    pub status: RamDedupStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Outcome of deduplication advice for a single mapping.
pub enum RamDedupStatus {
    /// The host accepted asynchronous dedup advice, without proving an active scanner.
    Accepted,
    /// This mapping was excluded by its ownership, layout or active strategy.
    Skipped(RamDedupSkipReason),
    /// The host or compiled backend cannot apply this advice.
    Unsupported {
        /// Host errno when available; absence denotes a backend capability limitation.
        errno: Option<i32>,
        /// Human-readable unsupported-operation detail, not a stable error code.
        reason: String,
    },
    /// The advice attempt failed; the original RAM mapping remains in place.
    Error {
        /// Host errno when an OS advice call failed.
        errno: Option<i32>,
        /// Diagnostic error detail; callers should use the typed outcome for decisions.
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Reasons for excluding a mapping without attempting host dedup advice.
pub enum RamDedupSkipReason {
    /// Guest device/shared-memory window rather than ordinary RAM.
    DeviceWindow,
    /// Mapping is shared, so private-RAM advice is inappropriate.
    SharedMapping,
    /// Mapping lacks the required private ownership semantics.
    NotPrivate,
    /// Huge-page backing is outside this advice strategy.
    HugePages,
    /// Read-only mapping does not qualify for writable-RAM advice.
    NotWritable,
    /// Address or length does not meet the host page alignment.
    Unaligned,
    /// An active cold pager owns the RAM optimization strategy.
    ColdPagerActive,
    /// Device memory preparation already owns the affected mappings.
    DevicePreparationActive,
}

/// Full-machine capture inside a bounded CPU/device/RAM freeze. Generic actions
/// retain their result type; this extension deliberately uses static dispatch.
/// The resumable operation thaws the source after successful or rejected actions.
/// Save-and-stop capture remains parked; freeze failure requires termination.
pub trait SnapshotControl: VmControl {
    /// Freeze CPUs, drain devices and invoke action within timeout. The borrowed
    /// guard cannot escape. A completed/rejected action resumes the source; failure
    /// to establish or release the barrier requires terminating the runner.
    fn with_snapshot_quiesced<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String>;
    /// Save-and-stop freeze for running, paused or offloaded sources. Returning
    /// leaves the source parked, including a rejected action; terminate its runner.
    fn with_snapshot_frozen<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String>;
}

/// Capture requires the runtime-provided quiescence guard. The destination RAM
/// file must be fresh and empty. The caller publishes RAM, state and an owned
/// filesystem copy in the same freeze; this interface does not publish them.
pub trait SnapshotCapture {
    /// Write complete RAM to the fresh empty destination and return CPU/device
    /// state. Caller holds this freeze through filesystem capture and atomic
    /// publication; a failed write may leave partial destination bytes.
    fn capture_machine_state(&self, file: &File) -> Result<MachineSnapshot, String>;
    /// Capture all RAM, or changed blocks against the supervisor-bound immutable
    /// baseline. A delta inventory is an optimization, never restore authority.
    fn capture_machine_state_with_ram_delta(
        &self,
        file: &File,
        baseline: Option<&RamDeltaSpec>,
    ) -> Result<(MachineSnapshot, Option<RamDeltaCapture>), String>;
}

/// Experimental cold-RAM control. Unavailable backends return an explicit error.
/// Fault handlers must synchronize concurrent faults and reject unowned addresses.
pub trait ColdRamControl: VmControl {
    /// Start one VM-owned experimental pager using caller-authorized storage.
    /// Supported on Apple Silicon HVF and Linux x86_64 KVM. Local Linux cold storage requires
    /// kernel-fault userfaultfd authority (permitted /dev/userfaultfd or
    /// CAP_SYS_PTRACE); it never changes sysctls or KSM policy. Linux currently
    /// accepts only private anonymous writable RAM, rejects file-backed/COW RAM,
    /// and excludes only builder-identified raw bundled-kernel firmware by exact
    /// guest address, host pointer, length and raw-region metadata (not snapshots),
    /// and probes automatic candidates by eviction/refault, not true read-access
    /// tracking: read-hot RAM may be evicted, then restored with a cooldown.
    /// Linux disables independent balloon discard under the drained device gate
    /// before registration. Physical sharing handles free-page reports through
    /// the pager: remap to sparse anonymous zero RAM, release pool references,
    /// then acknowledge guest reuse. Local UFFD compression only acknowledges
    /// reports; ordinary balloon behavior is unchanged without a pager.
    /// Store calls must be bounded local operations. Known pending faults defer
    /// maintenance; faults racing a pause are rechecked after all CPU acknowledgements.
    /// Linux pager maintenance has a 30-second aggregate CPU-pause budget; normal
    /// pause/resume retains its 3-second budget. Queueing, restore and release must
    /// fit the caller's control budget. Arbitrary-latency external stores are not
    /// supported by this latency contract; a stalled fault/transition may terminate
    /// the runner. The generic store trait cannot identify an external transport.
    /// Snapshot capture and whole-VM offload reject an active pager. Linux owns
    /// its kernel resolver; external fault handlers/RAM tokens remain unsupported.
    /// Unsupported backends reject before spawning a worker.
    /// The store exclusively owns its session references;
    /// restore/release RPCs serialize separately from CPU/device barriers.
    /// Local storage uses 64 KiB blocks; page-granular stores use 4 KiB on Linux
    /// and 16 KiB on Apple Silicon. HVF requires ordinary writable file RAM and
    /// excludes immutable kernel/snapshot regions. Its physical store supplies
    /// read-only, unlinked backing; a trusted owner must keep referenced slots
    /// immutable and retain disconnected references until kqueue proves peer exit.
    /// With `share_resident`, Linux/HVF map immutable store pages privately:
    /// reads remain shared and writes use kernel COW, without userfaultfd. The
    /// store mapping lookup is bounded metadata access without transport calls.
    /// Page publication happens outside the CPU/device barrier; live bytes are
    /// rechecked before remapping. Snapshots are bounded to 4 MiB per batch.
    /// The worker lives with the runner; a failed mapping transition terminates
    /// that runner. Call once, before allowing control requests or guest work.
    fn start_cold_pager<S: ColdRamStore + 'static>(
        &self,
        store: S,
        options: ColdRamOptions,
    ) -> io::Result<()>;
    /// Run action behind the supported cold-RAM CPU/device barrier. None means
    /// no eligible RAM; errors require the caller to terminate after failed drain.
    /// Unavailable backends return Unsupported without invoking action.
    fn with_ram_quiesced<T>(
        &self,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<Option<T>, String>;
    /// Return best-effort resident RAM bytes, or None when unavailable for this
    /// VM layout. Unsupported platforms return an error; this is not node RSS.
    fn experimental_ram_residency(&self) -> Result<Option<u64>, String>;
    /// Install a synchronized fault resolver for owned RAM. Unsupported backends
    /// reject installation; the caller retains handler resources through VM teardown.
    fn install_ram_fault_handler(&self, handler: Arc<MemoryFaultHandler>) -> Result<(), String>;
}

/// Memory operations available within a runtime-provided quiescence window.
/// Tokens remain subject to the safety requirements of [`RamAccess`].
pub trait FrozenMemory {
    /// Create bounded RAM tokens with the requested block size within this freeze.
    /// Unsupported backends/layouts return an error; tokens do not extend VM lifetime
    /// or relax RamAccess quiescence and residency requirements.
    fn experimental_ram_blocks(&self, bytes: usize) -> Result<Vec<RamBlock>, String>;
    /// Query owned ordinary RAM on macOS while this freeze owns CPU/device
    /// quiescence; raw firmware/device windows are excluded. Returns native page
    /// size and rows [guest address, Mach object, object offset, disposition].
    /// Queries do not fault contents. Identities are not PFNs, per-process RSS,
    /// or whole-host attribution; separate VM queries are not one atomic sample.
    /// Other backends return an explicit unsupported error.
    /// This observation grants no ownership or reclaim permission.
    fn experimental_ram_page_inventory(&self) -> Result<(u64, Vec<[u64; 4]>), String>;
    /// Install a host resolver while frozen; it must reject unowned faults and
    /// serialize restoration. Failure does not authorize releasing the RAM barrier.
    fn install_memory_fault_handler(
        &mut self,
        handler: Arc<MemoryFaultHandler>,
    ) -> Result<(), String>;
    /// Install or remove bounded device RAM preparation under the VM barrier.
    /// The callback runs before device access and must not reenter VM control;
    /// unsupported layouts return an error without replacing preparation.
    fn set_device_prepare(&self, handler: Option<Arc<MemoryPrepare>>) -> Result<(), String>;
}

/// Inspect opaque state and rebind captured filesystems to caller-owned copies.
/// Rebinding preserves the old state on error; it does not copy or publish files.
pub trait SnapshotState {
    /// Return validated captured CPU count; malformed snapshot data is InvalidData.
    fn cpu_count(&self) -> io::Result<usize>;
    /// Report whether a valid captured kernel layout is present; malformed state is an error.
    fn has_kernel_layout(&self) -> io::Result<bool>;
    /// Verify captured inode identities/content while every source writer is frozen.
    fn verify_frozen_filesystem_backing(&self, tag: &str) -> io::Result<usize>;
    /// Relocate selected supervisor-verified immutable lowers, retaining private state.
    fn rebind_filesystem_lower_copies(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize>;
    /// Restore private copies with pinned, inventory-verified read-only lowers.
    fn rebind_filesystem_shared_lowers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        shared_lowers: &[PathBuf],
    ) -> io::Result<usize>;
    /// Replace access policy on the selected captured filesystem atomically.
    /// Missing tags or malformed saved state return errors; caller owns policy authorization.
    fn rebind_filesystem_policy(
        &mut self,
        tag: &str,
        policy: &pvisor_core::overlay::FileAccessPolicy,
    ) -> io::Result<usize>;
    /// Add current Attempt host-authority exclusions to a restored overlay.
    /// Existing exclusions must remain; absolute/escaping paths and additions
    /// hiding saved inode paths are rejected. Other topology remains unchanged.
    fn rebind_filesystem_exclusions(
        &mut self,
        tag: &str,
        excluded_paths: &[PathBuf],
    ) -> io::Result<usize>;
    /// Return the saved guest RAM address/file-offset inventory after structural
    /// validation. This does not open/map RAM or authorize a restore.
    fn ram_mappings(&self) -> io::Result<Vec<RamMappingSnapshot>>;
    /// Rebind one captured filesystem root to its caller-owned independent copy.
    /// Validate the saved binding before replacing it; no filesystem bytes are copied.
    fn rebind_filesystem_copy(
        &mut self,
        tag: &str,
        source: &Path,
        destination: &Path,
    ) -> io::Result<usize>;
    /// Rebind captured backing layers to caller-verified copies while preserving
    /// layer order and writable-state separation. Failure preserves the old state.
    fn rebind_filesystem_layers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize>;
    /// Restore an overlay stage while retaining explicitly leased immutable
    /// lowers at their original canonical paths. `copies` must cover upper,
    /// work, preimages and every other mutable backing with independently owned,
    /// inventory-verified copies. Retained roots may only be lower/baseline
    /// bindings, never writable stage directories. The coordinator guarantees
    /// base immutability, pins dependencies through VM exit, and validates the
    /// stage inventory before calling. Runtime validation still verifies saved
    /// inode identities/content. Failure leaves machine state unchanged.
    fn rebind_filesystem_stage(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
        immutable_lowers: &[PathBuf],
    ) -> io::Result<usize>;
}

/// Validate a native restore and map sealed RAM privately. RAM mapping alone
/// does not validate CPU/device state or authorize execution of the snapshot.
pub trait RestoreState {
    /// Check captured state and sealed RAM geometry for cpu_count. Does not
    /// prove host/boot/build compatibility; that authorization belongs to the supervisor.
    fn validate(&self, cpu_count: usize) -> Result<(), String>;
    /// Privately map the sealed file at the supplied guest ranges after checking
    /// alignment, offsets and exact topology. The returned owner retains mappings;
    /// no CPUs run and snapshot execution is not authorized by this operation.
    fn map_ram(
        &self,
        ranges: &[(vm_memory::GuestAddress, usize)],
    ) -> Result<vm_memory::GuestMemoryMmap, String>;
}

/// Experimental access to a RAM token; callers enforce quiescence and ownership.
pub trait RamAccess {
    /// Starting guest physical address; valid only while the owning VM mapping lives.
    fn guest_address(&self) -> u64;
    /// Token length in bytes; access still requires the RamAccess safety conditions.
    fn length(&self) -> usize;
    /// # Safety
    /// All CPUs/devices must be quiescent and the block resident; output must
    /// cover the entire block.
    unsafe fn snapshot(&self, output: &mut [u8]) -> io::Result<()>;
    /// # Safety
    /// Requires exclusive CPU/device quiescence and restored content before reenable.
    unsafe fn observe(&self, enabled: bool) -> Result<(), String>;
    /// # Safety
    /// Requires verified persisted content, exclusive ownership and drained devices.
    unsafe fn discard(&self) -> Result<(), String>;
    /// # Safety
    /// The original file range must be detached and never reused by a mapping.
    unsafe fn reclaim_file(&self) -> Result<(), String>;
    /// # Safety
    /// The block must be inaccessible, restores serialized and input bytes verified.
    unsafe fn restore(&self, input: &[u8]) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Compiled guest CPU architecture; not a cross-architecture migration promise.
pub enum Architecture {
    /// 64-bit Arm guest architecture.
    Aarch64,
    /// 64-bit x86 guest architecture.
    X86_64,
    /// 64-bit RISC-V architecture identifier; capability presence does not imply host support.
    Riscv64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Native virtualization backend selected by this build.
pub enum Hypervisor {
    /// Linux Kernel-based Virtual Machine backend.
    Kvm,
    /// Apple Hypervisor.framework backend.
    Hvf,
    /// Windows Hypervisor Platform backend. Requires the WHP optional feature.
    Whp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Implemented runtime primitives; host permission and installed sandbox evidence are separate.
pub struct Capabilities {
    /// Guest architecture compiled into the native backend.
    pub architecture: Architecture,
    /// Native hypervisor implementation used on this platform.
    pub hypervisor: Hypervisor,
    /// Whether the compiled profile implements whole-machine capture/restore.
    pub machine_snapshot: bool,
    /// Whether the compiled profile exposes cold-RAM fault handling; host authority is still required.
    pub cold_ram_faults: bool,
}

/// Single-use VM configuration. Methods are identical on all supported targets.
/// The private field stores owned configuration; it defines no public methods.
/// Import [`VmConfiguration`] and [`VmRuntime`] to use this type. Those traits
/// are the complete public method contract, implemented by private adapters.
pub struct VmBuilder {
    pub(crate) inner: crate::builder::Builder,
}
/// Weak live control reference; transitions are serialized internally.
#[derive(Clone)]
pub struct VmmHandle {
    pub(crate) idle: Arc<crate::guest_idle::IdleGate>,
    pub(crate) vmm: std::sync::Weak<std::sync::Mutex<crate::vmm::Vmm>>,
    pub(crate) transition: Arc<std::sync::Mutex<()>>,
    pub(crate) cold_pager_started: Arc<std::sync::atomic::AtomicBool>,
}
/// EXP-001 M0: diagnostic execution phase, not a Linux runqueue assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcpuObservedState {
    /// A backend execution-entry boundary; not proof of sustained guest execution.
    Executing,
    /// Backend is processing a VM exit; no guest-idle inference is made.
    HandlingExit,
    /// Backend entered an observed event wait, not proof of Linux idle or a safe pause.
    WaitingForEvent,
    /// Reserved for a reliable host scheduling source; M0 does not infer this.
    HostDescheduled,
    /// A control park (including snapshot/RAM maintenance), never guest idle.
    ManualPaused,
    /// Backend CPU lifecycle has ended.
    Stopped,
    /// Includes time inside KVM_RUN, which can execute, halt or be descheduled.
    Unknown,
}

#[derive(Clone, Debug)]
/// Latest diagnostic state for one backend CPU within an observation session.
pub struct VcpuObservation {
    /// Backend CPU ordinal within this VM topology.
    pub id: u8,
    /// Registered backend CPU not yet stopped, not guest Linux CPU-online state.
    /// Unbooted HVF secondaries conservatively remain Unknown and block aggregation.
    pub online: bool,
    /// Last observed phase; Unknown is retained when execution cannot be inferred.
    pub state: VcpuObservedState,
    /// Collector sequence at this CPU record update.
    pub sequence: u64,
    /// Monotonic offsets from this VM's observation origin, not guest time.
    pub since: Duration,
    /// Saturating count of observed phase changes in this session.
    pub transitions: u64,
    /// Saturating count of observed event-wait entries in this session.
    pub wait_entries: u64,
    /// Closed observation intervals, including truncation when disabling;
    /// not necessarily a backend wake event.
    pub wait_exits: u64,
    /// Observed waiting time, including intervals truncated on disable.
    /// Add sampled_at - since only for an enabled, active WaitingForEvent record.
    pub completed_wait: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Why an observation snapshot cannot even identify an all-waiting opportunity.
pub enum VcpuObservationRejection {
    /// Observation is disabled; no waiting claim can be made.
    Disabled,
    /// Not all configured backend CPUs have a complete observation record.
    TopologyIncomplete,
    /// At least one CPU phase is unknown.
    Unknown,
    /// At least one eligible CPU is outside an event wait.
    NotAllWaiting,
    /// Even a full waiting window is observe-only: no complete wake/deadline contract.
    WakeDeadlineUnavailable,
}

/// Consistent bounded snapshot: one record per configured CPU, no event backlog.
/// Sequence and epoch are VM-local; the caller must bind its own Attempt/source
/// identity. A new enable session invalidates old records. All-waiting is only
/// an opportunity, never authorization to pause/offload. No timer completeness
/// or wake delivery guarantee is provided in M0.
#[derive(Clone, Debug)]
pub struct VcpuObservationSnapshot {
    /// Backend that produced these diagnostic records.
    pub hypervisor: Hypervisor,
    /// Whether collection is enabled for this snapshot.
    pub enabled: bool,
    /// Observation enable-session identity; old sessions cannot be combined.
    pub session: u64,
    /// Topology revision captured with the CPU records.
    pub topology_generation: u64,
    /// Collector-wide sequence at the sampled snapshot.
    pub sequence: u64,
    /// Captured under the same collector lock as the cloned CPU records.
    pub sampled_at: Duration,
    /// Bounded latest record for each configured backend CPU, not a transition log.
    pub vcpus: Vec<VcpuObservation>,
    /// All eligible records are waiting; this is never pause/offload permission.
    pub all_waiting: bool,
    /// Diagnostic epoch of the current all-waiting window.
    pub idle_epoch: u64,
    /// VM-origin monotonic start of the current all-waiting window, when known.
    pub all_waiting_since: Option<Duration>,
    /// Accumulated closed all-waiting windows in this session.
    pub completed_all_waiting: Duration,
    /// Reason the snapshot lacks complete wake/deadline authority, even if all_waiting.
    pub rejection: VcpuObservationRejection,
}

/// Optional bounded diagnostics, disabled by default. Calls never kick CPUs,
/// pause, resume, offload, or start an owner thread. Cloned handles share one
/// collector. Enabling mid-wait/park leaves Unknown until the next backend
/// boundary; it does not manufacture an idle record. Repeated enable is
/// idempotent; disabling truncates active wait intervals and marks them Unknown.
/// Disabled CPU teardown updates lifecycle only, not waiting time; re-enabling
/// starts a fresh session with Unknown records for CPUs not yet stopped.
/// Sampling and backend updates serialize on a private collector lock, without
/// holding device/queue locks. A dead VM or poisoned lock returns an error;
/// observation errors do not change VM control state. Counters saturate.
pub trait VcpuObservationControl {
    /// Enable/disable the shared diagnostic collector. Repeated enables are
    /// idempotent; a new session starts Unknown, and errors leave control state unchanged.
    fn set_vcpu_observation(&self, enabled: bool) -> Result<(), String>;
    /// Clone the consistent latest snapshot under the collector lock. Does not
    /// kick CPUs, start workers or grant an idle lease; stopped owners return an error.
    fn vcpu_observation(&self) -> Result<VcpuObservationSnapshot, String>;
}

/// Access available only within a quiescence callback; cannot escape its lifetime.
pub struct FrozenMachine<'a> {
    pub(crate) inner: &'a mut crate::vmm::Vmm,
}

#[derive(Clone, Copy, Debug)]
/// Backend RAM fault delivered to an installed resolver; not an arbitrary memory-access grant.
pub struct MemoryFault {
    /// Guest physical fault address; the handler must check ownership and bounds.
    pub guest_address: u64,
    /// Backend fault syndrome; interpretation is architecture-specific.
    pub syndrome: u64,
}
/// Resolver must reject unowned addresses and synchronize concurrent CPU faults.
pub type MemoryFaultHandler = dyn Fn(MemoryFault) -> Result<bool, String> + Send + Sync;
/// Empty ranges request conservative whole-RAM preparation. Errors are fail-stop.
pub type MemoryPrepare = dyn Fn(&[(u64, usize)]) -> Result<(), String> + Send + Sync;
/// Experimental RAM mapping token. Every unsafe method requires frozen CPUs,
/// drained devices and exclusive ownership of the affected mapping/file range.
#[derive(Clone)]
pub struct RamBlock {
    pub(crate) inner: crate::memory::Block,
}
#[derive(Clone, Copy, Debug)]
/// Best-effort backing/residency report from one completed reclaim operation.
pub struct RamReclaim {
    /// Bytes covered by published backing, not necessarily released physical capacity.
    pub backed_bytes: u64,
    /// Resident-byte estimate before reclaim; None when the host cannot measure it.
    pub resident_before_bytes: Option<u64>,
    /// Resident-byte estimate after reclaim; None when the host cannot measure it.
    pub resident_after_bytes: Option<u64>,
}

/// Host-independent attachment data. The permission model is explicit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PermissionSemantics {
    /// Emulate Linux ownership, permissions, extended attributes and idmaps.
    #[default]
    LinuxComplete,
    /// Disable extended attributes and idmaps, return the guest caller's uid/gid,
    /// and store permission bits on the host instead of in extended attributes.
    LinuxSimplified,
}
/// Content receipts for an exclusively owned immutable baseline generation.
/// `root` must be the overlay's apply baseline, and `file` must be its sealed
/// import index with this SHA-256 digest. Keep the generation lease alive for
/// the VM lifetime; prohibit all external writes. This is not a mutable-file
/// cache. The runtime loads/checks the bounded index lazily on first mutation,
/// and continues checking fresh metadata and xattrs. Missing/corrupt receipts
/// fail before upper mutation. Retained baselines retain their index on restore.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineContentIndex {
    /// Canonical immutable baseline root retained by the caller-owned generation lease.
    pub root: PathBuf,
    /// Sealed baseline content-index file bound to root.
    pub file: PathBuf,
    /// Lowercase SHA-256 hex digest authenticating the content index bytes.
    pub sha256: String,
}

#[derive(Clone, Debug)]
/// Host paths retain their native representation. The runtime validates UTF-8
/// atomically before replacing configuration for the backend that requires it.
pub struct OverlayConfig {
    /// Physical lower roots in highest-to-lowest overlay priority order.
    pub lower_dirs: Vec<PathBuf>,
    /// Stability promises in physical lower order (highest priority first).
    /// New attachments default an empty list to all Immutable. Transparent/live
    /// sources must explicitly declare Mutable. A nonempty mismatch is InvalidInput
    /// before configuration replacement. Caller guarantees contents, metadata,
    /// namespace, ancestors and mount identities until device teardown, including
    /// restore lifetimes. Read-only mounts and frozen baselines are not proof.
    /// This never describes upper or merged-view stability. Legacy saved device
    /// records with no declarations retain mutable semantics when restored.
    /// On the root device, read-only regular-file OPENs from an explicitly
    /// immutable lower retain guest data pages across opens by default. OPEN
    /// authorization and baseline observations still run. When every physical
    /// lower is explicitly immutable, the guest may also import directory images
    /// with long metadata validity. Mutable lowers use ordinary
    /// FUSE metadata timeouts and lookup. Upper files do not use the data-cache
    /// optimization.
    pub lower_mutability: Vec<pvisor_core::overlay::LayerMutability>,
    /// Writable stage, exclusively owned by this VM's filesystem connection.
    /// Read-only observers may coexist without a freshness guarantee; concurrent
    /// external writers are unsupported regardless of lower declarations.
    pub upper_dir: PathBuf,
    /// Optional private work directory; must be separate from lower/target and on the upper filesystem.
    pub work_dir: Option<PathBuf>,
    /// Optional exclusively owned preimage journal directory for review/apply observations.
    pub preimage_dir: Option<PathBuf>,
    /// Separate host destination for reviewed changes; configuring it does not apply anything.
    pub apply_target: Option<PathBuf>,
    /// Optional baseline root for conflict observations, independent of merged lower selection.
    pub baseline_lower: Option<PathBuf>,
    /// Optional leased immutable baseline content receipt; absence keeps ordinary fingerprinting.
    pub baseline_content_index: Option<BaselineContentIndex>,
    /// Relative paths excluded from the merged view and protected against recursive effects.
    pub excluded_paths: Vec<PathBuf>,
    /// Shared path access policy checked on each serving operation.
    pub access_policy: pvisor_core::overlay::FileAccessPolicy,
    /// Host/guest ownership, permissions and xattr translation model.
    pub semantics: PermissionSemantics,
}

/// Opaque backend state, preserving the existing serialized machine-state format.
/// Deserialize is not validation: restore validates CPU/device/RAM state internally.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct MachineSnapshot {
    pub(crate) state: serde_json::Value,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// One captured guest RAM region bound to a range in the sealed RAM file.
pub struct RamMappingSnapshot {
    /// Guest physical starting address, never a host pointer.
    pub base: u64,
    /// Length in bytes; restore checks topology and page alignment.
    pub len: u64,
    /// Byte offset of this region within the captured RAM file.
    pub file_offset: u64,
}
/// Supervisor-bound immutable MAP_PRIVATE baseline for optional delta capture.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamDeltaSpec {
    /// Device identity of the caller-pinned immutable baseline file.
    pub device: u64,
    /// Inode identity of the caller-pinned immutable baseline file.
    pub inode: u64,
    /// Logical baseline RAM length in bytes, bounded by validate().
    pub length: u64,
    /// Power-of-two block size, 4 KiB through 1 MiB; validate also bounds block count.
    pub block_bytes: u32,
    /// Lowercase SHA-256 of the complete immutable baseline contents.
    pub base_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// Versioned inventory of blocks captured differently from an immutable RAM baseline.
pub struct RamDeltaCapture {
    /// Delta encoding version; only version 1 is accepted.
    pub version: u32,
    /// Logical RAM bytes described by this delta inventory.
    pub length: u64,
    /// Block size in bytes; must match the validated baseline geometry.
    pub block_bytes: u32,
    /// SHA-256 identity of the unchanged baseline needed to reconstruct RAM.
    pub base_sha256: String,
    /// Sorted unique changed blocks, including blocks cleared to zero.
    pub changed_blocks: Vec<u64>,
}
/// Structural validation of baseline/delta geometry; never grants restore authority.
pub trait RamDeltaState {
    /// Reject invalid lengths/block sizes/digests and, for captures, unsupported
    /// versions or unordered/duplicate/out-of-range indices. Does not open or hash a file.
    fn validate(&self) -> Result<(), String>;
}

/// Caller-authorized machine state and retained sealed RAM used for private restore.
pub struct MachineRestore {
    /// CPU/device/kernel state; still validated against host and RAM topology at restore.
    pub state: MachineSnapshot,
    /// Shared owner of sealed RAM bytes; caller keeps contents immutable for all restored VMs.
    pub ram_file: Arc<File>,
}

/// Immutable block storage supplied by the host's pool/service adapter.
/// Object references belong to one store session; successful `put` acquires one
/// reference, and `release` consumes it. `restore` must verify the entire block
/// before returning success. `WouldBlock` from put certifies a healthy admission
/// rejection with no acquired reference; other errors require a stats check to
/// distinguish capacity rejection from a broken connection. No VM pointers or
/// OS mapping state cross this boundary. The pager never calls put under a VM barrier.
pub trait ColdRamStore: Send {
    /// Session-owned immutable block reference; Send does not permit cross-session reuse.
    type Object: Send;
    /// Publish verified immutable bytes and acquire exactly one reference.
    /// WouldBlock guarantees no reference was acquired; other errors may be uncertain.
    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object>;
    /// Restore and verify the entire referenced block into the exact output
    /// buffer before returning success. Partial/error bytes must not be used as restored RAM.
    fn restore(&mut self, object: &Self::Object, output: &mut [u8]) -> io::Result<()>;
    /// Consume one session reference after all dependent mappings are removed.
    /// The caller must reconcile uncertain errors rather than assuming release succeeded.
    fn release(&mut self, object: Self::Object) -> io::Result<()>;
    /// Return current storage/reference accounting, or an error for an unhealthy
    /// session; concurrent cross-session counts may be approximate.
    fn stats(&mut self) -> io::Result<ColdRamPoolStats>;
    /// An optional immutable, read-only pool file page. The object pins its slot
    /// until release; callers must unmap it before releasing the object. On a
    /// lost connection, a host pool must retain slots until the VM process exits.
    /// Mappings and references must stay in that process: inheriting mappings
    /// across fork or transferring their authority to another process is unsupported.
    fn shared_mapping(&self, object: &Self::Object) -> Option<SharedRamMapping>;
}

/// Pinned read-only pool-file region supplied for private shared-page mappings.
pub struct SharedRamMapping {
    /// Read-only file owner; the associated object reference must also remain pinned.
    pub file: Arc<File>,
    /// Byte offset of the object slot; the caller validates alignment and file bounds.
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, Default)]
/// Explicit cold-RAM strategy preferences; all false by default and validated on pager start.
pub struct ColdRamOptions {
    /// Enable optional diagnostic metrics; it does not change reclaim correctness or limits.
    pub metrics: bool,
    /// Request native pages for a shared host pool: 4 KiB on Linux x86_64,
    /// 16 KiB on Apple Silicon/HVF. HVF requires `share_resident` as well.
    /// Other backends reject this option. The default retains the
    /// backend's larger compression blocks for instance-local stores.
    pub page_granular: bool,
    /// Linux/HVF pool pages remain shared on reads and use kernel COW on writes.
    /// Requires page_granular and a store providing pinned shared_mapping slots.
    /// No UFFD permission is needed for this mode. Snapshots/offload remain
    /// incompatible; remapping takes the CPU/device barrier and rechecks bytes.
    pub share_resident: bool,
}

#[derive(Clone, Copy, Debug, Default)]
/// Process-local outstanding cold-RAM work, not node memory consumption.
pub struct ColdRamActivity {
    /// Bytes of pending file-backed work, or zero with no such worker.
    pub pending_file_bytes: u64,
    /// Bytes of pending snapshot-related work, or zero with no such worker.
    pub pending_snapshot_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
/// Store-reported accounting for the current session and shared immutable objects.
pub struct ColdRamPoolStats {
    /// Bytes charged by the store encoding; not decoded VM RAM or process RSS.
    pub encoded_bytes: u64,
    /// Number of currently retained immutable objects.
    pub objects: u64,
    /// Object references held by this store session, including local aliases.
    pub session_references: u64,
    /// Objects referenced by multiple sessions; a live approximate sharing count.
    pub cross_session_objects: u64,
}

/// Writable staging storage behind a VM's mmap-compatible RAM file.
/// Methods execute on the FUSE worker; they must be bounded and must not call
/// back into VM control. `flush_writes` drains staging, not checkpoint publication.
/// Readers observe staged writes without requiring generation commit.
pub trait RamFileStore: Send {
    /// Current logical RAM-file length in bytes; no generation is committed.
    fn logical_bytes(&self) -> u64;
    /// Resize staging storage to size bytes, preserving the store resize/error
    /// contract; failure may retain partial storage effects and must be surfaced.
    fn set_len(&mut self, size: u64) -> io::Result<()>;
    /// Read at offset into output, returning the bytes read; zero at EOF.
    /// Must remain bounded and avoid VM-control reentry from the FUSE worker.
    fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<usize>;
    /// Write all input bytes at offset into staging, or report an I/O error.
    /// A failed write may have partial effects; this is not a generation commit.
    fn write_at(&mut self, offset: u64, input: &[u8]) -> io::Result<()>;
    /// Drain staged writes according to the store contract. Does not publish
    /// a checkpoint/generation or authorize removing the backing owner.
    fn flush_writes(&self) -> io::Result<()>;
}

/// Owns the FUSE session and its private mount directory. Close mapped/file
/// users before dropping this owner; drop unmounts before deleting the directory.
pub struct RamFileMount {
    pub(crate) _inner: crate::ram_file::Mount,
}

/// Construct a mount owner and RAM file whose consumers must close before owner drop.
pub trait RamFileMapping: Sized {
    /// Create one mmap-compatible RAM inode in a private child of `directory`.
    /// The caller owns cache authorization and generation commit. The runtime
    /// owns mount readiness, request bounds, cached I/O and session lifetime.
    /// Requires the host's FUSE support; mount/readiness failure returns an error.
    fn mount(
        store: Arc<std::sync::Mutex<dyn RamFileStore>>,
        directory: &Path,
    ) -> io::Result<(Self, File)>;
}

/// Versioned whole-VM idle lease state, copied without acquiring VMM/device locks.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestIdleStatus {
    /// Boot-local ordinal of the accepted PIO offer, counting rejected offers too.
    /// None before acceptance in this epoch; retained after wake/retirement until
    /// the next arm. Correlates optional guest timer diagnostics without changing
    /// ABI v2. It is observation only, never permission to pause or skip a timer.
    #[serde(default)]
    pub offer_sequence: Option<u64>,
    /// Monotonically increasing lease identity; zero means never armed.
    pub epoch: u64,
    /// Changes on arm, offer, claim, wake or retirement. Notifications coalesce.
    pub sequence: u64,
    /// Whether guest handoff was explicitly configured at boot.
    pub enabled: bool,
    /// Whether all configured CPUs have completed the current idle handoff.
    pub offered: bool,
    /// Whether an atomic conditional pause has claimed the offer.
    pub claimed: bool,
    /// Remaining conservative host-clock budget. Zero invalidates the offer.
    /// u64::MAX explicitly denotes an accepted ABI-v2 offer with no guest timer;
    /// before offer this is the acquisition budget, afterwards the real timer.
    pub remaining_ns: u64,
    /// Latched terminal reason: guest_timer, handoff_timeout, device events or cancel.
    pub wake_reason: Option<String>,
    /// Candidate Host-owned filesystem READ/read-only OPEN work, including staging.
    /// Detached backend work retains no guest RAM views; the device RAM gate
    /// independently checks staging/completion accesses. A hint never permits pause.
    pub pending_host_reads: u64,
    /// Configured minimum remaining timer budget for reclaim admission, in ns.
    pub offload_minimum_ns: u64,
    /// Lifetime count of reclaim attempts skipped solely for insufficient budget.
    /// A skip keeps the claimed pause and its original wake deadline intact.
    pub offload_deadline_skips: u64,
}
/// Host ownership of a guest idle handoff. Linux/KVM x86-64, one or two vCPUs.
/// No callbacks call lifecycle controls. All notifications are latched independently
/// of guest RAM. Poll/wait is bounded; cancellation retires a lease but never
/// overrides an explicit pause. Unsupported calls return errors without effects.
pub trait GuestIdleControl {
    /// Arm acquisition for >2 ms and at most one second; a new arm requires
    /// retirement of the previous lease. An accepted offer replaces this timeout
    /// with the earliest guest timer (or explicit no timer). Dual-vCPU standby
    /// witnesses survive arm but both CPUs must revalidate pending interrupts;
    /// standby alone never grants pause permission. Returns its
    /// epoch. Does not pause the VM. The owner must independently detect
    /// controller failure and service external wake notifications.
    fn arm_guest_idle(&self, maximum: Duration) -> Result<u64, String>;
    /// Replace a retired, claimed idle epoch with a fresh acquisition, then resume.
    /// Requires the matching paused VM, revoked offer and >2 ms..=1 s budget.
    /// Serialized with native controls; the new arm precedes device/vCPU release.
    /// Returns the new authoritative epoch. This grants no pause permission:
    /// a fresh guest offer and pending-interrupt check are still required.
    /// The caller must retire the old listener before calling and own a new wake
    /// listener before release. Validation errors have no effect; native resume
    /// errors revoke the new arm and require runner termination. Unsupported
    /// profiles cannot own a claimed epoch. Ordinary resume never rearms.
    fn resume_and_arm_guest_idle(&self, epoch: u64, maximum: Duration) -> Result<u64, String>;
    /// Read latest status and expire any elapsed deadline.
    fn guest_idle_status(&self) -> GuestIdleStatus;
    /// Wait for a sequence change or timeout (at most 100 ms), also servicing
    /// the timer deadline. Must run independently of the control-command thread.
    fn wait_guest_idle(&self, sequence: u64, timeout: Duration) -> GuestIdleStatus;
    /// Atomically claim the matching, unrevoked offer, then complete native pause.
    /// False is a normal stale/expired denial with no pause. True requires explicit
    /// resume even if a wake raced the pause. Transition errors require termination.
    fn pause_guest_idle(&self, epoch: u64) -> Result<bool, String>;
    /// Optionally offload writable shared file-backed RAM after conditional pause.
    /// Two-vCPU reclaim requires both validated idle witnesses and uses their
    /// earliest timer deadline. Any wake before admission skips reclaim; a wake
    /// after admission stays latched until reclaim and serialized resume finish.
    /// Requires the matching claimed lease, more than the configured minimum
    /// timer budget remaining (or explicit no timer), no previous
    /// offload admission in that lease and an immediately idle device RAM gate.
    /// Serialized with pause/resume; rechecks the lease after closing the gate.
    /// None means admission was skipped without reclaim; the VM stays paused and
    /// its owner must continue monitoring wakes and resume before delivery. Some reports best-effort residency, not guaranteed
    /// capacity release. Once admitted, writeback cannot be interrupted: wakes
    /// remain latched independently, and the owner must await host backing
    /// publication before resume. The timer deadline is not a completion bound.
    /// Calling before pause or with a gate already closed by offload/maintenance
    /// is an error, not a skip. Errors require runner termination. Unsupported
    /// profiles cannot obtain a claimed lease. Never resumes or renews the lease.
    fn offload_guest_idle(&self, epoch: u64) -> Result<Option<RamReclaim>, String>;
    /// Retire only the matching epoch, wake its handoff and prevent stale reuse.
    /// This does not resume an already paused VM; the owner must call resume.
    fn retire_guest_idle(&self, epoch: u64);
}
