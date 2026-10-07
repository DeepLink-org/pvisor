//! Canonical pVisor Run configuration.
//!
//! TOML and the `pvisor run` command line both resolve into [`RunConfig`].
//! Runtime drivers only consume the resolved value and do not read config
//! files themselves.

use std::path::{Path, PathBuf};

use pvisor_core::gateway::{CaptureLevel, ModelRoute};
use pvisor_core::{FilesystemCapability, ResourceLimits};
#[cfg(feature = "gateway")]
use pvisor_gateway::config::ProxyConfig;
use pvisor_overlaynet::{NetworkAccessRule, NetworkBandwidthLimit};
use serde::{Deserialize, Serialize};

use crate::runtime::OverlayHint;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunConfig {
    pub run: RunSettings,
    pub container: ContainerSettings,
    pub vm: VmSettings,
    /// Process filesystem access policy. This is independent from OverlayFS
    /// change staging and from OverlayNet network policy.
    pub filesystem: FilesystemMode,
    /// Transactional OverlayFS configuration. Absence means no staged OverlayFS view.
    pub overlayfs: Option<OverlayFsSettings>,
    pub overlaynet: OverlayNetSettings,
    pub gateway: GatewaySettings,
    /// Durable Trace Event journal recording.
    pub record: RecordSettings,
    /// Session, workspace and user policy layers; resolved once per Attempt.
    pub policies: pvisor_core::SessionPolicies,
}

impl RunConfig {
    pub fn load_policy_defaults(
        &mut self,
        workspace: &Path,
        user_root: Option<&Path>,
    ) -> anyhow::Result<()> {
        fn load(root: &Path, directory_name: &str) -> anyhow::Result<pvisor_core::PolicyLayer> {
            let path = root.join(directory_name).join("policy.toml");
            let name = std::ffi::CString::new(directory_name)?;
            use std::io::Read;
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::fs::MetadataExt;
            const MAX_BYTES: u64 = 1024 * 1024;
            let open = || -> std::io::Result<std::fs::File> {
                // The supplied root is trusted; both policy path components are not.
                let root = std::fs::File::open(root)?;
                let fd = unsafe {
                    libc::openat(
                        root.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let directory = unsafe { std::fs::File::from_raw_fd(fd) };
                let metadata = directory.metadata()?;
                if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "policy directory must be owned by the current user and not writable by others",
                    ));
                }
                let fd = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        c"policy.toml".as_ptr(),
                        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(unsafe { std::fs::File::from_raw_fd(fd) })
            };
            let file = match open() {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Default::default());
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("open policy {}: {error}", path.display()));
                }
            };
            let metadata = file.metadata()?;
            anyhow::ensure!(
                metadata.is_file()
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o022 == 0,
                "policy {} must be a regular file owned by the current user and not writable by others",
                path.display()
            );
            anyhow::ensure!(
                metadata.len() <= MAX_BYTES,
                "policy {} exceeds {MAX_BYTES} bytes",
                path.display()
            );
            let mut source = String::new();
            file.take(MAX_BYTES + 1).read_to_string(&mut source)?;
            anyhow::ensure!(
                source.len() as u64 <= MAX_BYTES,
                "policy {} exceeds {MAX_BYTES} bytes",
                path.display()
            );
            toml::from_str(&source)
                .map_err(|error| anyhow::anyhow!("parse policy {}: {error}", path.display()))
        }

        fn inherit(layer: &mut pvisor_core::PolicyLayer, defaults: pvisor_core::PolicyLayer) {
            if layer.network.is_none() {
                layer.network = defaults.network;
            }
            if layer.filesystem.is_none() {
                layer.filesystem = defaults.filesystem;
            }
        }
        inherit(&mut self.policies.workspace, load(workspace, ".pvisor")?);
        if let Some(root) = user_root {
            inherit(&mut self.policies.user, load(root, "pvisor")?);
        }
        Ok(())
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&source)?)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunSettings {
    /// Internally resolved project association; not a user-facing configuration parameter.
    #[serde(skip)]
    pub workspace: Option<PathBuf>,
    pub agent: String,
    pub executor: RunExecutorKind,
    pub timeout_ms: Option<u64>,
    pub stdio: RunStdio,
    pub policy: RunPolicy,
    /// Inherit the complete supervisor environment. Safe CLI runs override
    /// this to false and project only baseline plus explicitly passed keys.
    pub inherit_env: bool,
    /// Host environment variables projected by name when `inherit_env=false`.
    pub pass_env: Vec<String>,
    /// Explicit host paths available outside the project stage.
    pub filesystem: Vec<FilesystemCapability>,
    pub resource_limits: ResourceLimits,
    pub command: Vec<String>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            workspace: None,
            agent: "agent".into(),
            executor: RunExecutorKind::Host,
            timeout_ms: None,
            stdio: RunStdio::Inherit,
            policy: RunPolicy::Observe,
            inherit_env: true,
            pass_env: Vec::new(),
            filesystem: Vec::new(),
            resource_limits: ResourceLimits::default(),
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunExecutorKind {
    #[default]
    Host,
    Container,
    Vm,
}

/// OCI CLI configuration used by [`crate::ContainerExecutor`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerSettings {
    /// Native OCI runtime executable (runc or crun).
    pub runtime: PathBuf,
    /// Image reference used for the Agent process.
    pub image: String,
    /// Prepared OCI rootfs directory. When omitted, `image` is prepared by pVisor.
    pub rootfs: Option<PathBuf>,
    /// Target-specific pVisor injected into the container. Defaults to the
    /// running executable; set it when the guest ABI differs from the host.
    pub pvisor_binary: Option<PathBuf>,
    /// Optional native OCI platform assertion. Must match the host architecture;
    /// cross-platform selection/emulation and artifact auto-discovery are unsupported.
    pub platform: Option<ContainerPlatform>,
    /// Container network namespace mode.
    pub network: ContainerNetwork,
    /// Container-native working directory used when the Run has no mounted cwd.
    pub workdir: Option<PathBuf>,
    /// Optional container user (`uid`, `uid:gid`, or a named user).
    pub user: Option<String>,
    /// Mount the image root filesystem read-only.
    pub read_only_rootfs: bool,
    /// Additional explicit bind mounts. The runtime automatically mounts the
    /// injected pVisor, delegated control directory, final Run cwd, and capture
    /// configuration when present.
    pub mounts: Vec<ContainerMount>,
}

