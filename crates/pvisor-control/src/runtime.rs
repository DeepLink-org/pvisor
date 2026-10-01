//! Stable value types shared by pVisor, Gateway, and storage contracts.
//!
//! A local Run and its execution Attempt identify policy requests and results.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.trim().is_empty()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

string_id!(RunId);
string_id!(AttemptId);

/// A versioned logical Agent reference. It describes identity, not placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
}

impl AgentRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            adapter: None,
        }
    }
}

/// One semantic Agent execution submitted to pVisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    #[serde(default = "runtime_schema_version")]
    pub schema_version: u32,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    pub agent: AgentRef,
    pub invocation: RunInvocation,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub capabilities: CapabilitySet,
    #[serde(default)]
    pub policies: crate::SessionPolicies,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl RunSpec {
    pub fn process(
        run_id: impl Into<RunId>,
        agent: impl Into<String>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id: run_id.into(),
            task_id: None,
            parent_run_id: None,
            agent: AgentRef::new(agent),
            invocation: RunInvocation::Process(ProcessInvocation::new(program)),
            input: Value::Null,
            runtime: RuntimeConfig::default(),
            capabilities: CapabilitySet::default(),
            policies: crate::SessionPolicies::default(),
            metadata: BTreeMap::new(),
        }
    }
}

fn runtime_schema_version() -> u32 {
    RUNTIME_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunInvocation {
    Process(ProcessInvocation),
}

/// Local or container-hosted process invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInvocation {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    #[serde(default)]
    pub stdin: StdioMode,
    #[serde(default)]
    pub stdout: StdioMode,
    #[serde(default)]
    pub stderr: StdioMode,
}

