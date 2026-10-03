//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Callers configure a [`PVisor`] and invoke [`PVisor::run`]. CLI and other
//! embedders talk to this API directly; there is no separate control-plane process.

#[cfg(feature = "gateway")]
use crate::GatewayDriverConfig;
#[cfg(feature = "gateway")]
use crate::TrajectoryEventSink;
use crate::config::{NetworkDriverConfig, PVisorConfig};
use crate::executor::RunExecutor;
use crate::executor::process::ProcessExecutor;
use crate::runtime::event::{EventSink, NoopEventSink, RunEventPublisher};
use crate::runtime::{
    ImplantPlan, OverlayHint, RuntimeCapabilities, RuntimeSupervisor, RuntimeSupervisorBuilder,
};

use pvisor_core::ControlController;
use pvisor_core::event::Event;
#[cfg(test)]
use pvisor_core::event::Receipt;
use pvisor_core::{
    AttemptId, CapabilityDimension, CapabilityEnforcementPlan, EnforcementPlanLevel, ExecutorPlan,
    IsolationKind, NetworkCapability, PolicyMode, RUNTIME_SCHEMA_VERSION, RunInvocation, RunResult,
    RunSpec, RunStatus,
};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum PVisorError {
    #[error("invalid RunSpec: {0}")]
    InvalidSpec(String),
    #[error("runtime prepare failed: {0}")]
    Prepare(#[source] anyhow::Error),
    #[error("AgentCtl setup failed: {0}")]
    AgentCtl(#[source] anyhow::Error),
    #[error("no executor supports this invocation")]
    UnsupportedInvocation,
    #[error(
        "executor `{executor}` lacks enforced evidence for requested capability dimensions: {dimensions}"
    )]
    UnsupportedPolicy {
        executor: String,
        dimensions: String,
    },
    #[error("event sink rejected run creation: {0}")]
    EventSink(#[source] anyhow::Error),
    #[error("run task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Run-filtered post-commit events, including its embedded Gateway observations.
pub struct RunEventStream {
    pub(crate) trace_id: String,
    pub(crate) receiver: broadcast::Receiver<Event>,
}
impl RunEventStream {
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        loop {
            let event = self.receiver.recv().await?;
            if event.trace_id == self.trace_id {
                return Ok(event);
            }
        }
    }
    pub fn try_recv(&mut self) -> Result<Event, broadcast::error::TryRecvError> {
        loop {
            let event = self.receiver.try_recv()?;
            if event.trace_id == self.trace_id {
                return Ok(event);
            }
        }
    }
}

/// Cloneable, provider-independent cancellation capability for an in-flight Run.
#[derive(Clone)]
pub struct RunCancellation {
    token: CancellationToken,
}

impl RunCancellation {
    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Handle for one in-flight Run: status, cancel, wait, event subscribe.
pub struct RunHandle {
    pub(crate) run_id: pvisor_core::RunId,
    pub(crate) attempt_id: AttemptId,
    pub(crate) status: watch::Receiver<RunStatus>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) vm_control: crate::executor::vm::control::VmControl,
    pub(crate) vm_status: watch::Sender<RunStatus>,
    pub(crate) control_operation: pvisor_core::operation::Operation,
    pub(crate) events: RunEventPublisher,
    pub(crate) agentctl: crate::AgentCtlControl,
    pub(crate) checkpoint_record: Option<crate::runtime::RunRecord>,
    pub(crate) join: JoinHandle<RunResult>,
}

/// Cloneable VM controls bound to one live Run attempt. Native controls still
/// require executor support, a live state, and the Run's cancellation authority.
#[derive(Clone)]
pub struct RunControlHandle {
    status: watch::Receiver<RunStatus>,
    vm_control: crate::executor::vm::control::VmControl,
    vm_status: watch::Sender<RunStatus>,
    control_operation: pvisor_core::operation::Operation,
    events: RunEventPublisher,
    cancellation: CancellationToken,
}

impl RunControlHandle {
    /// Wait through asynchronous attempt startup without issuing a primitive
    /// before the native control endpoint is usable. Cancellation still fences it.
    pub async fn wait_ready(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.status.borrow().attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
            "VM controls require a VM executor"
        );
        loop {
            let state = self.status.borrow().state;
            anyhow::ensure!(
                !self.cancellation.is_cancelled()
                    && !state.is_terminal()
                    && state != pvisor_core::RunState::Cancelling,
                "attempt ended before control readiness"
            );
            if matches!(
                state,
                pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
            ) {
                return Ok(());
            }
            tokio::select! {
                _ = self.cancellation.cancelled() => {
                    anyhow::bail!("attempt ended before control readiness");
                }
                changed = self.status.changed() => {
                    changed.map_err(|_| anyhow::anyhow!("attempt status closed before control readiness"))?;
                }
            }
        }
    }

    pub async fn control(
        &self,
        kind: pvisor_core::operation::OperationKind,
    ) -> anyhow::Result<pvisor_core::operation::Value> {
        use pvisor_core::operation::{OperationKind, Value, VmState};
        anyhow::ensure!(
            self.status.borrow().attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
            "pause/resume/offload require a VM executor"
        );
        kind.validate()?;
        anyhow::ensure!(
            !matches!(kind, OperationKind::RunExecute { .. }),
            "run.execute requires PVisor::run"
        );
        let mut operation = self.control_operation.clone();
        operation.kind = kind.clone();
        operation.rules.clear();
        operation
            .placements
            .retain(|placement| matches!(placement, pvisor_core::operation::Placement::Vm { .. }));
        operation.validate()?;
        let control = self.vm_control.clone();
        let events = self.events.clone();
        let status = self.vm_status.clone();
        let cancellation = self.cancellation.clone();
        tokio::spawn(async move {
            let _transition = control.transition.lock().await;
            anyhow::ensure!(!cancellation.is_cancelled(), "attempt is cancelling");
            anyhow::ensure!(
                matches!(
                    status.borrow().state,
                    pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
                ),
                "attempt is not running or suspended"
            );
            events
                .publish(
                    "vm.control_requested",
                    "runtime",
                    serde_json::json!({"operation": operation}),
                )
                .await?;
            match control.command(kind).await {
                Ok(reply) => {
                    let state = reply
                        .state
                        .ok_or_else(|| anyhow::anyhow!("VM control returned no state"))?;
                    let value = Value::Vm {
                        state,
                        memory: reply.memory,
                    };
                    status.send_modify(|status| {
                        if matches!(
                            status.state,
                            pvisor_core::RunState::Running | pvisor_core::RunState::Suspended
                        ) {
                            status.state = if state == VmState::Running {
                                pvisor_core::RunState::Running
                            } else {
                                pvisor_core::RunState::Suspended
                            };
                            status.updated_at_unix_ms = crate::unix_now_ms();
                            status.message = Some(format!("VM {state:?}"));
                        }
                    });
                    events
                        .publish(
                            "vm.control_completed",
                            "runtime",
                            serde_json::json!({"operation": operation, "value": value}),
                        )
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!("VM control completed but observation failed: {error}")
                        })?;
                    Ok(value)
                }
                Err(error) => {
                    let _ = events
                        .publish(
                            "vm.control_failed",
                            "runtime",
                            serde_json::json!({"operation": operation, "error": error.to_string()}),
                        )
                        .await;
                    Err(error)
                }
            }
        })
        .await?
    }
}

