//! Portable launch, host-only control and PID-1 exit contracts.
//!
//! Configuration and message records do not install isolation or authorize host
//! access. Guest paths use Linux syntax on every host. Only the Linux PID-1
//! supervisor mounts filesystems, applies process policy and executes workloads.
//! Launch validation performs no I/O; parsing control messages does
//! not validate operation-specific authority, process generations or stream credits.
//! The host-only adapter owns admission and lifecycle checks before dispatch.
//!
//! Exit claims belong only to root/PID 1 on the root virtio-fs device before
//! workloads start. Tokens stay in supervisor memory, outside JSON, arguments,
//! environment and workload descriptors. Report once after reaping and filesystem
//! synchronization. Both peers require a trusted kernel/PID 1; encoding alone
//! neither authenticates a caller nor consumes the exit capability.

//! Host consumers validate records without receiving executable commands:
//! ```compile_fail
//! use pvisor_guest::api::{GuestConfig, GuestValidation};
//! let command = GuestConfig::default().command();
//! ```
//! Implementation modules are private; import records from api:
//! ```compile_fail
//! use pvisor_guest::control::Request;
//! ```

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::PathBuf,
};

/// Absolute guest path of the host-supplied launch JSON.
pub const CONFIG_PATH: &str = "/.pvisor-guest.json";

/// Kernel-supplied PID 1 environment key. A value of `1` selects a bootstrap
/// root that contains no workload files; set it only with the mount below.
pub const DEFERRED_ROOT_ENV: &str = "PVISOR_DEFERRED_ROOT";
/// Virtio-fs tag of the workload root mounted by PID 1 before reading launch JSON.
pub const DEFERRED_ROOT_TAG: &str = "pvisor-rootfs";
/// Absolute mountpoint supplied by the trusted bootstrap filesystem.
pub const DEFERRED_ROOT_MOUNTPOINT: &str = "/.pvisor-rootfs";

/// A verified OCI layer read through the bootstrap's read-only `pvisor-layers`
/// share. Paths have Linux syntax and must be immediate SHA-256-named children
/// of `/.pvisor-layers`. The guest verifies compressed bytes before extraction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageLayer {
    /// Lowercase SHA-256 hex digest, without a prefix.
    pub sha256: String,
    /// OCI media type: an uncompressed tar or gzip layer.
    pub media_type: String,
}

/// Private Linux filesystem backing for one controlled workload. The caller
/// attaches an already formatted ext4 raw disk; no host filesystem tree is used
/// for image paths, symlinks, ownership or permissions. Layers are applied in
/// order on first boot, inside a chroot without proc/dev/sys mounted. The
/// supervisor then installs pseudo-filesystems and launches the normal process
/// supervisor. Extraction errors abort launch and may leave a partial disk;
/// callers must not publish that disk as a prepared image.
/// With GuestConfig.network, each entry installs a resolver for that virtual
/// gateway, replacing image resolver files/symlinks but rejecting directories.
/// Without networking, image resolver files remain unchanged.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootDisk {
    /// Linux block path, `/dev/vda` through `/dev/vdz`.
    pub device: String,
    /// Verified first-boot layers; empty reopens an existing filesystem.
    pub layers: Vec<ImageLayer>,
}