impl ProcessInvocation {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            inherit_env: true,
            stdin: StdioMode::Inherit,
            stdout: StdioMode::Inherit,
            stderr: StdioMode::Inherit,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdioMode {
    #[default]
    Inherit,
    Capture,
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Wall-clock limit for one Attempt. `None` means no pVisor deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Grace period between a cooperative process-tree termination request and
    /// forced termination.
    #[serde(default = "default_termination_grace_ms")]
    pub termination_grace_ms: u64,
    /// Maximum retained bytes for each captured stdout/stderr stream.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Optional process-tree resource budget. Executors must report which
    /// fields they can enforce; an unset field is intentionally unlimited.
    #[serde(default)]
    pub resource_limits: ResourceLimits,
    #[serde(default)]
    pub policy_mode: PolicyMode,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            timeout_ms: None,
            termination_grace_ms: default_termination_grace_ms(),
            max_output_bytes: default_max_output_bytes(),
            resource_limits: ResourceLimits::default(),
            policy_mode: PolicyMode::Audit,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Aggregate resident-memory budget when the executor has a process-tree
    /// controller; otherwise an address-space limit may be used and reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// Maximum number of processes/threads admitted for the Run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<u64>,
    /// CPU time budget in milliseconds. Native rlimit backends round this up
    /// to whole seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time_ms: Option<u64>,
    /// Maximum number of open file descriptors inherited by descendants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_files: Option<u64>,
    /// Maximum size of a file created by the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
}

impl ResourceLimits {
    pub fn is_empty(&self) -> bool {
        self.memory_bytes.is_none()
            && self.processes.is_none()
            && self.cpu_time_ms.is_none()
            && self.open_files.is_none()
            && self.file_size_bytes.is_none()
    }
}

fn default_max_output_bytes() -> usize {
    1024 * 1024
}

fn default_termination_grace_ms() -> u64 {
    2_000
}

/// `Audit` records requested capabilities. `Enforce` requires an executor that
/// can actually prevent ambient access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    #[default]
    Audit,
    Enforce,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub filesystem: Vec<FilesystemCapability>,
    #[serde(default)]
    pub network: NetworkCapability,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub allow_subprocess: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemCapability {
    pub path: String,
    pub access: FilesystemAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    Read,
    ReadWrite,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NetworkCapability {
    Scoped {
        layers: Vec<(crate::PolicyScope, crate::NetworkPolicyLayer)>,
        fallback: Box<NetworkCapability>,
    },
    /// Use the executor's ambient network. Only valid for audit/compatibility runs.
    #[default]
    Ambient,
    Deny,
    AllowList {
        /// Legacy host-only rules. Empty port/transport constraints mean any.
        #[serde(default)]
        hosts: Vec<String>,
        /// Structured rules for protocol- and port-scoped access.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rules: Vec<NetworkAccessRule>,
    },
    /// Ordered policy surface: explicit denies win, then the default action
    /// determines whether an allow rule is required.
    Policy {
        default_action: NetworkDefaultAction,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<NetworkAccessRule>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deny: Vec<NetworkAccessRule>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        limits: Vec<NetworkBandwidthLimit>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkDefaultAction {
    Allow,
    Deny,
}

/// One declarative network grant. `host` accepts an exact hostname,
/// `*.suffix`, an IP literal, or a CIDR. Empty `ports` or `transports` mean
/// unrestricted for that dimension. Hostname rules reject private and
/// loopback resolved addresses unless `allow_private_ips` is enabled; other
/// special-purpose addresses still require an explicit IP or CIDR grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAccessRule {
    pub host: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transports: Vec<NetworkTransport>,
    #[serde(default)]
    pub allow_private_ips: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkBandwidthLimit {
    /// Absent means all intercepted destinations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub bytes_per_second: u64,
}

/// Runtime-neutral network request presented to pVisor for authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub transport: NetworkTransport,
    /// Address selected from the request hostname's current DNS result. This
    /// is absent only during a pre-resolution policy check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ip: Option<std::net::IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkTransport {
    Http,
    Https,
    TcpTunnel,
}

/// Model invocation metadata. Request/response bodies intentionally stay in
/// Capture; pVisor receives only the information needed for policy and audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCallRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    pub call_id: String,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: String,
    pub protocol: String,
    pub upstream_host: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAccessPolicy {
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub allowed_providers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessReason {
    AmbientNetwork,
    TrustedLocal,
    NetworkAllowList,
    NetworkDenied,
    NetworkAllowListEmpty,
    HostNotAllowed,
    PortNotAllowed,
    TransportNotAllowed,
    ResolvedAddressNotAllowed,
    ExplicitlyDenied,
    ModelAllowed,
    ModelNotAllowed,
    ProviderNotAllowed,
}

impl AccessReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::AmbientNetwork => "ambient-network",
            Self::TrustedLocal => "trusted-local",
            Self::NetworkAllowList => "network-allowlist",
            Self::NetworkDenied => "network-denied",
            Self::NetworkAllowListEmpty => "network-allowlist-empty",
            Self::HostNotAllowed => "host-not-allowed",
            Self::PortNotAllowed => "port-not-allowed",
            Self::TransportNotAllowed => "transport-not-allowed",
            Self::ResolvedAddressNotAllowed => "resolved-address-not-allowed",
            Self::ExplicitlyDenied => "explicitly-denied",
            Self::ModelAllowed => "model-allowed",
            Self::ModelNotAllowed => "model-not-allowed",
            Self::ProviderNotAllowed => "provider-not-allowed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Created,
    Starting,
    Running,
    Checkpointing,
    Suspended,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    Process,
    Container,
    VirtualMachine,
    Wasm,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationKind {
    HostProcess,
    /// Unprivileged process isolated by a user namespace and host-kernel policy.
    RootlessProcess,
    /// Host process constrained by a platform sandbox such as macOS Seatbelt.
    SandboxedProcess,
    Container,
    VirtualMachine,
    Wasm,
}

/// Strength of the boundary claimed for one capability dimension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementLevel {
    #[default]
    Unenforced,
    Cooperative,
    Enforced,
}

/// Independently enforceable dimensions of a Run policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityDimension {
    Models,
    Tools,
    FilesystemRead,
    FilesystemWrite,
    Network,
    Secrets,
    Subprocess,
    Resources,
}

impl fmt::Display for CapabilityDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Models => "models",
            Self::Tools => "tools",
            Self::FilesystemRead => "filesystem_read",
            Self::FilesystemWrite => "filesystem_write",
            Self::Network => "network",
            Self::Secrets => "secrets",
            Self::Subprocess => "subprocess",
            Self::Resources => "resources",
        };
        f.write_str(name)
    }
}