impl Default for ContainerSettings {
    fn default() -> Self {
        Self {
            runtime: PathBuf::from("crun"),
            image: String::new(),
            rootfs: None,
            pvisor_binary: None,
            platform: None,
            network: ContainerNetwork::Host,
            workdir: None,
            user: None,
            read_only_rootfs: false,
            mounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerPlatform {
    LinuxAmd64,
    LinuxArm64,
}

impl std::str::FromStr for ContainerPlatform {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "linux/amd64" | "linux-amd64" | "amd64" | "x86_64" => Ok(Self::LinuxAmd64),
            "linux/arm64" | "linux-arm64" | "arm64" | "aarch64" => Ok(Self::LinuxArm64),
            _ => Err(format!(
                "unsupported container platform `{value}`; expected linux/amd64 or linux/arm64"
            )),
        }
    }
}

/// libkrun process isolation over a pVisor-provided Linux rootfs OverlayFS.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct VmSettings {
    /// Host-only live control endpoint; omitted uses an automatic private socket.
    /// The existing parent must be private (0700); existing paths are never overwritten.
    pub control_socket: Option<PathBuf>,
    /// New private live RAM backing file. Omitted: create an attempt-local file
    /// in the user's disk cache, except Linux dedup without an explicit backing
    /// uses private anonymous RAM. Existing files are never overwritten.
    pub ram_backing: Option<PathBuf>,
    /// Commit RAM as Seekable base/delta generations through a cached FUSE adapter. Requires
    /// /dev/fuse on Linux or the macFUSE kernel backend on Apple Silicon.
    pub ram_compression: bool,
    /// Linux x86_64 live cold pager over private anonymous RAM, with an
    /// instance-local compressed store (no FUSE or external service). Requires
    /// kernel-fault userfaultfd authority; disabled by default.
    pub cold_ram_compression: bool,
    /// Opt in to host RAM dedup advice and its cross-workload sharing risks.
    /// Only private RAM is eligible (including restored COW); live shared RAM
    /// is skipped. Accepted advice is not evidence of merged bytes or savings.
    pub ram_dedup: bool,
    /// Experimental shared cold-page pool socket (Linux x86_64 / Apple Silicon). Requires a separately
    /// managed pool; loss of that pool fails dependent VMs. Disabled by default.
    pub memory_pool: Option<PathBuf>,
    /// Host-owned immutable filesystem pool for native execution checkpoints.
    /// Must be independent of every VM-writable root and on the Job's volume.
    /// Linux x86-64 no-network snapshot profile only; omitted keeps owned copies.
    pub snapshot_filesystem_pool: Option<PathBuf>,
    /// Optional same-host node resource service. Restore pins a shared read-only
    /// RAM backing there until native teardown; service loss is not transparent.
    pub node_socket: Option<PathBuf>,
    /// Linux root filesystem exported to the libkrun guest; defaults to host `/`.
    pub rootfs: Option<PathBuf>,
    /// Explicit OCI image used instead of the host root filesystem.
    pub image: Option<String>,
    /// Content-addressed OCI cache. The platform cache directory is used when omitted.
    pub image_store: Option<PathBuf>,
    /// Reject apply operations that would mutate the configured rootfs lower.
    pub rootfs_immutable: bool,
    /// Optional directory containing libkrunfw. Packaged glibc/macOS builds
    /// discover it next to pVisor; source builds use a verified per-user
    /// download cache. The x86_64 Linux musl build embeds the kernel bundle
    /// and rejects this setting.
    pub library_dir: Option<PathBuf>,
    pub memory_mib: u32,
    pub cpus: u16,
}