/// Private guest RAM filesystem created before launching the workload.
/// The path must be a fresh absolute directory with an existing parent.
/// `size_bytes` caps filesystem capacity inside the existing guest RAM budget;
/// it does not reserve or add RAM. Cold launches start empty. VM RAM snapshots
/// preserve its contents; it does not belong to the host workspace stage.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryFilesystem {
    /// Absolute guest path; tmpfs additionally rejects traversal and workspace overlap.
    pub path: PathBuf,
    /// Positive tmpfs capacity charged inside existing guest RAM.
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Owned Linux launch message. Default is an empty, non-launchable value; validation
/// checks it before any workload/sidecar effects. Unknown fields
/// are rejected. Optional fields retain legacy defaults when absent.
pub struct GuestConfig {
    /// Nonempty program followed by literal NUL-free arguments; no shell expansion.
    pub argv: Vec<String>,
    /// Literal environment; names must be nonempty without equals/NUL, values NUL-free.
    pub env: BTreeMap<String, String>,
    /// Absolute NUL-free Linux guest working directory; existence is checked in the guest.
    pub cwd: PathBuf,
    #[serde(default)]
    /// Optional absolute NUL-free guest workspace mount; absent by default.
    pub workspace: Option<PathBuf>,
    /// Linux RLIMIT names, with soft and hard limits in native units.
    #[serde(default)]
    pub limits: BTreeMap<String, (u64, u64)>,
    #[serde(default)]
    /// Optional static guest IPv4 configuration; absent does not install a network.
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    /// Optional agent helper argv; when present it needs a nonempty, NUL-free program.
    pub agent: Option<Vec<String>>,
    /// Daemons started alongside the workload (see [`SidecarProcess`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<SidecarProcess>,
    /// AF_VSOCK → guest-TCP relays (see [`PortBridge`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<PortBridge>,
    /// Absolute mount point of the read-only shared payloads filesystem
    /// (virtio-fs tag `pvisor-payloads`); staged payload symlinks resolve
    /// here. One host copy serves every guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payloads_dir: Option<PathBuf>,
    /// Host directories shared into the guest as virtio-fs mounts (pod
    /// volume bind mounts the VM tier re-expresses as shares; see
    /// [`GuestShare`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<GuestShare>,
    /// Named virtio-console ports required for stdin, stdout and stderr.
    /// The runner fills this from its actual descriptors before booting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdio_ports: Option<[bool; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional private guest tmpfs; startup requires a fresh directory and valid capacity.
    pub temporary_filesystem: Option<TemporaryFilesystem>,
    /// Optional process setup. Omission inherits the bootstrap identity, session
    /// and console descriptors; all workloads use the same process supervisor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessConfig>,
    /// Additional virtio-fs shares installed before the workload starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    /// Enable the host-only vsock command adapter; false starts no listener.
    #[serde(default)]
    pub control: bool,
    /// After starting the command adapter, notify the host at `CONTROL_READY_PORT`.
    /// Requires `control`; the caller must bind the host listener before boot.
    /// A connection wakes the host but does not prove workload or service identity.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub control_ready: bool,
    /// Host-only virtio-console control adapter. Uses the same
    /// request/reply protocol and process registry as vsock, with independent
    /// command and signal ports so stop cannot queue behind a long exec.
    /// Requires explicit safe capability bounds, owned pipe/closed stdin and a new session;
    /// privileged device creation, ptrace and mount/driver administration are
    /// rejected. Device paths are masked before any workload is started.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub serial_control: bool,
    /// Optional ext4-backed Linux workload root, separate from bootstrap shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_disk: Option<RootDisk>,
}

/// Process setup shared by the initial workload and subsequent exec requests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessConfig {
    /// Guest UID/name and optional group. None preserves the bootstrap identity;
    /// Some(empty) selects root and defaults missing HOME from guest passwd.
    /// None preserves the launch environment, including absent HOME. Omitting
    /// ProcessConfig entirely retains the legacy root HOME default.
    pub user: Option<String>,
    /// Linux capability bounding set to retain. None leaves it unchanged.
    /// Values must be in 0..64; dropping capabilities precedes changing identity.
    pub capabilities: Option<Vec<u32>>,
    /// Start a separate session/process group, required for PTYs and control.
    pub new_session: bool,
    /// Initial stdin ownership; omission retains the bootstrap descriptor.
    pub stdin: StdinMode,
    /// Capture workload output for bootstrap attachment even without a control
    /// adapter. Without a PTY stdout/stderr are separate pipes (not terminals).
    /// With no control adapter, Pipe stdin is pumped from bootstrap stdin and
    /// cancelled on workload completion. Defaults to false for ordinary Sessions.
    /// With a PTY and Closed stdin, input is /dev/null so reads observe EOF;
    /// stdout/stderr still share the terminal and its configured dimensions.
    pub attached: bool,
    /// Optional PTY dimensions, rows then columns, both nonzero. A PTY combines
    /// output streams and requires a new session.
    pub tty: Option<[u16; 2]>,
    /// Opt-in absolute guest path to the glibc allocator-reclaim preload SO.
    /// The launcher appends it to LD_PRELOAD and supplies a private socketpair.
    /// Each registered process gets one worker; same-PID exec retains control
    /// when the environment/descriptor survive. Fork children close the channel
    /// and are not independently controlled. Static/secure-execution programs
    /// or programs that close the descriptor may not provide reclamation;
    /// requests then fail without stopping the workload. The SO must match the
    /// target ABI; an incompatible preload can cause workload startup to fail.
    /// Omission creates no channel and makes no preload changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocator_reclaim: Option<PathBuf>,
    /// Create the working directory before spawning; false requires it to exist.
    pub create_workdir: bool,
}

