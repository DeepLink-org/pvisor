mod safe;

use pvisor::job_service::policy::PolicySource;
use std::path::{Path, PathBuf};

const STAGE_OWNER_FILE: &str = ".pvisor-stage-owner";

fn spec_is_json(path: &Path) -> anyhow::Result<bool> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read spec file {}", path.display()))?;
    Ok(bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'))
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct ByteSize(u64);

impl FromStr for ByteSize {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_scaled(
            value,
            &[
                ("kib", 1 << 10),
                ("kb", 1_000),
                ("mib", 1 << 20),
                ("mb", 1_000_000),
                ("gib", 1 << 30),
                ("gb", 1_000_000_000),
                ("b", 1),
            ],
        )
        .map(ByteSize)
    }
}

fn parse_scaled(value: &str, units: &[(&str, u64)]) -> Result<u64, String> {
    let normalized = value.trim().to_ascii_lowercase();
    let (number, multiplier) = units
        .iter()
        .find_map(|(suffix, multiplier)| {
            normalized
                .strip_suffix(suffix)
                .map(|number| (number, *multiplier))
        })
        .unwrap_or((normalized.as_str(), 1));
    let number = number
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("invalid size/duration: {value:?}"))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("size/duration overflows u64: {value:?}"))
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct DurationMs(pub u64);

impl FromStr for DurationMs {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_scaled(
            value,
            &[("ms", 1), ("s", 1_000), ("m", 60_000), ("h", 3_600_000)],
        )
        .map(DurationMs)
    }
}
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use pvisor_core::gateway::{CaptureLevel, ModelRoute};
use pvisor_core::{
    FilesystemAccess, FilesystemCapability, PolicyMode, RunInvocation, RunSpec, RunState, StdioMode,
};
#[cfg(feature = "gateway")]
use pvisor_gateway::config::{OverlayConfig, ProxyConfig};
use pvisor_overlaynet::{NetworkAccessRule, NetworkBandwidthLimit};
use pvisor_overlaynet::{NetworkConfig, NetworkMode};
use serde::Deserialize;

use pvisor::{
    ContainerExecutor, NetworkDriverConfig, OverlayHint, PVisor, ProcessExecutor, RunBundle,
    RunExecutor, VmExecutor,
};
use pvisor::{
    ContainerMount, ContainerNetwork, ContainerPlatform, FilesystemAccessLevel, FilesystemMode,
    GatewayMode, GatewayProfile, OverlayFsCommit, OverlayFsSettings, OverlayNetMode,
    OverlayNetPolicy, OverlayNetSettings, RunConfig, RunExecutorKind, RunPolicy, RunStdio,
};
use pvisor::{RunLineage, default_run_home, resolve_run};

use super::trajectory::JournalRecording;
#[cfg(feature = "gateway")]
use pvisor::GatewayDriverConfig;

// Keep pVisor diagnostics separate from the Agent PTY in TUI runs.
macro_rules! run_log {
    ($($arg:tt)*) => {{
        #[cfg(unix)]
        pvisor::diagnostics::diagnostic(format_args!($($arg)*));
        #[cfg(not(unix))]
        eprintln!($($arg)*);
    }};
}

fn announce_control_socket(handle: &pvisor::RunHandle) {
    #[cfg(unix)]
    if let Ok(path) = handle.control_socket() {
        let status = handle.status();
        eprintln!(
            "pVisor live VM options: --vm-socket {} --vm-job-id {} --vm-attempt-id {} (status; suspend JOB --vm-pause/--vm-offload; resume JOB --vm-load)",
            path.display(),
            status.run_id,
            status.attempt.attempt_id
        );
    }
}

mod lifecycle;
pub use lifecycle::ForkArgs;
use lifecycle::execution_store_location;
pub(super) use lifecycle::{fork, resume_execution};

#[cfg(target_os = "linux")]
pub(super) const RUN_COMMAND_ABOUT: &str =
    "Start one Agent Job with independent filesystem, staging, and network policies";
#[cfg(target_os = "linux")]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = "Start one Agent Job under pVisor management. Host execution preserves the host filesystem view by default; use --filesystem sandbox for filesystem access restrictions, --stage for change staging, and --overlaynet for network policy.";

#[cfg(target_os = "macos")]
pub(super) const RUN_COMMAND_ABOUT: &str =
    "Start one Agent Job with independent filesystem, staging, and network policies";
#[cfg(target_os = "macos")]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = MACOS_RUN_COMMAND_LONG_ABOUT;

// Compile the macOS description in tests on every platform so Linux CI also
// checks its safety disclosures instead of leaving them to the macOS shard.
#[cfg(any(target_os = "macos", test))]
const MACOS_RUN_COMMAND_LONG_ABOUT: &str = "Start one Agent Job under pVisor management. Host execution preserves the host filesystem view by default. Use --filesystem sandbox for filesystem access restrictions, --stage for change staging, and --overlaynet for network policy.\n\nOn macOS, staged workspace views use macFUSE when requested, and Seatbelt is used for requested filesystem sandboxing or deny-all network isolation. Full-disk reads remain ambient unless filesystem sandboxing is requested; selective network policies remain cooperative. With --overlaynet-deny-all, Seatbelt blocks non-loopback IP traffic and ambient host Unix sockets while permitting loopback proxy access and Job-scoped Unix IPC.\n\nUnavailable isolation capabilities are reported as warnings in best-effort mode. With --strict, insufficient isolation guarantees cause the Job to fail before Agent execution.";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) const RUN_COMMAND_ABOUT: &str = "Start one Agent Job under pVisor management";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) const RUN_COMMAND_LONG_ABOUT: &str = RUN_COMMAND_ABOUT;

#[cfg(target_os = "linux")]
const EXECUTOR_HELP: &str = "Execution provider: host, container, or vm. `vm` uses the linked pvisor-vm runtime with KVM on Linux; host supports optional filesystem and network isolation";
#[cfg(target_os = "macos")]
const EXECUTOR_HELP: &str = "Execution provider: host, container, or vm. `vm` uses the linked pvisor-vm runtime with HVF on macOS; host supports optional filesystem and network isolation";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const EXECUTOR_HELP: &str = "Execution provider for the Agent command";

#[cfg(target_os = "linux")]
const DENY_ALL_HELP: &str = "Deny all OverlayNet egress. VM `auto` enforces this on guest TCP; host execution uses a private network namespace when supported";
#[cfg(target_os = "macos")]
const DENY_ALL_HELP: &str = "Deny all OverlayNet egress. VM `auto` enforces this on guest TCP; host execution uses Seatbelt when supported";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DENY_ALL_HELP: &str = "Deny all OverlayNet egress; direct sockets remain outside the cooperative host/container proxy rule";

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    /// Explicit runtime enables, also parsed by companions reusing RunArgs.
    #[arg(long = "feature", value_name = "NAME", value_delimiter = ',')]
    #[serde(default)]
    pub(super) features: Vec<pvisor::features::Feature>,
    /// Show a terminal with status bar; Ctrl-] opens the TUI command mode.
    #[arg(long)]
    tui: bool,
    /// Ask on `ask` file rules and unlisted proxy destinations; implies --tui and --safe.
    #[arg(long = "ask")]
    audit: bool,
    /// Prepared JSON RunSpec for delegated execution; requires --result-file and the host executor.
    #[arg(long, value_name = "FILE")]
    spec: Option<PathBuf>,
    /// TOML RunConfig layered beneath explicit CLI values; replaces personal Agent defaults.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Skip personal Agent defaults from $XDG_CONFIG_HOME/pvisor/agents/<program>.toml.
    #[arg(long)]
    no_agent_defaults: bool,

    /// Atomically write the finalized RunResult as JSON before exit; required with --spec.
    #[arg(long, value_name = "FILE")]
    result_file: Option<PathBuf>,

    /// Changeset directory; requests a private filesystem view unless --filesystem host is set.
    #[arg(long, value_name = "PATH")]
    stage: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Job options")]
    run: RunOverrides,
    #[command(flatten, next_help_heading = "Container executor options")]
    container: ContainerOverrides,
    #[command(flatten, next_help_heading = "VM executor options")]
    vm: VmOverrides,
    #[command(flatten, next_help_heading = "OverlayFS options")]
    overlayfs: OverlayFsOverrides,
    #[command(flatten, next_help_heading = "OverlayNet options")]
    overlaynet: OverlayNetOverrides,
    #[command(flatten, next_help_heading = "Gateway options")]
    gateway: GatewayOverrides,
    #[command(flatten, next_help_heading = "Recording options")]
    record: RecordOverrides,

    /// Agent command; replaces `run.command` from the TOML spec.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

impl RunArgs {
    fn cli_asks(&self) -> bool {
        self.overlayfs
            .access
            .iter()
            .any(|rule| rule.level == FilesystemLevel::Ask)
    }

    #[cfg(unix)]
    pub fn tui_requested(&self) -> bool {
        self.tui || self.audit || self.cli_asks()
    }

    #[cfg(unix)]
    pub fn audit_requested(&self) -> anyhow::Result<bool> {
        if self.audit || self.cli_asks() {
            return Ok(true);
        }
        if self.spec.is_some() {
            return Ok(false);
        }
        let mut config = load_run_config(self, personal_config_root().as_deref(), false)?;
        config
            .load_policy_defaults(&std::env::current_dir()?, personal_config_root().as_deref())?;
        if config
            .policies
            .filesystem(&Default::default())
            .rules()
            .iter()
            .any(|(_, _, action)| *action == "ask")
        {
            return Ok(true);
        }
        Ok(config.overlayfs.as_ref().is_some_and(|filesystem| {
            !filesystem.access_policy.ask().is_empty()
                || filesystem
                    .access
                    .iter()
                    .any(|rule| rule.level == FilesystemAccessLevel::Ask)
        }))
    }

    #[cfg(unix)]
    pub fn enable_tui(&mut self) {
        self.tui = true;
    }

    #[cfg(unix)]
    pub fn wants_tui(&self, audit: bool) -> bool {
        (self.tui || audit)
            && self.result_file.is_none()
            && self.spec.is_none()
            && self.run.stdio != Some(RunStdio::Capture)
    }
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RunOverrides {
    /// Require sandbox isolation and apply Agent-aware network/file presets; explicit CLI overrides win. Does not select an executor.
    #[arg(long)]
    safe: bool,
    /// Human-readable Job/Agent name.
    #[arg(long)]
    name: Option<String>,
    #[arg(long, value_parser = super::values::run_executor_kind(), help = EXECUTOR_HELP)]
    executor: Option<RunExecutorKind>,
    /// Filesystem access policy; independent from OverlayNet and OverlayFS staging.
    #[arg(long, value_parser = super::values::filesystem_mode())]
    filesystem: Option<FilesystemMode>,
    /// Fail the Job when it runs longer than DURATION (for example `30s` or `5m`).
    #[arg(long, value_name = "DURATION")]
    timeout: Option<DurationMs>,
    /// Agent stdio: `inherit` keeps the terminal, `capture` records output into the Job record.
    #[arg(long, value_parser = super::values::run_stdio())]
    stdio: Option<RunStdio>,
    /// Fail before execution unless every requested capability has a non-bypassable boundary.
    #[arg(long)]
    strict: bool,
    /// Maximum memory for the Agent execution (for example `256MiB` or `4GB`).
    #[arg(long, visible_alias = "mem", value_name = "SIZE")]
    memory: Option<ByteSize>,
    /// CPU count allocated to the executor.
    #[arg(long, value_name = "COUNT")]
    cpu: Option<u16>,
    /// Project one host environment variable by name; repeat as needed.
    #[arg(long, value_name = "NAME")]
    pass_env: Vec<String>,
    /// Clear environment names inherited from the TOML pass_env list before applying --pass-env.
    /// Emitted by the --safe preset; rarely needed by hand.
    #[arg(long, hide = true)]
    clear_pass_env: bool,
    /// Maximum processes/threads admitted for the Job.
    #[arg(long, value_name = "COUNT")]
    max_processes: Option<u64>,
    /// CPU-time budget (for example `500ms`, `5s`, or `1m`).
    #[arg(long, value_name = "DURATION")]
    max_cpu_time: Option<DurationMs>,
    /// Maximum open file descriptors.
    #[arg(long, value_name = "COUNT")]
    max_open_files: Option<u64>,
    /// Maximum size of a file created by the Agent (for example `8MiB`).
    #[arg(long = "max-file-size", value_name = "SIZE")]
    max_file_size: Option<ByteSize>,
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ContainerOverrides {
    /// Native OCI runtime executable (`runc` or `crun`).
    #[arg(long, value_name = "PATH")]
    container_runtime: Option<PathBuf>,
    /// OCI image containing the Agent command. Supplying this selects the container executor.
    #[arg(long, value_name = "IMAGE")]
    container_image: Option<String>,
    /// Existing OCI rootfs directory (otherwise container image is prepared).
    #[arg(long, value_name = "PATH")]
    container_rootfs: Option<PathBuf>,
    /// pVisor injected into the container; defaults to the running executable.
    /// Set it to a statically linked build when the guest ABI differs.
    #[arg(long, value_name = "PATH")]
    container_pvisor_binary: Option<PathBuf>,
    /// Assert the native OCI platform (`linux/amd64` or `linux/arm64`).
    /// Must match the host architecture; cross-platform selection is unsupported.
    #[arg(long, value_name = "PLATFORM")]
    container_platform: Option<ContainerPlatform>,
    /// Container network mode; host keeps the in-process Gateway reachable.
    #[arg(long, value_parser = super::values::container_network())]
    container_network: Option<ContainerNetwork>,
    /// Container-native workdir used when pVisor does not inject an OverlayFS cwd.
    #[arg(long, value_name = "PATH")]
    container_workdir: Option<PathBuf>,
    /// Container user (`uid`, `uid:gid`, or name).
    #[arg(long, value_name = "USER")]
    container_user: Option<String>,
    /// Mount the image root filesystem read-only.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    container_read_only_rootfs: Option<bool>,
    /// TOML inline-table bind mount; repeat to replace configured mounts.
    #[arg(long, value_name = "MOUNT")]
    container_mount: Vec<ContainerMountArg>,
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VmOverrides {
    /// Host-only VM control socket; requires an existing private (0700) parent and a new path.
    #[arg(long = "vm-control-socket", value_name = "PATH")]
    vm_control_socket: Option<PathBuf>,
    /// Create a private file backing the VM's live RAM (must not already exist).
    #[arg(long = "vm-ram-backing", value_name = "FILE")]
    vm_ram_backing: Option<PathBuf>,
    /// Commit RAM as Seekable base/delta generations (requires FUSE/macFUSE).
    #[arg(long = "vm-ram-compression", value_name = "BOOL", num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    vm_ram_compression: Option<bool>,
    /// Linux x86_64 live cold RAM compression in an instance-local store (requires kernel-fault userfaultfd access; no FUSE).
    #[arg(long = "vm-cold-ram-compression", value_name = "BOOL", num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    vm_cold_ram_compression: Option<bool>,
    /// Opt in to host RAM dedup and cross-workload sharing risks; shared live RAM is skipped, private restored COW is eligible. Advice is not merged bytes.
    #[arg(long = "vm-ram-dedup", value_name = "BOOL", num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    vm_ram_dedup: Option<bool>,
    /// Experimental Linux x86_64 / Apple Silicon cold-page sharing; pool loss fails dependent VMs.
    #[arg(long = "vm-memory-pool", value_name = "SOCKET")]
    vm_memory_pool: Option<PathBuf>,
    /// Same-host node resource service for shared immutable images and restored RAM.
    #[arg(long = "vm-node-socket", value_name = "SOCKET")]
    vm_node_socket: Option<PathBuf>,
    /// Immutable filesystem pool for Linux x86_64 no-network native checkpoints.
    /// Must be independent of VM-writable roots and on the Job's volume.
    #[arg(long = "vm-snapshot-filesystem-pool", value_name = "DIR")]
    vm_snapshot_filesystem_pool: Option<PathBuf>,
    /// Shorthand for `--executor vm`.
    #[arg(long)]
    vm: bool,
    /// VM rootfs source: host (Linux default), <PATH>, or image=<PATH>.
    #[arg(long)]
    rootfs: Option<String>,
    /// Content-addressed OCI image cache directory.
    #[arg(long = "image-store", value_name = "DIR")]
    vm_image_store: Option<PathBuf>,
    /// Directory containing libkrunfw; packaged builds discover it automatically.
    #[arg(long = "vm-library-dir", value_name = "PATH")]
    vm_library_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ContainerMountArg(ContainerMount);

impl FromStr for ContainerMountArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            mount: ContainerMount,
        }
        let source = format!("mount = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.mount))
            .map_err(|error| format!("invalid container mount: {error}"))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesystemMountArg {
    source: PathBuf,
    target: PathBuf,
    access: FilesystemLevel,
}

impl FromStr for FilesystemMountArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.rsplitn(2, ':');
        let access = parts.next().unwrap_or("read");
        let rest = parts.next().unwrap_or(value);
        let access = match access {
            "read" => FilesystemLevel::Read,
            "stage" => FilesystemLevel::Stage,
            "write" => FilesystemLevel::Write,
            "deny" => {
                return Err("--mount does not support deny; use --access PATH-GLOB:deny".into());
            }
            _ => {
                return Err(format!(
                    "invalid mount access `{access}`; use read, stage, or write"
                ));
            }
        };
        let mut paths = rest.splitn(2, ':');
        let source = PathBuf::from(paths.next().unwrap_or_default());
        if source.as_os_str().is_empty() {
            return Err("mount source is empty".into());
        }
        let target = paths
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| source.clone());
        Ok(Self {
            source,
            target,
            access,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesystemAccessArg {
    path: String,
    level: FilesystemLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum FilesystemLevel {
    Deny,
    Ask,
    Read,
    Warn,
    Stage,
    Write,
}

impl FromStr for FilesystemAccessArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (path, level) = value
            .rsplit_once(':')
            .ok_or_else(|| "access must use PATH-GLOB:LEVEL".to_string())?;
        let level = match level {
            "deny" => FilesystemLevel::Deny,
            "ask" => FilesystemLevel::Ask,
            "warn" => FilesystemLevel::Warn,
            _ => {
                return Err(format!(
                    "invalid access level `{level}`; use deny, ask, or warn; read-only sharing uses --mount PATH:read"
                ));
            }
        };
        if path.is_empty() {
            return Err("access path is empty".into());
        }
        Ok(Self {
            path: path.into(),
            level,
        })
    }
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OverlayFsOverrides {
    /// Stage persistence: checkpoint (default), or sync each first mutation.
    #[arg(long = "stage-durability", value_name = "checkpoint|strict")]
    durability: Option<pvisor_core::overlay::StageDurability>,
    /// Host path mount: SOURCE[:TARGET]:ACCESS. ACCESS is read, stage, or write.
    #[arg(long = "mount", value_name = "SOURCE[:TARGET]:ACCESS")]
    mounts: Vec<FilesystemMountArg>,
    /// Append an overlay-relative rule: PATH-GLOB:deny|ask|warn; ask opens permission prompts.
    /// Use `--mount` when a path must be staged or writable.
    #[arg(long = "access", value_name = "PATH-GLOB:LEVEL")]
    access: Vec<FilesystemAccessArg>,
    /// Explicitly remove default/config file rules before adding --access rules.
    #[arg(long)]
    clear_access: bool,
    /// Aggregate byte budget for the staged filesystem; the Job fails once the
    /// stage exceeds it.
    #[arg(long = "overlayfs-max-size", value_name = "SIZE")]
    max_size: Option<ByteSize>,
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OverlayNetOverrides {
    /// Network driver: auto selects VM smoltcp, proxy is host/container only, off disables it.
    #[arg(
        long,
        value_parser = super::values::overlay_net_mode(),
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "proxy"
    )]
    overlaynet: Option<OverlayNetMode>,
    /// Explicit proxy listen address; supplying it enables OverlayNet.
    #[arg(long, value_name = "ADDR")]
    overlaynet_listen: Option<String>,
    #[arg(long, value_parser = super::values::overlay_net_policy(), hide = true)]
    overlaynet_policy: Option<OverlayNetPolicy>,
    /// Allowed HOST[:PORT] or CIDR[:PORT]; enables the executor's OverlayNet driver.
    #[arg(long, value_name = "TARGET")]
    overlaynet_allow: Vec<OverlayNetTargetArg>,
    /// Denied HOST[:PORT] or CIDR[:PORT]; enables the executor's OverlayNet driver.
    #[arg(long, value_name = "TARGET")]
    overlaynet_deny: Vec<OverlayNetTargetArg>,
    /// Aggregate bandwidth limit; enables the executor's OverlayNet driver.
    #[arg(long, value_name = "[TARGET=]RATE")]
    overlaynet_limit: Vec<OverlayNetLimitArg>,
    #[arg(
        long,
        help = DENY_ALL_HELP,
        conflicts_with_all = [
            "overlaynet_allow",
            "overlaynet_deny",
            "overlaynet_limit",
            "overlaynet_rule",
            "overlaynet_policy"
        ]
    )]
    overlaynet_deny_all: bool,
    /// TOML inline-table fields for one structured rule; repeat to replace configured rules.
    #[arg(long, value_name = "RULE", hide = true)]
    overlaynet_rule: Vec<OverlayNetRuleArg>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OverlayNetTargetArg(NetworkAccessRule);

impl FromStr for OverlayNetTargetArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_overlaynet_target(value).map(Self)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OverlayNetLimitArg(NetworkBandwidthLimit);

impl FromStr for OverlayNetLimitArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (target, rate) = value
            .rsplit_once('=')
            .map_or((None, value), |(target, rate)| (Some(target), rate));
        let bytes_per_second = parse_bandwidth(rate)?;
        let target = target.map(parse_overlaynet_target).transpose()?;
        Ok(Self(NetworkBandwidthLimit {
            host: target.as_ref().map(|target| target.host.clone()),
            port: target.and_then(|target| target.ports.first().copied()),
            bytes_per_second,
        }))
    }
}

fn parse_overlaynet_target(value: &str) -> Result<NetworkAccessRule, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("OverlayNet target cannot be empty".into());
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| format!("invalid bracketed OverlayNet target `{value}`"))?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(|| format!("invalid OverlayNet target `{value}`"))?
                    .parse::<u16>()
                    .map_err(|_| format!("invalid port in OverlayNet target `{value}`"))?,
            )
        };
        (host, port)
    } else if value.matches(':').count() <= 1 {
        match value.rsplit_once(':') {
            Some((host, port))
                if !host.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                (
                    host,
                    Some(
                        port.parse::<u16>()
                            .map_err(|_| format!("invalid port in OverlayNet target `{value}`"))?,
                    ),
                )
            }
            _ => (value, None),
        }
    } else {
        (value, None)
    };
    if port == Some(0) {
        return Err("OverlayNet target port must not be zero".into());
    }
    pvisor_core::parse_network_rule(host).map_err(|error| error.to_string())?;
    Ok(NetworkAccessRule {
        host: host.to_string(),
        ports: port.into_iter().collect(),
        transports: Vec::new(),
        allow_private_ips: false,
    })
}