impl RunHandle {
    /// Pause every VM vCPU and wait for acknowledgement. Device I/O remains
    /// active and the Run's wall-time deadline continues. Other executors reject
    /// this operation. An uncertain transition cancels the attempt.
    pub async fn pause_vm(&self) -> anyhow::Result<()> {
        self.pause().await
    }

    /// Resume the paused VM and wait for acknowledgement. Repeated calls are
    /// idempotent while the VM remains available.
    pub async fn resume_vm(&self) -> anyhow::Result<()> {
        self.resume().await
    }

    pub async fn pause(&self) -> anyhow::Result<()> {
        self.control(pvisor_core::operation::OperationKind::RunPause)
            .await
            .map(|_| ())
    }

    pub async fn resume(&self) -> anyhow::Result<()> {
        self.control(pvisor_core::operation::OperationKind::RunResume)
            .await
            .map(|_| ())
    }

    /// Pause and reclaim live file-backed RAM. A new destination must be on the
    /// same filesystem as the current backing. None retains the existing file.
    pub async fn offload(
        &self,
        file: Option<std::path::PathBuf>,
    ) -> anyhow::Result<pvisor_core::operation::VmMemory> {
        let value = self
            .control(pvisor_core::operation::OperationKind::RunOffload { file })
            .await?;
        let pvisor_core::operation::Value::Vm {
            memory: Some(memory),
            ..
        } = value
        else {
            anyhow::bail!("offload returned no RAM report")
        };
        Ok(memory)
    }

    /// Execute an attempt-scoped control primitive using this handle's authority.
    /// Commands and observations remain intact if the caller stops waiting.
    /// Clone the attempt-scoped VM control authority while another task waits
    /// for Run completion. It shares cancellation and native transition ordering.
    pub fn controls(&self) -> RunControlHandle {
        RunControlHandle {
            status: self.status.clone(),
            vm_control: self.vm_control.clone(),
            vm_status: self.vm_status.clone(),
            control_operation: self.control_operation.clone(),
            events: self.events.clone(),
            cancellation: self.cancellation.clone(),
        }
    }

    pub async fn control(
        &self,
        kind: pvisor_core::operation::OperationKind,
    ) -> anyhow::Result<pvisor_core::operation::Value> {
        self.controls().control(kind).await
    }

    pub fn run_id(&self) -> &pvisor_core::RunId {
        &self.run_id
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn status(&self) -> RunStatus {
        self.status.borrow().clone()
    }

    pub async fn status_changed(&mut self) -> Option<RunStatus> {
        self.status.changed().await.ok()?;
        Some(self.status())
    }

    pub fn subscribe_events(&self) -> RunEventStream {
        self.events.subscribe()
    }

    /// Run-scoped cooperative AgentCtl desired-state and observation surface.
    pub fn agentctl(&self) -> crate::AgentCtlControl {
        self.agentctl.clone()
    }

    /// Cooperatively quiesce every connected AgentCtl client and snapshot the upper.
    pub async fn checkpoint(
        &self,
        checkpoint_id: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<crate::LogicalCheckpoint> {
        super::checkpoint::validate_checkpoint_id(checkpoint_id)?;
        let record = self
            .checkpoint_record
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Run {} has no OverlayFS stage", self.run_id))?;
        let deadline = crate::unix_now_ms().saturating_add(timeout.as_millis() as u64);
        let checkpoint = self
            .agentctl
            .begin_checkpoint(checkpoint_id.to_owned(), Some(deadline))?;
        loop {
            if let Some(captured) = checkpoint.try_capture(|| {
                crate::runtime::checkpoint::create_agent_quiesced_checkpoint(record, checkpoint_id)
            })? {
                return Ok(captured);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Cooperative cancel followed by executor-specific termination.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn cancellation(&self) -> RunCancellation {
        RunCancellation {
            token: self.cancellation.clone(),
        }
    }

    pub async fn wait(self) -> Result<RunResult, PVisorError> {
        Ok(self.join.await?)
    }
}

/// Builder for a configured [`PVisor`].
#[derive(Clone, Default)]
pub struct PVisorBuilder {
    runtime: RuntimeSupervisorBuilder,
    event_sink: Option<Arc<dyn EventSink>>,
    executors: Option<Vec<Arc<dyn RunExecutor>>>,
}

impl std::fmt::Debug for PVisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PVisorBuilder")
            .field("runtime", &self.runtime)
            .field(
                "event_sink",
                &self.event_sink.as_ref().map(|_| "<EventSink>"),
            )
            .field("executors", &self.executors.as_ref().map(|e| e.len()))
            .finish()
    }
}

impl PVisorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the top-level pVisor configuration.
    pub fn config(mut self, config: PVisorConfig) -> Self {
        #[cfg(feature = "gateway")]
        if let Some(gateway) = config.gateway {
            self.runtime = self.runtime.gateway(gateway);
        }
        self.runtime = self.runtime.overlay(config.overlay);
        self.runtime = self.runtime.network(config.network);
        self
    }

    /// Enable pVisor's built-in Agent protocol Gateway driver.
    #[cfg(feature = "gateway")]
    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.runtime = self.runtime.gateway(gateway);
        self
    }

    /// Configure the Attempt network policy and interception-driver selection.
    pub fn network(mut self, network: NetworkDriverConfig) -> Self {
        self.runtime = self.runtime.network(network);
        self
    }

    /// Inject the structured trajectory output port used by the Gateway driver.
    #[cfg(feature = "gateway")]
    pub fn trajectory_sink(mut self, sink: Arc<dyn TrajectoryEventSink>) -> Self {
        self.runtime = self.runtime.trajectory_sink(sink);
        self
    }

    pub fn overlay(mut self, overlay: OverlayHint) -> Self {
        self.runtime = self.runtime.overlay(overlay);
        self
    }

    /// Set durable Run storage independently of the optional Gateway.
    pub fn storage(mut self, storage: impl Into<std::path::PathBuf>) -> Self {
        self.runtime = self.runtime.storage(storage.into());
        self
    }

    pub fn control_controller(mut self, controller: Arc<dyn ControlController>) -> Self {
        self.runtime = self.runtime.control_controller(controller);
        self
    }

    pub fn event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn executors(mut self, executors: Vec<Arc<dyn RunExecutor>>) -> Self {
        self.executors = Some(executors);
        self
    }

    pub fn build(self) -> PVisor {
        let (event_sink, runtime) = match self.event_sink {
            Some(sink) => (sink, self.runtime),
            None => {
                let sink = Arc::new(NoopEventSink::default());
                #[cfg(feature = "gateway")]
                let runtime = self.runtime.live_events(Arc::clone(&sink));
                #[cfg(not(feature = "gateway"))]
                let runtime = self.runtime;
                (sink as Arc<dyn EventSink>, runtime)
            }
        };
        #[cfg(feature = "gateway")]
        let runtime = match event_sink.journal() {
            Some(journal) => runtime.journal(journal),
            None => runtime,
        };
        PVisor {
            executors: Arc::new(self.executors.unwrap_or_else(|| {
                vec![Arc::new(ProcessExecutor::default()) as Arc<dyn RunExecutor>]
            })),
            event_sink,
            runtime: runtime.build(),
        }
    }
}

/// Portable Agent execution runtime.
///
/// Owns Attempt prepare (capture / network / overlay), process execution, and
/// Run lifecycle. Hosts call [`Self::run`] directly — no forwarding control plane.
#[derive(Clone)]
pub struct PVisor {
    executors: Arc<Vec<Arc<dyn RunExecutor>>>,
    event_sink: Arc<dyn EventSink>,
    runtime: RuntimeSupervisor,
}

/// Typed preparation inputs resolved before any runtime side effects.
/// Legacy caller metadata is decoded once at admission, never reinterpreted by drivers.
#[derive(Clone)]
pub(crate) struct PreparedRun {
    pub executor: ExecutorPlan,
    pub operation: pvisor_core::operation::Operation,
    pub lineage: Option<crate::runtime::RunLineage>,
    pub environment: crate::runtime::EnvironmentProjection,
    pub workspace: Option<std::path::PathBuf>,
}

impl PreparedRun {
    pub(crate) fn new(
        spec: &RunSpec,
        executor: ExecutorPlan,
        operation: pvisor_core::operation::Operation,
    ) -> anyhow::Result<Self> {
        let lineage = spec
            .metadata
            .get("pvisor.lineage")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?;
        let RunInvocation::Process(process) = &spec.invocation;
        let environment = match spec.metadata.get("pvisor.environment") {
            Some(value) => serde_json::from_value(value.clone())?,
            None => crate::runtime::EnvironmentProjection {
                inherits_host: process.inherit_env,
                projected_keys: process.env.keys().cloned().collect(),
                runtime_injected_keys: Vec::new(),
            },
        };
        let workspace = spec
            .metadata
            .get("pvisor.workspace")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?;
        Ok(Self {
            executor,
            operation,
            lineage,
            environment,
            workspace,
        })
    }