/// Ownership of the initial process's input descriptor.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    /// Inherit the bootstrap console, retaining ordinary Session terminal behavior.
    /// An explicit PTY instead supplies terminal input without exporting an input
    /// writer for a control-channel attachment.
    #[default]
    Inherit,
    /// Open /dev/null for noninteractive workloads.
    Closed,
    /// Retain a pipe or PTY input writer for one control-channel attachment.
    Pipe,
}

/// A host-declared share; its tag identifies a VMM filesystem, never a host path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// VMM filesystem tag.
    pub tag: String,
    /// Absolute guest destination without parent traversal. The host must also
    /// reject overlap with guest system/control paths before attaching the share.
    pub target: PathBuf,
    /// Reject guest mutations at the mount boundary.
    pub read_only: bool,
    /// Populate an empty share from its image destination before mounting it.
    /// Only directories, regular files and symlinks are supported.
    pub initialize: bool,
    /// Mount only the share's fixed `file` entry onto a regular-file target.
    /// The host must expose only that admitted inode, not its parent directory.
    /// Incompatible with volume initialization; defaults to directory mounting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_file: bool,
}

/// A supervisor-owned guest-root helper started alongside the workload. Sidecars are
/// advisory by contract: they run with the workload environment overlaid by
/// their own. Stdin is null; stdout/stderr append to `log`, falling back to null
/// if log setup fails. Logs may be host-visible through virtio-fs. Their spawn
/// failure or exit never affects the workload or the reported guest exit
/// code. Workload process identity/limits do not apply to these trusted helpers.
/// `argv[0]` addresses a file staged into the rootfs by the host
/// executor; portable validation precedes guest startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarProcess {
    /// Nonempty program followed by literal NUL-free arguments; no shell expansion.
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Literal environment; names must be nonempty without equals/NUL, values NUL-free.
    pub env: BTreeMap<String, String>,
    /// Absolute guest path receiving detached sidecar stdout/stderr.
    pub log: PathBuf,
}

/// Relay one host connection end to end: listen on the guest AF_VSOCK
/// `vsock_port` (the host executor tunnels a unix socket into it) and
/// forward every accepted connection to guest `127.0.0.1:tcp_port`, where
/// the bridged service listens. The relay is advisory: a failed listener or
/// connection is reported on the console and never affects the workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortBridge {
    /// Nonzero host-only vsock listener port; unique among configured bridges.
    pub vsock_port: u32,
    /// Nonzero guest loopback TCP destination port.
    pub tcp_port: u16,
}

/// One host directory shared into the guest at `path`. Read-only shares are
/// host-enforced read-only passthrough: the host may keep updating the
/// directory (projected ConfigMap/Secret rotation) and guests observe those
/// updates; only the payloads share (`payloads_dir`) is physically immutable
/// and DAX-mapped. Read-write shares are plain writable passthrough.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestShare {
    /// Virtio-fs tag; unique within one guest.
    pub tag: String,
    /// Absolute guest mount point (never `/`).
    pub path: PathBuf,
    /// Host-enforced read-only passthrough; host updates remain visible.
    pub read_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Static guest IPv4 settings; host data planes separately enforce network policy.
pub struct NetworkConfig {
    /// Guest IPv4 octets; the supervisor configures a /24.
    pub address: [u8; 4],
    /// Virtual IPv4 router used for default routes and guest DNS.
    pub gateway: [u8; 4],
}

