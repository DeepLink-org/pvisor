//! Public replay contracts. Import traits explicitly to load requests and execute them.
//!
//! Replay is synchronous and owns its subprocess/bridge teardown. It does not create
//! a sandbox: callers must supply isolation and trusted runtime inputs. State and
//! output must be private and exclusively managed by replay. An unresolved durable
//! start blocks retries; preserve failed state and choose a fresh sandbox/run ID.
//! Native execution currently supports Unix hosts. Unknown agents and unsupported
//! versions return explicit errors; the selected adapter owns continuation support.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Versioned JSON request format accepted by ReplayRequest::from_json.
pub const REQUEST_SCHEMA_VERSION: &str = "sandbox-playback.request/v1";
/// Result format written to replay output and returned to callers.
pub const RESULT_SCHEMA_VERSION: &str = "sandbox-playback.result/v3";

/// Replay TOML input. Unknown keys are rejected; omitted runtime sections use defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayToml {
    /// Replay selection and continuation settings.
    pub replay: ReplayConfig,
    #[serde(default)]
    /// Outer runtime settings consumed by the frontend, not installed by replay.
    pub run: RunConfig,
    #[serde(default)]
    /// Outer filesystem settings consumed by the frontend.
    pub overlayfs: OverlayFsConfig,
    #[serde(default)]
    /// Outer network settings consumed by the frontend.
    pub overlaynet: OverlayNetConfig,
}

/// Replay selection parsed from TOML; conversion checks agent names and mutually exclusive modes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    /// Exact supported agent name.
    pub agent: String,
    /// Recorded trajectory file; existence is checked before execution.
    pub trajectory: PathBuf,
    /// Positive complete tool-batch ordinal; validated before execution.
    pub after_step: usize,
    /// Optional version-pinned executable; validated by the selected adapter.
    pub agent_entrypoint: Option<PathBuf>,
    /// Optional directory containing the adapter runtime manifest.
    pub agent_runtime: Option<PathBuf>,
    #[serde(default)]
    /// Claude-only tool names; execution rejects invalid or duplicate names.
    pub disallowed_tools: Vec<String>,
    /// Optional read-only trajectory asset root.
    pub trajectory_assets: Option<PathBuf>,
    /// Positive total model-turn budget including the replay prefix, when supplied.
    pub max_steps: Option<usize>,
    /// Optional routing key; not the agent continuation identity.
    pub session_id: Option<String>,
    #[serde(default)]
    /// Execute the prefix without live continuation; conflicts with prepare_only.
    pub replay_only: bool,
    #[serde(default)]
    /// Prepare artifacts without executing tools or an agent; conflicts with replay_only.
    pub prepare_only: bool,
    #[serde(default)]
    /// Permit unavailable fresh observations and report degraded quality.
    pub allow_stale_observations: bool,
    #[serde(default)]
    /// Disable thinking in supported continuation requests.
    pub disable_thinking: bool,
    /// Optional user message inserted at the live boundary; empty text is ignored,
    /// whitespace-only text is rejected at execution.
    pub boundary_user_prompt: Option<String>,
    /// Optional ASCII alphanumeric, dash or underscore run ID; otherwise generated.
    pub run_id: Option<String>,
    /// Existing workspace, defaulting to the caller-supplied cwd during conversion.
    pub workspace: Option<PathBuf>,
    /// State root; defaults to /tmp/pvisor-sandbox-replay/state.
    pub state_dir: Option<PathBuf>,
    /// Output root; defaults to /tmp/pvisor-sandbox-replay/output.
    pub output_dir: Option<PathBuf>,
}

/// Outer execution settings. Defaults select no explicit executor, timeout or policy and inherit no environment.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    #[serde(default)]
    /// Request outer runtime safe admission; false by default.
    pub safe: bool,
    /// Optional outer executor name, validated by the frontend.
    pub executor: Option<String>,
    /// Optional outer workload timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Optional outer policy mode, validated by the frontend.
    pub policy: Option<String>,
    #[serde(default)]
    /// Inherit the host environment when true; false by default.
    pub inherit_env: bool,
    #[serde(default)]
    /// Explicit host environment variable names forwarded by the frontend.
    pub pass_env: Vec<String>,
}

/// Outer filesystem configuration; defaults contain no paths or commit override.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayFsConfig {
    /// Optional absolute path visible to the replay agent.
    pub path: Option<PathBuf>,
    #[serde(default)]
    /// Host layers in declaration order; empty by default.
    pub compose: Vec<PathBuf>,
    /// Optional outer apply/drop/manual selection, validated by the frontend.
    pub commit: Option<String>,
}

