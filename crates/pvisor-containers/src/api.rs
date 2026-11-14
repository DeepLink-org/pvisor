//! Uniform contracts for native containers with optional bridge networking.
//!
//! Catalog operations are portable. Execution currently requires Windows x86_64,
//! WHP, a same-installation firmware DLL and ext4 template. Other hosts return
//! unsupported errors before publishing instances. Methods are synchronous.
//! The store must be private to the calling user and exclusively managed by this
//! library. Host administrators and processes running as that user remain trusted.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, net::Ipv4Addr, path::PathBuf, time::Duration};
use std::{io, sync::atomic::AtomicBool};

pub use crate::store::Store;

/// Caller-owned, cancellable input. The runtime reads on a scoped worker and
/// joins it before returning. Implementations must observe cancel while idle
/// and return promptly; zero means genuine EOF. No host input is inherited
/// unless the caller explicitly supplies a source.
pub trait InputSource: Send {
    /// Read at most buffer.len() bytes. Do not retain the buffer. Check the
    /// latched cancellation flag during blocking waits; cancellation may return
    /// zero or ConnectionAborted. Other I/O errors fail the attachment.
    fn read(&mut self, buffer: &mut [u8], cancel: &AtomicBool) -> io::Result<usize>;
}

/// Immutable image identity displayed by `images`; configuration stays private.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    /// Caller-visible tag/digest reference.
    pub reference: String,
    /// OCI configuration SHA-256 digest.
    pub id: String,
    /// Selected linux/amd64 manifest SHA-256 digest.
    pub digest: String,
}

/// One container's observed phase. Unknown is not proof of successful exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Owner admitted, workload readiness not yet confirmed.
    Starting,
    /// The current guest supervisor confirmed workload creation.
    Running,
    /// The owner recorded an observed runner exit.
    Exited,
    /// Owner lease disappeared without a terminal record.
    Unknown,
}

/// Built-in container networking. New launches default to Bridge; old saved
/// requests without this field deserialize as None and remain isolated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    /// Userspace IPv4 TCP/DNS egress plus same-store guest peer packets.
    /// No TAP, host bridge, privileged service or host socket bypass is used.
    Bridge,
    /// No virtual network interface; guest loopback remains usable.
    #[default]
    None,
}

/// Read-only container projection; selectors accept full/unique short IDs or names.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Container {
    /// Unique 32-character lowercase launch ID.
    pub id: String,
    /// Current owner generation. Restart preserves id but rotates this identity;
    /// old attachments and stop requests cannot address the replacement runner.
    #[serde(default)]
    pub generation: String,
    /// Unique user name, or an automatically selected name.
    pub name: String,
    /// Image reference at creation.
    pub image: String,
    /// Pinned OCI configuration digest, independent of later tag changes/removal.
    #[serde(default)]
    pub image_id: String,
    /// Literal resolved guest argv.
    pub command: Vec<String>,
    /// Reconciled phase, not merely a host PID liveness guess.
    pub state: State,
    /// Runner/workload status only when Exited; otherwise absent.
    pub exit_code: Option<i32>,
    /// Selected built-in network; absent legacy records remain offline.
    #[serde(default)]
    pub network: Network,
    /// Stable guest IPv4 address retained across starts, absent for None.
    #[serde(default)]
    pub ipv4: Option<Ipv4Addr>,
}

/// Native workload request. Unsupported capabilities are
/// excluded rather than silently ignored. Validation precedes image downloads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    /// Built-in network, Bridge by default for new requests.
    #[serde(default)]
    pub network: Network,
    /// Keep guest stdin open. Foreground callers pass an InputSource to wait;
    /// detached launches retain input ownership in the guest until stop/exit.
    #[serde(default)]
    pub interactive: bool,
    /// Real guest PTY dimensions (rows, columns), both nonzero. Output is combined
    /// on stdout; absence keeps stdout/stderr separate. Requires a new guest
    /// session, configured by this profile. Host console setup belongs to caller.
    #[serde(default)]
    pub tty: Option<[u16; 2]>,
    /// Public linux/amd64 image reference; an absent local tag is pulled.
    pub image: String,
    /// Optional unique ASCII name, 1..128 alphanumeric/underscore/dash/dot bytes.
    pub name: Option<String>,
    /// Replacement for image CMD; image ENTRYPOINT remains in effect.
    pub command: Vec<String>,
    /// Literal environment overrides; the host environment is not inherited.
    pub env: BTreeMap<String, String>,
    /// Remove these image environment names before applying explicit overrides.
    #[serde(default)]
    pub unset_env: Vec<String>,
    /// Optional absolute Linux working directory, created before launch.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Guest user/group override, using the shared process identity contract.
    #[serde(default)]
    pub user: Option<String>,
    /// Replace the image entrypoint with one literal executable. Empty clears it.
    /// Command arguments remain separate; no shell parsing is performed.
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// Return after launch readiness rather than attaching to completion.
    pub detach: bool,
    /// Remove instance files on completion, after foreground output is delivered.
    pub auto_remove: bool,
    /// vCPU count, 1..=64; defaults to 2.
    pub cpus: u8,
    /// Guest RAM in MiB, 128..=65536; defaults to 512.
    pub memory_mib: u32,
}