    pub(crate) fn is_krun(&self) -> bool {
        self.executor.name.starts_with("libkrun-")
    }
}

pub(crate) struct ResolvedRun {
    pub(crate) preparation: PreparedRun,
    pub(crate) spec: RunSpec,
    pub(crate) executor: Arc<dyn RunExecutor>,
    pub(crate) descriptor: ExecutorPlan,
    pub(crate) vm_network_executor: bool,
    pub(crate) operation: pvisor_core::operation::Operation,
    pub(crate) requested_operation: pvisor_core::operation::Operation,
    pub(crate) network_policy: pvisor_core::NetworkPolicy,
}

impl Default for PVisor {
    fn default() -> Self {
        Self::new()
    }
}

impl PVisor {
    pub fn new() -> Self {
        Self::builder().build()
    }

    pub fn builder() -> PVisorBuilder {
        PVisorBuilder::new()
    }

    pub fn capabilities(&self) -> RuntimeCapabilities {
        self.runtime.capabilities()
    }

    /// Dry-run implant plan (env / network markers) without starting capture.
    pub fn plan_for(&self, spec: &RunSpec) -> ImplantPlan {
        self.runtime.plan_for(spec)
    }

    /// Resolve the same Operation used by execution, without starting
    /// an Attempt or mounting filesystems.
    pub fn resolve_operation(
        &self,
        spec: RunSpec,
    ) -> Result<pvisor_core::operation::Operation, PVisorError> {
        Ok(self.resolve_run(spec)?.operation)
    }