/// Outer network configuration; defaults contain no overrides.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayNetConfig {
    /// Optional outer network mode, validated by the frontend.
    pub mode: Option<String>,
    /// Optional outer network policy, validated by the frontend.
    pub policy: Option<String>,
}

/// Supported version-pinned trajectory and continuation adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Claude Code.
    ClaudeCode,
    /// Codex.
    Codex,
    /// Mini-SWE-agent.
    MiniSweAgent,
    /// OpenHands.
    Openhands,
    /// OpenCode.
    Opencode,
    /// Pi agent.
    PiAgent,
    /// SWE-agent.
    SweAgent,
}

/// Replay side-effect selection; all modes validate the selected trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    /// Publish preparation artifacts without tool or agent execution.
    PrepareOnly,
    /// Execute the selected prefix without starting live continuation.
    ReplayOnly,
    /// Execute the prefix, then start live continuation within the total step budget.
    ReplayAndContinue,
}

/// Owned execution request. Loading does not install isolation; the caller supplies a fresh sandbox.
/// Execution validates paths, IDs, positive batch/budget values and adapter-specific settings before tool dispatch.
#[derive(Debug, Clone)]
pub struct PlaybackRequest {
    /// Selected version-pinned adapter.
    pub agent: AgentKind,
    /// Recorded trajectory file; existence is checked before execution.
    pub trajectory: PathBuf,
    /// Positive complete tool-batch ordinal; validated before execution.
    pub after_step: usize,
    /// Existing workspace; relative paths use the process cwd.
    pub workspace: PathBuf,
    /// State root; execution appends run_id and holds an exclusive lock.
    pub state_dir: PathBuf,
    /// Output root; execution creates a fresh run_id child and refuses reuse.
    pub output_dir: PathBuf,
    /// Optional version-pinned executable; validated by the selected adapter.
    pub agent_entrypoint: Option<PathBuf>,
    /// Optional directory containing the adapter runtime manifest.
    pub agent_runtime: Option<PathBuf>,
    /// Claude-only tool names; execution rejects invalid or duplicate names.
    pub disallowed_tools: Vec<String>,
    /// Optional read-only trajectory asset root.
    pub trajectory_assets: Option<PathBuf>,
    /// Optional routing key; not the agent continuation identity.
    pub session_id: Option<String>,
    /// Positive total model-turn budget including the replay prefix, when supplied.
    pub max_steps: Option<usize>,
    /// Preparation, replay-only or replay-and-continue selection.
    pub mode: ReplayMode,
    /// Permit unavailable fresh observations and report degraded quality.
    pub allow_stale_observations: bool,
    /// Optional ASCII alphanumeric, dash or underscore run ID; otherwise generated.
    pub run_id: Option<String>,
    /// Disable thinking in supported continuation requests.
    pub disable_thinking: bool,
    /// Optional user message inserted at the live boundary; empty text is ignored,
    /// whitespace-only text is rejected at execution.
    pub boundary_user_prompt: Option<String>,
}

/// Published replay artifact reference; paths refer to retained output files.
#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    /// Artifact purpose in the result manifest.
    pub role: String,
    /// Artifact serialization or media format.
    pub format: String,
    /// Published artifact path.
    pub path: PathBuf,
}

/// Observed adapter launch identity retained in a result.
#[derive(Debug, Clone, Serialize)]
pub struct AgentResult {
    #[serde(rename = "type")]
    /// Canonical agent name serialized as type.
    pub kind: String,
    /// Selected adapter version.
    pub version: String,
    /// Resolved executable when continuation launch was prepared.
    pub entrypoint: Option<PathBuf>,
    /// How the executable was selected, or prepare_only.
    pub launch_source: String,
    /// Tool restrictions requested for the agent.
    pub disallowed_tools: Vec<String>,
}

/// Furthest replay phase reported in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPhase {
    /// Preparation completed.
    Prepared,
    /// The selected tool prefix completed.
    Replayed,
    /// Live continuation was attempted; inspect agent_status for its outcome.
    Continued,
}

/// Freshness of observations used for replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayQuality {
    /// Fresh observations satisfied the adapter contract.
    Verified,
    /// Replay used allowed stale observations.
    Degraded,
}

/// Live agent outcome, separate from replay phase and process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent completed its continuation.
    Completed,
    /// Agent reached the configured step limit.
    MaxSteps,
    /// Agent continuation failed.
    Failed,
    /// No live agent was started.
    NotStarted,
}