/// Auditable claim for one dimension. `mechanisms` names the concrete boundary
/// rather than inferring enforcement from an executor or isolation label.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnforcementEvidence {
    #[serde(default)]
    pub level: EnforcementLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mechanisms: Vec<String>,
}

impl EnforcementEvidence {
    pub fn new(level: EnforcementLevel, mechanism: impl Into<String>) -> Self {
        Self {
            level,
            mechanisms: vec![mechanism.into()],
        }
    }

    pub fn is_enforced(&self) -> bool {
        self.level == EnforcementLevel::Enforced
    }
}

/// Observed controls returned by an executor after setup and teardown.
/// Admission plans must never be converted into this type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEnforcementEvidence {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<CapabilityDimension, EnforcementEvidence>,
}

impl CapabilityEnforcementEvidence {
    pub fn record(
        &mut self,
        dimension: CapabilityDimension,
        level: EnforcementLevel,
        mechanism: impl Into<String>,
    ) {
        let mechanism = mechanism.into();
        let evidence = self.dimensions.entry(dimension).or_default();
        if level > evidence.level {
            evidence.level = level;
        }
        if !evidence.mechanisms.contains(&mechanism) {
            evidence.mechanisms.push(mechanism);
        }
    }

    pub fn enforced(
        mut self,
        dimension: CapabilityDimension,
        mechanism: impl Into<String>,
    ) -> Self {
        self.record(dimension, EnforcementLevel::Enforced, mechanism);
        self
    }

    pub fn cooperative(
        mut self,
        dimension: CapabilityDimension,
        mechanism: impl Into<String>,
    ) -> Self {
        self.record(dimension, EnforcementLevel::Cooperative, mechanism);
        self
    }

    pub fn evidence(&self, dimension: CapabilityDimension) -> Option<&EnforcementEvidence> {
        self.dimensions.get(&dimension)
    }

    pub fn is_enforced(&self, dimension: CapabilityDimension) -> bool {
        self.evidence(dimension)
            .is_some_and(EnforcementEvidence::is_enforced)
    }

    pub fn missing_dimensions(
        &self,
        capabilities: &CapabilitySet,
        resources: &ResourceLimits,
    ) -> Vec<CapabilityDimension> {
        requested_enforcement_dimensions(capabilities, resources)
            .into_iter()
            .filter(|dimension| !self.is_enforced(*dimension))
            .collect()
    }
}

/// Expected controls at admission. A plan is never proof of installation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementPlanLevel {
    #[default]
    Unsupported,
    Cooperative,
    Planned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnforcementPlan {
    pub level: EnforcementPlanLevel,
    #[serde(default)]
    pub mechanisms: Vec<String>,
}
impl EnforcementPlan {
    pub fn is_planned(&self) -> bool {
        self.level == EnforcementPlanLevel::Planned
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEnforcementPlan {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<CapabilityDimension, EnforcementPlan>,
}

impl CapabilityEnforcementPlan {
    pub fn record(
        &mut self,
        dimension: CapabilityDimension,
        level: EnforcementPlanLevel,
        mechanism: impl Into<String>,
    ) {
        let mechanism = mechanism.into();
        let evidence = self.dimensions.entry(dimension).or_default();
        if level > evidence.level {
            evidence.level = level;
        }
        if !evidence.mechanisms.contains(&mechanism) {
            evidence.mechanisms.push(mechanism);
        }
    }

    pub fn planned(mut self, dimension: CapabilityDimension, mechanism: impl Into<String>) -> Self {
        self.record(dimension, EnforcementPlanLevel::Planned, mechanism);
        self
    }

    pub fn cooperative(
        mut self,
        dimension: CapabilityDimension,
        mechanism: impl Into<String>,
    ) -> Self {
        self.record(dimension, EnforcementPlanLevel::Cooperative, mechanism);
        self
    }

    pub fn plan(&self, dimension: CapabilityDimension) -> Option<&EnforcementPlan> {
        self.dimensions.get(&dimension)
    }

    pub fn is_planned(&self, dimension: CapabilityDimension) -> bool {
        self.plan(dimension)
            .is_some_and(EnforcementPlan::is_planned)
    }

    pub fn missing_dimensions(
        &self,
        capabilities: &CapabilitySet,
        resources: &ResourceLimits,
    ) -> Vec<CapabilityDimension> {
        requested_enforcement_dimensions(capabilities, resources)
            .into_iter()
            .filter(|dimension| !self.is_planned(*dimension))
            .collect()
    }
}

/// Executor-owned observations carried to the sole authoritative Run Bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorObservations {
    pub origin: crate::trace::Origin,
    pub enforcement: CapabilityEnforcementEvidence,
}
impl Default for ExecutorObservations {
    fn default() -> Self {
        Self {
            origin: crate::trace::Origin::Runtime,
            enforcement: Default::default(),
        }
    }
}

/// Selection identity recorded by the runtime; contains no enforcement claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorIdentity {
    pub name: String,
    pub kind: ExecutorKind,
    pub isolation: IsolationKind,
}
impl From<&ExecutorPlan> for ExecutorIdentity {
    fn from(plan: &ExecutorPlan) -> Self {
        Self {
            name: plan.name.clone(),
            kind: plan.kind,
            isolation: plan.isolation,
        }
    }
}