impl VmSettings {
    pub(crate) fn cold_pager_requested(&self) -> bool {
        self.cold_ram_compression
            || self.memory_pool.is_some()
            || std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_some()
    }

    pub(crate) fn validate_ram_dedup(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cold_ram_compression || cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "vm.cold_ram_compression requires Linux x86_64 kernel-fault userfaultfd support"
        );
        anyhow::ensure!(
            !self.cold_ram_compression
                || (self.memory_pool.is_none()
                    && std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_none()),
            "vm.cold_ram_compression cannot be combined with an external memory pool"
        );
        anyhow::ensure!(
            !self.cold_pager_requested()
                || (!self.ram_compression
                    && self.ram_backing.is_none()
                    && self.snapshot_filesystem_pool.is_none()),
            "cold pager requires private anonymous RAM, without vm.ram_backing, vm.ram_compression or snapshot capture/pool"
        );
        anyhow::ensure!(
            !self.ram_dedup
                || (!self.ram_compression
                    && !self.cold_ram_compression
                    && self.memory_pool.is_none()),
            "vm.ram_dedup cannot be combined with vm.memory_pool, vm.ram_compression or vm.cold_ram_compression"
        );
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        anyhow::ensure!(
            !self.ram_dedup || std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_none(),
            "vm.ram_dedup cannot be combined with PVISOR_EXPERIMENTAL_MEMORY_POOL"
        );
        Ok(())
    }
}