/// Failure retained in a finalized result.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayFailure {
    /// Stable machine-readable error category.
    pub category: String,
    /// Human-readable diagnostic; not an authorization signal.
    pub message: String,
}

/// Owned result using sandbox-playback.result/v3; failures may retain partial artifacts and effects.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    /// Result wire schema identifier.
    pub schema_version: &'static str,
    /// Furthest replay phase.
    pub phase: ReplayPhase,
    /// Observation freshness classification.
    pub quality: ReplayQuality,
    /// Live agent outcome.
    pub agent_status: AgentStatus,
    /// Execution identity shared with state and output directories.
    pub run_id: String,
    /// Adapter launch identity.
    pub agent: AgentResult,
    /// Requested complete batch boundary.
    pub after_step: usize,
    /// Durably finished tool operations, including repeated native IDs.
    pub replayed_tool_calls: usize,
    /// Model turns in the selected recorded prefix.
    pub prefix_model_turns: usize,
    /// Observed live continuation steps.
    pub continued_steps: usize,
    /// Retained execution state directory.
    pub state_dir: PathBuf,
    /// Retained execution output directory.
    pub output_dir: PathBuf,
    /// Published artifact references.
    pub artifacts: Vec<Artifact>,
    /// Finalized failure, when present.
    pub failure: Option<ReplayFailure>,
    /// Model-error classification only; does not authorize repeating uncertain side effects.
    pub retryable: bool,
    /// Adapter-specific diagnostics; not execution authority.
    pub metadata: Value,
}

/// Finalized result and CLI exit classification; a returned report may describe failure.
#[derive(Debug)]
pub struct ExecutionReport {
    /// Owned result; inspect failure and agent_status.
    pub result: ReplayResult,
    /// Suggested process exit code; zero only for successful finalization.
    pub exit_code: i32,
}

/// Stable failure category used in diagnostics and exit classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayErrorKind {
    /// Invalid request or configuration input.
    Configuration,
    /// Invalid or unsupported trajectory contents.
    Trajectory,
    /// Unknown agent adapter.
    UnsupportedAgent,
    /// Unsupported agent version.
    UnsupportedVersion,
    /// Invalid or inaccessible workspace.
    Workspace,
    /// Tool execution or artifact I/O failure.
    Executor,
    /// A durable start has no validated finish; repeating effects is unsafe.
    AmbiguousExecution,
    /// Agent continuation failed.
    Continuation,
    /// Model rate-limit response.
    ModelRateLimit,
    /// Model server failure.
    ModelInternal,
    /// Model connection failure.
    ModelConnection,
    /// Internal replay failure.
    Internal,
}

/// Failure before or during replay. Retained locations are diagnostic hints, not completion receipts.
#[derive(Debug)]
pub struct ReplayError {
    /// Stable error category.
    pub kind: ReplayErrorKind,
    /// Human-readable diagnostic.
    pub message: String,
    /// Optional retained run locations; files may be incomplete.
    pub locations: Option<ReplayLocations>,
}

/// Owned diagnostic locations retained after failure.
#[derive(Debug, Clone)]
pub struct ReplayLocations {
    /// Attempted run identity.
    pub run_id: String,
    /// Attempted state directory.
    pub state_dir: PathBuf,
    /// Attempted output directory.
    pub output_dir: PathBuf,
}

/// Names and version/profile identifiers for a selected adapter; no I/O.
pub trait AgentProfile {
    /// Canonical agent name used in configuration and wire records.
    fn as_str(&self) -> &'static str;
    /// Exact adapter-supported version; execution rejects mismatched launches.
    fn supported_version(self) -> &'static str;
    /// Versioned adapter protocol identifier.
    fn profile(self) -> &'static str;
}

/// TOML loading and owned request conversion. Neither method starts a workload.
///
/// Loading and conversion preserve caller ownership of runtime and filesystem setup:
/// ```
/// use pvisor_replay::api::{ReplayConfiguration, ReplayMode, ReplayToml};
/// let root = tempfile::tempdir()?;
/// let config = root.path().join("replay.toml");
/// std::fs::write(&config, "[replay]\nagent='codex'\ntrajectory='history.jsonl'\nafter_step=1\nprepare_only=true\n")?;
/// let request = ReplayToml::from_file(&config)?.into_request(root.path())?;
/// assert_eq!(request.workspace, root.path());
/// assert_eq!(request.mode, ReplayMode::PrepareOnly);
/// assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
/// std::fs::write(&config, "[replay]\nagent='codex'\ntrajectory='history.jsonl'\nafter_step=1\nprepare_only=true\nreplay_only=true\n")?;
/// assert!(ReplayToml::from_file(&config)?.into_request(root.path()).is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait ReplayConfiguration: Sized {
    /// Read UTF-8 TOML; I/O, parse and unknown-field failures return Configuration.
    /// Reads only the supplied file; does not create state or output directories.
    fn from_file(path: &Path) -> Result<Self, ReplayError>;
    /// Consume configuration, using cwd only as the default workspace. Reject unknown
    /// agents and conflicting modes; filesystem/budget validation occurs at execution.
    /// Outer run/overlay fields remain frontend-owned and are not installed here.
    fn into_request(self, cwd: &Path) -> Result<PlaybackRequest, ReplayError>;
}