/// Explicit virtio-vsock port, separate from workload networking.
pub const CONTROL_PORT: u32 = 1025;

/// Guest-to-host vsock port for a connection-only command-adapter wakeup.
pub const CONTROL_READY_PORT: u32 = 1026;

/// Named duplex console port for noninteractive command requests.
pub const SERIAL_COMMAND_PORT: &str = "pvisor-command";

/// Independent named console port for workload signals.
pub const SERIAL_SIGNAL_PORT: &str = "pvisor-signal";

/// Maximum simultaneously admitted operations on each host-only serial port.
pub const SERIAL_MAX_REQUESTS: usize = 32;

/// Maximum encoded control frame, including streamed output frames.
pub const MAX_FRAME: usize = 1024 * 1024;

/// One operation on the current VM generation. Requests are length-bounded.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Request {
    /// Check that the guest supervisor is accepting requests.
    Ping {},
    /// Attach framed input to the main process; explicit EOF closes its stdin.
    Input {},
    /// Signal the main workload's process group; does not signal host processes.
    Stop {
        /// Linux signal number, from 1 through 64.
        signal: i32,
    },
    /// Start another process in this guest. Environment overrides the launch environment.
    Exec {
        /// Executable and arguments; the program must be nonempty.
        argv: Vec<String>,
        /// Overrides merged with the workload launch environment.
        env: BTreeMap<String, String>,
        /// Environment names to remove before applying overrides.
        unset_env: Vec<String>,
        /// Absolute working-directory override; omission inherits launch cwd.
        cwd: Option<PathBuf>,
        /// Guest user/group override; omission inherits the launch identity.
        user: Option<String>,
        /// Optional terminal dimensions, rows followed by columns.
        tty: Option<[u16; 2]>,
    },
    /// Start a tracked exec; replies Started, Output*, Exit. Disconnect kills the
    /// process unless stdin was explicitly closed. Host owns stream retention.
    Start {
        /// Validated launch overrides, sharing the ordinary process launcher.
        launch: ExecLaunch,
    },
    /// Signal a registered child through the PID-1 registry (never a reused PID).
    Signal {
        /// Guest child PID returned by Started.
        pid: u32,
        /// Registration generation returned by Started; stale values fail.
        generation: u64,
        /// Signal in 1..=64, delivered to its managed process group.
        signal: i32,
    },
    /// Resize a registered PTY; rejects ordinary pipes and exited/reused PIDs.
    Resize {
        /// Live guest child ID returned by Started.
        pid: u32,
        /// Registration generation returned by Started.
        generation: u64,
        /// Nonzero terminal rows.
        rows: u16,
        /// Nonzero terminal columns.
        columns: u16,
    },
    /// Execute file operations inside the guest, using guest paths and root identity.
    File {
        /// Read/write stream or metadata operation; see FileRequest.
        request: FileRequest,
    },
    /// Return guest launch binding and actual guest memory/CPU observations.
    Info {},
    /// Report guest TCP listeners reachable through `Connect`: unique ports
    /// bound to wildcard or loopback addresses (other local addresses cannot
    /// be dialed through the 127.0.0.1 relay). Reply is `Value` carrying a
    /// sorted port array. Observation only; no guest state changes.
    Listeners {},
    /// Cooperatively inspect or trim a registered child's allocator.
    /// Requires ProcessConfig::allocator_reclaim at launch. Only host-CID peers
    /// may request this operation; no executable payload or address is accepted.
    /// PID 1 exchanges a bounded request with a persistent preload worker over
    /// its private socketpair. Original threads continue running; no ptrace,
    /// injected code or application safe point is used. Same-PID exec can retain
    /// the channel; fork descendants do not inherit independent control.
    /// Value replies contain status, pid, allocator and worker_tid. Trim adds
    /// malloc_trim_result and anonymous RSS before/after (null if unavailable).
    /// Guest counters do not measure host physical-memory savings.
    /// Failure replies Failure; disconnecting does not cancel an admitted call.
    ReclaimAllocator {
        /// Live registered guest child PID; PID 1 and unregistered PIDs fail.
        pid: u32,
        /// Registration identity fencing guest PID reuse; stale values fail.
        generation: u64,
        /// False inspects; true calls malloc_trim(0) in the preload worker.
        /// One request executes at a time. Responses have a two-second wait
        /// budget. Any channel/protocol/allocator error retires the channel;
        /// the target is neither stopped nor killed. A timed-out call may still
        /// complete, and cannot safely be canceled. Further requests then fail.
        trim: bool,
    },
    /// Connect to guest loopback TCP, then exchange framed data and EOF.
    Connect {
        /// Nonzero guest TCP port.
        port: u16,
    },
}