fn parse_bandwidth(value: &str) -> Result<u64, String> {
    let normalized = value.trim().to_ascii_lowercase();
    let units = [
        ("gbps", 1_000_000_000_u64, true),
        ("mbps", 1_000_000, true),
        ("kbps", 1_000, true),
        ("bps", 1, true),
        ("gb/s", 1_000_000_000, false),
        ("mb/s", 1_000_000, false),
        ("kb/s", 1_000, false),
        ("b/s", 1, false),
    ];
    for (suffix, multiplier, bits) in units {
        if let Some(amount) = normalized.strip_suffix(suffix) {
            let amount = amount
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid OverlayNet bandwidth `{value}`"))?;
            let scaled = amount
                .checked_mul(multiplier)
                .ok_or_else(|| format!("OverlayNet bandwidth `{value}` is too large"))?;
            let bytes = if bits { scaled.div_ceil(8) } else { scaled };
            return (bytes > 0)
                .then_some(bytes)
                .ok_or_else(|| "OverlayNet bandwidth must be greater than zero".into());
        }
    }
    Err(format!(
        "invalid OverlayNet bandwidth `{value}`; use e.g. `10mbps` or `2mb/s`"
    ))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OverlayNetRuleArg(NetworkAccessRule);

impl FromStr for OverlayNetRuleArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            rule: NetworkAccessRule,
        }
        let source = format!("rule = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.rule))
            .map_err(|error| format!("invalid OverlayNet rule: {error}"))
    }
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayOverrides {
    /// Adapt a supported client and enable Gateway capture.
    #[arg(long, value_parser = super::values::gateway_profile())]
    gateway_profile: Option<GatewayProfile>,
    /// Enable the in-process Gateway for LLM traffic capture, or disable it.
    #[arg(long, value_parser = super::values::gateway_mode())]
    gateway_mode: Option<GatewayMode>,
    /// Gateway admin API listen address.
    #[arg(long, value_name = "ADDR")]
    gateway_admin_listen: Option<String>,
    /// Gateway capture detail level.
    #[arg(long, value_enum)]
    gateway_level: Option<GatewayLevel>,
    /// Header name used to group Gateway requests into sessions.
    #[arg(long, value_name = "HEADER")]
    gateway_session_header: Option<String>,
    /// Enable or disable Gateway diagnostics.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    gateway_debug: Option<bool>,
    /// TOML inline-table fields for one model route; repeat to replace configured routes.
    #[arg(long, value_name = "ROUTE")]
    gateway_route: Vec<GatewayRouteArg>,
}

#[derive(Debug, Clone, Default, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordOverrides {
    /// Local directory or file for Trace Event journal.
    #[arg(long, value_name = "PATH")]
    record_destination: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum, serde::Serialize, serde::Deserialize)]
enum GatewayLevel {
    Summary,
    Dialogue,
    Full,
}

impl From<GatewayLevel> for CaptureLevel {
    fn from(level: GatewayLevel) -> Self {
        match level {
            GatewayLevel::Summary => Self::Summary,
            GatewayLevel::Dialogue => Self::Dialogue,
            GatewayLevel::Full => Self::Full,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GatewayRouteArg(ModelRoute);

impl FromStr for GatewayRouteArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        #[derive(Deserialize)]
        struct Wrapper {
            route: ModelRoute,
        }
        let source = format!("route = {{ {value} }}");
        toml::from_str::<Wrapper>(&source)
            .map(|wrapper| Self(wrapper.route))
            .map_err(|error| format!("invalid Gateway route: {error}"))
    }
}

fn personal_config_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
}

fn load_run_config(
    args: &RunArgs,
    root: Option<&Path>,
    diagnostic: bool,
) -> anyhow::Result<RunConfig> {
    if let Some(path) = &args.config {
        return RunConfig::from_file(path)
            .with_context(|| format!("load pVisor Run config {}", path.display()));
    }
    if args.no_agent_defaults {
        return Ok(RunConfig::default());
    }
    let Some(name) = args
        .command
        .first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str())
    else {
        return Ok(RunConfig::default());
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Ok(RunConfig::default());
    }
    let Some(root) = root else {
        return Ok(RunConfig::default());
    };
    let path = root.join("pvisor/agents").join(format!("{name}.toml"));
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RunConfig::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read personal Agent config {}", path.display()));
        }
    };
    let config = toml::from_str(&source)
        .with_context(|| format!("parse personal Agent config {}", path.display()))?;
    if diagnostic {
        run_log!("pVisor Agent defaults: {}", path.display());
    }
    Ok(config)
}

pub async fn run(mut args: RunArgs) -> anyhow::Result<i32> {
    pvisor::startup_mark("cli.run_begin");
    #[cfg(unix)]
    if pvisor_core::audit::configured() {
        args.audit = true;
    }
    anyhow::ensure!(
        args.spec.is_none() || args.config.is_none(),
        "--spec and --config are mutually exclusive"
    );
    if let Some(path) = args.spec.as_deref() {
        anyhow::ensure!(
            spec_is_json(path)?,
            "--spec requires a prepared JSON RunSpec; use --config for TOML RunConfig files"
        );
        return run_prepared_spec(args).await;
    }
    if let Some(path) = args.config.as_deref() {
        anyhow::ensure!(
            !spec_is_json(path)?,
            "--config requires a TOML RunConfig; use --spec for prepared JSON RunSpecs"
        );
    }
    // Host runs keep the best-effort lifecycle/evidence profile by default;
    // filesystem restrictions, staging, and network isolation remain opt-in.
    // `--safe`/`--ask` request the Agent-aware preset on top of that.
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut config = load_run_config(&args, personal_config_root().as_deref(), true)?;
    apply_run_options(&mut config, args.clone())?;
    normalize_filesystem_config(&mut config)?;
    if args.run.safe || args.audit {
        warn_safe_preset(&config, &args);
    }
    pvisor::startup_mark_run("cli.config_ready", &run_id);
    execute_config(
        config,
        run_id,
        args.run.safe || args.audit,
        None,
        PolicySource::CurrentDefaults,
    )
    .await
}

fn directory_size_bytes(root: &Path) -> anyhow::Result<u64> {
    fn visit(path: &Path) -> anyhow::Result<u64> {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("stat stage entry {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Ok(0);
        }
        if metadata.is_file() {
            return Ok(metadata.len());
        }
        if !metadata.is_dir() {
            return Ok(0);
        }
        let mut total = 0u64;
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("read stage directory {}", path.display()))?
        {
            total = total
                .checked_add(visit(&entry?.path())?)
                .ok_or_else(|| anyhow::anyhow!("stage size overflows u64"))?;
        }
        Ok(total)
    }
    visit(root)
}

async fn run_prepared_spec(args: RunArgs) -> anyhow::Result<i32> {
    anyhow::ensure!(
        args.features.is_empty(),
        "feature workload-aware-memory-offloading requires a VM executor; JSON --spec supports only host"
    );
    if let Some(path) = &args.config {
        RunConfig::from_file(path)?
            .features
            .validate(RunExecutorKind::Host)?;
    }
    anyhow::ensure!(
        !args.run.safe && !args.audit,
        "--safe/--ask cannot modify a prepared JSON RunSpec"
    );
    anyhow::ensure!(
        args.command.is_empty(),
        "a command cannot be combined with a JSON --spec"
    );
    anyhow::ensure!(
        args.run
            .executor
            .is_none_or(|executor| executor == RunExecutorKind::Host),
        "JSON --spec must execute with --executor host"
    );
    let spec_path = args.spec.clone().context("missing --spec JSON file")?;
    let result_path = args
        .result_file
        .clone()
        .context("JSON --spec requires --result-file")?;
    let stage_spec = Some(args.stage.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("pvisor-stage-{}", uuid::Uuid::new_v4()))
    }));
    let spec: RunSpec = serde_json::from_slice(
        &std::fs::read(&spec_path)
            .with_context(|| format!("read delegated RunSpec from {}", spec_path.display()))?,
    )
    .context("decode delegated RunSpec")?;
    let mut stage_guard = None;
    let registration_stage = stage_spec.clone();
    let (pvisor, config) = if let Some(stage_path) = stage_spec {
        let cleanup = args.stage.is_none();
        if cleanup {
            if stage_path.exists() {
                let nonempty = std::fs::read_dir(&stage_path)?.next().is_some();
                anyhow::ensure!(
                    !nonempty,
                    "temporary stage must be empty before use: {}",
                    stage_path.display()
                );
            } else {
                std::fs::create_dir_all(&stage_path)?;
            }
            std::fs::write(
                stage_path.join(STAGE_OWNER_FILE),
                spec.run_id.as_str().as_bytes(),
            )?;
            stage_guard = Some(TemporaryStageGuard::new(
                stage_path.clone(),
                spec.run_id.to_string(),
            ));
        }
        let mut config = RunConfig::default();
        config.run.agent = spec.agent.name.clone();
        let RunInvocation::Process(process) = &spec.invocation;
        config.run.command = std::iter::once(process.program.clone())
            .chain(process.args.iter().cloned())
            .collect();
        config.run.workspace = process.cwd.as_deref().map(PathBuf::from);
        apply_cli(&mut config, args)?;
        anyhow::ensure!(
            config.vm.control_socket.is_none(),
            "vm.control_socket requires a VM executor; JSON --spec supports only host"
        );
        anyhow::ensure!(
            config.run.executor == RunExecutorKind::Host,
            "JSON --spec currently supports only the host executor"
        );
        anyhow::ensure!(
            config.overlayfs.as_ref().is_none_or(|overlay| {
                overlay.base.is_none()
                    && overlay.target.is_none()
                    && overlay.merged_dir.is_none()
                    && overlay.compose.is_empty()
                    && overlay.access_policy == Default::default()
                    && overlay.commit == OverlayFsCommit::Manual
                    && overlay.stage.as_deref() == Some(stage_path.as_path())
            }),
            "JSON --spec accepts only --stage as an OverlayFS override"
        );
        anyhow::ensure!(
            config.record.destination.is_none(),
            "JSON --spec does not accept recording overrides"
        );
        let storage = resolve_run_storage(&stage_path)?;
        #[cfg(feature = "gateway")]
        let proxy = resolve_proxy(&config)?;
        #[allow(unused_mut)]
        let mut builder = PVisor::builder()
            .storage(&storage)
            .executors(vec![report_terminal(Arc::new(ProcessExecutor::default()))])
            .network(
                NetworkDriverConfig::new(
                    config.overlaynet.mode,
                    NetworkConfig {
                        capability: None,
                        mode: match config.overlaynet.policy {
                            OverlayNetPolicy::Public => NetworkMode::Public,
                            OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
                            OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
                        },
                        allowed_hosts: config.overlaynet.allow.clone(),
                        rules: config.overlaynet.rules.clone(),
                        deny_rules: config.overlaynet.deny.clone(),
                        limits: config.overlaynet.limits.clone(),
                    },
                )
                .listen(&config.overlaynet.listen),
            );
        if let Some(path) = &config.vm.control_socket {
            builder = builder.control_socket(path);
        }
        #[cfg(feature = "gateway")]
        if let Some(proxy) = proxy {
            builder = builder.gateway(
                GatewayDriverConfig::new(proxy)
                    .output_dir(&storage)
                    .gateway_enabled(config.gateway.mode == GatewayMode::Capture),
            );
        }
        (builder.build(), config)
    } else {
        (
            PVisor::builder()
                .executors(vec![report_terminal(Arc::new(ProcessExecutor::default()))])
                .build(),
            RunConfig::default(),
        )
    };
    let managed =
        pvisor::job_service::RuntimeJobService::start_managed(&pvisor, spec, config).await?;
    announce_control_socket(managed.handle());
    let agentctl = managed.handle().agentctl();
    let result = wait_cli_job(managed, registration_stage.as_deref()).await?;
    let output = pvisor::DelegatedRunOutput {
        agentctl: agentctl.snapshot(),
        result,
    };
    let write_result = pvisor::write_private_json(&result_path, &output)
        .with_context(|| format!("write delegated RunResult to {}", result_path.display()));
    let cleanup_result = stage_guard
        .as_mut()
        .map(|guard| cleanup_temporary_stage(&guard.path, output.result.run_id.as_str(), true))
        .transpose()?;
    if let Some(guard) = stage_guard.as_mut() {
        guard.disarm();
    }
    write_result?;
    let _ = cleanup_result;
    Ok(match output.result.state {
        RunState::Completed => output.result.exit_code.unwrap_or(0),
        RunState::Hibernated => 0,
        RunState::Cancelled => 130,
        _ => output.result.exit_code.unwrap_or(1),
    })
}

fn cleanup_temporary_stage(path: &Path, run_id: &str, successful: bool) -> anyhow::Result<()> {
    let owner = std::fs::read(path.join(STAGE_OWNER_FILE)).ok();
    let owned = owner.as_deref() == Some(run_id.as_bytes());
    if !owned {
        let error = anyhow::anyhow!(
            "temporary stage ownership marker is missing or does not match: {}",
            path.display()
        );
        if successful {
            return Err(error);
        }
        run_log!(
            "pVisor warning: refusing to remove unowned temporary stage {}",
            path.display()
        );
        return Ok(());
    }
    if let Err(error) = std::fs::remove_dir_all(path) {
        if successful {
            return Err(error)
                .with_context(|| format!("remove temporary stage {}", path.display()));
        }
        run_log!(
            "pVisor warning: failed to remove temporary stage {}: {error}",
            path.display()
        );
    }
    Ok(())
}

struct TemporaryStageGuard {
    path: PathBuf,
    run_id: String,
    armed: bool,
}

impl TemporaryStageGuard {
    fn new(path: PathBuf, run_id: String) -> Self {
        Self {
            path,
            run_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryStageGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = cleanup_temporary_stage(&self.path, &self.run_id, false);
        }
    }
}

impl RunArgs {
    pub(super) fn inherits_terminal_input(&self) -> anyhow::Result<bool> {
        let Some(path) = &self.spec else {
            // The normal CLI config path always constructs inherited stdin.
            return Ok(true);
        };
        let spec: RunSpec = serde_json::from_slice(&std::fs::read(path)?)?;
        let RunInvocation::Process(process) = &spec.invocation;
        Ok(process.stdin == StdioMode::Inherit)
    }
}

// Session publishes its final status only after driver teardown. Observe the
// typed executor outcome at the CLI boundary first, so a terminal signal arms
// service-owned escalation even if subsequent driver cleanup never returns.
struct TerminalReportingExecutor(Arc<dyn RunExecutor>);
#[async_trait::async_trait]
impl RunExecutor for TerminalReportingExecutor {
    fn descriptor(&self) -> pvisor_core::ExecutorPlan {
        self.0.descriptor()
    }
    fn supports(&self, invocation: &pvisor_core::RunInvocation) -> bool {
        self.0.supports(invocation)
    }
    fn supports_vm_network_attachment(&self) -> bool {
        self.0.supports_vm_network_attachment()
    }
    fn supports_guest_workspace_overlay(&self) -> bool {
        self.0.supports_guest_workspace_overlay()
    }
    fn supports_cpu_qos(&self) -> bool {
        self.0.supports_cpu_qos()
    }
    async fn execute(&self, session: &pvisor::Session) -> pvisor::ExecutorOutput {
        let output = self.0.execute(session).await;
        if output
            .failure
            .as_ref()
            .is_some_and(|f| f.kind == pvisor_core::RunFailureKind::ProcessExit)
            && let Some(signal) = output.executor_observations.termination_signal
            && [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal)
        {
            super::host_service::notify_cancel(signal);
        }
        if output.state == RunState::Cancelled {
            super::host_service::notify_cleanup();
        }
        output
    }
}

