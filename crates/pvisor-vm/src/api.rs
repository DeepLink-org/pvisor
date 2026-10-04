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
//!
//! # Filesystems and networking
//! Disable implicit init before attaching a native-init `/dev/root`. Filesystem
//! tags and virtual paths are unique. Host paths currently require UTF-8. DAX is
//! opt-in; pVisor overlays use zero DAX size. Synthetic files own their bytes.
//! Vsock never implicitly enables TSI, even when no network device is configured.
//!
//! # Snapshots and transitions
//! Restore input requires caller-validated host/boot/build identity, sealed RAM,
//! an independent rootfs copy and exclusive execution ownership. Restored VMs
//! start paused; explicitly resume only after installation in the ready callback.
//! `MachineSnapshot` preserves backend state as an opaque serialization payload;
//! identical Rust API shapes do not promise cross-architecture restore.
//! Freeze/drain failure parks the VM and requires runner termination. Returning
//! from a snapshot action resumes the source; cold publication must terminate
//! the source while frozen. Persistence and atomic rootfs/RAM publication remain
//! the caller's responsibility. Live handles weakly reference the owning VM.
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
    pub cpus: u8,
    pub memory_mib: u32,
}

/// Runtime services with the same signatures on every supported target.
pub struct VmPlatform;
pub trait RuntimeSupport {
    fn capabilities() -> Capabilities;
    /// Initialize process logging before constructing the VM.
    fn init_logging(filter: &str);
    /// Build-embedded kernel, if present. This does not load host firmware.
    /// The shared bytes are immutable; addresses describe the guest boot layout.
    fn embedded_kernel() -> Option<KernelImage>;
    /// Firmware ABI library name selected internally for the host platform.
    /// Process-local outstanding cold-RAM work; zero when no pager is active.
    /// These counters are diagnostic bounds, not physical memory attribution.
    fn cold_ram_activity() -> ColdRamActivity;
    fn firmware_name() -> &'static str;
    /// Pinned firmware release version used by automatic provisioning.
    fn firmware_version() -> &'static str;
    /// Firmware next to the current executable, if installed. No download or load.
    fn bundled_firmware_directory() -> Option<PathBuf>;
    /// Provision pinned, SHA-256-verified firmware in a per-user cache. `None`
    /// selects the platform cache under pvisor/firmware. A custom root must be
    /// trusted and exclusively controlled by the caller. This blocking operation
    /// can download and, on macOS, invoke /usr/bin/cc; use it before sandboxing.
    /// Concurrent installs serialize through a file lock. Unsupported platforms
    /// return an error without requiring target-specific calls from consumers.
    fn prepare_firmware(cache_root: Option<&Path>) -> io::Result<PathBuf>;
}

#[derive(Clone, Debug)]
pub struct KernelImage {
    pub bytes: Arc<[u8]>,
    pub guest_address: u64,
    pub entry_address: u64,
}

/// Command consumed by a caller-supplied init that understands KRUN_INIT,
/// KRUN_WORKDIR and kernel-provided arguments/environment. It does not configure
/// the built-in Rust supervisor: use its launch JSON for normal pVisor workloads.
/// Nothing is inherited from the host environment. Paths must be absolute;
/// unsupported quoting/control characters and oversized commands are rejected.
#[derive(Clone, Debug)]
pub struct GuestCommand {
    pub executable: String,
    pub arguments: Vec<String>,
    pub environment: std::collections::BTreeMap<String, String>,
    pub working_directory: String,
}

/// Guest init networking behavior. It never enables host-network TSI bypass.
#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkOptions {
    /// Request DHCP from a caller-supplied init that supports KRUN_DHCP.
    pub guest_dhcp: bool,
}

/// Configure owned resources before boot. Each successful call transfers the
/// supplied resource into this configuration. Failed calls leave the previous
/// configuration usable; init selection precedes root filesystem attachment.
pub trait VmConfiguration: Sized {
    /// Construct a configuration; equivalent to `from_config` with explicit sizing.
    fn new(cpus: u8, memory_mib: u32) -> io::Result<Self>;
    fn from_config(config: VmConfig) -> io::Result<Self>;
    fn ram_backing(&mut self, file: File) -> io::Result<()>;
    fn embedded_kernel(&mut self, bytes: &[u8], guest_addr: u64, entry_addr: u64)
        -> io::Result<()>;
    fn disable_implicit_init(&mut self) -> io::Result<()>;
    /// Configure a custom init's command; requires implicit init to be disabled.
    /// Validate atomically before replacing the previous command.
    fn guest_command(&mut self, command: GuestCommand) -> io::Result<()>;
    fn filesystem(&mut self, tag: &str, path: &Path, shm_size: usize) -> io::Result<()>;
    fn overlay(&mut self, tag: &str, overlay: OverlayConfig, shm_size: usize) -> io::Result<()>;
    fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: impl Into<Arc<[u8]>>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()>;
    fn network(&mut self, stream: std::os::unix::net::UnixStream, mac: [u8; 6]) -> io::Result<()>;
    fn network_with_options(
        &mut self,
        stream: std::os::unix::net::UnixStream,
        mac: [u8; 6],
        options: NetworkOptions,
    ) -> io::Result<()>;
    fn vsock_port(&mut self, guest_port: u32, host_path: PathBuf, listen: bool) -> io::Result<()>;
    fn snapshot_profile(&mut self) -> io::Result<()>;
    fn machine_restore(&mut self, restore: MachineRestore) -> io::Result<()>;
}