/// Native tracked exec overrides. Environment values are literal, never shell-expanded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecLaunch {
    /// Nonempty executable followed by literal arguments.
    pub argv: Vec<String>,
    /// Overrides merged into the launch environment; native transport keys are removed.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Treat `env` as the complete process environment instead of an overlay
    /// onto the launch environment. Callers that already carry a full
    /// environment (OCI exec specs) use this; native transport keys are
    /// still removed and HOME keeps its Docker-equivalent defaulting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_env: bool,
    /// Per-exec rlimits keyed by Linux resource name with soft and hard
    /// values, replacing the launch limits entirely. Empty inherits them,
    /// which is the OCI exec default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, (u64, u64)>,
    /// Optional absolute guest cwd; omission inherits workload cwd.
    pub cwd: Option<PathBuf>,
    /// Existing guest identity; omission inherits launch identity.
    pub user: Option<String>,
    /// Optional rows, columns; PTY combines stdout and stderr.
    pub tty: Option<[u16; 2]>,
}

/// Root-identity guest file primitives. Paths must be absolute and NUL-free.
/// Read replies Ready/Output*/Exit; Write consumes Data*/Eof after Ready and
/// replies Value. Failed writes may leave partial content and must not be retried
/// blindly. Metadata/list replies Value; lists are bounded to 1024 entries.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum FileRequest {
    /// Read an ordinary guest file as bounded Output frames.
    Read {
        /// Absolute guest path.
        path: PathBuf,
    },
    /// Truncate/create and stream bytes. Parent directories must already exist.
    Write {
        /// Absolute guest path.
        path: PathBuf,
        /// Unix permission bits, applied after writing.
        mode: u32,
    },
    /// Observe metadata, following symlinks while reporting their target.
    Stat {
        /// Absolute guest path.
        path: PathBuf,
    },
    /// List immediate entries; no recursive traversal or symlink following.
    List {
        /// Absolute guest directory.
        path: PathBuf,
    },
    /// Create directories recursively.
    MakeDir {
        /// Absolute guest directory.
        path: PathBuf,
        /// Unix permission bits.
        mode: u32,
    },
    /// Remove a file, symlink, or recursively a directory. Root is rejected.
    Remove {
        /// Absolute guest path other than root.
        path: PathBuf,
    },
    /// Rename using guest filesystem semantics.
    Move {
        /// Absolute existing guest source.
        source: PathBuf,
        /// Absolute guest destination.
        destination: PathBuf,
    },
    /// Change permission bits using guest filesystem semantics.
    Chmod {
        /// Absolute guest path.
        path: PathBuf,
        /// Unix permission bits.
        mode: u32,
    },
}

/// Explicit stdin framing avoids interpreting a transport half-close as a VM reset.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "input", deny_unknown_fields)]
pub enum StdinFrame {
    /// A bounded input chunk.
    Data {
        /// Bytes to write to the process or TCP stream, bounded by MAX_FRAME.
        bytes: Vec<u8>,
    },
    /// Close process stdin while retaining the duplex control connection.
    Eof {},
}

/// One request on a host-only serial control port. Both ends must use the
/// matching supervisor protocol. IDs are nonzero and strictly increasing on
/// each port; replies may interleave across concurrently admitted requests.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialRequest {
    /// Per-port request identity, never a guest PID.
    pub id: u64,
    /// Operation admitted by this port's capability profile.
    pub request: Request,
}

/// One serial response, attributed to exactly one admitted request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialReply {
    /// Identity copied from the corresponding SerialRequest.
    pub id: u64,
    /// Ordinary supervisor reply; process creation and reaping stay shared.
    pub reply: Reply,
}