    pub(crate) fn resolve_run(&self, mut spec: RunSpec) -> Result<ResolvedRun, PVisorError> {
        validate_spec(&spec)?;
        spec.metadata.remove("pvisor.executor");
        spec.metadata.remove("pvisor.operation");
        let requested_spec = spec.clone();
        let executor = self
            .executors
            .iter()
            .find(|executor| executor.supports(&spec.invocation))
            .cloned()
            .ok_or(PVisorError::UnsupportedInvocation)?;
        let mut descriptor = executor.descriptor();
        crate::runtime::apply_process_policies(&mut spec, &descriptor)
            .map_err(PVisorError::Prepare)?;
        let vm_executor = descriptor.kind == pvisor_core::ExecutorKind::VirtualMachine;
        let vm_network_executor = vm_executor && executor.supports_vm_network_attachment();
        if self.runtime.vm_network_is_requested()
            && descriptor.isolation == pvisor_core::IsolationKind::VirtualMachine
            && !vm_network_executor
        {
            return Err(PVisorError::InvalidSpec(format!(
                "executor `{}` reports virtual-machine isolation but does not support pVisor VM network attachments",
                descriptor.name
            )));
        }
        let has_file_policy = spec
            .policies
            .scopes()
            .iter()
            .any(|(_, layer)| layer.filesystem.is_some());
        let overlay = self.runtime.overlay_hint();
        if has_file_policy
            && overlay.lower_dirs.is_empty()
            && overlay.stage_dir.is_none()
            && overlay.upper_dir.is_none()
            && overlay.merged_dir.is_none()
        {
            return Err(PVisorError::InvalidSpec(
                "Session file policy requires a configured OverlayFS view".into(),
            ));
        }
        if spec
            .policies
            .scopes()
            .iter()
            .any(|(_, layer)| layer.network.is_some())
            && !vm_executor
            && !self.runtime.proxy_network_is_configured()
        {
            return Err(PVisorError::InvalidSpec(
                "Session network policy requires a configured network driver".into(),
            ));
        }
        self.runtime.apply_network_capability(&mut spec);
        spec.capabilities.network = spec.policies.network(spec.capabilities.network.clone());
        let network_policy = pvisor_core::NetworkPolicy::compile(&pvisor_core::NetworkConfig {
            capability: Some(spec.capabilities.network.clone()),
            ..Default::default()
        })
        .map_err(PVisorError::Prepare)?;
        // Runtime preparation supplies this capability from the bound listener.
        spec.metadata
            .remove(crate::executor::sandbox::SANDBOX_PROXY_KEY);
        let capability_plan = effective_capability_plan(
            &descriptor,
            &spec,
            self.runtime.proxy_network_is_configured(),
            vm_network_executor && self.runtime.vm_network_is_enforcing(),
        );
        if crate::executor::sandbox::sandbox_required(&spec) {
            for dimension in [
                CapabilityDimension::FilesystemRead,
                CapabilityDimension::FilesystemWrite,
                CapabilityDimension::Network,
            ] {
                let cooperative_linux_proxy = cfg!(target_os = "linux")
                    && dimension == CapabilityDimension::Network
                    && self.runtime.proxy_network_is_configured()
                    && !matches!(spec.capabilities.network, NetworkCapability::Deny);
                let cooperative_rootless_chroot = cfg!(target_os = "linux")
                    && descriptor.isolation == IsolationKind::RootlessProcess
                    && !crate::executor::sandbox::landlock_required(&spec)
                    && matches!(
                        dimension,
                        CapabilityDimension::FilesystemRead | CapabilityDimension::FilesystemWrite
                    );
                if !capability_plan.is_planned(dimension)
                    && !cooperative_linux_proxy
                    && !cooperative_rootless_chroot
                {
                    return Err(PVisorError::UnsupportedPolicy {
                        executor: descriptor.name,
                        dimensions: format!("required sandbox: {dimension}"),
                    });
                }
            }
        }
        if spec.runtime.policy_mode == PolicyMode::Enforce {
            let missing = capability_plan
                .missing_dimensions(&spec.capabilities, &spec.runtime.resource_limits);
            if !missing.is_empty() {
                return Err(PVisorError::UnsupportedPolicy {
                    executor: descriptor.name,
                    dimensions: missing
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }
        let operation = crate::runtime::operation::compile(
            &spec,
            &descriptor,
            &capability_plan,
            self.runtime.overlay_hint(),
        )
        .map_err(PVisorError::Prepare)?;
        let mut requested_operation = crate::runtime::operation::compile(
            &requested_spec,
            &descriptor,
            &capability_plan,
            self.runtime.overlay_hint(),
        )
        .map_err(PVisorError::Prepare)?;
        requested_operation.placements.clear();
        // Plans stay typed; caller metadata cannot supply trusted runtime decisions.
        descriptor.capability_plan = capability_plan;
        let preparation = PreparedRun::new(&spec, descriptor.clone(), operation.clone())
            .map_err(PVisorError::Prepare)?;
        Ok(ResolvedRun {
            preparation,
            spec,
            executor,
            descriptor,
            vm_network_executor,
            operation,
            requested_operation,
            network_policy,
        })
    }

    /// Start one Run: resolve Operation → prepare controls → execute → teardown.
    pub async fn run(&self, spec: RunSpec) -> Result<RunHandle, PVisorError> {
        crate::Session::start(
            &self.runtime,
            Arc::clone(&self.event_sink),
            self.resolve_run(spec)?,
        )
        .await
    }
}

fn effective_capability_plan(
    descriptor: &ExecutorPlan,
    spec: &RunSpec,
    proxy_network_configured: bool,
    vm_network_enforcing: bool,
) -> CapabilityEnforcementPlan {
    let mut plan = descriptor.capability_plan.clone();
    if matches!(
        descriptor.isolation,
        IsolationKind::RootlessProcess | IsolationKind::SandboxedProcess
    ) && spec
        .metadata
        .get("pvisor.filesystem.mode")
        .and_then(serde_json::Value::as_str)
        == Some("host")
    {
        plan.dimensions.remove(&CapabilityDimension::FilesystemRead);
        plan.dimensions
            .remove(&CapabilityDimension::FilesystemWrite);
    }
    if proxy_network_configured {
        plan.record(
            CapabilityDimension::Network,
            EnforcementPlanLevel::Cooperative,
            "explicit-proxy-environment",
        );
    }
    if matches!(spec.capabilities.network, NetworkCapability::Deny) {
        match descriptor.isolation {
            IsolationKind::RootlessProcess => plan.record(
                CapabilityDimension::Network,
                EnforcementPlanLevel::Planned,
                "linux-network-namespace",
            ),
            IsolationKind::SandboxedProcess => plan.record(
                CapabilityDimension::Network,
                EnforcementPlanLevel::Planned,
                "macos-seatbelt-network-deny",
            ),
            _ => {}
        }
    }
    if crate::executor::sandbox::sandbox_required(spec)
        && descriptor.isolation == IsolationKind::SandboxedProcess
    {
        plan.record(
            CapabilityDimension::FilesystemRead,
            EnforcementPlanLevel::Planned,
            "macos-seatbelt-read-policy",
        );
        if proxy_network_configured || matches!(spec.capabilities.network, NetworkCapability::Deny)
        {
            plan.record(
                CapabilityDimension::Network,
                EnforcementPlanLevel::Planned,
                "macos-seatbelt-proxy-only",
            );
        }
    }
    if vm_network_enforcing {
        plan.record(
            CapabilityDimension::Network,
            EnforcementPlanLevel::Planned,
            "vm-smoltcp-network-boundary",
        );
    }
    plan
}

fn validate_spec(spec: &RunSpec) -> Result<(), PVisorError> {
    if spec.schema_version != RUNTIME_SCHEMA_VERSION {
        return Err(PVisorError::InvalidSpec(format!(
            "unsupported schema_version {}; expected {}",
            spec.schema_version, RUNTIME_SCHEMA_VERSION
        )));
    }
    if spec.run_id.is_empty() {
        return Err(PVisorError::InvalidSpec("run_id must not be empty".into()));
    }
    let run_id = spec.run_id.as_str().trim();
    if run_id == "." || run_id == ".." || run_id.contains('/') || run_id.contains('\\') {
        return Err(PVisorError::InvalidSpec(
            "run_id must be one non-empty path-safe segment".into(),
        ));
    }
    if spec.agent.name.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "agent.name must not be empty".into(),
        ));
    }
    let pvisor_core::RunInvocation::Process(process) = &spec.invocation;
    if process.program.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "process program must not be empty".into(),
        ));
    }
    if process.stdin == pvisor_core::StdioMode::Capture {
        return Err(PVisorError::InvalidSpec(
            "captured stdin is not supported in pVisor v1".into(),
        ));
    }
    if spec.runtime.max_output_bytes == 0 {
        return Err(PVisorError::InvalidSpec(
            "runtime.max_output_bytes must be greater than zero".into(),
        ));
    }
    let limits = &spec.runtime.resource_limits;
    if [
        limits.memory_bytes,
        limits.processes,
        limits.cpu_time_ms,
        limits.open_files,
        limits.file_size_bytes,
    ]
    .into_iter()
    .flatten()
    .any(|value| value == 0)
    {
        return Err(PVisorError::InvalidSpec(
            "runtime resource limits must be greater than zero when configured".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventSink, MemoryEventSink, Session};
    use async_trait::async_trait;
    use pvisor_core::{
        ExecutorKind, NetworkCapability, RunFailureKind, RunInvocation, RunState, StdioMode,
    };
    use std::sync::Mutex;

    fn starting_vm_controls() -> RunControlHandle {
        let run_id = pvisor_core::RunId::from("control-readiness");
        let attempt_id = AttemptId::from("attempt-readiness");
        let (vm_status, status) = watch::channel(RunStatus {
            run_id: run_id.clone(),
            state: RunState::Starting,
            attempt: pvisor_core::AttemptInfo {
                attempt_id: attempt_id.clone(),
                number: 1,
                executor: ExecutorPlan {
                    name: "readiness-fixture".into(),
                    kind: ExecutorKind::VirtualMachine,
                    isolation: IsolationKind::VirtualMachine,
                    capability_plan: CapabilityEnforcementPlan::default(),
                    supports_checkpoint: false,
                    supports_migration: false,
                },
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
            updated_at_unix_ms: 0,
            message: None,
        });
        let cancellation = CancellationToken::new();
        let (live, _) = broadcast::channel(16);
        RunControlHandle {
            status,
            vm_status,
            vm_control: crate::executor::vm::control::VmControl::new(cancellation.clone()),
            cancellation,
            control_operation: PVisor::new()
                .resolve_operation(RunSpec::process("readiness", "test", "/bin/true"))
                .unwrap(),
            events: RunEventPublisher::new(
                run_id,
                attempt_id,
                "test",
                Arc::new(NoopEventSink::default()),
                live,
            ),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn vm_control_readiness_waits_for_start_and_is_fenced_by_cancellation() {
        let controls = starting_vm_controls();
        let mut pending = controls.clone();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), pending.wait_ready())
                .await
                .is_err()
        );
        controls
            .vm_status
            .send_modify(|status| status.state = RunState::Running);
        pending.wait_ready().await.unwrap();
        controls
            .vm_status
            .send_modify(|status| status.state = RunState::Suspended);
        pending.wait_ready().await.unwrap();
        controls.cancellation.cancel();
        assert!(pending.wait_ready().await.is_err());

        // Cancellation must wake a waiter even before any status update.
        let controls = starting_vm_controls();
        let mut pending = controls.clone();
        let waiter = tokio::spawn(async move { pending.wait_ready().await });
        tokio::task::yield_now().await;
        controls.cancellation.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn vm_control_readiness_rejects_terminal_and_non_vm_attempts() {
        let mut controls = starting_vm_controls();
        controls
            .vm_status
            .send_modify(|status| status.state = RunState::Completed);
        assert!(controls.wait_ready().await.is_err());
        controls.vm_status.send_modify(|status| {
            status.state = RunState::Running;
            status.attempt.executor.kind = ExecutorKind::Process;
        });
        assert!(controls.wait_ready().await.is_err());
    }

    #[test]
    fn preparation_rejects_malformed_provenance_and_ignores_metadata_plans() {
        for key in ["pvisor.lineage", "pvisor.environment", "pvisor.workspace"] {
            let mut spec = RunSpec::process("admission", "agent", "true");
            spec.metadata.insert(key.into(), serde_json::json!(42));
            assert!(PVisor::new().resolve_run(spec).is_err(), "{key}");
        }
        let mut spec = RunSpec::process("admission", "agent", "true");
        spec.metadata.insert(
            "pvisor.executor".into(),
            serde_json::json!({"name": "libkrun-forged"}),
        );
        spec.metadata
            .insert("pvisor.operation".into(), serde_json::Value::Null);
        let resolved = PVisor::new().resolve_run(spec).unwrap();
        assert!(!resolved.preparation.is_krun());
        assert!(!resolved.spec.metadata.contains_key("pvisor.executor"));
        assert!(!resolved.spec.metadata.contains_key("pvisor.operation"));
        assert_eq!(resolved.preparation.operation, resolved.operation);
        assert_eq!(resolved.preparation.executor.name, resolved.descriptor.name);
    }

    #[tokio::test]
    async fn cancellation_finishes_the_attempt() {
        let runtime = PVisor::builder().build();
        let mut spec = RunSpec::process("cancellation", "agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "sleep 30".into()];
        let handle = runtime.run(spec).await.unwrap();
        assert!(handle.pause().await.is_err());
        assert!(handle.resume().await.is_err());
        assert!(handle.offload(None).await.is_err());
        assert!(!handle.cancellation().is_cancelled());
        handle.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.state, RunState::Cancelled);
    }

    #[tokio::test]
    async fn plain_proxy_attempt_commits_results_and_releases_resources() {
        let storage = tempfile::tempdir().unwrap();
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder()
            .event_sink(sink.clone())
            .storage(storage.path())
            .network(NetworkDriverConfig::new(
                crate::OverlayNetMode::Proxy,
                pvisor_overlaynet::NetworkConfig {
                    mode: pvisor_overlaynet::NetworkMode::NoNetwork,
                    ..Default::default()
                },
            ))
            .build();
        let mut spec = RunSpec::process("proxy-attempt", "agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "test -n \"$HTTP_PROXY\"".into()];
        let RunInvocation::Process(process) = &mut spec.invocation;
        process
            .env
            .insert("PRIVATE_KEY".into(), "operation-secret-value".into());
        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed, "{result:?}");
        assert_eq!(
            crate::RunBundle::read(storage.path()).unwrap().run.state,
            RunState::Completed
        );
        let events = sink.events();
        let phases = events
            .iter()
            .filter(|event| event.operation.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            phases.iter().map(|event| event.name()).collect::<Vec<_>>(),
            [
                "requested",
                "rewritten",
                "placed",
                "dispatched",
                "completed"
            ]
        );
        let pvisor_core::event::Fact::Requested {
            operation: requested,
        } = &phases[0].data
        else {
            panic!("requested");
        };
        let pvisor_core::event::Fact::Rewritten { before, after } = &phases[1].data else {
            panic!("rewritten");
        };
        assert_eq!(requested, before);
        assert_eq!(
            requested
                .rules
                .iter()
                .find(|rule| rule.id == "net.aggregate")
                .unwrap()
                .action,
            "ambient"
        );
        assert_eq!(
            after
                .rules
                .iter()
                .find(|rule| rule.id == "net.aggregate")
                .unwrap()
                .action,
            "deny"
        );
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("operation-secret-value")
        );
        for pair in events.windows(2) {
            assert_eq!(pair[1].caused_by, [pair[0].id.clone()]);
        }
        let record = crate::RunRecord::read(storage.path()).unwrap();
        assert!(record.network_interception_metrics.is_some());
        assert!(!storage.path().join("control.sock").exists());
        let _lease = crate::runtime::RunLease::acquire(storage.path()).unwrap();
    }

    #[test]
    fn host_filesystem_mode_only_removes_local_process_filesystem_evidence() {
        let mut process = ExecutorPlan {
            name: "local-rootless-v1".into(),
            kind: ExecutorKind::Process,
            isolation: IsolationKind::RootlessProcess,
            capability_plan: CapabilityEnforcementPlan::default()
                .planned(CapabilityDimension::FilesystemRead, "test-read")
                .planned(CapabilityDimension::FilesystemWrite, "test-write"),
            supports_checkpoint: false,
            supports_migration: false,
        };
        let mut process_spec = RunSpec::process("host-fs", "agent", "/bin/true");
        process_spec.metadata.insert(
            "pvisor.filesystem.mode".into(),
            serde_json::Value::String("host".into()),
        );
        let process_evidence = effective_capability_plan(&process, &process_spec, false, false);
        assert!(!process_evidence.is_planned(CapabilityDimension::FilesystemRead));
        assert!(!process_evidence.is_planned(CapabilityDimension::FilesystemWrite));

        process.isolation = IsolationKind::VirtualMachine;
        process.kind = ExecutorKind::VirtualMachine;
        let vm_evidence = effective_capability_plan(&process, &process_spec, false, false);
        assert!(vm_evidence.is_planned(CapabilityDimension::FilesystemRead));
        assert!(vm_evidence.is_planned(CapabilityDimension::FilesystemWrite));
    }

    #[derive(Default)]
    struct RejectCompletedSink {
        kinds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventSink for RejectCompletedSink {
        async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
            if event.name() == "run.completed" {
                anyhow::bail!("simulated terminal commit failure");
            }
            self.kinds.lock().unwrap().push(event.name().to_string());
            Ok(crate::trace::Journal::memory().append(event.clone())?)
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

    #[derive(Default)]
    struct CommitThenLoseAcknowledgementSink {
        kinds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventSink for CommitThenLoseAcknowledgementSink {
        async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
            self.kinds.lock().unwrap().push(event.name().to_string());
            if event.name() == "run.completed" {
                anyhow::bail!("simulated acknowledgement loss after commit");
            }
            Ok(crate::trace::Journal::memory().append(event.clone())?)
        }
    }

    struct RejectAllTerminalEventsSink;

    #[async_trait]
    impl EventSink for RejectAllTerminalEventsSink {
        async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
            if matches!(
                event.name(),
                "run.completed" | "run.cancelled" | "run.failed"
            ) {
                anyhow::bail!("simulated terminal rejection");
            }
            Ok(crate::trace::Journal::memory().append(event.clone())?)
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

    struct RejectCompletionFact(std::path::PathBuf);
    #[async_trait]
    impl EventSink for RejectCompletionFact {
        async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
            if event.name() == "completed" {
                anyhow::bail!("completion audit unavailable");
            }
            if event.name() == "run.completed" {
                let bundle = crate::RunBundle::read(&self.0)?;
                anyhow::ensure!(
                    bundle
                        .run
                        .warnings
                        .iter()
                        .any(|warning| warning.contains("execution completion audit gap")),
                    "audit gap was not persisted before terminal publication"
                );
            }
            Ok(crate::trace::Journal::memory().append(event.clone())?)
        }
    }

    #[tokio::test]
    async fn completion_audit_gap_is_durable_before_terminal_publication() {
        let storage = tempfile::tempdir().unwrap();
        let runtime = PVisor::builder()
            .storage(storage.path())
            .event_sink(Arc::new(RejectCompletionFact(storage.path().to_path_buf())))
            .build();
        let mut spec = RunSpec::process("audit-gap", "agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];
        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("execution completion audit gap"))
        );
        let bundle = crate::RunBundle::read(storage.path()).unwrap();
        assert_eq!(bundle.run.state, result.state);
        assert_eq!(bundle.run.warnings, result.warnings);
    }

    struct RejectCreatedSink;

    #[async_trait]
    impl EventSink for RejectCreatedSink {
        async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
            if event.name() == "run.created" {
                anyhow::bail!("simulated creation rejection");
            }
            Ok(crate::trace::Journal::memory().append(event.clone())?)
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_creation_finalizes_prepared_run_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder()
            .storage(&storage)
            .event_sink(Arc::new(RejectCreatedSink))
            .build();
        let spec = RunSpec::process("run-created-rejected", "test-agent", "/bin/true");

        let error = match runtime.run(spec).await {
            Ok(_) => panic!("run creation unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::EventSink(_)));

        let record = crate::runtime::RunRecord::read(&storage).unwrap();
        assert_eq!(record.state, crate::RunRecordState::Failed);
        assert!(record.finished_at_unix_ms.is_some());
        let bundle = crate::RunBundle::read(&storage).unwrap();
        assert_eq!(bundle.run.state, RunState::Failed);
        assert_eq!(
            bundle.run.failure.as_ref().map(|failure| failure.kind),
            Some(RunFailureKind::Infrastructure)
        );
        assert!(
            bundle
                .run
                .failure
                .as_ref()
                .unwrap()
                .message
                .contains("event sink rejected run creation")
        );
        assert!(!storage.join("control.sock").exists());
        let _lease = crate::runtime::RunLease::acquire(&storage).unwrap();
    }

    struct InconsistentExecutor;
    #[async_trait]
    impl RunExecutor for InconsistentExecutor {
        fn descriptor(&self) -> ExecutorPlan {
            ProcessExecutor::default().descriptor()
        }
        fn supports(&self, invocation: &RunInvocation) -> bool {
            ProcessExecutor::default().supports(invocation)
        }
        async fn execute(&self, context: &Session) -> crate::ExecutorOutput {
            let mut output = ProcessExecutor::default().execute(context).await;
            // Backends cannot publish a terminal state or declare a nonzero exit successful.
            context.transition(RunState::Completed, None).await;
            assert!(!context.status().state.is_terminal());
            output.state = RunState::Completed;
            output.exit_code = Some(7);
            output.failure = None;
            output
        }
    }
    #[tokio::test]
    async fn session_normalizes_exit_before_terminal_and_bundle_commit() {
        let storage = tempfile::tempdir().unwrap();
        let runtime = PVisor::builder()
            .storage(storage.path())
            .executors(vec![Arc::new(InconsistentExecutor)])
            .build();
        let spec = RunSpec::process("session-outcome", "agent", "/bin/true");
        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.failure.unwrap().kind, RunFailureKind::ProcessExit);
        let bundle = crate::RunBundle::read(storage.path()).unwrap();
        assert_eq!(bundle.run.state, RunState::Failed);
    }

    struct PlannedNetworkExecutor;
    #[async_trait]
    impl RunExecutor for PlannedNetworkExecutor {
        fn descriptor(&self) -> ExecutorPlan {
            let mut plan = ProcessExecutor::default().descriptor();
            plan.capability_plan = plan
                .capability_plan
                .planned(CapabilityDimension::Network, "uninstalled-test-control");
            plan
        }
        fn supports(&self, invocation: &RunInvocation) -> bool {
            ProcessExecutor::default().supports(invocation)
        }
        async fn execute(&self, context: &Session) -> crate::ExecutorOutput {
            ProcessExecutor::default().execute(context).await
        }
    }

    #[tokio::test]
    async fn prestart_cancellation_skips_workload_and_keeps_unknown_evidence() {
        let storage = tempfile::tempdir().unwrap();
        let marker = storage.path().join("must-not-start");
        let runtime = PVisor::builder()
            .storage(storage.path())
            .executors(vec![Arc::new(PlannedNetworkExecutor)])
            .build();
        let mut spec = RunSpec::process("cancel-session", "agent", "/bin/sh");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        spec.capabilities.allow_subprocess = true;
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), format!("touch {}", marker.display())];
        let handle = runtime.run(spec).await.unwrap();
        handle.cancel();
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Cancelled);
        assert!(!marker.exists());
        assert!(
            !result
                .executor_observations
                .enforcement
                .is_enforced(CapabilityDimension::Network)
        );
        assert_eq!(
            crate::RunBundle::read(storage.path()).unwrap().run.state,
            RunState::Cancelled
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enforcement_plan_never_substitutes_for_executor_observations() {
        let storage = tempfile::tempdir().unwrap();
        let runtime = PVisor::builder()
            .storage(storage.path())
            .executors(vec![Arc::new(PlannedNetworkExecutor)])
            .build();
        let mut spec = RunSpec::process("missing-proof", "agent", "/bin/true");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        spec.capabilities.allow_subprocess = true;
        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(
            result
                .failure
                .unwrap()
                .message
                .contains("lack executor observations")
        );
        let bundle = crate::RunBundle::read(storage.path()).unwrap();
        assert!(!bundle.safety.network_non_bypassable);
        assert!(
            bundle
                .executor_plan
                .unwrap()
                .capability_plan
                .is_planned(CapabilityDimension::Network)
        );
        assert!(
            !bundle
                .executor_observations
                .enforcement
                .is_enforced(CapabilityDimension::Network)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_failure_has_runtime_origin_and_no_enforced_claims() {
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let result = runtime
            .run(RunSpec::process(
                "startup-observations",
                "agent",
                "/missing/pvisor-agent",
            ))
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(
            result
                .executor_observations
                .enforcement
                .dimensions
                .is_empty()
        );
        assert!(sink.events().iter().any(|event| matches!(
            event.data,
            pvisor_core::event::Fact::Completed {
                origin: pvisor_core::event::Origin::Runtime,
                ..
            }
        )));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_completes_and_emits_lifecycle() {
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-success", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "printf pvisor".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.run(spec).await.unwrap();
        assert_eq!(handle.status().state, RunState::Created);
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(result.output.stdout.as_deref(), Some("pvisor"));
        assert_eq!(
            result.executor_observations.origin,
            pvisor_core::event::Origin::Backend
        );

        let emitted = sink.events();
        assert!(emitted.iter().all(|event| {
            !event.id.is_empty()
                && event.trace_id == "run-success"
                && event.scope.len() == 4
                && event.producer == "pvisor"
        }));
        let phases: Vec<_> = emitted
            .iter()
            .filter(|event| event.operation.is_some())
            .collect();
        assert!(matches!(
            &phases[0].data,
            pvisor_core::event::Fact::Requested { operation }
                if operation.run_id == "run-success" && operation.placements.is_empty()
        ));
        assert!(matches!(
            &phases.iter().find(|event| event.name() == "dispatched").unwrap().data,
            pvisor_core::event::Fact::Dispatched { run_id, .. } if run_id == "run-success"
        ));
        assert!(matches!(
            phases.last().unwrap().data,
            pvisor_core::event::Fact::Completed { .. }
        ));
        assert!(
            phases
                .iter()
                .all(|event| event.operation == phases[0].operation
                    && event.context == phases[0].context)
        );
        for pair in emitted.windows(2) {
            assert_eq!(pair[1].caused_by, vec![pair[0].id.clone()]);
        }
        let kinds: Vec<_> = emitted
            .into_iter()
            .map(|event| event.name().to_string())
            .collect();
        assert_eq!(kinds.first().map(String::as_str), Some("run.created"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.completed"));
        assert!(kinds.iter().any(|kind| kind == "run.state_changed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_receives_live_agentctl_endpoint() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-agentctl", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "test -S \"$PVISOR_AGENTCTL_ENDPOINT\" && \
             test -n \"$PVISOR_AGENTCTL_TOKEN\" && \
             test \"$PVISOR_AGENTCTL_VERSION\" = 1 && \
             test \"$PVISOR_AGENTCTL_TRANSPORT\" = unix"
                .into(),
        ];

        let handle = runtime.run(spec).await.unwrap();
        assert_eq!(handle.agentctl().snapshot().run_id, "run-agentctl");
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_sink_failure_prevents_completed_result() {
        let sink = Arc::new(RejectCompletedSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-terminal-failure", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.kind),
            Some(RunFailureKind::Infrastructure)
        );
        assert!(
            result
                .failure
                .as_ref()
                .unwrap()
                .message
                .contains("terminal event sink failed")
        );
        let kinds = sink.kinds.lock().unwrap().clone();
        assert!(!kinds.iter().any(|kind| kind == "run.completed"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_terminal_append_does_not_publish_a_conflicting_terminal() {
        let sink = Arc::new(CommitThenLoseAcknowledgementSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-terminal-unknown", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("outcome is unknown"))
        );
        let terminal_kinds = sink
            .kinds
            .lock()
            .unwrap()
            .iter()
            .filter(|kind| {
                matches!(
                    kind.as_str(),
                    "run.completed" | "run.cancelled" | "run.failed"
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(terminal_kinds, vec!["run.completed"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacement_terminal_failure_is_reported_in_the_result() {
        let runtime = PVisor::builder()
            .event_sink(Arc::new(RejectAllTerminalEventsSink))
            .build();
        let mut spec = RunSpec::process("run-terminal-double-reject", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("publish finalization failure event failed"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundle_failure_is_published_as_the_only_terminal_result() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder()
            .storage(&storage)
            .event_sink(sink.clone())
            .build();
        let mut spec = RunSpec::process("run-bundle-failure", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "mkdir -p \"$1\" && mkdir \"$1/run-bundle.json\"".into(),
            "sh".into(),
            storage.display().to_string(),
        ];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(
            result
                .failure
                .as_ref()
                .unwrap()
                .message
                .contains("write durable Run Bundle failed")
        );
        let terminal_kinds = sink
            .events()
            .into_iter()
            .map(|event| event.name().to_string())
            .filter(|kind| {
                matches!(
                    kind.as_str(),
                    "run.completed" | "run.cancelled" | "run.failed"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal_kinds, vec!["run.failed"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn storage_only_run_persists_a_bundle_without_network_drivers() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder().storage(&storage).build();
        let mut spec = RunSpec::process("run-storage-only", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(
            crate::RunBundle::read(&storage).unwrap().run.state,
            RunState::Completed
        );
        assert_eq!(
            crate::runtime::RunRecord::read(&storage).unwrap().state,
            crate::runtime::RunRecordState::Completed
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn durable_run_metadata_records_environment_keys_without_secret_values() {
        const SECRET: &str = "pvisor-secret-value-must-not-be-persisted";
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder().storage(&storage).build();
        let mut spec = RunSpec::process("run-secret-projection", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];
        process.inherit_env = false;
        process
            .env
            .insert("PRIVATE_API_TOKEN".into(), SECRET.into());

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);

        let bundle_raw = std::fs::read_to_string(storage.join(crate::RUN_BUNDLE_FILENAME)).unwrap();
        let record_raw = std::fs::read_to_string(storage.join("run.json")).unwrap();
        assert!(!bundle_raw.contains(SECRET));
        assert!(!record_raw.contains(SECRET));
        let bundle = crate::RunBundle::read(&storage).unwrap();
        assert!(!bundle.environment.inherits_host);
        assert!(
            bundle
                .environment
                .projected_keys
                .iter()
                .any(|key| key == "PRIVATE_API_TOKEN")
        );
    }

    #[tokio::test]
    async fn required_sandbox_refuses_an_unsandboxed_executor_before_launch() {
        let mut spec = RunSpec::process("required-no-fallback", "test", "/bin/true");
        spec.metadata.insert(
            crate::executor::sandbox::REQUIRED_SANDBOX_KEY.into(),
            true.into(),
        );
        let error = match PVisor::new().run(spec).await {
            Ok(_) => panic!("required sandbox silently fell back"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::UnsupportedPolicy { .. }));
    }

    #[tokio::test]
    async fn run_id_must_be_a_capture_safe_path_segment() {
        for invalid in ["../escape", "nested/run", r"nested\run", ".", ".."] {
            let spec = RunSpec::process(invalid, "agent", "echo");
            let error = match PVisor::new().run(spec).await {
                Ok(_) => panic!("invalid run id was accepted: {invalid}"),
                Err(error) => error,
            };
            assert!(matches!(error, PVisorError::InvalidSpec(_)));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_cancels_the_process_tree() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-cancel", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "sleep 30 & wait".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.run(spec).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        handle.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle.wait())
            .await
            .expect("process tree did not terminate")
            .unwrap();
        assert_eq!(result.state, RunState::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_deadline_is_a_typed_failure() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-timeout", "test-agent", "/bin/sleep");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["30".into()];
        process.stdout = StdioMode::Null;
        process.stderr = StdioMode::Null;
        spec.runtime.timeout_ms = Some(20);

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(
            result.failure.unwrap().kind,
            RunFailureKind::DeadlineExceeded
        );
    }

    #[tokio::test]
    async fn host_process_refuses_enforced_policy() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-enforce", "test-agent", "echo");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        let error = match runtime.run(spec).await {
            Ok(_) => panic!("host process must not claim capability enforcement"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            PVisorError::UnsupportedPolicy { dimensions, .. }
                if dimensions == "network, subprocess"
        ));
    }

    #[test]
    fn ambient_network_requires_an_enforced_boundary() {
        let spec = RunSpec::process("run-ambient", "test-agent", "echo");
        assert_eq!(
            pvisor_core::requested_enforcement_dimensions(
                &spec.capabilities,
                &spec.runtime.resource_limits,
            ),
            vec![
                CapabilityDimension::Network,
                CapabilityDimension::Subprocess
            ]
        );
    }

    #[tokio::test]
    #[cfg(feature = "gateway")]
    async fn gateway_driver_does_not_elevate_host_process_enforcement() {
        let proxy = pvisor_gateway::config::ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:19081"
admin_listen = "127.0.0.1:9876"
agent_id = "test"

[[models]]
name = "*"
upstream = "https://example.com"
"#,
        )
        .unwrap();
        let runtime = PVisor::builder()
            .gateway(GatewayDriverConfig::new(proxy))
            .build();
        let mut spec = RunSpec::process("run-enforce-capture", "test-agent", "echo");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        let plan = runtime.plan_for(&spec);
        assert_eq!(
            plan.env.get("PVISOR_OVERLAYNET_DRIVER").map(String::as_str),
            Some("explicit-proxy")
        );
        assert_eq!(
            plan.env
                .get("PVISOR_OVERLAYNET_STRENGTH")
                .map(String::as_str),
            Some("cooperative")
        );
        let error = match runtime.run(spec).await {
            Ok(_) => panic!("explicit proxy capture cannot enforce host process capabilities"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            PVisorError::UnsupportedPolicy { dimensions, .. }
                if dimensions == "network, subprocess"
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires an enabled macFUSE kernel extension"]
    async fn overlay_run_does_not_require_gateway() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let storage = temporary.path().join("storage");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("base.txt"), b"base").unwrap();

        let pvisor = PVisor::builder()
            .storage(&storage)
            .overlay(OverlayHint {
                lower_dirs: vec![target.clone()],
                ..OverlayHint::default()
            })
            .build();
        let mut spec = RunSpec::process("run-overlay-only", "agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "test \"$(cat base.txt)\" = base && printf changed > base.txt && printf new > new.txt"
                .into(),
        ];

        let result = pvisor.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(std::fs::read(target.join("base.txt")).unwrap(), b"base");
        assert!(!target.join("new.txt").exists());

        let record =
            crate::runtime::resolve_run(Some(std::path::Path::new("run-overlay-only")), &storage)
                .unwrap();
        assert!(record.gateway_listen.is_none());
        let mut overlay = record.overlay.unwrap();
        assert_eq!(format!("{:?}", overlay.state), "Staged");
        crate::runtime::apply_overlay(&mut overlay).unwrap();
        assert_eq!(std::fs::read(target.join("base.txt")).unwrap(), b"changed");
        assert_eq!(std::fs::read(target.join("new.txt")).unwrap(), b"new");
    }

    #[test]
    fn builder_injects_runtime_and_network_markers() {
        let pvisor = PVisor::builder().build();
        let mut spec = RunSpec::process("run-implant", "agent", "echo");
        spec.capabilities.network = NetworkCapability::Deny;
        let plan = pvisor.plan_for(&spec);
        assert_eq!(
            plan.env.get("PVISOR_RUNTIME").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env.get("PVISOR_NETWORK_POLICY").map(String::as_str),
            Some("deny")
        );
        assert!(plan.notes.iter().any(|note| note.contains("network")));
    }
}