/// Consume configuration and run one VM in its dedicated runner process. The
/// callback receives live control after installation and must return promptly.
/// Normal guest shutdown exits the process; setup failure returns an error.
pub trait VmRuntime: Sized {
    fn run(self, on_ready: impl FnOnce(VmmHandle) -> io::Result<()>) -> io::Result<()>;
}

/// Object-safe live control, suitable for supervision and mock implementations.
/// Implementations serialize transitions; handles do not extend VM lifetime.
/// A failed drain/offload transition requires terminating the runner.
pub trait VmControl: Send + Sync {
    fn is_paused(&self) -> Result<bool, String>;
    fn pause(&self) -> Result<(), String>;
    fn resume(&self) -> Result<(), String>;
    fn offload_ram(&self) -> Result<RamReclaim, String>;
}

/// Full-machine capture inside a bounded CPU/device/RAM freeze. Generic actions
/// retain their result type; this extension deliberately uses static dispatch.
/// Returning from the action resumes the source, including a failed action.
/// Freeze failure instead leaves the source parked and requires termination.
pub trait SnapshotControl: VmControl {
    fn with_snapshot_quiesced<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<T, String>;
}

/// Capture requires the runtime-provided quiescence guard. The destination RAM
/// file must be fresh and empty. The caller publishes RAM, state and an owned
/// filesystem copy in the same freeze; this interface does not publish them.
pub trait SnapshotCapture {
    fn capture_machine_state(&self, file: &File) -> Result<MachineSnapshot, String>;
}

/// Experimental cold-RAM control. Unavailable backends return an explicit error.
/// Fault handlers must synchronize concurrent faults and reject unowned addresses.
pub trait ColdRamControl: VmControl {
    /// Start one VM-owned experimental pager using caller-authorized storage.
    /// Supported on Apple Silicon HVF. Unsupported backends reject before
    /// spawning a worker. The store exclusively owns its session references;
    /// restore/release RPCs serialize separately from CPU/device barriers.
    /// Blocks are 64 KiB; snapshots are bounded to 4 MiB per sampling batch.
    /// The worker lives with the runner; a failed mapping transition terminates
    /// that runner. Call once, before allowing control requests or guest work.
    fn start_cold_pager<S: ColdRamStore + 'static>(
        &self,
        store: S,
        options: ColdRamOptions,
    ) -> io::Result<()>;
    fn with_ram_quiesced<T>(
        &self,
        action: impl FnOnce(&mut FrozenMachine<'_>) -> Result<T, String>,
    ) -> Result<Option<T>, String>;
    fn experimental_ram_residency(&self) -> Result<Option<u64>, String>;
    fn install_ram_fault_handler(&self, handler: Arc<MemoryFaultHandler>) -> Result<(), String>;
}

/// Memory operations available within a runtime-provided quiescence window.
/// Tokens remain subject to the safety requirements of [`RamAccess`].
pub trait FrozenMemory {
    fn experimental_ram_blocks(&self, bytes: usize) -> Result<Vec<RamBlock>, String>;
    fn experimental_ram_page_inventory(&self) -> Result<(u64, Vec<[u64; 4]>), String>;
    fn install_memory_fault_handler(
        &mut self,
        handler: Arc<MemoryFaultHandler>,
    ) -> Result<(), String>;
    fn set_device_prepare(&self, handler: Option<Arc<MemoryPrepare>>) -> Result<(), String>;
}