/// Dimensions that require non-bypassable evidence when `PolicyMode::Enforce`
/// is selected. Network is always included because `Ambient` is explicitly an
/// audit/compatibility mode, not an enforceable boundary.
pub fn requested_enforcement_dimensions(
    capabilities: &CapabilitySet,
    resources: &ResourceLimits,
) -> Vec<CapabilityDimension> {
    use std::collections::BTreeSet;

    let mut requested = BTreeSet::new();
    if !capabilities.models.is_empty() {
        requested.insert(CapabilityDimension::Models);
    }
    if !capabilities.tools.is_empty() {
        requested.insert(CapabilityDimension::Tools);
    }
    for filesystem in &capabilities.filesystem {
        requested.insert(CapabilityDimension::FilesystemRead);
        if filesystem.access == FilesystemAccess::ReadWrite {
            requested.insert(CapabilityDimension::FilesystemWrite);
        }
    }
    // PolicyMode::Enforce must fail closed for `Ambient` too: selecting ambient
    // access is not evidence that direct sockets are confined by a boundary.
    requested.insert(CapabilityDimension::Network);
    if !capabilities.secrets.is_empty() {
        requested.insert(CapabilityDimension::Secrets);
    }
    // `false` is a deny policy, not the absence of a request. Enforce mode must
    // have a non-bypassable mechanism that prevents fork/exec. When subprocesses
    // are explicitly allowed there is no deny boundary to prove.
    if !capabilities.allow_subprocess {
        requested.insert(CapabilityDimension::Subprocess);
    }
    if !resources.is_empty() {
        requested.insert(CapabilityDimension::Resources);
    }
    requested.into_iter().collect()
}