impl Default for VmSettings {
    fn default() -> Self {
        Self {
            control_socket: None,
            ram_backing: None,
            ram_compression: false,
            cold_ram_compression: false,
            ram_dedup: false,
            memory_pool: None,
            snapshot_filesystem_pool: None,
            node_socket: None,
            rootfs: None,
            image: None,
            image_store: None,
            rootfs_immutable: false,
            library_dir: None,
            memory_mib: 2048,
            cpus: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerNetwork {
    /// Share the runtime host network. This keeps an in-process Gateway and
    /// OverlayNet proxy reachable at their injected loopback addresses.
    #[default]
    Host,
    Bridge,
    None,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContainerMount {
    pub source: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunStdio {
    #[default]
    Inherit,
    Capture,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunPolicy {
    #[default]
    Observe,
    Enforce,
}

/// Whether a host process receives pVisor's synthetic-root/Landlock or
/// Seatbelt filesystem access restrictions.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemMode {
    /// Preserve the host process filesystem view and permissions.
    #[default]
    Host,
    /// Restrict filesystem access to pVisor-declared roots.
    Sandbox,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayFsSettings {
    /// First-mutation fsyncs (strict), or durable checkpoint/completion boundaries.
    pub durability: pvisor_core::overlay::StageDurability,
    #[serde(skip)]
    pub access_policy: pvisor_core::overlay::FileAccessPolicy,
    /// New unified filesystem mounts. Runtime normalization converts these into executor capabilities.
    pub mount: Vec<FilesystemMount>,
    /// New unified Agent-visible access rules.
    pub access: Vec<FilesystemAccessRule>,
    /// Optional host base layer and default apply destination (normally the workspace).
    #[serde(skip)]
    pub base: Option<PathBuf>,
    /// Absolute path where the staged overlay is exposed inside a libkrun guest.
    #[serde(rename = "path")]
    #[serde(skip)]
    pub target: Option<PathBuf>,
    /// Host mount point used as the Agent-visible overlay view.
    #[serde(skip)]
    pub merged_dir: Option<PathBuf>,
    /// Read-only host layers, listed bottom-to-top as supplied on the CLI.
    #[serde(skip)]
    pub compose: Vec<PathBuf>,
    /// Durable writable stage root. Defaults to the generated per-Run storage directory.
    pub stage: Option<PathBuf>,
    /// Aggregate byte budget for the whole staged filesystem.
    #[serde(rename = "max_size")]
    pub stage_size_bytes: Option<u64>,
    #[serde(skip)]
    pub commit: OverlayFsCommit,
}

impl Default for OverlayFsSettings {
    fn default() -> Self {
        Self {
            durability: Default::default(),
            access_policy: Default::default(),
            mount: Vec::new(),
            access: Vec::new(),
            base: None,
            target: None,
            merged_dir: None,
            compose: Vec::new(),
            stage: None,
            stage_size_bytes: None,
            commit: OverlayFsCommit::Manual,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilesystemMount {
    pub source: PathBuf,
    #[serde(default)]
    pub target: Option<PathBuf>,
    pub access: FilesystemAccessLevel,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilesystemAccessRule {
    pub path: String,
    pub level: FilesystemAccessLevel,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccessLevel {
    Deny,
    Ask,
    Read,
    Warn,
    Stage,
    Write,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayFsCommit {
    #[default]
    Manual,
    Apply,
    Drop,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayNetSettings {
    pub mode: OverlayNetMode,
    pub listen: String,
    pub policy: OverlayNetPolicy,
    pub allow: Vec<String>,
    /// Structured grants for port-, transport-, and address-scoped policy.
    pub rules: Vec<NetworkAccessRule>,
    pub deny: Vec<NetworkAccessRule>,
    pub limits: Vec<NetworkBandwidthLimit>,
}

impl Default for OverlayNetSettings {
    fn default() -> Self {
        Self {
            mode: OverlayNetMode::Auto,
            listen: "127.0.0.1:19081".into(),
            policy: OverlayNetPolicy::Public,
            allow: Vec::new(),
            rules: Vec::new(),
            deny: Vec::new(),
            limits: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayNetMode {
    #[default]
    Auto,
    Off,
    Proxy,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayNetPolicy {
    #[default]
    Public,
    Deny,
    Allowlist,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewaySettings {
    pub mode: GatewayMode,
    pub profile: Option<GatewayProfile>,
    pub zcode_builtin_config: Option<PathBuf>,
    pub admin_listen: String,
    pub level: CaptureLevel,
    pub session_header: String,
    pub debug: bool,
    pub routes: Vec<ModelRoute>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            mode: GatewayMode::Off,
            profile: None,
            zcode_builtin_config: None,
            admin_listen: "127.0.0.1:9876".into(),
            level: CaptureLevel::Dialogue,
            session_header: "x-pvisor-session-id".into(),
            debug: false,
            routes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayMode {
    #[default]
    Off,
    Capture,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayProfile {
    ZcodeBigmodel,
}

impl GatewayProfile {
    pub fn routes(self) -> Vec<ModelRoute> {
        match self {
            Self::ZcodeBigmodel => vec![ModelRoute {
                name: "*".into(),
                provider: Some("anthropic".into()),
                upstream: Some("https://open.bigmodel.cn/api/anthropic/v1".into()),
                upstream_anthropic: Some("https://open.bigmodel.cn/api/anthropic/v1".into()),
                api_key_env: None,
                api_key: None,
                forward: None,
            }],
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordSettings {
    /// Local directory or file for Trace Event journal.
    pub destination: Option<PathBuf>,
}

/// Resolved configuration for the internal OverlayNet + optional Gateway sink.
#[derive(Debug, Clone)]
#[cfg(feature = "gateway")]
pub struct GatewayDriverConfig {
    pub proxy: ProxyConfig,
    pub output_dir: PathBuf,
    pub gateway_enabled: bool,
    pub model_wait: Option<std::sync::Arc<dyn pvisor_gateway::model_wait::ModelWaitLifecycle>>,
}

/// Programmatic network-driver configuration. `Auto` selects smoltcp for a
/// libkrun VM and otherwise remains inactive unless Gateway/proxy is requested.
#[derive(Debug, Clone)]
pub struct NetworkDriverConfig {
    pub mode: OverlayNetMode,
    pub network: pvisor_overlaynet::NetworkConfig,
    pub listen: String,
}

impl Default for NetworkDriverConfig {
    fn default() -> Self {
        Self {
            mode: OverlayNetMode::Auto,
            network: pvisor_overlaynet::NetworkConfig::default(),
            listen: "127.0.0.1:0".into(),
        }
    }
}

impl NetworkDriverConfig {
    pub fn listen(mut self, listen: impl Into<String>) -> Self {
        self.listen = listen.into();
        self
    }

    pub fn new(mode: OverlayNetMode, network: pvisor_overlaynet::NetworkConfig) -> Self {
        Self {
            mode,
            network,
            listen: "127.0.0.1:0".into(),
        }
    }
}

#[cfg(feature = "gateway")]
impl GatewayDriverConfig {
    pub fn new(proxy: ProxyConfig) -> Self {
        Self {
            proxy,
            output_dir: PathBuf::from(".pvisor/run"),
            gateway_enabled: true,
            model_wait: None,
        }
    }

    pub fn output_dir(mut self, output_dir: impl Into<PathBuf>) -> Self {
        self.output_dir = output_dir.into();
        self
    }

    pub fn gateway_enabled(mut self, enabled: bool) -> Self {
        self.gateway_enabled = enabled;
        self
    }

    /// Install an Attempt-bound cooperative wait/admission lifecycle. Calls
    /// without the explicit inference-idle declaration retain normal behavior.
    pub fn model_wait(
        mut self,
        lifecycle: std::sync::Arc<dyn pvisor_gateway::model_wait::ModelWaitLifecycle>,
    ) -> Self {
        self.model_wait = Some(lifecycle);
        self
    }
}

/// Programmatic pVisor driver assembly configuration.
#[derive(Debug, Clone, Default)]
pub struct PVisorConfig {
    #[cfg(feature = "gateway")]
    pub gateway: Option<GatewayDriverConfig>,
    pub network: NetworkDriverConfig,
    pub overlay: OverlayHint,
}

impl PVisorConfig {
    #[cfg(feature = "gateway")]
    pub fn with_gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.gateway = Some(gateway);
        self
    }

    pub fn with_overlay(mut self, overlay: OverlayHint) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn with_network(mut self, network: NetworkDriverConfig) -> Self {
        self.network = network;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_config_toml_roundtrip() {
        let config: RunConfig = toml::from_str(
            r#"
filesystem = "sandbox"

[run]
executor = "container"
command = ["codex"]

[container]
runtime = "podman"
image = "example/agent:latest"
network = "none"

[overlayfs]

[[overlayfs.mount]]
source = "/tmp/lower"
access = "read"

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]
transports = ["tcp_tunnel"]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"

[record]
destination = "/tmp/events"

[[gateway.routes]]
name = "openai"
upstream = "https://api.openai.com/v1"
"#,
        )
        .unwrap();
        assert_eq!(config.filesystem, FilesystemMode::Sandbox);
        assert_eq!(
            config.overlayfs.as_ref().unwrap().mount[0].source,
            PathBuf::from("/tmp/lower")
        );
        assert_eq!(config.run.executor, RunExecutorKind::Container);
        assert_eq!(config.container.runtime, Path::new("podman"));
        assert_eq!(config.container.image, "example/agent:latest");
        assert_eq!(config.container.network, ContainerNetwork::None);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
        assert_eq!(config.overlaynet.deny.len(), 1);
        assert_eq!(config.overlaynet.limits[0].bytes_per_second, 1_250_000);
        assert_eq!(config.gateway.routes.len(), 1);
        assert_eq!(
            config.record.destination.as_deref(),
            Some(Path::new("/tmp/events"))
        );
        assert_eq!(config.run.command, ["codex"]);
    }

    #[test]
    fn vm_config_toml_roundtrip() {
        let config: RunConfig = toml::from_str(
            r#"
[run]
executor = "vm"
command = ["agent"]

[vm]
rootfs = "/opt/rootfs"
ram_backing = "/opt/ram/session.ram"
ram_compression = true
library_dir = "/opt/libkrun/lib"
memory_mib = 4096
cpus = 4
"#,
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/opt/rootfs")));
        assert_eq!(
            config.vm.ram_backing.as_deref(),
            Some(Path::new("/opt/ram/session.ram"))
        );
        assert_eq!(
            config.vm.library_dir.as_deref(),
            Some(Path::new("/opt/libkrun/lib"))
        );
        assert_eq!(config.vm.memory_mib, 4096);
        assert!(config.vm.ram_compression);
        assert_eq!(config.vm.cpus, 4);
        let encoded = toml::to_string_pretty(&config).unwrap();
        let decoded: RunConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn vm_memory_settings_parse_defaults_explicit_bools_and_paths() {
        let omitted: RunConfig = toml::from_str("[vm]\n").unwrap();
        assert!(!omitted.vm.ram_compression);
        assert!(!omitted.vm.cold_ram_compression);
        assert!(!omitted.vm.ram_dedup);
        assert!(omitted.vm.node_socket.is_none());
        assert!(omitted.vm.snapshot_filesystem_pool.is_none());
        for enabled in [false, true] {
            let config: RunConfig = toml::from_str(&format!(
                "[vm]\nram_compression = {enabled}\ncold_ram_compression = {enabled}\nram_dedup = {enabled}\nnode_socket = 'relative/node.sock'\nsnapshot_filesystem_pool = '/tmp/snapshot pool'\n"
            ))
            .unwrap();
            // Serialization is independent of runtime strategy compatibility.
            assert_eq!(config.vm.ram_compression, enabled);
            assert_eq!(config.vm.cold_ram_compression, enabled);
            assert_eq!(config.vm.ram_dedup, enabled);
            assert_eq!(
                config.vm.node_socket.as_deref(),
                Some(Path::new("relative/node.sock"))
            );
            assert_eq!(
                config.vm.snapshot_filesystem_pool.as_deref(),
                Some(Path::new("/tmp/snapshot pool"))
            );
            let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
            assert_eq!(decoded.vm, config.vm);
        }
    }

    #[test]
    fn control_socket_defaults_to_auto_and_roundtrips() {
        let default: RunConfig = toml::from_str("").unwrap();
        assert_eq!(default.vm.control_socket, None);
        let config: RunConfig =
            toml::from_str("[vm]\ncontrol_socket = '/private/control.sock'\n").unwrap();
        assert_eq!(
            config.vm.control_socket,
            Some(PathBuf::from("/private/control.sock"))
        );
        let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn cold_ram_compression_defaults_off_and_roundtrips() {
        assert!(!VmSettings::default().cold_ram_compression);
        let legacy: RunConfig = toml::from_str("[vm]\ncpus = 1\n").unwrap();
        assert!(!legacy.vm.cold_ram_compression);
        let config: RunConfig = toml::from_str("[vm]\ncold_ram_compression = true\n").unwrap();
        assert!(config.vm.cold_ram_compression);
        assert!(!config.vm.ram_compression);
        assert!(config.vm.memory_pool.is_none());
        let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn cold_ram_compression_rejects_conflicting_strategies() {
        for conflict in [
            VmSettings {
                ram_dedup: true,
                ..Default::default()
            },
            VmSettings {
                ram_compression: true,
                ..Default::default()
            },
            VmSettings {
                ram_backing: Some("unused.ram".into()),
                ..Default::default()
            },
            VmSettings {
                memory_pool: Some("pool.sock".into()),
                ..Default::default()
            },
            VmSettings {
                snapshot_filesystem_pool: Some("snapshots".into()),
                ..Default::default()
            },
        ] {
            let settings = VmSettings {
                cold_ram_compression: true,
                ..conflict
            };
            assert!(settings.validate_ram_dedup().is_err());
        }
        assert!(
            VmSettings {
                cold_ram_compression: true,
                ..Default::default()
            }
            .validate_ram_dedup()
            .is_ok()
        );
    }

    #[test]
    fn ram_dedup_defaults_off_and_roundtrips() {
        assert!(!VmSettings::default().ram_dedup);
        let legacy: RunConfig = toml::from_str("[vm]\ncpus = 1\n").unwrap();
        assert!(!legacy.vm.ram_dedup);
        let config: RunConfig = toml::from_str("[vm]\nram_dedup = true\n").unwrap();
        assert!(config.vm.ram_dedup);
        let decoded: RunConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn ram_dedup_rejects_other_ram_strategies() {
        for settings in [
            VmSettings {
                ram_dedup: true,
                ram_compression: true,
                ..Default::default()
            },
            VmSettings {
                ram_dedup: true,
                memory_pool: Some("/private/pool/socket".into()),
                ..Default::default()
            },
        ] {
            assert!(
                settings
                    .validate_ram_dedup()
                    .unwrap_err()
                    .to_string()
                    .contains("vm.ram_dedup")
            );
        }
        assert!(VmSettings::default().validate_ram_dedup().is_ok());
    }

    #[test]
    fn overlaynet_defaults_to_auto_for_executor_specific_selection() {
        let config = RunConfig::default();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Auto);
        assert_eq!(PVisorConfig::default().network.mode, OverlayNetMode::Auto);
    }

    #[test]
    fn configuration_requires_canonical_vm_names() {
        assert!(toml::from_str::<RunConfig>("[run]\nexecutor = \"kvm\"\n").is_err());
        assert!(toml::from_str::<RunConfig>("[kvm]\nrootfs = \"/opt/rootfs\"\n").is_err());
        let config: RunConfig =
            toml::from_str("[run]\nexecutor = \"vm\"\n[vm]\nrootfs = \"/opt/rootfs\"\n").unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/opt/rootfs")));
    }
}

#[cfg(test)]
mod session_policy_tests {
    use super::*;
    #[test]
    fn policy_defaults_keep_explicit_scopes_and_reject_invalid_rules() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let user = root.path().join("user");
        std::fs::create_dir_all(workspace.join(".pvisor")).unwrap();
        std::fs::create_dir_all(user.join("pvisor")).unwrap();
        std::fs::write(
            user.join("pvisor/policy.toml"),
            "[filesystem]\ndeny = ['secrets/**']\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join(".pvisor/policy.toml"),
            "[filesystem]\nallow = ['secrets/workspace']\n",
        )
        .unwrap();
        let mut config = RunConfig::default();
        config.policies.session.filesystem = Some(
            pvisor_core::FileAccessPolicy::new_with_allow(
                vec![],
                vec![],
                vec![],
                vec!["secrets/session".into()],
            )
            .unwrap(),
        );
        config
            .load_policy_defaults(&workspace, Some(&user))
            .unwrap();
        let policy = config.policies.filesystem(&Default::default());
        assert_eq!(
            policy.authorize(Path::new("secrets/session")),
            pvisor_core::FileAccessDecision::Deny
        );
        assert_eq!(
            policy.authorize(Path::new("secrets/workspace")),
            pvisor_core::FileAccessDecision::Deny
        );
        assert_eq!(
            policy.authorize(Path::new("secrets/other")),
            pvisor_core::FileAccessDecision::Deny
        );
        std::fs::write(
            workspace.join(".pvisor/policy.toml"),
            "[filesystem]\ndeny = ['../bad']\n",
        )
        .unwrap();
        assert!(
            RunConfig::default()
                .load_policy_defaults(&workspace, Some(&user))
                .is_err()
        );
    }
    #[test]
    fn policy_defaults_reject_symlinks_special_files_and_unbounded_input() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let directory = workspace.join(".pvisor");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("policy.toml");
        let source = root.path().join("external.toml");
        std::fs::write(&source, "[network]\nallow = [{ host = 'api.example' }]\n").unwrap();
        let load = || RunConfig::default().load_policy_defaults(&workspace, None);
        symlink(&source, &path).unwrap();
        assert!(load().is_err());
        std::fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(load().is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::File::create(&path)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert!(load().is_err());
        std::fs::copy(&source, &path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(load().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load().is_ok());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
        symlink(root.path(), &directory).unwrap();
        assert!(load().is_err());
    }
}