/// Catalog, launch and lifecycle operations. Errors retain existing catalog
/// records unless the operation explicitly removes them. Host and guest paths
/// are never shell-expanded; callers own output writers and cancellation.
pub trait Containers: Sized {
    /// Open/create a private store. None uses PVISOR_DOCKER_HOME or local app
    /// data. Refuse reparse/symlink roots. Does not require WHP or boot a VM.
    fn open(root: Option<PathBuf>) -> anyhow::Result<Self>;
    /// Download and verify the selected manifest, config and every compressed
    /// layer before publishing its tag. Failed/partial bytes are not published.
    fn pull(&self, reference: &str) -> anyhow::Result<Image>;
    /// List published images without registry requests or VM startup.
    fn images(&self) -> anyhow::Result<Vec<Image>>;
    /// Untag by exact reference, configuration digest or unique digest prefix
    /// (at least 12 hex digits). Multiple tags for one ID require force. Without
    /// force, retained containers pin the image. Force removes only catalog tags;
    /// running/stopped containers and immutable blobs are preserved. A selector's
    /// validation/conflict failure changes nothing. An I/O failure may follow
    /// removal of some selected tags; inspect images before retrying. Disk garbage
    /// collection is deliberately separate. Returns removed reference identities.
    fn remove_image(&self, selector: &str, force: bool) -> anyhow::Result<Vec<Image>>;
    /// Resolve image configuration and create one independent filesystem/owner.
    /// Reused names fail before creating a second instance. Background launch
    /// waits for readiness; foreground callers must then call wait.
    fn run(&self, request: Run) -> anyhow::Result<Container>;
    /// Start a stopped, successfully prepared instance in the background,
    /// retaining its private ext4 writes and pinned launch configuration. A live
    /// Running instance is unchanged. Unknown/failed preparation and auto-remove
    /// instances are rejected. A new owner generation fences stale requests.
    /// Failure after admission leaves an inspectable record, not a fresh disk.
    /// Requires Windows/WHP; other hosts fail without modifying records.
    fn start(&self, selector: &str) -> anyhow::Result<Container>;
    /// List running instances, or all retained instances when all is true.
    /// An absent owner without a terminal record projects Unknown.
    fn list(&self, all: bool) -> anyhow::Result<Vec<Container>>;
    /// Attach to the original workload's output streams and return its
    /// exit code. Foreground auto-remove waits for this delivery, with a bounded
    /// abandoned-client grace. PTY output is combined on stdout. An input source
    /// requires Run.interactive; None leaves existing guest stdin ownership alone.
    /// Completion cancels and joins the source worker, including on failure.
    /// Host console modes and initial PTY dimensions belong to the caller.
    fn wait(
        &self,
        container: &Container,
        input: Option<&mut dyn InputSource>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<i32>;
    /// Execute another process through the current PID-1 registry.
    /// Requires Running. Closing stdin is explicit; closing the client does not
    /// undo a completed/ongoing guest side effect. Main exit reaps remaining execs.
    /// Launch environment, cwd, identity and limits use the shared guest contract;
    /// unset_env removes inherited names before overlaying launch.env (ignored
    /// when launch.replace_env selects a complete environment). TTY uses a real
    /// guest PTY with combined stdout; None input means explicit EOF. Unexpected
    /// attachment loss cancels that exec's process group, not the main workload;
    /// explicit EOF permits the exec to continue. Source workers are joined
    /// before return. Independent requests execute concurrently, up
    /// to 32 per command port. Slow output consumers fail their own bounded
    /// delivery rather than blocking unrelated executions.
    fn exec(
        &self,
        selector: &str,
        launch: pvisor_guest::control::ExecLaunch,
        unset_env: Vec<String>,
        input: Option<&mut dyn InputSource>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<i32>;
    /// Copy retained workload logs, keeping stdout/stderr separate. When follow
    /// is true, retain open log files through owner exit/auto-removal and drain
    /// the selected generation's final bytes. A restart ends this attachment;
    /// it does not attach to the next owner. Writer failures are returned.
    /// Boot diagnostics are excluded. Does not start or restart an instance.
    fn logs(
        &self,
        selector: &str,
        follow: bool,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<()>;
    /// Send the requested Linux signal, or the pinned image StopSignal (TERM
    /// when omitted by the image). Names/numbers use the shared guest parser.
    /// Duration::MAX waits indefinitely without escalation; finite grace must
    /// fit the monotonic clock. After a finite timeout, request owner-controlled
    /// force-stop and await teardown. Invalid signals/grace fail before delivery.
    /// No arbitrary recorded host PID is killed; old generations stay fenced.
    fn stop(&self, selector: &str, timeout: Duration, signal: Option<&str>) -> anyhow::Result<()>;
    /// Remove a stopped/unknown instance; force first stops a live one. A live
    /// owner lease otherwise prevents deletion. Image blobs are never removed.
    fn remove(&self, selector: &str, force: bool) -> anyhow::Result<()>;
    /// Dispatch this executable's private owner/runner modes before CLI parsing.
    /// Returns false for normal invocation. Private modes validate saved identity
    /// and acquire launch leases; this is not authority against the same host user.
    fn internal_if_requested() -> anyhow::Result<bool>;
}