/// Versioned request loading and blocking execution, including artifacts and receipts.
///
/// Prepare a recorded Codex tool batch using only private local files. No SDK,
/// shell, model or VM is required; preparation must not execute its tool:
/// ```
/// use pvisor_replay::api::{PlaybackRequest, ReplayPhase, ReplayRequest, REQUEST_SCHEMA_VERSION};
/// use serde_json::json;
/// let root = tempfile::tempdir()?;
/// let workspace = root.path().join("workspace");
/// std::fs::create_dir(&workspace)?;
/// let trajectory = root.path().join("history.jsonl");
/// let events = [
///     json!({"type":"session_meta","payload":{"id":"docs"}}),
///     json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"prepare"}]}}),
///     json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"tool"}]}}),
///     json!({"type":"response_item","payload":{"type":"function_call","call_id":"one","name":"exec_command","arguments":json!({"cmd":"touch must-not-exist"}).to_string()}}),
///     json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"one","output":"old observation"}}),
/// ];
/// std::fs::write(&trajectory, events.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))?;
/// let path = root.path().join("request.json");
/// let mut input = json!({"schema_version":"unsupported", "agent":{"type":"codex"},
///     "trajectory":trajectory, "after_step":1, "workspace":workspace,
///     "state_dir":root.path().join("state"), "output_dir":root.path().join("output"),
///     "prepare_only":true, "run_id":"docs"});
/// std::fs::write(&path, serde_json::to_vec(&input)?)?;
/// assert!(PlaybackRequest::from_json(&path).is_err());
/// assert!(!root.path().join("state").exists());
/// input["schema_version"] = json!(REQUEST_SCHEMA_VERSION);
/// std::fs::write(&path, serde_json::to_vec(&input)?)?;
/// let report = PlaybackRequest::from_json(&path)?.execute()?;
/// assert_eq!(report.exit_code, 0);
/// assert_eq!(report.result.phase, ReplayPhase::Prepared);
/// assert_eq!(report.result.replayed_tool_calls, 0);
/// let manifest = report.result.output_dir.join("manifest.json");
/// let published = std::fs::read(&manifest)?;
/// assert!(!workspace.join("must-not-exist").exists());
/// // An existing output child is never overwritten by another execution.
/// assert!(PlaybackRequest::from_json(&path)?.execute().is_err());
/// assert_eq!(std::fs::read(&manifest)?, published);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait ReplayRequest: Sized {
    /// Read JSON, rejecting unknown fields, wrong schema, unknown agents or conflicting
    /// modes. No workload starts and no state/output directories are created.
    /// Paths are preserved; existence and execution constraints are checked by execute.
    fn from_json(path: &Path) -> Result<Self, ReplayError>;
    /// Consume the request. Validate inputs and the selected prefix, hold the exclusive
    /// state lock, and create a fresh output child before dispatch. PrepareOnly still
    /// writes artifacts. Other modes can execute tools and remote model requests.
    /// Concurrent use of one state directory is refused. Errors retain effects and
    /// diagnostics; a missing finish receipt blocks retry, even after process failure.
    /// A report may contain a finalized failure; inspect its result and exit_code.
    /// Process/bridge cleanup is bounded and failures remain errors. Platform or
    /// adapter limitations return errors; callers must provide their own sandbox.
    fn execute(self) -> Result<ExecutionReport, ReplayError>;
}

/// Pure error classification; retryability never grants replay admission.
pub trait ReplayErrorClassification {
    /// Stable machine-readable category string.
    fn category(self) -> &'static str;
    /// CLI code: configuration 2, trajectory 10, unsupported 11, workspace 20,
    /// executor 30, ambiguous 31, continuation/model 40, internal 50.
    fn exit_code(self) -> i32;
    /// True only for model rate-limit, internal-server or connection failures.
    /// Retained execution receipts independently determine whether retry is safe.
    fn retryable(self) -> bool;
}