/// One bounded stdin chunk for an already admitted serial request. It does not
/// allocate a new request ID. The sender waits for SerialInputAck before sending
/// the next chunk, so a slow child cannot grow queues or block unrelated calls.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialInput {
    /// Existing request identity.
    pub input_id: u64,
    /// Ordinary stdin bytes or explicit EOF; data is limited to 16 KiB.
    pub input: StdinFrame,
}

/// Cancel an attachment that disconnected without explicit stdin EOF. Exec
/// cleanup uses the existing process registry; main input is closed, not relaunched.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialCancel {
    /// Existing request identity; unknown/completed identities have no effect.
    pub cancel_id: u64,
}

/// Host serial frame. Request IDs increase; input and cancellation refer back
/// to existing IDs and never change request admission order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SerialHostFrame {
    /// Admit an operation.
    Request(SerialRequest),
    /// Deliver stdin.
    Input(SerialInput),
    /// Close an abandoned attachment.
    Cancel(SerialCancel),
}

/// Return one stdin credit after forwarding the chunk to the ordinary adapter.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialInputAck {
    /// Request whose next stdin frame may now be sent.
    pub input_ack: u64,
}

/// Guest serial frame. Input credits are transport control, not operation replies.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SerialGuestFrame {
    /// Ordinary reply attributed to an operation.
    Reply(SerialReply),
    /// Release one stdin credit.
    InputAck(SerialInputAck),
}

/// Guest reply. Output chunks are at most 16 KiB before JSON encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Reply {
    /// The operation was accepted; subsequent frames depend on the request.
    Ready,
    /// Tracked exec accepted. PID remains guest-local and is fenced by the reaper.
    Started {
        /// Guest child process ID.
        pid: u32,
        /// Monotonic registration identity fencing guest PID reuse.
        generation: u64,
    },
    /// Bounded JSON metadata from File, Info or ReclaimAllocator. No host paths or credentials.
    Value {
        /// Operation-specific metadata; stream transfers use Output instead.
        value: serde_json::Value,
    },
    /// A chunk from descriptor 1 or 2.
    Output {
        /// Descriptor 1 for stdout/TCP or 2 for stderr.
        stream: u8,
        /// Output bytes, no larger than 16 KiB before encoding.
        bytes: Vec<u8>,
    },
    /// Exec terminated; contains the process exit code or 128 + signal.
    Exit {
        /// Workload exit code, 128 + signal, or zero for TCP EOF.
        code: i32,
    },
    /// Native operation failed; effects already acknowledged may be partial.
    Failure {
        /// Linux errno, or EIO for errors without an errno.
        errno: i32,
        /// Human-readable detail, never interpreted as a host command.
        message: String,
    },
    /// The operation failed before acceptance.
    Error {
        /// Human-readable failure detail; never interpreted as a host command.
        message: String,
    },
}

/// Linux `_IOR('v', 3, [u8; 16])`: return a fresh 128-bit token to PID 1.
pub const EXIT_CLAIM_IOCTL: u32 = 0x8010_7603;

/// Linux `_IOW('v', 4, [u8; 20])`: token followed by a little-endian u32 status.
/// Only statuses in `0..=255` are accepted; duplicate reports are rejected.
pub const EXIT_REPORT_IOCTL: u32 = 0x4014_7604;

/// Number of token bytes returned by the claim operation.
pub const EXIT_TOKEN_BYTES: usize = 16;

/// Exact report payload length: token plus four-byte exit code, with no padding.
pub const EXIT_REPORT_BYTES: usize = EXIT_TOKEN_BYTES + 4;