/// Admission-only executor plan. Installed controls are ExecutorObservations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorPlan {
    pub name: String,
    pub kind: ExecutorKind,
    pub isolation: IsolationKind,
    #[serde(default)]
    pub capability_plan: CapabilityEnforcementPlan,
    pub supports_checkpoint: bool,
    pub supports_migration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptInfo {
    pub attempt_id: AttemptId,
    pub number: u32,
    /// Admission-time plan, never runtime enforcement evidence.
    pub executor: ExecutorPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatus {
    pub run_id: RunId,
    pub state: RunState,
    pub attempt: AttemptInfo,
    pub updated_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureKind {
    InvalidSpec,
    Unsupported,
    Spawn,
    ProcessExit,
    Workload,
    DeadlineExceeded,
    Infrastructure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFailure {
    pub kind: RunFailureKind,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub state: RunState,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
    #[serde(default)]
    pub output: ProcessOutput,
    /// Small structured workload result. Large outputs belong in `artifacts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_stream_ref: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub executor_observations: ExecutorObservations,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_run_spec_json_roundtrips() {
        let mut spec = RunSpec::process("run-1", "codex", "codex");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["exec".into(), "review".into()];
        process.stdout = StdioMode::Capture;
        spec.capabilities.network = NetworkCapability::AllowList {
            hosts: vec!["api.openai.com".into()],
            rules: Vec::new(),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let decoded: RunSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.run_id.as_str(), "run-1");
        assert_eq!(decoded.schema_version, RUNTIME_SCHEMA_VERSION);
        assert!(matches!(
            decoded.capabilities.network,
            NetworkCapability::AllowList { .. }
        ));
    }

    #[test]
    fn local_request_and_result_wire_shapes_use_run_and_attempt_identity() {
        let request = serde_json::json!({
            "run_id": "local-run", "attempt_id": "local-attempt", "host": "api.example.com",
            "port": 443, "transport": "https", "resolved_ip": "203.0.113.1"
        });
        let decoded: NetworkAccessRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), request);
        let request = serde_json::json!({
            "run_id": "local-run", "attempt_id": "local-attempt", "call_id": "call-1",
            "client_model": "model", "upstream_model": "model", "provider": "provider",
            "protocol": "openai", "upstream_host": "api.example.com"
        });
        let decoded: ModelCallRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), request);
        let result = serde_json::json!({
            "run_id": "local-run", "attempt_id": "local-attempt", "state": "completed",
            "started_at_unix_ms": 1, "finished_at_unix_ms": 2,
            "output": {"stdout_truncated": false, "stderr_truncated": false},
            "metrics": {}, "artifacts": [], "warnings": [],
            "executor_observations": {"origin": "backend", "enforcement": {"dimensions": {}}}
        });
        let decoded: RunResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), result);
    }

    #[test]
    fn network_policy_roundtrips_deny_and_bandwidth_limits() {
        let capability = NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Deny,
            allow: vec![NetworkAccessRule {
                host: "api.example.com".into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            deny: vec![NetworkAccessRule {
                host: "169.254.0.0/16".into(),
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }],
            limits: vec![NetworkBandwidthLimit {
                host: None,
                port: None,
                bytes_per_second: 1_250_000,
            }],
        };
        let json = serde_json::to_string(&capability).unwrap();
        assert_eq!(
            serde_json::from_str::<NetworkCapability>(&json).unwrap(),
            capability
        );
    }

    #[test]
    fn enforcement_evidence_checks_only_requested_dimensions() {
        let capabilities = CapabilitySet {
            filesystem: vec![FilesystemCapability {
                path: "/workspace".into(),
                access: FilesystemAccess::ReadWrite,
            }],
            network: NetworkCapability::Deny,
            ..CapabilitySet::default()
        };
        let evidence = CapabilityEnforcementEvidence::default()
            .enforced(CapabilityDimension::FilesystemRead, "mount-namespace")
            .enforced(CapabilityDimension::FilesystemWrite, "mount-namespace");

        assert_eq!(
            evidence.missing_dimensions(&capabilities, &ResourceLimits::default()),
            vec![
                CapabilityDimension::Network,
                CapabilityDimension::Subprocess
            ]
        );
    }

    #[test]
    fn subprocess_deny_requires_evidence_but_explicit_allow_does_not() {
        let denied = CapabilitySet::default();
        assert!(
            requested_enforcement_dimensions(&denied, &ResourceLimits::default())
                .contains(&CapabilityDimension::Subprocess)
        );

        let allowed = CapabilitySet {
            allow_subprocess: true,
            ..CapabilitySet::default()
        };
        assert!(
            !requested_enforcement_dimensions(&allowed, &ResourceLimits::default())
                .contains(&CapabilityDimension::Subprocess)
        );
    }

    #[test]
    fn executor_descriptor_cannot_claim_observed_enforcement() {
        let descriptor = ExecutorPlan {
            name: "process".into(),
            kind: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
            capability_plan: CapabilityEnforcementPlan::default()
                .planned(CapabilityDimension::Resources, "posix-rlimit"),
            supports_checkpoint: false,
            supports_migration: false,
        };
        let mut json = serde_json::to_value(descriptor).unwrap();
        assert_eq!(
            json["capability_plan"]["dimensions"]["resources"]["level"],
            "planned"
        );
        json["capability_plan"]["dimensions"]["resources"]["level"] = "enforced".into();
        assert!(serde_json::from_value::<ExecutorPlan>(json).is_err());
    }
}