/// Inspect opaque state and rebind captured filesystems to caller-owned copies.
/// Rebinding preserves the old state on error; it does not copy or publish files.
pub trait SnapshotState {
    fn cpu_count(&self) -> io::Result<usize>;
    fn ram_mappings(&self) -> io::Result<Vec<RamMappingSnapshot>>;
    fn rebind_filesystem_copy(
        &mut self,
        tag: &str,
        source: &Path,
        destination: &Path,
    ) -> io::Result<usize>;
    fn rebind_filesystem_layers(
        &mut self,
        tag: &str,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<usize>;
}

/// Validate a native restore and map sealed RAM privately. RAM mapping alone
/// does not validate CPU/device state or authorize execution of the snapshot.
pub trait RestoreState {
    fn validate(&self, cpu_count: usize) -> Result<(), String>;
    fn map_ram(
        &self,
        ranges: &[(vm_memory::GuestAddress, usize)],
    ) -> Result<vm_memory::GuestMemoryMmap, String>;
}

/// Experimental access to a RAM token; callers enforce quiescence and ownership.
pub trait RamAccess {
    fn guest_address(&self) -> u64;
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
pub enum Architecture {
    Aarch64,
    X86_64,
    Riscv64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hypervisor {
    Kvm,
    Hvf,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub architecture: Architecture,
    pub hypervisor: Hypervisor,
    pub machine_snapshot: bool,
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
    pub(crate) inner: crate::handle::Handle,
}
/// Access available only within a quiescence callback; cannot escape its lifetime.
pub struct FrozenMachine<'a> {
    pub(crate) inner: &'a mut crate::vmm::Vmm,
}

#[derive(Clone, Copy, Debug)]
pub struct MemoryFault {
    pub guest_address: u64,
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
pub struct RamReclaim {
    pub backed_bytes: u64,
    pub resident_before_bytes: Option<u64>,
    pub resident_after_bytes: Option<u64>,
}

/// Host-independent attachment data. The permission model is explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionSemantics {
    LinuxComplete,
    LinuxSimplified,
}
#[derive(Clone, Debug)]
/// Host paths retain their native representation. The runtime validates UTF-8
/// atomically before replacing configuration for the backend that requires it.
pub struct OverlayConfig {
    pub lower_dirs: Vec<PathBuf>,
    pub upper_dir: PathBuf,
    pub work_dir: Option<PathBuf>,
    pub preimage_dir: Option<PathBuf>,
    pub apply_target: Option<PathBuf>,
    pub baseline_lower: Option<PathBuf>,
    pub excluded_paths: Vec<PathBuf>,
    pub access_policy: pvisor_overlay_core::FileAccessPolicy,
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
pub struct RamMappingSnapshot {
    pub base: u64,
    pub len: u64,
    pub file_offset: u64,
}
pub struct MachineRestore {
    pub state: MachineSnapshot,
    pub ram_file: Arc<File>,
}

/// Immutable block storage supplied by the host's pool/service adapter.
/// Object references belong to one store session; successful `put` acquires one
/// reference, and `release` consumes it. `restore` must verify the entire block
/// before returning success. Rejected puts must leave the session usable; stats
/// distinguishes capacity rejection from a broken connection. No VM pointers or
/// OS mapping state cross this boundary. The pager never calls put under a VM barrier.
pub trait ColdRamStore: Send {
    type Object: Send;
    fn put(&mut self, bytes: &[u8]) -> io::Result<Self::Object>;
    fn restore(&mut self, object: &Self::Object, output: &mut [u8]) -> io::Result<()>;
    fn release(&mut self, object: Self::Object) -> io::Result<()>;
    fn stats(&mut self) -> io::Result<ColdRamPoolStats>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ColdRamOptions {
    pub metrics: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ColdRamActivity {
    pub pending_file_bytes: u64,
    pub pending_snapshot_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ColdRamPoolStats {
    pub encoded_bytes: u64,
    pub objects: u64,
    pub session_references: u64,
    pub cross_session_objects: u64,
}

/// Writable staging storage behind a VM's mmap-compatible RAM file.
/// Methods execute on the FUSE worker; they must be bounded and must not call
/// back into VM control. `flush_writes` drains staging, not checkpoint publication.
/// Readers observe staged writes without requiring generation commit.
pub trait RamFileStore: Send {
    fn logical_bytes(&self) -> u64;
    fn set_len(&mut self, size: u64) -> io::Result<()>;
    fn read_at(&self, offset: u64, output: &mut [u8]) -> io::Result<usize>;
    fn write_at(&mut self, offset: u64, input: &[u8]) -> io::Result<()>;
    fn flush_writes(&self) -> io::Result<()>;
}

/// Owns the FUSE session and its private mount directory. Close mapped/file
/// users before dropping this owner; drop unmounts before deleting the directory.
pub struct RamFileMount {
    pub(crate) _inner: crate::ram_file::Mount,
}

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