pub(super) fn report_terminal(executor: Arc<dyn RunExecutor>) -> Arc<dyn RunExecutor> {
    Arc::new(TerminalReportingExecutor(executor))
}

pub(super) async fn wait_cli_job(
    managed: pvisor::job_service::ManagedJobRun,
    stage: Option<&Path>,
) -> anyhow::Result<pvisor_core::RunResult> {
    managed
        .wait_with(|handle| async move {
            if let Some(stage) = stage {
                super::host_cancel::register(&handle, stage)?;
            }
            wait_cli_run(handle).await
        })
        .await
}

pub(super) async fn wait_cli_run(
    handle: pvisor::RunHandle,
) -> anyhow::Result<pvisor_core::RunResult> {
    let token = handle.cancellation.clone();
    let cancellation = handle.cancellation();
    let wait = handle.wait();
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => Ok(result?),
        _ = token.cancelled() => {
            super::host_service::notify_cleanup();
            Ok(wait.await?)
        }
        _ = delegated_shutdown_signal() => {
            super::host_service::notify_cleanup();
            cancellation.cancel();
            Ok(wait.await?)
        }
    }
}

async fn delegated_shutdown_signal() {
    #[cfg(unix)]
    if let Some(token) = super::host_service::worker_cancellation() {
        token.cancelled().await;
        return;
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn execute_config(
    mut config: RunConfig,
    run_id: String,
    safe: bool,
    lineage: Option<RunLineage>,
    policy_source: PolicySource,
) -> anyhow::Result<i32> {
    config.features.validate(config.run.executor)?;
    normalize_filesystem_config(&mut config)?;
    if config.run.executor == RunExecutorKind::Vm {
        config.vm.control_socket =
            super::host_service::vm_control_socket(config.vm.control_socket.as_deref())?;
    }
    pvisor::startup_mark_run("cli.rootfs_begin", &run_id);
    resolve_default_vm_rootfs(&mut config)?;
    let mut _image_attachment: Option<pvisor::cache::DirectImage> = None;
    let prepared_image = if config.run.executor == RunExecutorKind::Vm && config.vm.rootfs.is_none()
    {
        let image = config
            .vm
            .image
            .clone()
            .context("VM image must be explicitly configured")?;
        let store = config.vm.image_store.clone();
        run_log!("pVisor image: resolving {image}");
        let (prepared, mount) =
            tokio::task::spawn_blocking(move || pvisor::cache::prepare_vm_image(&image, store))
                .await
                .context("OCI image preparation task failed")??;
        _image_attachment = mount;
        run_log!(
            "pVisor image: {} ({})",
            prepared.digest,
            prepared.rootfs.display()
        );
        config.vm.rootfs = Some(prepared.rootfs.clone());
        config.vm.rootfs_immutable = true;
        if config.run.command.is_empty() {
            config.run.command = prepared.entrypoint.clone();
            config.run.command.extend(prepared.cmd.clone());
        }
        Some(prepared)
    } else {
        None
    };
    if config.run.executor == RunExecutorKind::Vm {
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        anyhow::bail!(
            "VM execution is unsupported on Intel macOS; use Linux x86_64 or Apple Silicon macOS"
        );
        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        {
            let (rootfs, workspace) = resolve_vm_layout(&config)?;
            config.vm.rootfs = Some(rootfs.clone());
            config.run.workspace = Some(workspace.clone());
            let has_guest_overlay = config
                .overlayfs
                .as_ref()
                .and_then(|overlay| overlay.target.as_ref())
                .is_some();
            if !has_guest_overlay {
                let overlay = config
                    .overlayfs
                    .get_or_insert_with(OverlayFsSettings::default);
                // Keep the host workspace path stable inside the guest. The
                // workspace is a separate virtio-fs mount; using the rootfs as its
                // base would make `cwd` point at a path that does not exist in an
                // image guest and would bypass workspace staging.
                overlay.base = Some(workspace.clone());
                overlay.target = Some(workspace.clone());
                overlay.commit = OverlayFsCommit::Manual;
            }
            #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
            anyhow::ensure!(
                config.vm.library_dir.is_none(),
                "--vm-library-dir is unavailable in the static musl build; libkrun's kernel bundle is embedded"
            );
        }
    } else {
        if let Some(base) = config
            .overlayfs
            .as_ref()
            .and_then(|overlay| overlay.base.as_ref())
        {
            // The OverlayFS base is the project association for host and
            // container runs when supplied by a config file.
            config.run.workspace = Some(base.clone());
        }
        // On host/container runs the path is a real host mount point visible
        // to the Agent. VM runs keep `target` as the guest-visible path.
        if let Some(overlay) = &mut config.overlayfs {
            // The target is translated to a host merged mount below, after
            // the workspace has been canonicalized. A missing path means the
            // current workspace itself, preserving transparent cwd semantics.
            if overlay.merged_dir.is_none() {
                overlay.merged_dir = overlay.target.take();
            }
        }
    }
    validate(&config, safe)?;

    let filesystem_isolated = config.filesystem == FilesystemMode::Sandbox;

    // The rootless capability check launches `unshare`, but does not depend on
    // workspace, storage, OverlayFS, or Gateway configuration. Run it while
    // those independent inputs are resolved instead of blocking at executor
    // construction.
    #[cfg(target_os = "linux")]
    let rootless_probe = (config.run.executor == RunExecutorKind::Host).then(|| {
        tokio::task::spawn_blocking(move || {
            pvisor::rootless_runtime_available(!filesystem_isolated)
        })
    });

    if config.overlaynet.mode == OverlayNetMode::Proxy && !safe {
        run_log!(
            "pVisor OverlayNet boundary: explicit cooperative proxy; direct sockets remain ambient"
        );
    } else if config.run.executor == RunExecutorKind::Vm
        && config.overlaynet.mode == OverlayNetMode::Auto
    {
        run_log!(
            "pVisor OverlayNet boundary: non-bypassable pvisor-vm virtio-net → smoltcp IPv4 TCP/DNS"
        );
    }

    let workspace = config
        .run
        .workspace
        .as_deref()
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let workspace = resolve_workspace(&workspace)?;
    if config.run.executor == RunExecutorKind::Container {
        let mut sources = vec![workspace.clone()];
        sources.extend(
            config
                .container
                .mounts
                .iter()
                .map(|mount| mount.source.clone()),
        );
        if let Some(rootfs) = &config.container.rootfs
            && rootfs != Path::new("/")
        {
            sources.push(rootfs.clone());
        }
        super::host_service::reject_guest_exposure(sources)?;
    }
    let storage = resolve_run_storage(&select_run_storage(&config, &workspace, &run_id)?)?;
    policy_source.load_defaults(&mut config, &workspace, personal_config_root().as_deref())?;
    if config
        .policies
        .scopes()
        .iter()
        .any(|(_, layer)| layer.filesystem.is_some())
        && config.overlayfs.is_none()
    {
        config.overlayfs = Some(Default::default());
    }
    if config
        .policies
        .scopes()
        .iter()
        .any(|(_, layer)| layer.network.is_some())
        && config.run.executor != RunExecutorKind::Vm
        && config.overlaynet.mode == OverlayNetMode::Auto
    {
        config.overlaynet.mode = OverlayNetMode::Proxy;
    }
    let mut overlay = resolve_overlay(&config, &workspace, &storage)?;
    #[cfg(unix)]
    super::terminal::announce_stage(
        overlay
            .as_ref()
            .and_then(|hint| hint.stage_dir.as_deref())
            .unwrap_or(&storage),
    );
    if config.run.executor == RunExecutorKind::Vm
        && config.vm.rootfs_immutable
        && config
            .overlayfs
            .as_ref()
            .and_then(|overlay| overlay.target.as_ref())
            .is_none()
        && let Some(overlay) = &mut overlay
    {
        overlay.protect_target = true;
    }
    let overlay_enabled = overlay.is_some();
    let network_namespace_required = config.run.executor == RunExecutorKind::Host
        && config.overlaynet.policy == OverlayNetPolicy::Deny;
    let resolved_stage_for_limit = overlay.as_ref().and_then(|hint| hint.stage_dir.clone());
    #[cfg(feature = "gateway")]
    let proxy = resolve_proxy(&config)?;

    #[cfg(feature = "gateway")]
    if config.gateway.debug {
        // `pvisor run` is a foreground CLI: mirror opted-in gateway/network
        // diagnostics to stderr as well as the Run log, so proxy failures can
        // be diagnosed without locating storage after the child exits.
        pvisor_gateway::runtime::debug::enable_debug_stderr();
        pvisor_gateway::runtime::debug::enable_debug(&storage)?;
    }

    if config
        .record
        .destination
        .as_ref()
        .is_some_and(|path| path.to_string_lossy().contains("://"))
    {
        bail!("--record-destination only accepts a local path; remote URIs are unsupported");
    }
    let mut json_writer = None;
    let event_sink: Arc<dyn pvisor::EventSink> = if config.gateway.mode == GatewayMode::Capture
        || config.overlaynet.mode == OverlayNetMode::Proxy
        || config.record.destination.is_some()
    {
        let destination = config
            .record
            .destination
            .clone()
            .unwrap_or_else(|| storage.join(".capture"));
        let writer = JournalRecording::open(&destination)
            .with_context(|| format!("open trace journal destination {}", destination.display()))?;
        let event_sink = Arc::new(writer.journal.clone());
        json_writer = Some(writer);
        event_sink
    } else {
        Arc::new(pvisor::trace::Journal::memory())
    };

    #[cfg(target_os = "linux")]
    let rootless_available = match rootless_probe {
        Some(probe) => probe.await.context("rootless capability probe failed")?,
        None => false,
    };

    let executor: Arc<dyn RunExecutor> = match config.run.executor {
        #[cfg(target_os = "linux")]
        RunExecutorKind::Host => {
            // The --safe/--ask preset demands its boundary; the independent
            // --filesystem/--overlaynet policies stay best-effort and fall
            // back to the host process with a warning.
            if rootless_available {
                match ProcessExecutor::rootless_with_launcher(std::env::current_exe()?) {
                    Ok(executor) => Arc::new(executor),
                    Err(error) if safe => {
                        return Err(error)
                            .context("required sandbox unavailable: Linux rootless launcher");
                    }
                    Err(error) => {
                        run_log!(
                            "pVisor safe-best-effort: rootless launcher unavailable ({error}); falling back to host process"
                        );
                        Arc::new(ProcessExecutor::default())
                    }
                }
            } else if safe {
                bail!("required sandbox unavailable: Linux rootless namespaces must be enabled");
            } else {
                run_log!(
                    "pVisor safe-best-effort: user/mount/PID namespaces unavailable; falling back to host process"
                );
                Arc::new(ProcessExecutor::default())
            }
        }
        #[cfg(target_os = "macos")]
        RunExecutorKind::Host => {
            match ProcessExecutor::seatbelt_with_launcher(std::env::current_exe()?) {
                Ok(executor) => Arc::new(executor),
                Err(error) if safe => {
                    return Err(error)
                        .context("required sandbox unavailable: macOS Seatbelt launcher");
                }
                Err(error) => {
                    run_log!(
                        "pVisor safe-best-effort: Seatbelt unavailable ({error}); falling back to host process"
                    );
                    Arc::new(ProcessExecutor::default())
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        RunExecutorKind::Host if safe => {
            bail!("required host sandbox is unsupported on this platform")
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        RunExecutorKind::Host if filesystem_isolated => Arc::new(ProcessExecutor::default()),
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        RunExecutorKind::Host => Arc::new(ProcessExecutor::default()),
        RunExecutorKind::Container => Arc::new(ContainerExecutor::new(config.container.clone())?),
        RunExecutorKind::Vm => {
            pvisor::startup_mark_run("cli.vm_inputs_ready", &run_id);
            Arc::new(VmExecutor::new(config.vm.clone())?.with_features(config.features.clone())?)
        }
    };
    policy_source.validate_executor(&executor.descriptor())?;
    let mut builder = PVisor::builder()
        .storage(&storage)
        .event_sink(event_sink)
        .executors(vec![report_terminal(executor)])
        .network(
            NetworkDriverConfig::new(
                config.overlaynet.mode,
                NetworkConfig {
                    capability: None,
                    mode: match config.overlaynet.policy {
                        OverlayNetPolicy::Public => NetworkMode::Public,
                        OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
                        OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
                    },
                    allowed_hosts: config.overlaynet.allow.clone(),
                    rules: config.overlaynet.rules.clone(),
                    deny_rules: config.overlaynet.deny.clone(),
                    limits: config.overlaynet.limits.clone(),
                },
            )
            .listen(&config.overlaynet.listen),
        );
    if let Some(path) = &config.vm.control_socket {
        builder = builder.control_socket(path);
    }
    #[cfg(feature = "gateway")]
    if let Some(proxy) = proxy {
        builder = builder.gateway(
            GatewayDriverConfig::new(proxy)
                .output_dir(&storage)
                .gateway_enabled(config.gateway.mode == GatewayMode::Capture),
        );
    }
    if let Some(overlay) = overlay {
        builder = builder.overlay(overlay);
    }
    let pvisor = builder.build();

    let (program, program_args) = config
        .run
        .command
        .split_first()
        .context("missing Agent command; pass it after `--` or set run.command")?;
    let mut spec = RunSpec::process(run_id.as_str(), &config.run.agent, program);
    if config.run.executor == RunExecutorKind::Vm
        && config
            .vm
            .rootfs
            .as_deref()
            .is_some_and(|path| path != Path::new("/"))
    {
        let store = execution_store_location(&config, &workspace, &storage, &run_id)?;
        spec.metadata.insert(
            pvisor::job_execution::STORE_KEY.into(),
            serde_json::to_value(store)?,
        );
    }
    spec.policies = config.policies.clone();
    spec.capabilities.filesystem = resolve_filesystem_grants(&config, &workspace, &storage)?;
    if let Some(path) = &config.gateway.zcode_builtin_config {
        spec.metadata.insert(
            "pvisor.gateway.zcode_builtin_config".into(),
            serde_json::to_value(path.canonicalize()?)?,
        );
    }
    if let Some(profile) = config.gateway.profile {
        spec.metadata.insert(
            "pvisor.gateway.profile".into(),
            serde_json::to_value(profile)?,
        );
    }
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = program_args.to_vec();
    process.stdin = StdioMode::Inherit;
    process.stdout = match config.run.stdio {
        RunStdio::Inherit => StdioMode::Inherit,
        RunStdio::Capture => StdioMode::Capture,
    };
    process.stderr = process.stdout;
    process.inherit_env = config.run.inherit_env;
    if let Some(image) = &prepared_image {
        process.inherit_env = false;
        process.env.extend(image.env.clone());
    }
    if !process.inherit_env {
        project_safe_baseline_environment(&mut process.env);
    }
    for key in &config.run.pass_env {
        anyhow::ensure!(
            valid_environment_name(key),
            "--pass-env requires a valid environment variable name, got {key:?}"
        );
        if let Ok(value) = std::env::var(key) {
            process.env.insert(key.clone(), value);
        }
    }
    if !overlay_enabled {
        process.cwd = Some(workspace.display().to_string());
    }
    if safe {
        spec.metadata
            .insert(pvisor::sandbox::REQUIRED_SANDBOX_KEY.into(), true.into());
    }
    spec.runtime.timeout_ms = config.run.timeout_ms;
    spec.runtime.resource_limits = config.run.resource_limits.clone();
    spec.metadata.insert(
        "pvisor.environment".into(),
        serde_json::json!({
            "inherits_host": process.inherit_env,
            "projected_keys": process.env.keys().cloned().collect::<Vec<_>>(),
        }),
    );
    spec.metadata.insert(
        "pvisor.workspace".into(),
        serde_json::Value::String(workspace.display().to_string()),
    );
    spec.metadata.insert(
        "pvisor.stage".into(),
        serde_json::json!({
            "scope": "whole-rootfs",
            "path": config.overlayfs.as_ref().and_then(|overlay| overlay.stage.as_ref()).map(|path| path.display().to_string()),
            "size_limit_bytes": config.overlayfs.as_ref().and_then(|overlay| overlay.stage_size_bytes),
        }),
    );
    if config.run.executor == RunExecutorKind::Vm {
        if let Some(target) = config
            .overlayfs
            .as_ref()
            .and_then(|overlay| overlay.target.as_ref())
        {
            spec.metadata.insert(
                "pvisor.vm.overlay_target".into(),
                serde_json::Value::String(target.display().to_string()),
            );
            spec.metadata.insert(
                "pvisor.vm.guest_cwd".into(),
                serde_json::Value::String(target.display().to_string()),
            );
        } else {
            spec.metadata.insert(
                "pvisor.vm.guest_cwd".into(),
                serde_json::Value::String("/".into()),
            );
        }
        if let Some(image) = &prepared_image {
            spec.metadata.insert(
                "pvisor.vm.image_digest".into(),
                serde_json::Value::String(image.digest.clone()),
            );
        }
    }
    if config.run.policy == RunPolicy::Enforce {
        spec.runtime.policy_mode = PolicyMode::Enforce;
    }
    if let Some(lineage) = &lineage {
        spec.metadata
            .insert("pvisor.lineage".into(), serde_json::to_value(lineage)?);
    }
    // CLI runs always carry the best-effort lifecycle/evidence profile;
    // filesystem restrictions, staging, and network isolation remain opt-in.
    spec.metadata
        .insert("pvisor.safe".into(), serde_json::Value::Bool(true));
    spec.metadata.insert(
        "pvisor.filesystem.mode".into(),
        serde_json::Value::String(
            match config.filesystem {
                FilesystemMode::Host => "host",
                FilesystemMode::Sandbox => "sandbox",
            }
            .into(),
        ),
    );

    if safe {
        spec.metadata
            .insert(pvisor::sandbox::LANDLOCK_SANDBOX_KEY.into(), true.into());
    }
    {
        let network_boundary = if config.run.executor == RunExecutorKind::Vm
            && config.overlaynet.mode == OverlayNetMode::Auto
        {
            "non-bypassable smoltcp IPv4 TCP/DNS"
        } else if cfg!(any(target_os = "linux", target_os = "macos"))
            && config.run.executor == RunExecutorKind::Host
            && config.overlaynet.policy == OverlayNetPolicy::Deny
        {
            if cfg!(target_os = "linux") {
                "private deny-all network namespace"
            } else {
                "Seatbelt deny-all socket policy"
            }
        } else {
            "cooperative network review"
        };
        let filesystem_boundary = if filesystem_isolated {
            "restricted filesystem access"
        } else {
            "host filesystem access"
        };
        let staging = if overlay_enabled {
            "staged workspace"
        } else {
            "workspace writes are direct"
        };
        run_log!("pVisor safe profile: {filesystem_boundary} + {staging} + {network_boundary}");
        run_log!("workspace: {}", workspace.display());
        run_log!("Job storage: {}", storage.display());
        match config.run.executor {
            RunExecutorKind::Host => {
                #[cfg(target_os = "linux")]
                {
                    let process_boundary = if filesystem_isolated {
                        "rootless user/mount/PID namespaces + PID 1 reaper"
                    } else if network_namespace_required {
                        "private network namespace + process supervisor"
                    } else {
                        "host process"
                    };
                    run_log!(
                        "boundary: {process_boundary}; filesystem and network boundaries follow the selected policies"
                    );
                }
                #[cfg(target_os = "macos")]
                if filesystem_isolated && overlay_enabled {
                    run_log!(
                        "boundary: Seatbelt-enforced staged writes; reads and selective network policies remain ambient/cooperative"
                    );
                } else if filesystem_isolated {
                    run_log!(
                        "boundary: Seatbelt-enforced filesystem writes; reads and selective network policies remain ambient/cooperative"
                    );
                } else if network_namespace_required {
                    run_log!(
                        "boundary: Seatbelt-enforced deny-all network; filesystem access remains host-visible"
                    );
                } else {
                    run_log!(
                        "boundary: host process; filesystem access and selective network policies remain host-visible/cooperative"
                    );
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                run_log!("boundary: review-only host process");
            }
            RunExecutorKind::Container => run_log!(
                "boundary: OCI container process; direct sockets remain outside proxy enforcement"
            ),
            RunExecutorKind::Vm => {
                run_log!(
                    "boundary: pvisor-vm Linux guest (KVM on Linux, HVF on macOS); virtio-net is owned by pVisor smoltcp and Gateway capture uses a virtual guest route"
                )
            }
        }
    }
    pvisor::startup_mark_run("cli.session_begin", &run_id);
    let managed =
        pvisor::job_service::RuntimeJobService::start_managed(&pvisor, spec, config.clone())
            .await?;
    announce_control_socket(managed.handle());
    pvisor::startup_mark_run("cli.session_started", &run_id);
    let result = wait_cli_job(managed, Some(&storage)).await?;
    pvisor::startup_mark_run("cli.run_finished", &run_id);
    drop(pvisor);
    if let Some(writer) = json_writer {
        writer.finish()?;
    }
    if result.state == RunState::Completed
        && let Some(limit) = config
            .overlayfs
            .as_ref()
            .and_then(|overlay| overlay.stage_size_bytes)
        && let Some(path) = resolved_stage_for_limit
        && path.exists()
    {
        let actual = directory_size_bytes(&path).context("measure OverlayFS stage size")?;
        if actual > limit {
            bail!(
                "stage size limit exceeded: {} uses {} bytes (limit {})",
                path.display(),
                actual,
                limit
            );
        }
    }
    let record = resolve_run(Some(Path::new(&run_id)), &storage)
        .with_context(|| format!("load finalized Run record for {run_id}"))?;
    pvisor::startup_mark_run("cli.result_loaded", &run_id);
    let bundle = RunBundle::read(&record.stage_dir()).with_context(|| {
        format!(
            "load finalized Run Bundle from {}",
            record.stage_dir().display()
        )
    })?;
    let bundle_path = RunBundle::path(&record.stage_dir());
    run_log!("Run Bundle: {}", bundle_path.display());
    run_log!(
        "Review: pvisor status --review {}",
        record.stage_dir().display()
    );
    if bundle.filesystem.is_some() {
        run_log!(
            "Decide: pvisor apply {} | pvisor drop {}",
            record.stage_dir().display(),
            record.stage_dir().display()
        );
    }

    if result.state != RunState::Completed {
        if let Some(failure) = &result.failure {
            run_log!("pVisor Job failed: {:?}: {}", failure.kind, failure.message);
        }
        for warning in &result.warnings {
            run_log!("pVisor Job warning: {warning}");
        }
    }
    Ok(match result.state {
        RunState::Completed => result.exit_code.unwrap_or(0),
        RunState::Hibernated => 0,
        RunState::Cancelled => 130,
        _ => result.exit_code.unwrap_or(1),
    })
}

fn normalize_filesystem_config(config: &mut RunConfig) -> anyhow::Result<()> {
    let Some(filesystem) = config.overlayfs.as_mut() else {
        return Ok(());
    };
    let mut mount_target: Option<PathBuf> = None;
    for mount in std::mem::take(&mut filesystem.mount) {
        let target = mount.target.unwrap_or_else(|| mount.source.clone());
        if target != mount.source {
            if let Some(existing) = &mount_target {
                anyhow::ensure!(
                    existing == &target,
                    "filesystem mounts with different targets cannot share one overlay view"
                );
            } else {
                mount_target = Some(target.clone());
            }
        }
        match mount.access {
            FilesystemAccessLevel::Deny
            | FilesystemAccessLevel::Ask
            | FilesystemAccessLevel::Warn => anyhow::bail!(
                "filesystem mounts require read, stage, or write; use --access for deny, ask, or warn"
            ),
            FilesystemAccessLevel::Stage => {
                filesystem.compose.push(mount.source);
            }
            FilesystemAccessLevel::Read | FilesystemAccessLevel::Write => {
                anyhow::ensure!(
                    target == mount.source,
                    "read/write shares require target to equal source"
                );
                config.run.filesystem.push(FilesystemCapability {
                    path: mount.source.display().to_string(),
                    access: if mount.access == FilesystemAccessLevel::Read {
                        FilesystemAccess::Read
                    } else {
                        FilesystemAccess::ReadWrite
                    },
                });
            }
        }
    }
    if let Some(target) = mount_target {
        filesystem.target = Some(target);
    }
    for rule in std::mem::take(&mut filesystem.access) {
        match rule.level {
            FilesystemAccessLevel::Deny => {
                let mut deny = filesystem.access_policy.deny().to_vec();
                deny.push(normalize_policy_glob(&rule.path));
                filesystem.access_policy = pvisor_core::FileAccessPolicy::new_with_ask(
                    deny,
                    filesystem.access_policy.ask().to_vec(),
                    filesystem.access_policy.warn().to_vec(),
                )?;
            }
            FilesystemAccessLevel::Ask => {
                let mut ask = filesystem.access_policy.ask().to_vec();
                ask.push(normalize_policy_glob(&rule.path));
                filesystem.access_policy = pvisor_core::FileAccessPolicy::new_with_ask(
                    filesystem.access_policy.deny().to_vec(),
                    ask,
                    filesystem.access_policy.warn().to_vec(),
                )?;
            }
            FilesystemAccessLevel::Warn => {
                let mut warn = filesystem.access_policy.warn().to_vec();
                warn.push(normalize_policy_glob(&rule.path));
                filesystem.access_policy = pvisor_core::FileAccessPolicy::new_with_ask(
                    filesystem.access_policy.deny().to_vec(),
                    filesystem.access_policy.ask().to_vec(),
                    warn,
                )?;
            }
            FilesystemAccessLevel::Read
            | FilesystemAccessLevel::Stage
            | FilesystemAccessLevel::Write => anyhow::bail!(
                "filesystem access rules use deny, ask, or warn; use --mount for read, stage, or write"
            ),
        }
    }
    Ok(())
}

fn normalize_policy_glob(path: &str) -> String {
    path.strip_prefix("/workspace/")
        .or_else(|| path.strip_prefix("/workspace"))
        .unwrap_or(path.trim_start_matches('/'))
        .to_string()
}

fn project_safe_baseline_environment(env: &mut std::collections::BTreeMap<String, String>) {
    for key in [
        "PATH",
        "HOME",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "COLORTERM",
        "TZ",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.entry(key.into()).or_insert(value);
        }
    }
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        && !name.as_bytes()[0].is_ascii_digit()
}

fn apply_safe_defaults(config: &mut RunConfig) -> anyhow::Result<()> {
    // Preserve Codex account/routing discovery through CODEX_* and provider variables.
    config.run.inherit_env = config
        .run
        .command
        .first()
        .and_then(|command| Path::new(command).file_name())
        .and_then(|name| name.to_str())
        == Some("codex");
    if config.overlaynet.listen == OverlayNetSettings::default().listen {
        config.overlaynet.listen = free_loopback_address()?;
    }
    if config.gateway.admin_listen == pvisor::GatewaySettings::default().admin_listen {
        config.gateway.admin_listen = free_loopback_address()?;
    }
    if config.run.agent == "agent"
        && let Some(program) = config.run.command.first()
    {
        config.run.agent = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("agent")
            .to_owned();
    }
    Ok(())
}

fn free_loopback_address() -> anyhow::Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

/// Resolve ordinary defaults/config, then the opt-in preset, then explicit CLI values.
fn apply_run_options(config: &mut RunConfig, args: RunArgs) -> anyhow::Result<()> {
    if args.run.safe || args.audit {
        // Resolve the actual command/executor/routes first, without mistaking --name for an Agent.
        let mut requested = config.clone();
        apply_cli(&mut requested, args.clone())?;
        use clap::Parser;
        let patch = safe::patch(&requested, args.audit);
        let super::Command::Run(patch) = super::Cli::try_parse_from(
            ["pvisor".to_owned(), "run".to_owned()]
                .into_iter()
                .chain(patch),
        )?
        .command
        else {
            unreachable!("expected run command")
        };
        apply_cli_unvalidated(config, *patch)?;
        // The preset requests the sandboxed filesystem view so the launcher
        // installs the synthetic root/Landlock or Seatbelt write controls.
        // An explicit --filesystem value in `args` still wins below.
        config.filesystem = FilesystemMode::Sandbox;
    }
    apply_cli(config, args.clone())?;
    apply_safe_defaults(config)?;
    Ok(())
}

fn warn_safe_preset(config: &RunConfig, args: &RunArgs) {
    run_log!(
        "pVisor --safe: executor={:?}, network={:?}; CLI > safe preset > config > defaults",
        config.run.executor,
        config.overlaynet.policy
    );
    for rule in &config.overlaynet.rules {
        run_log!(
            "pVisor --safe: allowed destination {:?}, ports {:?}",
            rule.host,
            rule.ports
        );
    }
    if config.overlaynet.policy == OverlayNetPolicy::Deny {
        run_log!(
            "pVisor --safe: ordinary egress denied; configured Gateway routes remain separate. Use --overlaynet-allow or configure Gateway routes for an unrecognized/custom Agent."
        );
    }
    if config.overlaynet.mode == OverlayNetMode::Off {
        run_log!("pVisor --safe warning: explicit CLI disabled OverlayNet");
    } else if config.overlaynet.mode == OverlayNetMode::Auto
        && config.run.executor != RunExecutorKind::Vm
        && config.gateway.mode == GatewayMode::Off
    {
        run_log!(
            "pVisor --safe warning: explicit CLI selected auto without VM/Gateway; no selective proxy is installed"
        );
    }
    run_log!(
        "pVisor --safe warning: destination rules cannot distinguish inference from telemetry/upload APIs on the same host; Gateway routes are not an inference-path filter."
    );
    run_log!(
        "pVisor --safe: OverlayFS denies private-key paths and asks or warns on sensitive paths according to the effective rules. Glob rules cover the overlay view; required sandbox restricts access outside that view. Explicit shares, renamed copies and embedded secrets need separate rules. No bulk-read or tool-call attribution monitoring."
    );
    if let Some(overlay) = &config.overlayfs {
        run_log!(
            "pVisor --safe: file deny={:?}, ask={:?}, warn={:?}",
            overlay.access_policy.deny(),
            overlay.access_policy.ask(),
            overlay.access_policy.warn()
        );
    }
    if !config.container.mounts.is_empty()
        || config
            .overlayfs
            .as_ref()
            .is_some_and(|overlay| !overlay.compose.is_empty())
    {
        run_log!(
            "pVisor --safe warning: configured extra file shares are preserved; sensitive files in these paths remain accessible"
        );
    }
    if !args.overlaynet.overlaynet_allow.is_empty()
        || !args.overlaynet.overlaynet_rule.is_empty()
        || args.overlaynet.overlaynet_policy.is_some()
        || !args.run.pass_env.is_empty()
        || !args.container.container_mount.is_empty()
        || !args.overlayfs.mounts.is_empty()
    {
        run_log!(
            "pVisor --safe warning: explicit CLI overrides preset network/filesystem defaults; additional destinations, credentials or files may be exposed"
        );
    }
    if config.vm.rootfs.as_deref() == Some(Path::new("/")) {
        run_log!(
            "pVisor --safe warning: host rootfs exposes host files, including credentials; the preset does not replace your rootfs"
        );
    }
}

fn apply_cli(config: &mut RunConfig, args: RunArgs) -> anyhow::Result<()> {
    apply_cli_unvalidated(config, args)?;
    config.features.validate(config.run.executor)
}

// Safe patches are intermediate layers: validate VM-only features only after
// the explicit executor override has been applied to the complete configuration.
fn apply_cli_unvalidated(config: &mut RunConfig, args: RunArgs) -> anyhow::Result<()> {
    for feature in &args.features {
        config.features.enable(*feature);
    }
    if let Some(path) = args.stage.clone() {
        // A staged command needs a private filesystem view to prevent writes
        // through the original workspace or host /tmp. Explicit CLI policy
        // still takes precedence below.
        config.filesystem = FilesystemMode::Sandbox;
        config
            .overlayfs
            .get_or_insert_with(OverlayFsSettings::default)
            .stage = Some(path);
    }
    let explicit_executor = args.run.executor;
    let explicit_overlaynet_mode = args.overlaynet.overlaynet;
    let rootfs_source = args.vm.rootfs.clone();
    anyhow::ensure!(
        !(args.vm.vm && explicit_executor.is_some_and(|executor| executor != RunExecutorKind::Vm)),
        "--vm cannot be combined with a non-vm --executor"
    );
    let host_rootfs = rootfs_source.as_deref() == Some("host");
    let container_rootfs = explicit_executor == Some(RunExecutorKind::Container)
        || args.container.container_runtime.is_some()
        || args.container.container_image.is_some()
        || args.container.container_rootfs.is_some();
    if host_rootfs {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "--rootfs host is only supported on Linux"
        );
        anyhow::ensure!(
            explicit_executor
                .is_none_or(|executor| container_rootfs || executor == RunExecutorKind::Vm),
            "--rootfs requires --executor vm or container (or no explicit executor)"
        );
    }
    if let Some(ref source) = rootfs_source {
        if let Some(path) = source.strip_prefix("image=") {
            anyhow::ensure!(!path.is_empty(), "--rootfs image=<PATH> requires a path");
            if container_rootfs {
                config.container.image = path.to_string();
                config.container.rootfs = None;
            } else {
                config.vm.image = Some(path.to_string());
                config.vm.rootfs = None;
                config.vm.rootfs_immutable = false;
            }
        } else if source == "host" && container_rootfs {
            config.container.rootfs = Some(PathBuf::from("/"));
            config.container.image.clear();
        } else if source != "host" {
            if container_rootfs {
                config.container.rootfs = Some(PathBuf::from(source));
                config.container.image.clear();
            } else {
                config.vm.rootfs = Some(PathBuf::from(source));
                config.vm.image = None;
                config.vm.rootfs_immutable = false;
            }
        }
    }
    if let Some(value) = args.run.name {
        config.run.agent = value;
    }
    if let Some(value) = explicit_executor {
        config.run.executor = value;
    }
    if let Some(value) = args.run.filesystem {
        config.filesystem = value;
    }
    if args.vm.vm {
        config.run.executor = RunExecutorKind::Vm;
    }
    if let Some(value) = args.run.timeout {
        config.run.timeout_ms = Some(value.0);
    }
    if let Some(value) = args.run.stdio {
        config.run.stdio = value;
    }
    if args.run.strict {
        config.run.policy = RunPolicy::Enforce;
    }
    if args.run.clear_pass_env {
        config.run.pass_env.clear();
    }
    if !args.run.pass_env.is_empty() {
        config.run.pass_env = args.run.pass_env;
    }
    if let Some(value) = args.run.memory {
        config.run.resource_limits.memory_bytes = Some(value.0);
    }
    if let Some(value) = args.run.max_processes {
        config.run.resource_limits.processes = Some(value);
    }
    if let Some(value) = args.run.max_cpu_time {
        config.run.resource_limits.cpu_time_ms = Some(value.0);
    }
    if let Some(value) = args.run.max_open_files {
        config.run.resource_limits.open_files = Some(value);
    }
    if let Some(value) = args.run.max_file_size {
        config.run.resource_limits.file_size_bytes = Some(value.0);
    }
    if !args.command.is_empty() {
        config.run.command = args.command;
    }

    let enables_container = args.container.container_runtime.is_some()
        || args.container.container_image.is_some()
        || args.container.container_rootfs.is_some()
        || args.container.container_pvisor_binary.is_some()
        || args.container.container_platform.is_some()
        || args.container.container_network.is_some()
        || args.container.container_workdir.is_some()
        || args.container.container_user.is_some()
        || args.container.container_read_only_rootfs.is_some()
        || !args.container.container_mount.is_empty();
    if let Some(value) = args.container.container_runtime {
        config.container.runtime = value;
    }
    if let Some(value) = args.container.container_image {
        config.container.image = value;
    }
    if let Some(value) = args.container.container_rootfs {
        config.container.rootfs = Some(value);
    }
    if let Some(value) = args.container.container_pvisor_binary {
        config.container.pvisor_binary = Some(value);
    }
    if let Some(value) = args.container.container_platform {
        config.container.platform = Some(value);
    }
    if let Some(value) = args.container.container_network {
        config.container.network = value;
    }
    if let Some(value) = args.container.container_workdir {
        config.container.workdir = Some(value);
    }
    if let Some(value) = args.container.container_user {
        config.container.user = Some(value);
    }
    if let Some(value) = args.container.container_read_only_rootfs {
        config.container.read_only_rootfs = value;
    }
    if !args.container.container_mount.is_empty() {
        config.container.mounts = args
            .container
            .container_mount
            .into_iter()
            .map(|mount| mount.0)
            .collect();
    }
    if enables_container && explicit_executor.is_none() {
        config.run.executor = RunExecutorKind::Container;
    }

    let enables_vm = config.vm.control_socket.is_some()
        || args.vm.vm_control_socket.is_some()
        || rootfs_source.is_some()
        || args.vm.vm_ram_compression == Some(true)
        || args.vm.vm_cold_ram_compression == Some(true)
        || args.vm.vm_ram_dedup == Some(true)
        || args.vm.vm_memory_pool.is_some()
        || args.vm.vm_node_socket.is_some()
        || args.vm.vm_snapshot_filesystem_pool.is_some()
        || args.vm.vm_ram_backing.is_some()
        || args.vm.vm_image_store.is_some()
        || args.vm.vm_library_dir.is_some();
    if host_rootfs && !container_rootfs {
        config.vm.rootfs = Some(PathBuf::from("/"));
        config.vm.image = None;
        config.vm.rootfs_immutable = false;
    }
    if let Some(value) = args.vm.vm_image_store {
        config.vm.image_store = Some(value);
    }
    if let Some(value) = args.vm.vm_library_dir {
        config.vm.library_dir = Some(value);
    }
    if let Some(value) = args.vm.vm_ram_backing {
        config.vm.ram_backing = Some(value);
    }
    if let Some(value) = args.vm.vm_memory_pool {
        config.vm.memory_pool = Some(value);
    }
    if let Some(value) = args.vm.vm_control_socket {
        config.vm.control_socket = Some(value);
    }
    if let Some(value) = args.vm.vm_node_socket {
        config.vm.node_socket = Some(value);
    }
    if let Some(value) = args.vm.vm_snapshot_filesystem_pool {
        config.vm.snapshot_filesystem_pool = Some(value);
    }
    if let Some(value) = args.vm.vm_ram_compression {
        config.vm.ram_compression = value;
    }
    if let Some(value) = args.vm.vm_cold_ram_compression {
        config.vm.cold_ram_compression = value;
    }
    if let Some(value) = args.vm.vm_ram_dedup {
        config.vm.ram_dedup = value;
    }
    if let Some(value) = args.run.cpu {
        config.vm.cpus = value;
    }
    if let Some(bytes) = args.run.memory {
        let mib = bytes.0.div_ceil(1024 * 1024);
        config.vm.memory_mib = u32::try_from(mib)
            .map_err(|_| anyhow::anyhow!("--memory value is too large for VM memory"))?;
    }
    if enables_vm && explicit_executor.is_none() {
        config.run.executor = RunExecutorKind::Vm;
    }

    let enables_overlayfs = !args.overlayfs.mounts.is_empty()
        || args.overlayfs.durability.is_some()
        || !args.overlayfs.access.is_empty()
        || args.overlayfs.clear_access
        || args.overlayfs.max_size.is_some()
        || args.stage.is_some();
    if enables_overlayfs {
        let overlayfs = config
            .overlayfs
            .get_or_insert_with(OverlayFsSettings::default);
        if args.overlayfs.clear_access {
            overlayfs.access_policy = Default::default();
            overlayfs.access.clear();
        }
        if !args.overlayfs.access.is_empty() {
            overlayfs
                .access
                .extend(args.overlayfs.access.into_iter().map(|access| {
                    pvisor::FilesystemAccessRule {
                        path: access.path,
                        level: match access.level {
                            FilesystemLevel::Deny => FilesystemAccessLevel::Deny,
                            FilesystemLevel::Ask => FilesystemAccessLevel::Ask,
                            FilesystemLevel::Read => FilesystemAccessLevel::Read,
                            FilesystemLevel::Warn => FilesystemAccessLevel::Warn,
                            FilesystemLevel::Stage => FilesystemAccessLevel::Stage,
                            FilesystemLevel::Write => FilesystemAccessLevel::Write,
                        },
                    }
                }));
        }
        if !args.overlayfs.mounts.is_empty() {
            overlayfs.compose.clear();
            overlayfs.mount = args
                .overlayfs
                .mounts
                .into_iter()
                .map(|mount| pvisor::FilesystemMount {
                    source: mount.source,
                    target: Some(mount.target),
                    access: match mount.access {
                        FilesystemLevel::Deny => FilesystemAccessLevel::Deny,
                        FilesystemLevel::Ask => FilesystemAccessLevel::Ask,
                        FilesystemLevel::Read => FilesystemAccessLevel::Read,
                        FilesystemLevel::Warn => FilesystemAccessLevel::Warn,
                        FilesystemLevel::Stage => FilesystemAccessLevel::Stage,
                        FilesystemLevel::Write => FilesystemAccessLevel::Write,
                    },
                })
                .collect();
        }
        if let Some(value) = args.overlayfs.max_size {
            overlayfs.stage_size_bytes = Some(value.0);
        }
        if let Some(value) = args.overlayfs.durability {
            overlayfs.durability = value;
        }
    }

    let enables_overlaynet = !args.overlaynet.overlaynet_allow.is_empty()
        || !args.overlaynet.overlaynet_deny.is_empty()
        || !args.overlaynet.overlaynet_limit.is_empty()
        || !args.overlaynet.overlaynet_rule.is_empty()
        || args.overlaynet.overlaynet_deny_all
        || args.overlaynet.overlaynet_listen.is_some();
    if let Some(value) = explicit_overlaynet_mode {
        config.overlaynet.mode = value;
        if value == OverlayNetMode::Off {
            config.overlaynet.policy = OverlayNetPolicy::Public;
            config.overlaynet.allow.clear();
            config.overlaynet.rules.clear();
            config.overlaynet.deny.clear();
            config.overlaynet.limits.clear();
        }
    }
    if let Some(value) = args.overlaynet.overlaynet_listen {
        config.overlaynet.listen = value;
    }
    if let Some(value) = args.overlaynet.overlaynet_policy {
        config.overlaynet.policy = value;
        if value != OverlayNetPolicy::Allowlist {
            config.overlaynet.allow.clear();
            config.overlaynet.rules.clear();
        }
    }
    if args.overlaynet.overlaynet_deny_all {
        config.overlaynet.policy = OverlayNetPolicy::Deny;
        config.overlaynet.allow.clear();
        config.overlaynet.rules.clear();
        config.overlaynet.deny.clear();
        config.overlaynet.limits.clear();
    }
    if !args.overlaynet.overlaynet_allow.is_empty() {
        config.overlaynet.policy = OverlayNetPolicy::Allowlist;
        config.overlaynet.allow.clear();
        config.overlaynet.rules = args
            .overlaynet
            .overlaynet_allow
            .into_iter()
            .map(|target| target.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_deny.is_empty() {
        config.overlaynet.deny = args
            .overlaynet
            .overlaynet_deny
            .into_iter()
            .map(|target| target.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_limit.is_empty() {
        config.overlaynet.limits = args
            .overlaynet
            .overlaynet_limit
            .into_iter()
            .map(|limit| limit.0)
            .collect();
    }
    if !args.overlaynet.overlaynet_rule.is_empty() {
        config.overlaynet.rules = args
            .overlaynet
            .overlaynet_rule
            .into_iter()
            .map(|rule| rule.0)
            .collect();
    }
    if enables_overlaynet && explicit_overlaynet_mode.is_none() {
        config.overlaynet.mode = if config.run.executor == RunExecutorKind::Vm {
            OverlayNetMode::Auto
        } else {
            OverlayNetMode::Proxy
        };
    }

    if let Some(value) = args.gateway.gateway_mode {
        config.gateway.mode = value;
        if value == GatewayMode::Capture && explicit_overlaynet_mode.is_none() {
            config.overlaynet.mode = if config.run.executor == RunExecutorKind::Vm {
                OverlayNetMode::Auto
            } else {
                OverlayNetMode::Proxy
            };
        }
    }
    if let Some(profile) = args.gateway.gateway_profile {
        config.gateway.profile = Some(profile);
        anyhow::ensure!(
            args.gateway.gateway_mode != Some(GatewayMode::Off),
            "--gateway-profile conflicts with --gateway-mode off"
        );
        config.gateway.mode = GatewayMode::Capture;
        if explicit_overlaynet_mode.is_none() {
            config.overlaynet.mode = if config.run.executor == RunExecutorKind::Vm {
                OverlayNetMode::Auto
            } else {
                OverlayNetMode::Proxy
            };
        }
    }
    if let Some(value) = args.gateway.gateway_admin_listen {
        config.gateway.admin_listen = value;
    }
    if let Some(value) = args.gateway.gateway_level {
        config.gateway.level = value.into();
    }
    if let Some(value) = args.gateway.gateway_session_header {
        config.gateway.session_header = value;
    }
    if let Some(value) = args.gateway.gateway_debug {
        config.gateway.debug = value;
    }

    if !args.gateway.gateway_route.is_empty() {
        config.gateway.routes = args
            .gateway
            .gateway_route
            .into_iter()
            .map(|route| route.0)
            .collect();
    }

    if let Some(value) = args.record.record_destination {
        config.record.destination = Some(value);
    }

    Ok(())
}

fn resolve_default_vm_rootfs(config: &mut RunConfig) -> anyhow::Result<()> {
    if config.run.executor == RunExecutorKind::Vm
        && config.vm.rootfs.is_none()
        && config.vm.image.is_none()
    {
        config.vm.rootfs = Some(PathBuf::from("/"));
        config.vm.rootfs_immutable = false;
    }
    validate_vm_rootfs_platform(config)
}

fn validate_vm_rootfs_platform(config: &RunConfig) -> anyhow::Result<()> {
    if config.run.executor == RunExecutorKind::Vm
        && config.vm.rootfs.as_deref() == Some(Path::new("/"))
    {
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "the host root filesystem can only be used as a VM rootfs on Linux; use --rootfs image=<PATH> or --rootfs <PATH> with a prepared Linux rootfs"
        );
    }
    Ok(())
}

fn validate(config: &RunConfig, safe: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.vm.control_socket.is_none() || config.run.executor == RunExecutorKind::Vm,
        "vm.control_socket requires a VM executor"
    );
    config.vm.validate_ram_dedup()?;
    anyhow::ensure!(
        cfg!(feature = "gateway")
            || (config.gateway.mode == GatewayMode::Off && !config.gateway.debug),
        "Gateway capture/debug requires a build with the gateway feature"
    );
    anyhow::ensure!(
        safe || !config
            .run
            .filesystem
            .iter()
            .any(|grant| grant.access == FilesystemAccess::Read),
        "read-only shares require --safe or --ask"
    );
    if let Some(filesystem) = &config.overlayfs {
        anyhow::ensure!(
            filesystem.compose.is_empty() || filesystem.commit != OverlayFsCommit::Apply,
            "composed filesystem layers cannot be combined with automatic apply"
        );
    }
    if let Some(profile) = config.gateway.profile {
        anyhow::ensure!(
            config.gateway.mode == GatewayMode::Capture,
            "Gateway profile requires capture mode"
        );
        anyhow::ensure!(
            config.gateway.routes.is_empty(),
            "Gateway profile cannot be combined with custom model routes"
        );
        anyhow::ensure!(
            config
                .run
                .command
                .first()
                .and_then(|program| Path::new(program).file_name())
                .and_then(|s| s.to_str())
                == Some("zcode"),
            "Gateway profile requires a direct zcode command"
        );
        if profile == GatewayProfile::ZcodeBigmodel {
            let path = config
                .gateway
                .zcode_builtin_config
                .as_deref()
                .context("zcode-bigmodel requires gateway.zcode_builtin_config")?;
            anyhow::ensure!(
                path.is_absolute() && path.is_file(),
                "gateway.zcode_builtin_config must name an existing absolute file"
            );
            anyhow::ensure!(
                config.run.executor == RunExecutorKind::Host,
                "zcode-bigmodel currently requires the host executor"
            );
        }
    } else {
        anyhow::ensure!(
            config.gateway.zcode_builtin_config.is_none(),
            "gateway.zcode_builtin_config requires the zcode-bigmodel profile"
        );
    }
    if safe {
        anyhow::ensure!(
            config.run.executor != RunExecutorKind::Container,
            "--safe does not yet support the container executor; select host/VM"
        );
        anyhow::ensure!(
            config.overlaynet.mode != OverlayNetMode::Off,
            "--safe requires OverlayNet and cannot be combined with --overlaynet off"
        );
    }
    validate_vm_rootfs_platform(config)?;
    if config.run.command.is_empty() {
        bail!("missing Agent command; pass it after `--` or set run.command");
    }
    let overlay_path = config
        .overlayfs
        .as_ref()
        .and_then(|overlay| overlay.target.as_deref().or(overlay.merged_dir.as_deref()));
    if let Some(path) = overlay_path {
        anyhow::ensure!(
            path.is_absolute(),
            "filesystem workspace path must be an absolute Agent-visible path"
        );
        anyhow::ensure!(
            !path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
            "filesystem workspace path must not contain .."
        );
    }
    anyhow::ensure!(
        config.container.platform.is_none() || config.run.executor == RunExecutorKind::Container,
        "container.platform requires the container executor and cannot be ignored by host/VM execution"
    );
    if config.run.executor == RunExecutorKind::Container {
        if config.overlaynet.mode == OverlayNetMode::Proxy
            && config.container.network != ContainerNetwork::Host
        {
            bail!("the in-process OverlayNet/Gateway requires container.network = \"host\"");
        }
        ContainerExecutor::new(config.container.clone())?;
    }
    if config.run.executor == RunExecutorKind::Vm {
        VmExecutor::new(config.vm.clone())?;
        let rootfs = config
            .vm
            .rootfs
            .as_deref()
            .context("VM execution requires vm.rootfs or --rootfs <PATH>")?;
        if overlay_path.is_none() {
            anyhow::ensure!(
                config
                    .overlayfs
                    .as_ref()
                    .and_then(|overlay| overlay.base.as_deref())
                    == Some(rootfs),
                "VM execution requires vm.rootfs as its OverlayFS base"
            );
        }
        anyhow::ensure!(
            config.overlaynet.mode != OverlayNetMode::Proxy,
            "pvisor-vm uses the smoltcp driver; choose --overlaynet auto or off"
        );
    }
    if config.overlaynet.mode == OverlayNetMode::Off {
        if config.overlaynet.policy != OverlayNetPolicy::Public
            || !config.overlaynet.allow.is_empty()
            || !config.overlaynet.rules.is_empty()
            || !config.overlaynet.deny.is_empty()
            || !config.overlaynet.limits.is_empty()
        {
            bail!("OverlayNet policy options require --overlaynet auto or proxy");
        }
        if config.gateway.mode == GatewayMode::Capture {
            bail!("--gateway-mode capture requires OverlayNet auto or proxy");
        }
    }
    if config.overlaynet.mode == OverlayNetMode::Proxy
        || config.gateway.mode == GatewayMode::Capture
    {
        let listen: std::net::SocketAddr = config.overlaynet.listen.parse().with_context(|| {
            format!(
                "invalid OverlayNet listen address {}",
                config.overlaynet.listen
            )
        })?;
        if listen.port() == 0 {
            bail!("OverlayNet port 0 is not supported; choose an explicit free port");
        }
    }
    if config.overlaynet.policy != OverlayNetPolicy::Allowlist
        && (!config.overlaynet.allow.is_empty() || !config.overlaynet.rules.is_empty())
    {
        bail!("OverlayNet allow entries and rules require --overlaynet-policy allowlist");
    }
    match config.gateway.mode {
        GatewayMode::Off if !config.gateway.routes.is_empty() => {
            bail!("Gateway routes require --gateway-mode capture");
        }
        // Capture without explicit routes uses the Gateway's default route.
        GatewayMode::Capture if config.gateway.routes.is_empty() => {}
        _ => {}
    }
    Ok(())
}

fn resolve_workspace(workspace: &Path) -> anyhow::Result<PathBuf> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolve pVisor workspace {}", workspace.display()))?;
    anyhow::ensure!(
        workspace.is_dir(),
        "pVisor workspace must be a directory: {}",
        workspace.display()
    );
    Ok(workspace)
}

#[cfg_attr(all(target_os = "macos", target_arch = "x86_64"), allow(dead_code))]
fn resolve_vm_layout(config: &RunConfig) -> anyhow::Result<(PathBuf, PathBuf)> {
    let rootfs = config
        .vm
        .rootfs
        .as_deref()
        .context("VM execution requires vm.rootfs or --rootfs <PATH>")?;
    let rootfs = resolve_directory(rootfs, "libkrun rootfs")?;
    let workspace = config
        .overlayfs
        .as_ref()
        .filter(|overlay| overlay.target.is_some())
        .and_then(|overlay| overlay.base.clone())
        .or_else(|| config.run.workspace.clone())
        .unwrap_or(std::env::current_dir()?);
    let workspace = resolve_workspace(&workspace)?;
    Ok((rootfs, workspace))
}

fn resolve_run_storage(storage: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(storage)
        .with_context(|| format!("create pVisor Job storage {}", storage.display()))?;
    storage
        .canonicalize()
        .with_context(|| format!("resolve pVisor Job storage {}", storage.display()))
}

fn select_run_storage(
    config: &RunConfig,
    workspace: &Path,
    run_id: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(stage) = config
        .overlayfs
        .as_ref()
        .and_then(|overlay| overlay.stage.clone())
    {
        return resolve_run_storage(&stage);
    }
    let run_home = default_run_home();
    let run_home = if run_home.is_absolute() {
        run_home
    } else {
        std::env::current_dir()?.join(run_home)
    };
    let preferred = run_home.join(run_id);
    let Some(overlayfs) = &config.overlayfs else {
        return Ok(preferred);
    };
    let mut read_only_layers = Vec::with_capacity(overlayfs.compose.len() + 1);
    for layer in &overlayfs.compose {
        read_only_layers.push(resolve_directory(layer, "OverlayFS compose layer")?);
    }
    read_only_layers.push(resolve_directory(
        overlayfs.base.as_deref().unwrap_or(workspace),
        "OverlayFS base",
    )?);
    if read_only_layers
        .iter()
        .any(|layer| paths_overlap(layer, &preferred))
    {
        Ok(std::env::temp_dir().join("pvisor-runs").join(run_id))
    } else {
        Ok(preferred)
    }
}

fn resolve_directory(path: &Path, description: &str) -> anyhow::Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve {description} {}", path.display()))?;
    anyhow::ensure!(
        path.is_dir(),
        "{description} must be a directory: {}",
        path.display()
    );
    Ok(path)
}

fn resolve_overlay(
    config: &RunConfig,
    workspace: &Path,
    storage: &Path,
) -> anyhow::Result<Option<OverlayHint>> {
    let Some(overlayfs) = &config.overlayfs else {
        return Ok(None);
    };
    let base = resolve_directory(
        overlayfs.base.as_deref().unwrap_or(workspace),
        "OverlayFS base",
    )?;
    let stage = overlayfs
        .stage
        .clone()
        .unwrap_or_else(|| storage.to_path_buf());
    let stage = if stage.exists() {
        stage
            .canonicalize()
            .with_context(|| format!("resolve OverlayFS stage {}", stage.display()))?
    } else {
        std::fs::create_dir_all(&stage)
            .with_context(|| format!("create OverlayFS stage {}", stage.display()))?;
        stage
            .canonicalize()
            .with_context(|| format!("resolve OverlayFS stage {}", stage.display()))?
    };
    anyhow::ensure!(
        base != stage && !base.starts_with(&stage),
        "OverlayFS stage must not contain its base: base={}, stage={}",
        base.display(),
        stage.display()
    );
    let mut compose = Vec::with_capacity(overlayfs.compose.len());
    for layer in overlayfs.compose.iter().rev() {
        let layer = resolve_directory(layer, "OverlayFS compose layer")?;
        anyhow::ensure!(
            layer != stage && !layer.starts_with(&stage),
            "OverlayFS stage must not contain a compose layer: compose={}, stage={}",
            layer.display(),
            stage.display()
        );
        compose.push(layer);
    }
    // The overlay implementation expects highest-priority lowers first. The
    // workspace/base is the implicit bottom layer beneath explicit compose
    // entries.
    compose.push(base);
    let merged_dir = overlayfs.merged_dir.clone();
    Ok(Some(OverlayHint {
        durability: Some(overlayfs.durability),
        execution_snapshot: None,
        access_policy: overlayfs.access_policy.clone(),
        lower_dirs: compose,
        stage_dir: Some(stage.clone()),
        merged_dir,
        auto_apply: overlayfs.commit == OverlayFsCommit::Apply,
        auto_discard: overlayfs.commit == OverlayFsCommit::Drop,
        ..OverlayHint::default()
    }))
}

#[cfg(feature = "gateway")]
fn resolve_proxy(config: &RunConfig) -> anyhow::Result<Option<ProxyConfig>> {
    // VM Auto uses smoltcp directly. A loopback HTTP listener is still needed
    // only when the explicit Gateway capture sink is enabled.
    if config.run.executor == RunExecutorKind::Vm && config.gateway.mode == GatewayMode::Off {
        return Ok(None);
    }
    if config.gateway.mode != GatewayMode::Capture {
        return Ok(None);
    }
    let network = NetworkConfig {
        capability: None,
        mode: match config.overlaynet.policy {
            OverlayNetPolicy::Public => NetworkMode::Public,
            OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
            OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
        },
        allowed_hosts: config.overlaynet.allow.clone(),
        rules: config.overlaynet.rules.clone(),
        deny_rules: config.overlaynet.deny.clone(),
        limits: config.overlaynet.limits.clone(),
    };
    let proxy = ProxyConfig {
        listen: config.overlaynet.listen.clone(),
        admin_listen: config.gateway.admin_listen.clone(),
        agent_id: config.run.agent.clone(),
        session_header: config.gateway.session_header.clone(),
        capture_level: config.gateway.level,
        debug: config.gateway.debug,
        network,
        overlay: OverlayConfig::default(),
        models: if config.gateway.mode == GatewayMode::Capture {
            config
                .gateway
                .profile
                .map(GatewayProfile::routes)
                .unwrap_or_else(|| config.gateway.routes.clone())
        } else {
            Vec::new()
        },
    };
    proxy.validate()?;
    Ok(Some(proxy))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn resolve_filesystem_grants(
    config: &RunConfig,
    workspace: &Path,
    storage: &Path,
) -> anyhow::Result<Vec<FilesystemCapability>> {
    anyhow::ensure!(
        config.run.filesystem.is_empty() || config.run.executor == RunExecutorKind::Host,
        "filesystem grants currently require the host executor"
    );
    let grants: Vec<_> = config
        .run
        .filesystem
        .iter()
        .map(|grant| {
            let path = Path::new(&grant.path);
            anyhow::ensure!(
                path.is_absolute(),
                "filesystem grant must be absolute: {}",
                path.display()
            );
            let path = path
                .canonicalize()
                .with_context(|| format!("resolve filesystem grant {}", path.display()))?;
            anyhow::ensure!(
                !cfg!(target_os = "linux")
                    || grant.access != FilesystemAccess::Read
                    || !path.starts_with("/tmp"),
                "read-only filesystem grants must be outside the private /tmp: {}",
                path.display()
            );
            anyhow::ensure!(
                (config
                    .overlayfs
                    .as_ref()
                    .is_some_and(|filesystem| filesystem.base.as_ref() == Some(&path)))
                    || (!paths_overlap(&path, workspace) && !paths_overlap(&path, storage)),
                "filesystem grant overlaps project or Job storage: {}",
                path.display()
            );
            Ok(FilesystemCapability {
                path: path.display().to_string(),
                access: grant.access,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for read in grants
        .iter()
        .filter(|grant| grant.access == FilesystemAccess::Read)
    {
        for write in grants
            .iter()
            .filter(|grant| grant.access == FilesystemAccess::ReadWrite)
        {
            anyhow::ensure!(
                !paths_overlap(Path::new(&read.path), Path::new(&write.path)),
                "read-only and writable filesystem grants overlap: {} and {}",
                read.path,
                write.path
            );
        }
    }
    Ok(grants)
}

#[cfg(test)]
mod tests {
    use pvisor_vm::api::RuntimeSupport;

    #[test]
    fn companions_reusing_run_args_parse_features_and_preserve_guest_boundary() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct Companion {
            #[command(subcommand)]
            action: Action,
        }
        #[derive(clap::Subcommand)]
        enum Action {
            Run(Box<RunArgs>),
        }
        let parsed = Companion::try_parse_from([
            "pvisor-tui",
            "run",
            "--feature",
            "workload-aware-memory-offloading,workload-aware-memory-offloading",
            "--feature=workload-aware-memory-offloading",
            "--",
            "echo",
            "--feature",
            "guest-only",
        ])
        .unwrap();
        let Action::Run(args) = parsed.action;
        assert_eq!(args.features.len(), 3);
        assert_eq!(args.command, ["echo", "--feature", "guest-only"]);
    }

    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    #[test]
    fn safe_patch_does_not_validate_features_before_explicit_vm_override() {
        for option in ["--safe", "--ask"] {
            let mut config = RunConfig::default();
            config.features.workload_aware_memory_offloading = true;
            let args = preset_args(&[option, "--executor", "vm", "--", "true"]);
            apply_run_options(&mut config, args).unwrap();
            assert!(config.features.workload_aware_memory_offloading);
            assert_eq!(config.run.executor, RunExecutorKind::Vm);
        }
    }

    #[test]
    fn feature_config_cli_precedence_and_executor_validation() {
        use pvisor::features::Feature;
        let mut config: RunConfig = toml::from_str(
            "[run]\nexecutor = 'vm'\n[features]\nworkload-aware-memory-offloading = false",
        )
        .unwrap();
        let mut args = preset_args(&["--", "true"]);
        args.features = vec![Feature::WorkloadAwareMemoryOffloading];
        apply_cli(&mut config, args).unwrap();
        assert!(config.features.workload_aware_memory_offloading);
        apply_cli(&mut config, preset_args(&["--", "true"])).unwrap();
        assert!(config.features.workload_aware_memory_offloading);
        for executor in ["host", "container"] {
            assert!(
                apply_cli(
                    &mut config.clone(),
                    preset_args(&["--executor", executor, "--", "true"])
                )
                .unwrap_err()
                .to_string()
                .contains("requires --executor vm")
            );
        }
        let mut args = preset_args(&["--", "true"]);
        args.features = vec![Feature::WorkloadAwareMemoryOffloading];
        assert!(apply_cli(&mut RunConfig::default(), args).is_err());
    }

    #[test]
    fn stage_durability_defaults_to_checkpoint_and_accepts_strict_override() {
        use pvisor_core::overlay::StageDurability;
        let args = preset_args(&[
            "--stage",
            "/tmp/stage",
            "--stage-durability",
            "strict",
            "--",
            "true",
        ]);
        assert_eq!(args.overlayfs.durability, Some(StageDurability::Strict));
        assert_eq!(
            OverlayFsSettings::default().durability,
            StageDurability::Checkpoint
        );
        assert!("unsupported".parse::<StageDurability>().is_err());
    }
    use super::*;

    #[test]
    fn control_socket_selects_vm_and_preserves_or_overrides_config() {
        let mut config = RunConfig::default();
        apply_cli(
            &mut config,
            preset_args(&["--vm-control-socket", "/private/control.sock", "--", "true"]),
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(
            config.vm.control_socket,
            Some(PathBuf::from("/private/control.sock"))
        );
        apply_cli(&mut config, preset_args(&["--", "true"])).unwrap();
        assert_eq!(
            config.vm.control_socket,
            Some(PathBuf::from("/private/control.sock"))
        );
        apply_cli(
            &mut config,
            preset_args(&["--vm-control-socket", "/other/control.sock", "--", "true"]),
        )
        .unwrap();
        assert_eq!(
            config.vm.control_socket,
            Some(PathBuf::from("/other/control.sock"))
        );
        let mut configured = RunConfig::default();
        configured.vm.control_socket = Some(PathBuf::from("/private/control.sock"));
        apply_cli(&mut configured, preset_args(&["--", "true"])).unwrap();
        assert_eq!(configured.run.executor, RunExecutorKind::Vm);
    }

    #[test]
    fn control_socket_does_not_override_explicit_non_vm_executor() {
        for executor in ["host", "container"] {
            let mut config = RunConfig::default();
            apply_cli(
                &mut config,
                preset_args(&[
                    "--executor",
                    executor,
                    "--vm-control-socket",
                    "/private/control.sock",
                    "--",
                    "true",
                ]),
            )
            .unwrap();
            assert_ne!(config.run.executor, RunExecutorKind::Vm);
            assert!(
                validate(&config, false)
                    .unwrap_err()
                    .to_string()
                    .contains("control_socket")
            );
        }
    }

    #[tokio::test]
    async fn prepared_host_spec_rejects_control_socket_before_launch() {
        let directory = tempfile::tempdir().unwrap();
        let spec_path = directory.path().join("spec.json");
        let spec = RunSpec::process("run-prepared", "test", "true");
        std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
        let args = preset_args(&[
            "--executor",
            "host",
            "--vm-control-socket",
            "/private/control.sock",
            "--spec",
            spec_path.to_str().unwrap(),
            "--result-file",
            directory.path().join("result.json").to_str().unwrap(),
        ]);
        assert!(
            run_prepared_spec(args)
                .await
                .unwrap_err()
                .to_string()
                .contains("control_socket")
        );
    }

    #[test]
    fn cold_ram_compression_is_explicit_selects_vm_and_preserves_config() {
        let mut config = RunConfig::default();
        apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
        assert!(!config.vm.cold_ram_compression);
        apply_run_options(
            &mut config,
            preset_args(&["--vm-cold-ram-compression", "--", "true"]),
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert!(config.vm.cold_ram_compression);
        assert!(!config.vm.ram_compression);
        assert!(config.vm.ram_backing.is_none());
        assert!(config.vm.memory_pool.is_none());
        apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
        assert!(config.vm.cold_ram_compression);
        let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn vm_memory_bool_overrides_parse_and_apply_without_clobbering_omissions() {
        for (index, flag) in [
            "--vm-ram-compression",
            "--vm-cold-ram-compression",
            "--vm-ram-dedup",
        ]
        .into_iter()
        .enumerate()
        {
            for (option, expected) in [
                (None, None),
                (Some(flag.to_owned()), Some(true)),
                (Some(format!("{flag}=true")), Some(true)),
                (Some(format!("{flag}=false")), Some(false)),
            ] {
                // No separator: a bare optional boolean must not consume the command.
                let values = match option.as_deref() {
                    Some(option) => vec![option, "echo", "hello"],
                    None => vec!["echo", "hello"],
                };
                let args = preset_args(&values);
                assert_eq!(
                    [
                        args.vm.vm_ram_compression,
                        args.vm.vm_cold_ram_compression,
                        args.vm.vm_ram_dedup,
                    ][index],
                    expected,
                    "{flag}: {option:?}"
                );
                for initial in [false, true] {
                    let mut config = RunConfig::default();
                    match index {
                        0 => config.vm.ram_compression = initial,
                        1 => config.vm.cold_ram_compression = initial,
                        2 => config.vm.ram_dedup = initial,
                        _ => unreachable!(),
                    }
                    apply_run_options(&mut config, args.clone()).unwrap();
                    assert_eq!(
                        [
                            config.vm.ram_compression,
                            config.vm.cold_ram_compression,
                            config.vm.ram_dedup,
                        ][index],
                        expected.unwrap_or(initial),
                        "{flag}: {option:?}, initial={initial}"
                    );
                    assert_eq!(config.run.command, ["echo", "hello"]);
                    assert_eq!(
                        config.run.executor,
                        if expected == Some(true) {
                            RunExecutorKind::Vm
                        } else {
                            RunExecutorKind::Host
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn vm_memory_path_overrides_select_vm_and_preserve_omitted_config() {
        for flag in ["--vm-node-socket", "--vm-snapshot-filesystem-pool"] {
            let args = preset_args(&[flag, "relative/path with spaces", "--", "true"]);
            let mut config: RunConfig = toml::from_str(
                "[vm]\nnode_socket = 'configured/node.sock'\nsnapshot_filesystem_pool = 'configured/snapshots'\n",
            )
            .unwrap();
            apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
            assert_eq!(
                config.vm.node_socket.as_deref(),
                Some(Path::new("configured/node.sock"))
            );
            assert_eq!(
                config.vm.snapshot_filesystem_pool.as_deref(),
                Some(Path::new("configured/snapshots"))
            );
            if flag == "--vm-node-socket" {
                assert_eq!(
                    args.vm.vm_node_socket.as_deref(),
                    Some(Path::new("relative/path with spaces"))
                );
                assert!(args.vm.vm_snapshot_filesystem_pool.is_none());
            } else {
                assert_eq!(
                    args.vm.vm_snapshot_filesystem_pool.as_deref(),
                    Some(Path::new("relative/path with spaces"))
                );
                assert!(args.vm.vm_node_socket.is_none());
            }
            apply_run_options(&mut config, args).unwrap();
            assert_eq!(config.run.executor, RunExecutorKind::Vm);
            assert_eq!(
                config.vm.node_socket.as_deref(),
                Some(Path::new(if flag == "--vm-node-socket" {
                    "relative/path with spaces"
                } else {
                    "configured/node.sock"
                }))
            );
            assert_eq!(
                config.vm.snapshot_filesystem_pool.as_deref(),
                Some(Path::new(if flag == "--vm-snapshot-filesystem-pool" {
                    "relative/path with spaces"
                } else {
                    "configured/snapshots"
                }))
            );
            let settings = config.vm.clone();
            apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
            assert_eq!(config.vm, settings);
            let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
            assert_eq!(decoded.vm, config.vm);
        }
    }

    #[test]
    fn memory_pool_is_explicit_and_selects_vm_without_fuse() {
        let args = preset_args(&["--vm-memory-pool", "/private/tmp/pool/socket", "--", "bash"]);
        let mut config = RunConfig::default();
        assert!(config.vm.memory_pool.is_none());
        apply_run_options(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(
            config.vm.memory_pool.as_deref(),
            Some(Path::new("/private/tmp/pool/socket"))
        );
        assert!(!config.vm.ram_compression);
        let encoded = toml::to_string(&config).unwrap();
        let decoded: RunConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.vm.memory_pool, config.vm.memory_pool);
    }

    #[test]
    fn ram_dedup_is_explicit_selects_vm_and_preserves_config() {
        let mut config = RunConfig::default();
        apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
        assert!(!config.vm.ram_dedup);
        apply_run_options(&mut config, preset_args(&["--vm-ram-dedup", "--", "true"])).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert!(config.vm.ram_dedup);
        apply_run_options(&mut config, preset_args(&["--", "true"])).unwrap();
        assert!(config.vm.ram_dedup);
        let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn ram_dedup_conflicts_are_rejected_before_execution() {
        for option in ["--vm-ram-compression", "--vm-memory-pool"] {
            let values = if option == "--vm-memory-pool" {
                vec![
                    "--vm-ram-dedup",
                    option,
                    "/private/pool/socket",
                    "--",
                    "true",
                ]
            } else {
                vec!["--vm-ram-dedup", option, "--", "true"]
            };
            let mut config = RunConfig::default();
            apply_run_options(&mut config, preset_args(&values)).unwrap();
            assert!(
                validate(&config, false)
                    .unwrap_err()
                    .to_string()
                    .contains("vm.ram_dedup")
            );
        }
    }

    #[test]
    fn spec_format_is_detected_from_content() {
        let temp = tempfile::tempdir().unwrap();
        let json = temp.path().join("config.toml");
        std::fs::write(&json, b"  {\"run_id\": \"x\" }").unwrap();
        assert!(spec_is_json(&json).unwrap());
        std::fs::write(&json, b"[run]\nagent = \"x\"\n").unwrap();
        assert!(!spec_is_json(&json).unwrap());
    }
    use clap::{CommandFactory, Parser};
    use proptest::prelude::*;

    use crate::cli::Cli;
    use pvisor::FilesystemMount;

    fn preset_args(values: &[&str]) -> RunArgs {
        let crate::cli::Command::Run(args) =
            Cli::try_parse_from(["pvisor", "run"].into_iter().chain(values.iter().copied()))
                .unwrap()
                .command
        else {
            unreachable!("expected run command")
        };
        *args
    }

    #[test]
    fn safe_and_ask_runs_use_persistent_job_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let storage = temporary.path().join("job");
        std::fs::create_dir(&workspace).unwrap();
        for flag in ["--safe", "--ask"] {
            let mut config = RunConfig::default();
            apply_run_options(&mut config, preset_args(&[flag, "--", "bash"])).unwrap();
            normalize_filesystem_config(&mut config).unwrap();
            let overlay = resolve_overlay(&config, &workspace, &storage)
                .unwrap()
                .unwrap();
            assert_eq!(overlay.stage_dir, Some(storage.canonicalize().unwrap()));
            assert!(!overlay.auto_discard);
            assert!(
                config
                    .overlayfs
                    .unwrap()
                    .access_policy
                    .denied(Path::new(".ssh/key"))
            );
        }
    }

    #[test]
    fn ask_enables_permission_prompts_not_an_agent_command() {
        let args = preset_args(&["--tui", "--ask", "--", "bash"]);
        assert!(args.audit_requested().unwrap());
        assert!(args.audit);
        assert_eq!(args.command, ["bash"]);
    }

    #[test]
    fn replacing_file_defaults_requires_an_explicit_clear() {
        let mut config = RunConfig::default();
        apply_run_options(
            &mut config,
            preset_args(&[
                "--ask",
                "--clear-access",
                "--access",
                "custom:ask",
                "--",
                "bash",
            ]),
        )
        .unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        let policy = &config.overlayfs.unwrap().access_policy;
        assert_eq!(policy.ask(), ["custom"]);
        assert!(policy.deny().is_empty());
        assert!("secret:read".parse::<FilesystemAccessArg>().is_err());
        assert!("secret:warn".parse::<FilesystemAccessArg>().is_ok());
    }

    #[test]
    fn read_shares_are_read_only_capabilities_not_overlay_layers() {
        let mut config = RunConfig::default();
        apply_run_options(
            &mut config,
            preset_args(&["--safe", "--mount", "/reference:read", "--", "bash"]),
        )
        .unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        assert!(config.overlayfs.as_ref().unwrap().compose.is_empty());
        assert_eq!(config.run.filesystem[0].access, FilesystemAccess::Read);
        assert_eq!(config.run.filesystem[0].path, "/reference");
        assert!(validate(&config, false).is_err());
        assert!(validate(&config, true).is_ok());
    }

    #[test]
    fn ask_rules_select_audit_without_an_extra_flag_and_keep_configured_rules() {
        let args = preset_args(&["--access", ".env:ask", "--", "bash"]);
        assert!(args.audit_requested().unwrap());
        assert!(args.tui_requested());

        let directory = tempfile::tempdir().unwrap();
        let spec = directory.path().join("run.toml");
        std::fs::write(
            &spec,
            "[[overlayfs.access]]\npath = 'secrets/*.pem'\nlevel = 'ask'\n",
        )
        .unwrap();
        let args = preset_args(&["--config", spec.to_str().unwrap(), "--", "bash"]);
        assert!(args.audit_requested().unwrap());
        let mut config = load_run_config(&args, None, false).unwrap();
        let mut effective = args;
        effective.audit = true;
        apply_run_options(&mut config, effective).unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        assert!(
            config
                .overlayfs
                .unwrap()
                .access_policy
                .ask()
                .contains(&"secrets/*.pem".into())
        );
    }

    #[test]
    fn safe_preset_requests_the_sandboxed_filesystem_view() {
        let mut config = RunConfig::default();
        apply_run_options(&mut config, preset_args(&["--safe", "--", "codex"])).unwrap();
        assert_eq!(config.filesystem, FilesystemMode::Sandbox);

        // An explicit --filesystem value still wins over the preset.
        let mut config = RunConfig::default();
        apply_run_options(
            &mut config,
            preset_args(&["--safe", "--filesystem", "host", "--", "codex"]),
        )
        .unwrap();
        assert_eq!(config.filesystem, FilesystemMode::Host);
    }

    #[test]
    fn safe_preset_restores_agent_api_grants_and_accepts_explicit_grants() {
        for (program, hosts) in [
            (
                "codex",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            (
                "/opt/bin/codex",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            (
                "bash",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            (
                "sh",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            (
                "zsh",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            (
                "fish",
                vec!["api.openai.com", "chatgpt.com", "ab.chatgpt.com"],
            ),
            ("claude", vec!["api.anthropic.com"]),
            ("gemini", vec!["generativelanguage.googleapis.com"]),
            ("zcode", vec!["api.z.ai", "open.bigmodel.cn"]),
            ("unknown-agent", vec![]),
        ] {
            let mut config = RunConfig::default();
            apply_run_options(&mut config, preset_args(&["--safe", "--", program])).unwrap();
            assert_eq!(
                config.overlaynet.policy,
                if hosts.is_empty() {
                    OverlayNetPolicy::Deny
                } else {
                    OverlayNetPolicy::Allowlist
                }
            );
            assert_eq!(
                config
                    .overlaynet
                    .rules
                    .iter()
                    .map(|rule| rule.host.as_str())
                    .collect::<Vec<_>>(),
                hosts
            );
            assert!(
                config
                    .overlaynet
                    .rules
                    .iter()
                    .all(|rule| rule.ports == [443])
            );
            assert_eq!(config.filesystem, FilesystemMode::Sandbox);
            apply_run_options(
                &mut config,
                preset_args(&[
                    "--safe",
                    "--overlaynet-allow",
                    "api.example.com:443",
                    "--",
                    program,
                ]),
            )
            .unwrap();
            assert!(
                config
                    .overlaynet
                    .rules
                    .iter()
                    .any(|rule| rule.host == "api.example.com" && rule.ports == [443])
            );
        }
    }

    #[test]
    fn safe_preset_precedence_preserves_unrelated_config_and_explicit_overrides() {
        let source = r#"
[run]
command = ["claude"]
executor = "vm"
timeout_ms = 1234
pass_env = ["CONFIG_SECRET"]
[overlaynet]
mode = "off"
policy = "public"
[overlayfs]
[[overlayfs.mount]]
source = "/configured/share"
access = "read"
"#;
        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(&mut config, preset_args(&["--safe"])).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.run.timeout_ms, Some(1234));
        assert!(config.run.pass_env.is_empty());
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Auto);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Allowlist);
        assert_eq!(config.overlaynet.rules[0].host, "api.anthropic.com");
        assert_eq!(
            config.overlayfs.as_ref().unwrap().mount[0].source,
            PathBuf::from("/configured/share")
        );
        assert_eq!(
            config.overlayfs.as_ref().unwrap().commit,
            OverlayFsCommit::Manual
        );

        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(
            &mut config,
            preset_args(&[
                "--safe",
                "--executor",
                "host",
                "--overlaynet-allow",
                "inference.example:8443",
                "--pass-env",
                "CLI_TOKEN",
                "--mount",
                "/explicit/share:read",
                "--",
                "codex",
            ]),
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Host);
        assert_eq!(config.run.timeout_ms, Some(1234));
        assert_eq!(config.run.pass_env, ["CLI_TOKEN"]);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].host, "inference.example");
        assert_eq!(config.overlaynet.rules[0].ports, [8443]);
        assert_eq!(
            config.overlayfs.as_ref().unwrap().mount[0].source,
            PathBuf::from("/explicit/share")
        );

        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(&mut config, preset_args(&[])).unwrap();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Off);
        assert_eq!(config.run.pass_env, ["CONFIG_SECRET"]);
        assert_eq!(
            config.overlayfs.as_ref().unwrap().mount[0].source,
            PathBuf::from("/configured/share")
        );
    }

    #[test]
    fn file_access_rules_follow_cli_safe_config_priority() {
        let source = r#"
[[overlayfs.access]]
path = "configured-secret"
level = "deny"
[[overlayfs.access]]
path = "configured-warning"
level = "warn"
"#;
        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(&mut config, preset_args(&["--safe", "--", "codex"])).unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        let policy = &config.overlayfs.as_ref().unwrap().access_policy;
        assert!(policy.deny().contains(&"**/.ssh".into()));
        assert!(policy.deny().contains(&"configured-secret".into()));

        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(
            &mut config,
            preset_args(&[
                "--safe",
                "--access",
                "custom/*.pem:warn",
                "--access",
                "private/**:deny",
                "--",
                "codex",
            ]),
        )
        .unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        let policy = &config.overlayfs.as_ref().unwrap().access_policy;
        assert!(policy.deny().contains(&"private/**".into()));
        assert!(policy.denied(Path::new(".ssh/key")));
        assert!(policy.warn().contains(&"custom/*.pem".into()));
        let overlay = resolve_overlay(
            &config,
            Path::new("."),
            &tempfile::tempdir().unwrap().path().join("stage"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(&overlay.access_policy, policy);

        let mut config: RunConfig = toml::from_str(source).unwrap();
        apply_run_options(
            &mut config,
            preset_args(&["--access", "../outside:deny", "--", "codex"]),
        )
        .unwrap();
        assert!(normalize_filesystem_config(&mut config).is_err());

        let mut config = RunConfig::default();
        apply_run_options(
            &mut config,
            preset_args(&["--ask", "--access", "secrets/*.pem:ask", "--", "codex"]),
        )
        .unwrap();
        normalize_filesystem_config(&mut config).unwrap();
        let policy = &config.overlayfs.as_ref().unwrap().access_policy;
        assert!(policy.ask().contains(&"secrets/*.pem".into()));
        assert!(policy.ask().contains(&"**/.env".into()));
        assert!(policy.denied(Path::new(".ssh/key")));
        assert!(policy.warn().is_empty());
    }

    #[test]
    fn safe_preset_respects_explicit_network_disable_and_gateway_routes() {
        for (options, expected) in [
            (vec!["--overlaynet", "off"], OverlayNetPolicy::Public),
            (
                vec!["--overlaynet-policy", "public"],
                OverlayNetPolicy::Public,
            ),
            (vec!["--overlaynet-deny-all"], OverlayNetPolicy::Deny),
        ] {
            let mut args = vec!["--safe"];
            args.extend(options);
            args.extend(["--", "codex"]);
            let mut config = RunConfig::default();
            apply_run_options(&mut config, preset_args(&args)).unwrap();
            assert_eq!(config.overlaynet.policy, expected);
            assert!(config.overlaynet.rules.is_empty());
            if config.overlaynet.mode == OverlayNetMode::Off {
                assert!(validate(&config, true).is_err());
            }
        }
        let mut config = RunConfig::default();
        apply_run_options(
            &mut config,
            preset_args(&[
                "--safe",
                "--gateway-mode",
                "capture",
                "--gateway-route",
                r#"name="*", upstream="https://private.example/v1", api_key_env="PRIVATE_KEY""#,
                "--",
                "zcode",
            ]),
        )
        .unwrap();
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);
        #[cfg(feature = "gateway")]
        {
            let proxy = resolve_proxy(&config).unwrap().unwrap();
            assert_eq!(
                proxy.models[0].upstream.as_deref(),
                Some("https://private.example/v1")
            );
            assert_eq!(proxy.network.mode, NetworkMode::NoNetwork);
        }
    }

    #[test]
    fn safe_requires_isolation_without_a_separate_sandbox_option() {
        let args = preset_args(&["--safe", "--", "/bin/true"]);
        assert!(args.run.safe);
        let mut config = RunConfig::default();
        apply_run_options(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Host);
        validate(&config, true).unwrap();
        config.overlaynet.mode = OverlayNetMode::Off;
        assert!(
            validate(&config, true)
                .unwrap_err()
                .to_string()
                .contains("--safe requires OverlayNet")
        );
        apply_run_options(
            &mut config,
            preset_args(&["--safe", "--vm", "--", "claude"]),
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Auto);
        use clap::CommandFactory;
        assert!(
            !Cli::command()
                .find_subcommand("run")
                .unwrap()
                .get_arguments()
                .any(|arg| arg.get_long() == Some("sandbox"))
        );
        assert!(
            toml::from_str::<RunConfig>(
                r#"[run]
sandbox = "required""#
            )
            .is_err()
        );
    }

    #[test]
    fn filesystem_policy_is_independent_from_overlaynet() {
        let args = preset_args(&[
            "--overlaynet-deny-all",
            "--filesystem",
            "sandbox",
            "--",
            "true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.filesystem, FilesystemMode::Sandbox);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);

        let args = preset_args(&["--overlaynet-deny-all", "--", "true"]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.filesystem, FilesystemMode::Host);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);
    }

    #[test]
    fn stage_requests_a_private_filesystem_unless_explicitly_overridden() {
        for network in [None, Some("--overlaynet-deny-all")] {
            for filesystem in [None, Some("host")] {
                let mut args = vec!["--stage", "/tmp/stage"];
                if let Some(network) = network {
                    args.push(network);
                }
                if let Some(filesystem) = filesystem {
                    args.extend(["--filesystem", filesystem]);
                }
                args.extend(["--", "true"]);
                let mut config = RunConfig::default();
                apply_run_options(&mut config, preset_args(&args)).unwrap();
                assert_eq!(
                    config.filesystem,
                    if filesystem.is_some() {
                        FilesystemMode::Host
                    } else {
                        FilesystemMode::Sandbox
                    }
                );
            }
        }
    }

    #[test]
    fn fork_inherits_or_reidentifies_the_agent_with_its_command() {
        use super::lifecycle::fork_command;
        let source = vec!["/bin/sh".into(), "-c".into(), "work".into()];
        assert_eq!(
            fork_command("sh", &source, Vec::new()),
            ("sh".into(), source)
        );
        assert_eq!(
            fork_command("sh", &[], vec!["/usr/local/bin/codex".into()]),
            ("codex".into(), vec!["/usr/local/bin/codex".into()])
        );
    }

    #[test]
    fn cli_record_destination_is_the_only_persistence_selection() {
        let args = preset_args(&["--record-destination", "/tmp/events", "--", "codex"]);
        assert_eq!(
            args.record.record_destination.as_deref(),
            Some(std::path::Path::new("/tmp/events"))
        );
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help.contains("--record-format"),
            "removed --record-format must not appear in help"
        );
        assert!(
            toml::from_str::<RunConfig>("[record]\nformat = \"json\"\ndestination = \"/tmp/e\"\n")
                .is_err(),
            "record.format must be rejected by deny_unknown_fields"
        );
    }

    #[test]
    fn cli_selects_and_configures_container_executor() {
        let args = preset_args(&[
            "--container-runtime",
            "podman",
            "--container-image",
            "example/agent:latest",
            "--container-pvisor-binary",
            "/opt/artifacts/pvisor-linux-amd64",
            "--container-network",
            "none",
            "--container-read-only-rootfs",
            "--container-mount",
            r#"source="/tmp", target="/workspace", read_only=true"#,
            "--",
            "agent",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Container);
        assert_eq!(config.container.runtime, Path::new("podman"));
        assert_eq!(config.container.image, "example/agent:latest");
        assert_eq!(
            config.container.pvisor_binary.as_deref(),
            Some(Path::new("/opt/artifacts/pvisor-linux-amd64"))
        );
        assert_eq!(config.container.platform, None);
        assert_eq!(config.container.network, ContainerNetwork::None);
        assert!(config.container.read_only_rootfs);
        assert_eq!(config.container.mounts.len(), 1);
        assert!(config.container.mounts[0].read_only);
        validate(&config, false).unwrap();
    }

    #[test]
    fn cli_and_config_platform_selection_require_native_architecture() {
        for (cli_platform, config_platform, architecture) in [
            ("linux/amd64", "linux-amd64", "x86_64"),
            ("linux/arm64", "linux-arm64", "aarch64"),
        ] {
            let args = preset_args(&[
                "--container-image",
                "example/agent:latest",
                "--container-platform",
                cli_platform,
                "--",
                "agent",
            ]);
            let mut cli_config = RunConfig::default();
            apply_cli(&mut cli_config, args).unwrap();
            assert_eq!(cli_config.run.executor, RunExecutorKind::Container);
            let file_config: RunConfig = toml::from_str(&format!(
                "[run]\nexecutor = 'container'\ncommand = ['agent']\n[container]\nimage = 'example/agent:latest'\nplatform = '{config_platform}'\n"
            ))
            .unwrap();
            for config in [cli_config, file_config] {
                let result = validate(&config, false);
                if std::env::consts::ARCH == architecture {
                    result.unwrap();
                } else {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("cross-platform selection is not supported")
                    );
                }
            }
        }
    }

    #[test]
    fn container_platform_cannot_be_ignored_by_a_host_or_vm_config() {
        for executor in ["host", "vm"] {
            let config: RunConfig = toml::from_str(&format!(
                "[run]\nexecutor = '{executor}'\ncommand = ['agent']\n[container]\nplatform = 'linux-amd64'\n"
            ))
            .unwrap();
            assert!(
                validate(&config, false)
                    .unwrap_err()
                    .to_string()
                    .contains("container.platform requires the container executor")
            );
        }
    }

    #[test]
    fn executor_help_names_the_rust_vm_runtime_and_platform_backend() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert!(EXECUTOR_HELP.contains("pvisor-vm"));
            assert!(!EXECUTOR_HELP.contains("libkrun"));
            assert!(EXECUTOR_HELP.contains(if cfg!(target_os = "linux") {
                "KVM on Linux"
            } else {
                "HVF on macOS"
            }));
        }
        let help = Cli::try_parse_from(["pvisor", "run", "--help"])
            .unwrap_err()
            .to_string();
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(help.contains("cross-platform selection is unsupported"));
    }

    #[test]
    fn proxy_requires_host_network_for_container_executor() {
        let mut config = RunConfig::default();
        config.run.command = vec!["agent".into()];
        config.run.executor = RunExecutorKind::Container;
        config.container.image = "example/agent:latest".into();
        config.container.network = ContainerNetwork::Bridge;
        config.run.workspace = Some("/tmp/run".into());
        config.overlaynet.mode = OverlayNetMode::Proxy;
        let error = validate(&config, false).unwrap_err();
        assert!(error.to_string().contains("container.network = \"host\""));
    }

    #[test]
    fn cli_selects_and_configures_vm_executor() {
        let temporary = tempfile::tempdir().unwrap();
        let libraries = temporary.path().join("lib");
        std::fs::create_dir(&libraries).unwrap();
        let args = preset_args(&[
            "--rootfs",
            temporary.path().to_str().unwrap(),
            "--vm-library-dir",
            libraries.to_str().unwrap(),
            "--memory",
            "4294967296",
            "--cpu",
            "4",
            "--",
            "agent",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.rootfs.as_deref(), Some(temporary.path()));
        assert_eq!(config.vm.library_dir.as_deref(), Some(libraries.as_path()));
        assert_eq!(config.vm.memory_mib, 4096);
        assert_eq!(config.vm.cpus, 4);
    }

    #[test]
    fn ram_backing_flag_selects_vm_and_overrides_config() {
        let mut config = RunConfig::default();
        config.vm.ram_backing = Some("old.ram".into());
        apply_cli(
            &mut config,
            preset_args(&[
                "--vm-ram-backing",
                "new ram.file",
                "--vm-ram-compression",
                "--",
                "agent",
            ]),
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert!(config.vm.ram_compression);
        assert_eq!(
            config.vm.ram_backing.as_deref(),
            Some(Path::new("new ram.file"))
        );
    }

    #[test]
    fn vm_defaults_to_host_root_without_an_image() {
        let mut config = RunConfig::default();
        config.run.executor = RunExecutorKind::Vm;
        assert!(config.vm.image.is_none());
        let result = resolve_default_vm_rootfs(&mut config);
        assert_eq!(result.is_ok(), cfg!(target_os = "linux"));
        assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/")));
        assert!(config.vm.image.is_none());
    }

    #[test]
    fn vm_preserves_explicit_root_sources() {
        let mut config = RunConfig::default();
        config.run.executor = RunExecutorKind::Vm;
        config.vm.image = Some("ubuntu:latest".into());
        resolve_default_vm_rootfs(&mut config).unwrap();
        assert!(config.vm.rootfs.is_none());
        assert_eq!(config.vm.image.as_deref(), Some("ubuntu:latest"));
        config.vm.rootfs = Some(PathBuf::from("/prepared-root"));
        resolve_default_vm_rootfs(&mut config).unwrap();
        assert_eq!(
            config.vm.rootfs.as_deref(),
            Some(Path::new("/prepared-root"))
        );
    }

    #[cfg(not(feature = "gateway"))]
    #[test]
    fn unavailable_gateway_is_rejected_before_execution() {
        let mut config = RunConfig::default();
        config.gateway.mode = GatewayMode::Capture;
        assert!(
            validate(&config, false)
                .unwrap_err()
                .to_string()
                .contains("gateway feature")
        );
        config.gateway.mode = GatewayMode::Off;
        config.gateway.debug = true;
        assert!(
            validate(&config, false)
                .unwrap_err()
                .to_string()
                .contains("gateway feature")
        );
    }

    #[test]
    fn host_rootfs_obeys_the_linux_vm_boundary() {
        let args = preset_args(&["--rootfs", "host", "--", "/bin/true"]);
        let mut config = RunConfig::default();
        let result = apply_cli(&mut config, args);

        #[cfg(target_os = "linux")]
        {
            result.unwrap();
            assert_eq!(config.run.executor, RunExecutorKind::Vm);
            assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/")));
            assert!(config.vm.image.is_none());
            assert!(!config.vm.rootfs_immutable);
        }
        #[cfg(not(target_os = "linux"))]
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("only supported on Linux")
        );
    }

    #[test]
    fn host_rootfs_conflicts_with_other_vm_rootfs_sources() {
        for rootfs_value in ["image=/tmp/rootfs-image", "/tmp/rootfs"] {
            let error = Cli::try_parse_from([
                "pvisor",
                "run",
                "--rootfs",
                "host",
                "--rootfs",
                rootfs_value,
                "--",
                "/bin/true",
            ])
            .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_rootfs_rejects_an_explicit_non_vm_executor() {
        let args = preset_args(&["--executor", "host", "--rootfs", "host", "--", "/bin/true"]);
        let error = apply_cli(&mut RunConfig::default(), args).unwrap_err();
        assert!(error.to_string().contains("requires --executor vm"));
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn vm_rejects_the_host_only_explicit_proxy_mode() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = RunConfig::default();
        config.run.command = vec!["agent".into()];
        config.run.executor = RunExecutorKind::Vm;
        config.vm.rootfs = Some(temporary.path().to_path_buf());
        // Static builds already own their kernel and reject a dynamic firmware
        // directory. Set up valid VM inputs before checking the proxy contract.
        #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
        {
            config.vm.library_dir = Some(temporary.path().to_path_buf());
            std::fs::write(
                temporary
                    .path()
                    .join(pvisor_vm::api::VmPlatform::firmware_name()),
                [],
            )
            .unwrap();
        }
        config.overlayfs = Some(OverlayFsSettings {
            base: Some(temporary.path().to_path_buf()),
            ..OverlayFsSettings::default()
        });
        config.overlaynet.mode = OverlayNetMode::Proxy;
        let error = validate(&config, false).unwrap_err();
        assert!(error.to_string().contains("smoltcp driver"));
    }

    #[test]
    fn explicit_off_is_not_overridden_by_vm_policy_flags() {
        let args = preset_args(&[
            "--executor",
            "vm",
            "--overlaynet",
            "off",
            "--overlaynet-deny-all",
            "--",
            "true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Off);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);
    }

    #[test]
    fn vm_resolves_a_guest_overlay_separate_from_the_rootfs() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        let project = temporary.path().join("project");
        std::fs::create_dir(&rootfs).unwrap();
        std::fs::create_dir(&project).unwrap();
        let mut config = RunConfig::default();
        config.vm.rootfs = Some(rootfs.clone());
        config.overlayfs = Some(OverlayFsSettings {
            base: Some(project.clone()),
            target: Some("/work/project".into()),
            ..OverlayFsSettings::default()
        });
        let (resolved_rootfs, resolved_workspace) = resolve_vm_layout(&config).unwrap();
        assert_eq!(resolved_rootfs, rootfs.canonicalize().unwrap());
        assert_eq!(resolved_workspace, project.canonicalize().unwrap());
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn overlayfs_path_is_valid_for_vm_executor() {
        let mut config = RunConfig::default();
        config.run.command = vec!["true".into()];
        config.overlayfs = Some(OverlayFsSettings {
            base: Some("/tmp/project".into()),
            target: Some("/workspace".into()),
            ..OverlayFsSettings::default()
        });
        let rootfs = tempfile::tempdir().unwrap();
        config.vm.rootfs = Some(rootfs.path().to_owned());
        if pvisor_vm::api::VmPlatform::embedded_kernel().is_none() {
            std::fs::write(
                rootfs
                    .path()
                    .join(pvisor_vm::api::VmPlatform::firmware_name()),
                b"local firmware fixture",
            )
            .unwrap();
            config.vm.library_dir = Some(rootfs.path().to_owned());
        }
        config.run.executor = RunExecutorKind::Vm;
        assert!(validate(&config, false).is_ok());
    }

    #[test]
    fn cli_exposes_overlay_path_and_ordered_compose_layers() {
        let args = preset_args(&[
            "--rootfs",
            "image=ubuntu:latest",
            "--mount",
            "/tmp/project:read",
            "--stage",
            "/tmp/stage",
            "--",
            "/bin/true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        let overlay = config.overlayfs.unwrap();
        assert_eq!(overlay.mount[0].source, PathBuf::from("/tmp/project"));
        assert_eq!(overlay.mount[0].target, Some(PathBuf::from("/tmp/project")));
        assert_eq!(overlay.stage.as_deref(), Some(Path::new("/tmp/stage")));
    }

    #[test]
    fn image_selects_the_daemonless_vm_executor() {
        let args = preset_args(&[
            "--rootfs",
            "image=ubuntu:24.04",
            "--image-store",
            "/tmp/pvisor-images",
            "--",
            "/bin/true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.image.as_deref(), Some("ubuntu:24.04"));
        assert_eq!(
            config.vm.image_store.as_deref(),
            Some(Path::new("/tmp/pvisor-images"))
        );
        assert!(config.vm.rootfs.is_none());
    }

    #[test]
    fn cli_lists_replace_config_lists() {
        let mut config = RunConfig::default();
        config.overlaynet.allow = vec!["old.example".into()];
        let args = preset_args(&["--overlaynet-allow", "new.example", "--", "true"]);
        apply_cli(&mut config, args).unwrap();
        assert!(config.overlaynet.allow.is_empty());
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].host, "new.example");
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Allowlist);
    }

    #[test]
    fn safe_defaults_disable_inheritance_and_cli_maps_resource_limits() {
        let args = preset_args(&[
            "--pass-env",
            "EXPLICIT_TOKEN",
            "--memory",
            "1048576",
            "--max-processes",
            "8",
            "--max-open-files",
            "32",
            "--overlayfs-max-size",
            "2MiB",
            "--",
            "true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        apply_safe_defaults(&mut config).unwrap();
        assert!(!config.run.inherit_env);
        assert_eq!(config.run.pass_env, ["EXPLICIT_TOKEN"]);
        assert_eq!(config.run.resource_limits.memory_bytes, Some(1_048_576));
        assert_eq!(config.run.resource_limits.processes, Some(8));
        assert_eq!(config.run.resource_limits.open_files, Some(32));
        assert_eq!(
            config
                .overlayfs
                .as_ref()
                .and_then(|overlay| overlay.stage_size_bytes),
            Some(2 * 1024 * 1024)
        );
    }

    #[test]
    fn safe_environment_uses_the_command_identity_not_the_job_name() {
        let mut codex = RunConfig::default();
        apply_run_options(
            &mut codex,
            preset_args(&["--safe", "--name", "other", "--", "/usr/bin/codex"]),
        )
        .unwrap();
        assert!(codex.run.inherit_env);

        let mut shell = RunConfig::default();
        apply_run_options(
            &mut shell,
            preset_args(&[
                "--safe",
                "--name",
                "codex",
                "--pass-env",
                "AUTH_TOKEN",
                "--",
                "bash",
            ]),
        )
        .unwrap();
        assert!(!shell.run.inherit_env);
        assert_eq!(shell.run.pass_env, ["AUTH_TOKEN"]);
    }

    #[test]
    fn simple_network_flags_repeat_and_infer_policy() {
        let args = preset_args(&[
            "--overlaynet-allow",
            "api.example.com:443",
            "--overlaynet-allow",
            "packages.example.com",
            "--overlaynet-deny",
            "169.254.0.0/16",
            "--overlaynet-deny",
            "bad.example.com:80",
            "--overlaynet-limit",
            "10mbps",
            "--overlaynet-limit",
            "api.example.com:443=2mbps",
            "--",
            "true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();

        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Allowlist);
        assert_eq!(config.overlaynet.rules.len(), 2);
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
        assert_eq!(config.overlaynet.deny.len(), 2);
        assert_eq!(config.overlaynet.deny[1].ports, [80]);
        assert_eq!(config.overlaynet.limits.len(), 2);
        assert_eq!(config.overlaynet.limits[0].bytes_per_second, 1_250_000);
        assert_eq!(
            config.overlaynet.limits[1].host.as_deref(),
            Some("api.example.com")
        );
        assert_eq!(config.overlaynet.limits[1].bytes_per_second, 250_000);
        assert!(config.run.workspace.is_none());
        validate(&config, false).unwrap();
    }

    #[test]
    fn overlaynet_without_value_defaults_to_proxy() {
        let args = preset_args(&["--overlaynet", "--", "true"]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
    }

    #[test]
    fn help_exposes_driver_selection_and_the_simple_network_policy_surface() {
        let error = Cli::try_parse_from(["pvisor", "run", "--help"]).unwrap_err();
        let help = error.to_string();
        assert!(help.contains("--overlaynet-allow"));
        assert!(help.contains("--overlaynet-deny"));
        assert!(help.contains("--overlaynet-limit"));
        assert!(help.contains("--overlaynet-deny-all"));
        assert!(help.contains("--overlaynet [<MODE>]"));
        #[cfg(target_os = "linux")]
        {
            assert!(help.to_ascii_lowercase().contains("network"));
            assert!(help.contains("private network namespace"));
        }
        #[cfg(target_os = "macos")]
        assert!(help.contains("ambient host Unix sockets"));
        assert!(help.contains("--mount"));
        assert!(help.contains("--rootfs"));
        assert!(!help.contains("--workspace"));
        assert!(!help.contains("--overlaynet-policy"));
        assert!(!help.contains("--overlaynet-rule"));
    }

    #[test]
    fn macos_help_disclosures_are_rendered_on_every_platform() {
        use clap::CommandFactory;

        let mut command = Cli::command()
            .mut_subcommand("run", |run| run.long_about(MACOS_RUN_COMMAND_LONG_ABOUT));
        let help = command
            .find_subcommand_mut("run")
            .expect("run subcommand")
            .render_long_help()
            .to_string();
        // Terminal wrapping must not affect checks of the safety description.
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for disclosure in [
            "filesystem sandbox",
            "macFUSE",
            "Seatbelt",
            "Full-disk reads remain ambient",
            "selective network policies remain cooperative",
            "ambient host Unix sockets",
            "Job-scoped Unix IPC",
            "reported as warnings in best-effort mode",
            "With --strict",
            "fail before Agent execution",
        ] {
            assert!(
                help.contains(disclosure),
                "missing disclosure: {disclosure}"
            );
        }
    }

    #[test]
    fn safe_help_describes_the_effective_platform_boundary() {
        let help = Cli::try_parse_from(["pvisor", "run", "--help"])
            .unwrap_err()
            .to_string();

        #[cfg(target_os = "linux")]
        {
            assert!(help.contains("host filesystem view by default"));
            assert!(help.contains("--filesystem sandbox"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("macFUSE"));
            assert!(help.contains("Seatbelt"));
            assert!(help.contains("Full-disk reads remain ambient"));
            assert!(help.contains("fail before Agent execution"));
        }
    }

    #[test]
    fn help_exposes_compositional_overlayfs_without_a_mode_switch() {
        let error = Cli::try_parse_from(["pvisor", "run", "--help"]).unwrap_err();
        let help = error.to_string();
        for option in ["--mount", "--access", "--stage", "--overlayfs-max-size"] {
            assert!(help.contains(option), "missing {option}");
        }
        for obsolete in ["--overlayfs-mode", "--overlayfs-lower"] {
            assert!(!help.contains(obsolete), "obsolete option {obsolete}");
        }
    }

    #[test]
    fn deny_all_is_discoverable_and_replaces_configured_policy_details() {
        let args = preset_args(&["--overlaynet-deny-all", "--", "true"]);
        let mut config = RunConfig::default();
        config.overlaynet.allow = vec!["old.example".into()];
        config.overlaynet.deny = vec![NetworkAccessRule {
            host: "blocked.example".into(),
            ports: Vec::new(),
            transports: Vec::new(),
            allow_private_ips: false,
        }];
        config.overlaynet.limits = vec![NetworkBandwidthLimit {
            host: None,
            port: None,
            bytes_per_second: 1_000,
        }];

        apply_cli(&mut config, args).unwrap();

        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.policy, OverlayNetPolicy::Deny);
        assert!(config.overlaynet.allow.is_empty());
        assert!(config.overlaynet.rules.is_empty());
        assert!(config.overlaynet.deny.is_empty());
        assert!(config.overlaynet.limits.is_empty());
    }

    #[test]
    fn gateway_capture_enables_overlaynet_without_a_driver_flag() {
        let args = preset_args(&[
            "--gateway-mode",
            "capture",
            "--gateway-route",
            r#"name="openai", upstream="https://api.openai.com/v1""#,
            "--",
            "true",
        ]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.gateway.mode, GatewayMode::Capture);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
    }

    #[test]
    fn target_parser_handles_cidrs_portless_ipv6_and_malformed_inputs() {
        let cidr = parse_overlaynet_target("10.0.0.0/8:8080").unwrap();
        assert_eq!(cidr.host, "10.0.0.0/8");
        assert_eq!(cidr.ports, [8080]);

        assert!(
            parse_overlaynet_target("2001:db8::1")
                .unwrap()
                .ports
                .is_empty()
        );

        for invalid in ["", "api.example.com:", "https://api.example.com", "[::1"] {
            assert!(
                parse_overlaynet_target(invalid).is_err(),
                "accepted invalid target {invalid:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn target_parser_preserves_valid_domain_ports(
            label in "[a-z][a-z0-9]{0,12}",
            port in 1u16..=u16::MAX,
        ) {
            let host = format!("api-{label}.example.com");
            let target = parse_overlaynet_target(&format!("{host}:{port}"))
                .expect("generated domain target should parse");
            prop_assert_eq!(target.host, host);
            prop_assert_eq!(target.ports, vec![port]);
            prop_assert!(target.transports.is_empty());
            prop_assert!(!target.allow_private_ips);
        }

        #[test]
        fn target_parser_preserves_valid_ipv6_ports(
            segment in 1u16..=u16::MAX,
            port in 1u16..=u16::MAX,
        ) {
            let input = format!("[2001:db8::{segment:x}]:{port}");
            let target = parse_overlaynet_target(&input)
                .expect("generated IPv6 target should parse");
            prop_assert_eq!(target.host, format!("2001:db8::{segment:x}"));
            prop_assert_eq!(target.ports, vec![port]);
        }

        #[test]
        fn bandwidth_parser_matches_bit_and_byte_units(
            amount in 1u64..=1_000_000u64,
            unit in prop_oneof![
                Just(("bps", 1u64, true)),
                Just(("kbps", 1_000u64, true)),
                Just(("mbps", 1_000_000u64, true)),
                Just(("b/s", 1u64, false)),
                Just(("kb/s", 1_000u64, false)),
                Just(("mb/s", 1_000_000u64, false)),
                Just(("GB/S", 1_000_000_000u64, false)),
            ],
        ) {
            let (suffix, multiplier, bits) = unit;
            let parsed = parse_bandwidth(&format!("{amount}{suffix}"))
                .expect("generated bandwidth should parse");
            let scaled = amount * multiplier;
            let expected = if bits { scaled.div_ceil(8) } else { scaled };
            prop_assert_eq!(parsed, expected);
        }

        #[test]
        fn bandwidth_parser_rejects_zero_unsupported_and_overflow(
            unit in prop_oneof![
                Just("bps"),
                Just("kbps"),
                Just("mbps"),
                Just("gbps"),
                Just("b/s"),
                Just("kb/s"),
                Just("mb/s"),
                Just("gb/s"),
            ],
            amount in 1u64..=1_000_000u64,
            unsupported_suffix in prop_oneof![Just(""), Just("fast"), Just("tbps")],
            overflow_amount in 18_446_744_074u64..=u64::MAX,
        ) {
            let zero_input = format!("0{unit}");
            let unsupported_input = format!("{amount}{unsupported_suffix}");
            let overflow_input = format!("{overflow_amount}gbps");
            prop_assert!(parse_bandwidth(&zero_input).is_err());
            prop_assert!(parse_bandwidth(&unsupported_input).is_err());
            prop_assert!(parse_bandwidth(&overflow_input).is_err());
            prop_assert!(parse_bandwidth("").is_err());
        }

        #[test]
        fn target_parser_rejects_zero_and_out_of_range_ports(
            port in prop_oneof![Just(0u32), 65_536u32..=u32::MAX]
        ) {
            let input = format!("api.example.com:{port}");
            prop_assert!(
                port == 0 || port > u32::from(u16::MAX),
                "this property only generates invalid ports"
            );
            prop_assert!(parse_overlaynet_target(&input).is_err());
        }
    }

    #[test]
    fn cli_structured_rules_replace_config_rules() {
        let mut config = RunConfig::default();
        config.overlaynet.rules = vec![NetworkAccessRule {
            host: "old.example".into(),
            ports: vec![80],
            transports: Vec::new(),
            allow_private_ips: false,
        }];
        let args = preset_args(&[
            "--overlaynet-rule",
            r#"host="new.example", ports=[443], transports=["tcp_tunnel"]"#,
            "--",
            "true",
        ]);
        apply_cli(&mut config, args).unwrap();
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].host, "new.example");
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
    }

    #[test]
    fn overlayfs_options_enable_the_driver_and_select_a_stage() {
        let args = preset_args(&["--stage", "/tmp/pvisor-stage", "--", "true"]);
        let mut config = RunConfig::default();
        apply_cli(&mut config, args).unwrap();
        let overlayfs = config.overlayfs.expect("OverlayFS should be enabled");
        assert_eq!(
            overlayfs.stage.as_deref(),
            Some(Path::new("/tmp/pvisor-stage"))
        );
    }

    #[test]
    fn overlayfs_defaults_base_to_workspace_and_stage_to_run_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let compose = temporary.path().join("compose");
        let storage = temporary.path().join("run");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&compose).unwrap();
        std::fs::create_dir_all(&storage).unwrap();
        let config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                compose: vec![compose.clone()],
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };

        let hint = resolve_overlay(&config, &workspace, &storage)
            .unwrap()
            .unwrap();
        assert_eq!(
            hint.lower_dirs,
            [
                compose.canonicalize().unwrap(),
                workspace.canonicalize().unwrap()
            ]
        );
        assert_eq!(
            hint.stage_dir.as_deref(),
            Some(storage.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn overlayfs_compose_preserves_bottom_to_top_priority() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let bottom = temporary.path().join("bottom");
        let top = temporary.path().join("top");
        let storage = temporary.path().join("run");
        for path in [&workspace, &bottom, &top, &storage] {
            std::fs::create_dir_all(path).unwrap();
        }
        let config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                compose: vec![bottom.clone(), top.clone()],
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        let hint = resolve_overlay(&config, &workspace, &storage)
            .unwrap()
            .unwrap();
        assert_eq!(
            hint.lower_dirs,
            [
                top.canonicalize().unwrap(),
                bottom.canonicalize().unwrap(),
                workspace.canonicalize().unwrap()
            ]
        );
    }

    #[test]
    fn overlayfs_allows_hidden_stage_inside_base_or_compose() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let compose = temporary.path().join("compose");
        let storage = temporary.path().join("run");
        std::fs::create_dir_all(workspace.join("stage")).unwrap();
        std::fs::create_dir_all(compose.join("stage")).unwrap();
        std::fs::create_dir_all(&storage).unwrap();

        for (base, layers, stage) in [
            (workspace.clone(), Vec::new(), workspace.join("stage")),
            (
                workspace.clone(),
                vec![compose.clone()],
                compose.join("stage"),
            ),
        ] {
            let config = RunConfig {
                overlayfs: Some(OverlayFsSettings {
                    base: Some(base),
                    compose: layers,
                    stage: Some(stage),
                    ..OverlayFsSettings::default()
                }),
                ..RunConfig::default()
            };
            assert!(resolve_overlay(&config, &workspace, &storage).is_ok());
        }

        let config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                base: Some(workspace.clone()),
                stage: Some(temporary.path().to_path_buf()),
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        assert!(resolve_overlay(&config, &workspace, &storage).is_err());
    }

    #[test]
    fn composed_layers_cannot_be_auto_applied() {
        let config = RunConfig {
            run: pvisor::RunSettings {
                command: vec!["true".into()],
                ..pvisor::RunSettings::default()
            },
            overlayfs: Some(OverlayFsSettings {
                compose: vec!["/tmp/layer".into()],
                commit: OverlayFsCommit::Apply,
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        assert!(
            validate(&config, false)
                .unwrap_err()
                .to_string()
                .contains("cannot be combined")
        );
    }

    #[test]
    fn compose_replaces_configured_layers_and_enables_overlayfs() {
        let args = preset_args(&[
            "--mount",
            "/tmp/first:read",
            "--mount",
            "/tmp/second:read",
            "--",
            "true",
        ]);
        let mut config = RunConfig {
            overlayfs: Some(OverlayFsSettings {
                mount: vec![FilesystemMount {
                    source: "/tmp/old".into(),
                    target: Some("/tmp/old".into()),
                    access: FilesystemAccessLevel::Read,
                }],
                ..OverlayFsSettings::default()
            }),
            ..RunConfig::default()
        };
        apply_cli(&mut config, args).unwrap();
        let mounts = config.overlayfs.unwrap().mount;
        assert_eq!(mounts[0].source, PathBuf::from("/tmp/first"));
        assert_eq!(mounts[1].source, PathBuf::from("/tmp/second"));
    }
}