/// Pure portable validation of the whole Linux launch message.
/// Successful validation neither creates commands/processes nor installs policy,
/// filesystems or isolation; the Linux supervisor owns those operations.
///
/// Construct and validate a launch without starting a guest or accessing its paths:
/// ```
/// use pvisor_guest::api::{GuestConfig, GuestValidation};
/// use std::io::ErrorKind;
/// assert_eq!(GuestConfig::default().validate().unwrap_err().kind(), ErrorKind::InvalidInput);
/// let mut launch = GuestConfig {
///     argv: vec!["/bin/echo".into(), "$LITERAL".into()],
///     cwd: "/work".into(),
///     ..Default::default()
/// };
/// let before = serde_json::to_value(&launch)?;
/// launch.validate()?;
/// assert_eq!(serde_json::to_value(&launch)?, before);
/// let decoded: GuestConfig = serde_json::from_slice(&serde_json::to_vec(&launch)?)?;
/// decoded.validate()?;
/// assert_eq!(decoded.argv, launch.argv);
/// launch.cwd = "relative".into();
/// assert_eq!(launch.validate().unwrap_err().kind(), ErrorKind::InvalidInput);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait GuestValidation {
    /// Reject invalid argv/environment, guest paths, reversed limits, identity/
    /// capability/PTY controls, shares, bridges, tmpfs and root-disk/layer inputs
    /// with InvalidInput, without I/O or mutation. Validates every configured
    /// sidecar under the same rules. Default is non-launchable (empty argv/cwd).
    /// Existence, supported resource names and native OS capabilities are checked
    /// by the supervisor. Guest paths always use Linux syntax, even on Windows.
    fn validate(&self) -> io::Result<()>;
}

/// Bounded frame reading on an exclusively borrowed synchronous input stream.
///
/// Each read consumes one frame. A truncated transport must be discarded:
/// ```
/// use pvisor_guest::api::{GuestFrameRead, GuestFrameWrite, Request};
/// use std::io::ErrorKind;
/// let mut wire = Vec::new();
/// wire.write_frame(&Request::Ping {})?;
/// assert_eq!(&wire[4..], br#"{"operation":"Ping"}"#);
/// wire.write_frame(&Request::Ping {})?;
/// let mut input = wire.as_slice();
/// assert!(matches!(input.read_frame::<Request>()?, Request::Ping {}));
/// assert!(!input.is_empty());
/// assert!(matches!(input.read_frame::<Request>()?, Request::Ping {}));
/// assert!(input.is_empty());
/// let mut truncated = &wire[..wire.len() - 1];
/// let _: Request = truncated.read_frame()?;
/// assert_eq!(truncated.read_frame::<Request>().unwrap_err().kind(), ErrorKind::UnexpectedEof);
/// # Ok::<(), std::io::Error>(())
/// ```
pub trait GuestFrameRead: Read {
    /// Consume one four-byte big-endian length and JSON payload of at most MAX_FRAME
    /// bytes. Oversized lengths return InvalidData after consuming only the header;
    /// truncated frames/JSON return UnexpectedEof; JSON syntax/type errors return InvalidData.
    /// Values are decoded, not authorized. Errors can leave the stream mid-frame;
    /// discard that transport rather than assuming the next read is synchronized.
    fn read_frame<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T>;
}

/// Bounded frame writing on an exclusively borrowed synchronous output stream.
pub trait GuestFrameWrite: Write {
    /// Serialize before writing; oversized payloads return InvalidInput without
    /// touching output. Then write the big-endian length and JSON bytes without
    /// flushing or closing. Serialization and I/O errors propagate; I/O failure
    /// can leave a partial frame and must not be retried on the same transport.
    fn write_frame<T: Serialize>(&mut self, value: &T) -> io::Result<()>;
}

/// Host-independent Linux workload signal parsing on a borrowed string.
pub trait GuestSignal {
    /// Accept numbers 1..=64, TERM/KILL/INT/QUIT/HUP/USR1/USR2/WINCH, optional
    /// uppercase SIG prefix, or empty text for TERM. Other spellings return
    /// InvalidInput. No process is signalled or authority granted by parsing.
    fn parse_signal(&self) -> io::Result<i32>;
}

/// Pure PID-1 exit report encoding on the borrowed fixed-size token.
pub trait GuestExitReport {
    /// Copy the token and little-endian u32 status into the fixed report layout.
    /// Codes outside 0..=255 return InvalidInput before I/O. The token remains
    /// borrowed and reusable as bytes; the root device enforces one report only.
    fn report(&self, code: i32) -> io::Result<[u8; EXIT_REPORT_BYTES]>;
}
